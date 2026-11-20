#[expect(
    clippy::disallowed_types,
    reason = "the finite CLI owns bounded manifest files and durable fsync custody"
)]
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::response::Response;
use async_nats::jetstream::stream::RawMessage;
use async_nats::{HeaderMap, HeaderName, HeaderValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use infra_messaging::wire::{DeadLetterRecord, EVENT_TYPE, MESSAGE_ID, restore_dead_letter};
use infra_messaging::{
    BROKER_OPERATION_BUDGET, CloseOutcome, Credentials, EnvelopeError, Messaging, MessagingOptions,
    MessagingStartup, PreparedEvent, PublishError, TransportSecurity,
};
use operation_context::{Deadline, OperationContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, &'static str>;
const FILE_LIMIT: u64 = 4 * 1024 * 1024;
const COMMAND_BUDGET: Duration = Duration::from_secs(30);
const CLOSE_RESERVE: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Connection {
    servers: Vec<String>,
    root_ca_path: Option<PathBuf>,
    credentials_file: Option<PathBuf>,
    #[serde(default)]
    allow_plaintext: bool,
    source_stream: String,
    max_payload_bytes: usize,
    generation: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    stream: String,
    sequence: u64,
    stored_at: String,
    subject: String,
    headers_base64: String,
    payload_base64: String,
    payload_sha256: String,
}

#[derive(Debug)]
enum RestoreError {
    Record(&'static str),
    Envelope(EnvelopeError),
}

impl RestoreError {
    fn reason(&self) -> &'static str {
        match self {
            Self::Record(reason) => reason,
            Self::Envelope(_) => "unrestorable",
        }
    }

    fn write_report_fields(&self, report: &mut serde_json::Value) {
        report["reason"] = json!(self.reason());
        if let Self::Envelope(cause) = self {
            report["detail"] = json!(cause.to_string());
        }
    }
}

impl Record {
    fn from_native(stream: String, message: RawMessage) -> Result<Self> {
        let payload = STANDARD
            .decode(&message.payload)
            .map_err(|_| "payload_encoding")?;
        Ok(Self {
            stream,
            sequence: message.sequence,
            stored_at: message.time.format(&Rfc3339).map_err(|_| "timestamp")?,
            subject: message.subject,
            headers_base64: message.headers.unwrap_or_default(),
            payload_base64: message.payload,
            payload_sha256: digest(&payload),
        })
    }

    fn restore_event(&self) -> std::result::Result<PreparedEvent, RestoreError> {
        let payload = STANDARD
            .decode(&self.payload_base64)
            .map_err(|_| RestoreError::Record("payload_encoding"))?;
        if digest(&payload) != self.payload_sha256 {
            return Err(RestoreError::Record("payload_digest"));
        }
        let headers = stored_headers(&self.headers_base64).map_err(RestoreError::Record)?;
        restore_dead_letter(DeadLetterRecord {
            headers,
            payload: payload.into(),
            stream: self.stream.clone(),
            stream_sequence: self.sequence,
            stored_at: OffsetDateTime::parse(&self.stored_at, &Rfc3339)
                .map_err(|_| RestoreError::Record("timestamp"))?,
        })
        .map_err(RestoreError::Envelope)
    }
}

fn stored_headers(encoded: &str) -> Result<HeaderMap> {
    let raw = STANDARD.decode(encoded).map_err(|_| "header_encoding")?;
    if raw.is_empty() {
        return Ok(HeaderMap::new());
    }
    let text = std::str::from_utf8(&raw).map_err(|_| "header_encoding")?;
    let mut lines = text.split("\r\n").peekable();
    if lines.next() != Some("NATS/1.0") {
        return Err("header_framing");
    }
    let mut headers = HeaderMap::new();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or("header_line")?;
        let preserve_identity = matches!(name, MESSAGE_ID | EVENT_TYPE);
        let name: HeaderName = name.parse().map_err(|_| "header_name")?;
        let mut value = if preserve_identity {
            value.trim_prefix(' ')
        } else {
            value.trim()
        }
        .to_owned();
        while let Some(continuation) = lines.next_if(|next| next.starts_with([' ', '\t'])) {
            if preserve_identity {
                value.push_str(continuation);
            } else {
                value.push(' ');
                value.push_str(continuation.trim());
            }
        }
        let value: HeaderValue = value.parse().map_err(|_| "header_value")?;
        headers.append(name, value);
    }
    Ok(headers)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u8,
    generation: Option<String>,
    source_stream: String,
    record: Record,
    publication_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    stream: String,
    sequence: u64,
    duplicate: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Publication {
    NotAttempted,
    Rejected,
    Ambiguous,
    Confirmed,
}

impl Publication {
    fn after_rejection(self) -> Self {
        match self {
            Self::NotAttempted | Self::Rejected => Self::Rejected,
            Self::Ambiguous | Self::Confirmed => self,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Retirement {
    Retained,
    Absent,
    Stale,
    Refused,
    Unknown,
    Retired,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    selection_sha256: String,
    publication: Publication,
    retirement: Retirement,
    ack: Option<Ack>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    version: u8,
    generation: String,
    client_hostname: String,
    selection_sha256: String,
    operation: String,
}

#[derive(Default)]
struct Resources {
    native: Option<(async_nats::Client, watch::Receiver<bool>)>,
    publisher: Option<MessagingStartup>,
}

impl Resources {
    async fn connect(&mut self, config: &Connection) -> Result<async_nats::Client> {
        if config.servers.is_empty()
            || (!config.allow_plaintext
                && config.servers.iter().any(|url| !url.starts_with("tls://")))
        {
            return Err("tls_required");
        }
        let (closed_tx, closed) = watch::channel(false);
        let mut options = async_nats::ConnectOptions::new()
            .name("dlq-recovery-inspect")
            .connection_timeout(BROKER_OPERATION_BUDGET)
            .request_timeout(Some(BROKER_OPERATION_BUDGET))
            .event_callback(move |event| {
                let closed_tx = closed_tx.clone();
                async move {
                    if matches!(event, async_nats::Event::Closed) {
                        closed_tx.send_replace(true);
                    }
                }
            });
        if let Some(ca) = &config.root_ca_path {
            options = options.add_root_certificates(ca.clone());
        }
        if let Some(path) = &config.credentials_file {
            options = options
                .credentials_file(path)
                .await
                .map_err(|_| "credentials")?;
        }
        let client = options
            .connect(config.servers.clone())
            .await
            .map_err(|_| "connection")?;
        self.native = Some((client.clone(), closed));
        Ok(client)
    }

    async fn close(self, deadline: Instant) -> bool {
        let cancel = CancellationToken::new();
        let publisher_close = match self.publisher {
            Some(startup) => startup.close(deadline, &cancel).await,
            None => CloseOutcome::Complete,
        };
        let mut complete = publisher_close == CloseOutcome::Complete;
        if let Some((client, mut closed)) = self.native {
            let wait = async {
                if !*closed.borrow() {
                    client.drain().await.map_err(|_| "drain")?;
                }
                closed
                    .wait_for(|observed| *observed)
                    .await
                    .map(drop)
                    .map_err(|_| "close_unobserved")
            };
            complete &= matches!(tokio::time::timeout_at(deadline, wait).await, Ok(Ok(())));
        }
        complete
    }
}

fn digest(bytes: &[u8]) -> String {
    const_hex::encode(Sha256::digest(bytes))
}

#[derive(Clone, Copy)]
enum Persist {
    CreateNew,
    Replace,
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "finite owned CLI opens its manifest for a bounded read"
)]
fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| "file_read")?;
    read_open_file(file)
}

#[expect(
    clippy::disallowed_types,
    reason = "finite owned CLI reads at most FILE_LIMIT plus one byte before decoding"
)]
fn read_open_file(file: File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_read")?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("file_limit");
    }
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&read_file(path)?).map_err(|_| "file_format")
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "finite CLI must fsync its private manifest and directory before broker dispatch"
)]
fn save(path: &Path, value: &impl Serialize, persist: Persist) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| "file_create")?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|_| "file_write")?;
    file.write_all(b"\n").map_err(|_| "file_write")?;
    file.as_file().sync_all().map_err(|_| "file_sync")?;
    match persist {
        Persist::CreateNew => {
            file.persist_noclobber(path)
                .map_err(|error| match error.error.kind() {
                    ErrorKind::AlreadyExists => "selection_exists",
                    _ => "selection_persist",
                })?;
        }
        Persist::Replace => {
            file.persist(path).map_err(|_| "file_replace")?;
        }
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "directory_sync")
}

