//! Exact-record inspection and immutable publication; the owned controller retires.
//!
//! `inspect CONNECTION STREAM SEQUENCE SELECTION [--payload]` is read-only.
//! `redrive SELECTION` is an internal command of messaging-recovery.sh: its
//! connection and one-use grant come from the owned client container's mount.

#[allow(
    clippy::disallowed_types,
    reason = "the finite CLI owns bounded manifest files and durable fsync custody"
)]
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use async_nats::jetstream::ErrorCode;
use async_nats::jetstream::response::Response;
use async_nats::jetstream::stream::RawMessage;
use async_nats::{HeaderMap, HeaderName, HeaderValue};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use infra_messaging::wire::{DeadLetterRecord, restore_dead_letter};
use infra_messaging::{
    CloseOutcome, Messaging, MessagingOptions, MessagingStartup, PreparedEvent, PublishError,
};
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
            payload_base64: STANDARD.encode(&payload),
            payload_sha256: digest(&payload),
        })
    }

    fn prepared(&self) -> Result<PreparedEvent> {
        let payload = STANDARD
            .decode(&self.payload_base64)
            .map_err(|_| "payload_encoding")?;
        if digest(&payload) != self.payload_sha256 {
            return Err("payload_digest");
        }
        let headers = stored_headers(&self.headers_base64)?;
        restore_dead_letter(DeadLetterRecord {
            subject: self.subject.clone(),
            headers,
            payload: payload.into(),
            stream: self.stream.clone(),
            stream_sequence: self.sequence,
            stored_at: OffsetDateTime::parse(&self.stored_at, &Rfc3339).map_err(|_| "timestamp")?,
        })
        .map_err(|_| "unrestorable")
    }
}

fn stored_headers(encoded: &str) -> Result<HeaderMap> {
    // The native RawMessage -> StreamMessage conversion inserts repeated
    // names. Preserve all source values here, in original order, so the wire
    // owner's first-value identity rules also hold for operator reconstruction.
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
        let name: HeaderName = name.parse().map_err(|_| "header_name")?;
        let mut value = value.trim().to_owned();
        while lines
            .peek()
            .is_some_and(|next| next.starts_with([' ', '\t']))
        {
            let continuation = lines.next().ok_or("header_line")?;
            value.push(' ');
            value.push_str(continuation.trim());
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    selection_sha256: String,
    publication: String,
    retirement: String,
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
    startup: Option<MessagingStartup>,
    messaging: Option<Messaging>,
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
            .connection_timeout(Duration::from_secs(5))
            .request_timeout(Some(Duration::from_secs(5)))
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
        let mut complete = true;
        if let Some(messaging) = self.messaging {
            complete &= messaging.close(deadline, &cancel).await == CloseOutcome::Complete;
        }
        if let Some(startup) = self.startup {
            complete &= startup.close(deadline, &cancel).await == CloseOutcome::Complete;
        }
        if let Some((client, mut closed)) = self.native {
            let wait = async {
                if !*closed.borrow() {
                    client.drain().await.map_err(|_| "drain")?;
                }
                while !*closed.borrow_and_update() {
                    closed.changed().await.map_err(|_| "close_unobserved")?;
                }
                Ok::<(), &'static str>(())
            };
            complete &= matches!(tokio::time::timeout_at(deadline, wait).await, Ok(Ok(())));
        }
        complete
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 15)]));
    }
    hex
}

#[allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "finite owned CLI reads at most FILE_LIMIT bytes before decoding its manifest"
)]
fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| "file_read")?;
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

// tempfile creates a private file; persistence and directory fsync cover crash
// boundaries before dispatch. Selections use no-clobber; only state is replaced.
#[allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "finite CLI must fsync its private manifest and directory before broker dispatch"
)]
fn save(path: &Path, value: &impl Serialize, exclusive: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| "file_create")?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|_| "file_write")?;
    file.write_all(b"\n").map_err(|_| "file_write")?;
    file.as_file().sync_all().map_err(|_| "file_sync")?;
    if exclusive {
        file.persist_noclobber(path)
            .map_err(|_| "selection_exists")?;
    } else {
        file.persist(path).map_err(|_| "file_replace")?;
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

#[allow(
    clippy::print_stdout,
    reason = "finite CLI returns its explicit JSON inspection result"
)]
async fn inspect(args: &[String], resources: &mut Resources) -> Result<()> {
    if !(args.len() == 5 || (args.len() == 6 && args[5] == "--payload")) {
        return Err("usage_inspect_connection_stream_sequence_selection");
    }
    let config: Connection = read_json(Path::new(&args[1]))?;
    let sequence: u64 = args[3].parse().map_err(|_| "sequence")?;
    if sequence == 0 {
        return Err("sequence");
    }
    let client = resources.connect(&config).await?;
    let Some(record) = fetch(client, &args[2], sequence).await? else {
        println!(
            "{}",
            json!({"inspection": "missing", "publication": "not_attempted", "retirement": "absent"})
        );
        return Ok(());
    };
    let prepared = record.prepared();
    let selection = Selection {
        version: 1,
        generation: config.generation,
        source_stream: config.source_stream,
        publication_id: prepared
            .as_ref()
            .ok()
            .map(|event| event.publication_id().to_owned()),
        record,
    };
    save(Path::new(&args[4]), &selection, true)?;
    let mut report = json!({
        "inspection": if prepared.is_ok() { "restorable" } else { "malformed_unrestorable" },
        "stream": selection.record.stream,
        "sequence": selection.record.sequence,
        "payload_sha256": selection.record.payload_sha256,
        "publication_id": selection.publication_id,
        "publication": "not_attempted",
        "retirement": "retained",
    });
    if args.len() == 6 {
        report["payload_base64"] = json!(selection.record.payload_base64);
    }
    println!("{report}");
    Ok(())
}