async fn fetch(client: async_nats::Client, stream: &str, sequence: u64) -> Result<Option<Record>> {
    #[derive(Deserialize)]
    struct Stored {
        message: RawMessage,
    }
    let context = async_nats::jetstream::new(client);
    context
        .get_stream(stream)
        .await
        .map_err(|_| "stream_unavailable")?;
    let response: Response<Stored> = context
        .request(
            format!("STREAM.MSG.GET.{stream}"),
            &json!({"seq": sequence}),
        )
        .await
        .map_err(|_| "record_unavailable")?;
    match response {
        Response::Ok(stored) => Record::from_native(stream.to_owned(), stored.message).map(Some),
        Response::Err { error } if error.error_code() == ErrorCode::NO_MESSAGE_FOUND => Ok(None),
        Response::Err { .. } => Err("record_unavailable"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "finite CLI returns its explicit JSON inspection result"
)]
async fn inspect(args: &[String], resources: &mut Resources) -> Result<()> {
    const USAGE: &str = "usage_inspect_connection_stream_sequence_selection";
    let [_, connection, stream, sequence, selection_path, flags @ ..] = args else {
        return Err(USAGE);
    };
    let include_payload = match flags {
        [] => false,
        [flag] if flag == "--payload" => true,
        _ => return Err(USAGE),
    };
    let config: Connection = read_json(Path::new(connection))?;
    let sequence: u64 = sequence.parse().map_err(|_| "sequence")?;
    if sequence == 0 {
        return Err("sequence");
    }
    let client = resources.connect(&config).await?;
    let Some(record) = fetch(client, stream, sequence).await? else {
        println!(
            "{}",
            json!({
                "inspection": "missing",
                "publication": Publication::NotAttempted,
                "retirement": Retirement::Absent,
            })
        );
        return Ok(());
    };
    let (inspection, publication_id, failure) = match record.restore_event() {
        Ok(event) => ("restorable", Some(event.publication_id().to_owned()), None),
        Err(error) => ("malformed_unrestorable", None, Some(error)),
    };
    let selection = Selection {
        version: 1,
        generation: config.generation,
        source_stream: config.source_stream,
        publication_id,
        record,
    };
    save(Path::new(selection_path), &selection, Persist::CreateNew)?;
    let mut report = json!({
        "inspection": inspection,
        "stream": selection.record.stream,
        "sequence": selection.record.sequence,
        "payload_sha256": selection.record.payload_sha256,
        "publication_id": selection.publication_id,
        "publication": Publication::NotAttempted,
        "retirement": Retirement::Retained,
    });
    if let Some(error) = failure {
        error.write_report_fields(&mut report);
    }
    if include_payload {
        report["payload_base64"] = json!(selection.record.payload_base64);
    }
    println!("{report}");
    Ok(())
}

fn owned_connection(selection: &Selection, selection_hash: &str) -> Result<Connection> {
    let root = std::env::var_os("MESSAGING_RECOVERY_SESSION").ok_or("owned_session_required")?;
    let root = Path::new(&root);
    let generation =
        std::env::var("MESSAGING_RECOVERY_GENERATION").map_err(|_| "owned_session_required")?;
    let hostname = std::env::var("HOSTNAME").map_err(|_| "owned_session_required")?;
    let grant: Grant = read_json(&root.join("grant.json"))?;
    let config: Connection = read_json(&root.join("connection.json"))?;
    if grant.version != 1
        || grant.operation != "redrive"
        || grant.generation != generation
        || grant.client_hostname != hostname
        || grant.selection_sha256 != selection_hash
        || selection.generation.as_deref() != Some(generation.as_str())
        || config.generation.as_deref() != Some(generation.as_str())
        || config.source_stream != selection.source_stream
        || config.allow_plaintext
    {
        return Err("stale_or_unowned_session");
    }
    Ok(config)
}

#[expect(
    clippy::print_stdout,
    reason = "finite CLI returns its explicit JSON publication state"
)]
fn finish(state_path: &Path, state: &State) -> Result<()> {
    save(state_path, state, Persist::Replace)?;
    println!("{}", serde_json::to_value(state).map_err(|_| "report")?);
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "finite owned CLI distinguishes its existing publication journal before dispatch"
)]
fn read_state(path: &Path, selection_sha256: String) -> Result<State> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(State {
                version: 1,
                selection_sha256,
                publication: Publication::NotAttempted,
                retirement: Retirement::Retained,
                ack: None,
            });
        }
        Err(_) => return Err("file_read"),
    };
    let state: State = serde_json::from_slice(&read_open_file(file)?).map_err(|_| "file_format")?;
    if state.version != 1 || state.selection_sha256 != selection_sha256 {
        return Err("state_identity");
    }
    Ok(state)
}