fn owned_connection(selection: &Selection, selection_hash: &str) -> Result<Connection> {
    // The controller creates this private mount and grant only after verifying
    // every original container and network. There is no remote/assumption flag.
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

#[allow(
    clippy::print_stdout,
    reason = "finite CLI returns its explicit JSON publication state"
)]
async fn redrive(args: &[String], resources: &mut Resources, deadline: Instant) -> Result<()> {
    if args.len() != 2 {
        return Err("usage_redrive_selection");
    }
    let path = Path::new(&args[1]);
    let bytes = read_file(path)?;
    let selection: Selection = serde_json::from_slice(&bytes).map_err(|_| "selection_format")?;
    let selection_hash = digest(&bytes);
    if selection.version != 1 {
        return Err("selection_version");
    }
    let config = owned_connection(&selection, &selection_hash)?;
    let prepared = selection.record.prepared()?;
    if selection.publication_id.as_deref() != Some(prepared.publication_id()) {
        return Err("selection_identity");
    }
    let state_path = path.with_extension("state.json");
    #[allow(
        clippy::disallowed_methods,
        reason = "finite owned CLI distinguishes its existing publication journal before dispatch"
    )]
    let mut state = if state_path.exists() {
        let state: State = read_json(&state_path)?;
        if state.version != 1 || state.selection_sha256 != selection_hash {
            return Err("state_identity");
        }
        state
    } else {
        State {
            version: 1,
            selection_sha256: selection_hash,
            publication: "not_attempted".into(),
            retirement: "retained".into(),
            ack: None,
        }
    };
    let client = resources.connect(&config).await?;
    let current = fetch(client, &selection.record.stream, selection.record.sequence).await?;
    if current.as_ref() != Some(&selection.record) {
        state.retirement = if current.is_none() { "absent" } else { "stale" }.into();
        save(&state_path, &state, false)?;
        println!("{}", serde_json::to_value(&state).map_err(|_| "report")?);
        return Ok(());
    }
    // An earlier positive ACK is retained, but never skips the current identity
    // and lifecycle checks. Retrying ambiguity prepares exactly the same ID.
    if state.publication != "confirmed" {
        resources.startup = Some(
            Messaging::prepare(
                MessagingOptions {
                    connection_name: "dlq-recovery-redrive".into(),
                    servers: config.servers,
                    credentials: None,
                    credentials_file: config.credentials_file,
                    root_ca_path: config.root_ca_path,
                    allow_plaintext: false,
                    source_stream: config.source_stream.clone(),
                    dlq_stream: None,
                    max_payload_bytes: config.max_payload_bytes,
                    consumer: None,
                },
                deadline,
                CancellationToken::new(),
            )
            .map_err(|_| "publisher_configuration")?,
        );
        let startup = resources.startup.as_mut().ok_or("publisher_owner")?;
        let Ok(messaging) = startup.admit().await else {
            state.publication = "rejected".into();
            save(&state_path, &state, false)?;
            println!("{}", serde_json::to_value(&state).map_err(|_| "report")?);
            return Ok(());
        };
        let producer = messaging.producer();
        resources.messaging = Some(messaging);
        resources.startup = None;
        state.publication = "ambiguous".into();
        state.ack = None;
        save(&state_path, &state, false)?;
        match producer
            .publish(&prepared, deadline, &CancellationToken::new())
            .await
        {
            Ok(ack) if ack.stream == config.source_stream && ack.sequence > 0 => {
                state.publication = "confirmed".into();
                state.ack = Some(Ack {
                    stream: ack.stream,
                    sequence: ack.sequence,
                    duplicate: ack.duplicate,
                });
            }
            Err(PublishError::Rejected) => state.publication = "rejected".into(),
            Ok(_) | Err(PublishError::Ambiguous) => {}
        }
        save(&state_path, &state, false)?;
    }
    println!("{}", serde_json::to_value(&state).map_err(|_| "report")?);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
#[allow(
    clippy::print_stderr,
    reason = "finite CLI returns sanitized JSON terminal failures"
)]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let deadline = Instant::now() + COMMAND_BUDGET;
    let mut resources = Resources::default();
    let operation = async {
        match args.first().map(String::as_str) {
            Some("inspect") => inspect(&args, &mut resources).await,
            Some("redrive") => redrive(&args, &mut resources, deadline - CLOSE_RESERVE).await,
            _ => Err("usage_inspect_or_owned_redrive"),
        }
    };
    let result = tokio::time::timeout_at(deadline - CLOSE_RESERVE, operation)
        .await
        .unwrap_or(Err("command_deadline"));
    let closed = resources.close(deadline).await;
    if let Err(reason) = result {
        eprintln!(
            "{}",
            json!({"error": reason, "retirement": "refused", "publication": "consult_persisted_state"})
        );
    }
    if !closed {
        eprintln!(
            "{}",
            json!({"error": "close_unobserved", "custody": "retained_by_session"})
        );
    }
    if result.is_ok() && closed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "example manifest assertions"
)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn stored_headers_preserve_repeated_identity_values_and_continuations() {
        let encoded = STANDARD.encode(b"NATS/1.0\r\nMessage-Id: original\r\nMessage-Id: later\r\nX-Note: first\r\n second\r\n\r\n");
        let headers = stored_headers(&encoded).unwrap();
        assert_eq!(headers.get("Message-Id").unwrap().as_str(), "original");
        assert_eq!(headers.get_all("Message-Id").count(), 2);
        assert_eq!(headers.get("X-Note").unwrap().as_str(), "first second");
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "owned synchronous tempfile test observes manifest bytes and private permissions"
    )]
    fn selection_creation_is_exclusive_and_failed_replacement_keeps_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.json");
        save(&path, &json!({"logical_id": "original"}), true).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(
            save(&path, &json!({"logical_id": "different"}), true),
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
}