async fn redrive(args: &[String], resources: &mut Resources, deadline: Instant) -> Result<()> {
    let [_, selection_path] = args else {
        return Err("usage_redrive_selection");
    };
    let path = Path::new(selection_path);
    let bytes = read_file(path)?;
    let selection: Selection = serde_json::from_slice(&bytes).map_err(|_| "selection_format")?;
    let selection_hash = digest(&bytes);
    if selection.version != 1 {
        return Err("selection_version");
    }
    let config = owned_connection(&selection, &selection_hash)?;
    let prepared = selection
        .record
        .restore_event()
        .map_err(|error| error.reason())?;
    if selection.publication_id.as_deref() != Some(prepared.publication_id()) {
        return Err("selection_identity");
    }
    let state_path = path.with_extension("state.json");
    let mut state = read_state(&state_path, selection_hash)?;
    let client = resources.connect(&config).await?;
    let current = fetch(client, &selection.record.stream, selection.record.sequence).await?;
    if current.as_ref() != Some(&selection.record) {
        state.retirement = if current.is_none() {
            Retirement::Absent
        } else {
            Retirement::Stale
        };
        finish(&state_path, &state)?;
        return Ok(());
    }
    if state.publication != Publication::Confirmed {
        let publication_after_rejection = state.publication.after_rejection();
        let startup = Messaging::prepare(
            MessagingOptions {
                connection_name: "dlq-recovery-redrive".into(),
                servers: config.servers,
                credentials: config.credentials_file.map(Credentials::File),
                root_ca_path: config.root_ca_path,
                security: TransportSecurity::Tls,
                source_stream: config.source_stream.clone(),
                max_payload_bytes: config.max_payload_bytes,
                consumer: None,
            },
            deadline,
            CancellationToken::new(),
        )
        .map_err(|_| "publisher_configuration")?;
        let startup = resources.publisher.insert(startup);
        let Ok(messaging) = startup.admit().await else {
            state.publication = publication_after_rejection;
            finish(&state_path, &state)?;
            return Ok(());
        };
        let producer = messaging.producer();
        state.publication = Publication::Ambiguous;
        state.ack = None;
        save(&state_path, &state, Persist::Replace)?;
        match producer
            .publish(
                &OperationContext::from_deadline(Deadline::at(deadline)),
                &prepared,
            )
            .await
        {
            Ok(ack) if ack.stream == config.source_stream && ack.sequence > 0 => {
                state.publication = Publication::Confirmed;
                state.ack = Some(Ack {
                    stream: ack.stream,
                    sequence: ack.sequence,
                    duplicate: ack.duplicate,
                });
            }
            Err(PublishError::Rejected(_)) => state.publication = publication_after_rejection,
            Ok(_) | Err(PublishError::Ambiguous) => {}
        }
        save(&state_path, &state, Persist::Replace)?;
    }
    finish(&state_path, &state)?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
#[expect(
    clippy::disallowed_methods,
    reason = "finite CLI returns sanitized JSON terminal failures"
)]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let deadline = Instant::now() + COMMAND_BUDGET;
    let mut resources = Resources::default();
    let operation = async {
        match args.first().map(String::as_str) {
            Some("inspect") => inspect(&args, &mut resources).await,
            Some("redrive") => {
                Box::pin(redrive(&args, &mut resources, deadline - CLOSE_RESERVE)).await
            }
            _ => Err("usage_inspect_or_owned_redrive"),
        }
    };
    let result = tokio::time::timeout_at(deadline - CLOSE_RESERVE, operation)
        .await
        .unwrap_or(Err("command_deadline"));
    let closed = resources.close(deadline).await;
    if result.is_ok() && closed {
        return ExitCode::SUCCESS;
    }
    let mut stderr = io::stderr().lock();
    if let Err(reason) = result {
        let _ = writeln!(
            stderr,
            "{}",
            json!({
                "error": reason,
                "retirement": Retirement::Refused,
                "publication": "consult_persisted_state",
            })
        );
    }
    if !closed {
        let _ = writeln!(
            stderr,
            "{}",
            json!({"error": "close_unobserved", "custody": "retained_by_session"})
        );
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        clippy::exit,
        reason = "the subprocess fixture polls for exit, reaps the child on timeout, and exits directly from the child branch"
    )]
    fn invalid_invocation_returns_failure_with_a_broken_stderr_pipe() {
        use std::process::{Command, Stdio};

        const CHILD: &str = "DLQ_RECOVERY_BROKEN_STDERR_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(super::main(), ExitCode::FAILURE);
            std::process::exit(1);
        }

        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::invalid_invocation_returns_failure_with_a_broken_stderr_pipe",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdout(Stdio::null())
            .stderr(writer)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("dlq_recovery fixture did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(1));
    }

    #[test]
    fn a_rejected_retry_preserves_earlier_publication_uncertainty() {
        std::assert_matches!(
            Publication::Ambiguous.after_rejection(),
            Publication::Ambiguous
        );
        std::assert_matches!(
            Publication::NotAttempted.after_rejection(),
            Publication::Rejected
        );
    }

    #[test]
    fn stored_headers_preserve_repeated_identity_values_and_continuations() {
        let encoded = STANDARD.encode(b"NATS/1.0\r\nMessage-Id: original\r\nMessage-Id: later\r\nX-Note: first\r\n second\r\n\r\n");
        let headers = stored_headers(&encoded).unwrap();
        assert_eq!(headers.get("Message-Id").unwrap().as_str(), "original");
        assert_eq!(headers.get_all("Message-Id").count(), 2);
        assert_eq!(headers.get("X-Note").unwrap().as_str(), "first second");
    }

    #[test]
    fn raw_record_cannot_redrive_a_trimmed_identity() {
        let payload = b"{}";
        let mut record = Record {
            stream: "DLQ".to_owned(),
            sequence: 1,
            stored_at: "2026-09-29T10:00:00Z".to_owned(),
            subject: "orders.dlq".to_owned(),
            headers_base64: String::new(),
            payload_base64: STANDARD.encode(payload),
            payload_sha256: digest(payload),
        };
        for (id, event_type, restorable) in [
            ("event-1", "order.created", true),
            (" event-1", "order.created", false),
            ("event-1 ", "order.created", false),
            ("event-1", "order.created ", false),
        ] {
            record.headers_base64 = STANDARD.encode(format!(
                "NATS/1.0\r\nMessage-Id: {id}\r\nEvent-Type: {event_type}\r\nEvent-Schema: v2\r\nCreated-At: 2026-09-29T10:00:00Z\r\nOriginal-Subject: orders.created\r\n\r\n"
            ));
            if restorable {
                assert_eq!(record.restore_event().unwrap().message_id(), "event-1");
            } else {
                let error = record.restore_event().unwrap_err();
                let mut report = json!({});
                error.write_report_fields(&mut report);
                assert_eq!(
                    report,
                    json!({"reason": "unrestorable", "detail": "event identity is invalid"})
                );
            }
        }
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "owned synchronous tempfile test observes manifest bytes and private permissions"
    )]
    fn selection_creation_is_exclusive_and_failed_replacement_keeps_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.json");
        save(
            &path,
            &json!({"logical_id": "original"}),
            Persist::CreateNew,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            save(
                &path,
                &json!({"logical_id": "different"}),
                Persist::CreateNew
            ),
            Err("selection_exists")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn invalid_selection_destination_is_reported_as_a_persistence_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x".repeat(4096));
        assert_eq!(
            save(&path, &json!({}), Persist::CreateNew),
            Err("selection_persist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_inaccessible_journal_path_does_not_start_a_new_publication() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().join("selection.state.json");
        assert!(matches!(read_state(&path, String::new()), Err("file_read")));
    }
}
