# async-nats 0.50.0 transport repair

Source: the published `async-nats-0.50.0.crate` archive, verified before
extraction against the registry checksum retained in the original Cargo.lock:
`d83a251fa1a4c9d0fe6e816b7acd60549e473e08d14f27a1d992c2675abff05f`.
Its `.cargo_vcs_info.json` identifies upstream commit
`9b382a2a01b5404cd66bee6c2b4f0c82c9943063`, path `async-nats`, in
[nats.rs](https://github.com/nats-io/nats.rs).
The published normalized manifests, upstream lockfile and license files are
retained unchanged. No dependency version or feature is upgraded.

The accepted repair bounds each native server attempt across DNS, TCP,
TLS, INFO and authentication using one deadline and candidate time shares;
native trust-store loading moves to an awaited blocking operation. Native
Client exposes force-close and observed runner completion while raw Subscriber
ownership retains its existing lifetime. The native retry, consumer and
settlement owners remain in place.

Runtime delta is limited to `src/connector.rs`, `src/client.rs`, `src/lib.rs`
and `src/tls.rs`. Nine native regression tests live inside the existing `src/lib.rs` test
module. The published README logo reference is removed because its target is
not included in the published crate archive; no documentation asset is copied.
Only `PATCHES.md` is added to the published tree. The complete changed-file
inventory and exact diff follow below.

Retire this patch only when a published client supplies both equivalent
bounded candidate recovery and observed forced termination, and the affected
cancellation, Subscriber lifetime, recovery and finality proof passes. Remove
the workspace patch/exclusion, Docker source/context entries and optional
messaging profile removal entries together with the retired vendor.

## Dependency and delivery custody

The root path patch selects this excluded non-workspace package. Source
selection reused the initializer's guarded `_lock_records` parser, required
the exact registry identity and verified checksum, and removed only async-nats's
source/checksum fields. The same guarded generation derived the two new
infra-messaging test edges from its manifest: already-resolved rcgen 0.14.10
and tokio-rustls 0.26.6. Every package version, other dependency vector and
non-target package byte was preserved; parsed before/after data admitted only
those named changes before an atomic write. No dependency resolution or
unlocked Cargo invocation was used.

- Lock before this projection: `9507b2d057a2f72dc929f802923e2ee947ef406b9d268e1ec06c48bf45401b14`.
- Lock after this projection: `1479c2eb8bfbce28c192deed1ae54809e06e69f0ce1535ca39f96cbe6bad0085`.

The messaging profile owns the root patch and workspace exclusion, vendor
removal, Docker context admission and cooked-layer source copy. Its TLS
discovery tests retain the existing TLS fixture dependencies when messaging
is the only transport profile. Classification selects Rust/dependency,
messaging integration, runtime-image and initializer proof. The assembled
delivery's locked graph/build and profile checks own validation.

The existing `make test` owner also runs the nine native regressions through
this package's unchanged published lockfile, with only the retained
`jetstream,aws-lc-rs,nkeys` features and `transport_resilience` lib-test filter.
This standalone test graph uses Tokio 1.52.3 and rustls 0.23.40; the production
workspace graph uses its own locked versions. Workspace builds and adapter
tests remain the production-graph proof. Locked standalone metadata confirms
that the native harness dependencies are obtainable, not that its tests pass.

## Exact published-source delta

| File | Published SHA256 | Patched SHA256 |
| --- | --- | --- |
| `src/client.rs` | `47c469411809864cf2448d29ab58839c3d19b92808f53c7bca97ff946ac5829f` | `9e65df99ded423fed3d15d49257a8809ead9e6a42ea7b24a4efe9f3a384cc20b` |
| `src/connector.rs` | `dc064bd6623ac345125b1c93db044424615572350a3e1553d44d2cc2d4b37267` | `cbab2ddecfbedfa2140593b6216f39dc4f235787fe8643fef8a63489a24babb6` |
| `src/lib.rs` | `90e270319d172fa339ba822ec92ab4295c32a881bee393394c7f8b511a553ec1` | `d877e3153a7b0dee8f90da7ca5ddc6b30e93d19d6e11472b212b1c6e266b0762` |
| `src/tls.rs` | `73c26aa759d7a30cafc1a51558abfeea3a7b2a36574782c91ae57d81fe010961` | `25a7384509cf87c5d5df743faf69ad59a572d6332d9868374cade5303732a80f` |
| `README.md` | `32dd7c1718f89d5c6cf29a5c159fd9a6372cf9e641010515637dfb089d693b0d` | `c5bc4e503d40dbb5bfc7989f17b74f232eba52599d23db5a7b3af0f37ea8aedf` |

```diff
--- a/src/client.rs
+++ b/src/client.rs
@@ -94,6 +94,8 @@
     pub(crate) state: tokio::sync::watch::Receiver<State>,
     pub(crate) sender: mpsc::Sender<Command>,
     poll_sender: PollSender<Command>,
+    close_sender: tokio::sync::watch::Sender<bool>,
+    closed: tokio::sync::watch::Receiver<bool>,
     next_subscription_id: Arc<AtomicU64>,
     subscription_capacity: usize,
     inbox_prefix: Arc<str>,
@@ -237,6 +239,8 @@
         info: tokio::sync::watch::Receiver<Option<ServerInfo>>,
         state: tokio::sync::watch::Receiver<State>,
         sender: mpsc::Sender<Command>,
+        close_sender: tokio::sync::watch::Sender<bool>,
+        closed: tokio::sync::watch::Receiver<bool>,
         capacity: usize,
         inbox_prefix: String,
         request_timeout: Option<Duration>,
@@ -250,6 +254,8 @@
             state,
             sender,
             poll_sender,
+            close_sender,
+            closed,
             next_subscription_id: Arc::new(AtomicU64::new(1)),
             subscription_capacity: capacity,
             inbox_prefix: inbox_prefix.into(),
@@ -258,6 +264,21 @@
             connection_stats: statistics,
             skip_subject_validation,
         }
+    }
+
+    /// Requests immediate termination of the connection runner, including reconnects.
+    /// This does not wait for termination. Use [`Client::wait_closed`] to observe it.
+    pub fn force_close(&self) {
+        self.close_sender.send_replace(true);
+    }
+
+    /// Waits until the connection runner has dropped its owned resources.
+    /// Returns false if the runner disappears without reporting completion.
+    /// This method does not initiate shutdown or impose a timeout.
+    pub async fn wait_closed(&self) -> bool {
+        let mut closed = self.closed.clone();
+        let observed = closed.wait_for(|closed| *closed).await.is_ok();
+        observed
     }
 
     /// Validates a subject for publishing (protocol-framing safety only).
@@ -814,7 +835,12 @@
             })
             .await?;
 
-        Ok(Subscriber::new(sid, self.sender.clone(), receiver))
+        Ok(Subscriber::new(
+            sid,
+            self.sender.clone(),
+            self.close_sender.clone(),
+            receiver,
+        ))
     }
 
     /// Subscribes to a subject with a queue group to receive [messages][Message].
@@ -863,7 +889,12 @@
             })
             .await?;
 
-        Ok(Subscriber::new(sid, self.sender.clone(), receiver))
+        Ok(Subscriber::new(
+            sid,
+            self.sender.clone(),
+            self.close_sender.clone(),
+            receiver,
+        ))
     }
 
     /// Flushes the internal buffer ensuring that all messages are sent.
--- a/src/connector.rs
+++ b/src/connector.rs
@@ -50,7 +50,7 @@
 use std::sync::Arc;
 use std::time::Duration;
 use tokio::net::{TcpSocket, TcpStream};
-use tokio::time::sleep;
+use tokio::time::{sleep, timeout_at, Instant};
 use tokio_rustls::rustls;
 
 /// Metadata about a server in the connection pool.
@@ -398,75 +398,101 @@
         &mut self,
         server_addr: &ServerAddr,
     ) -> Result<(ServerInfo, Connection), ConnectError> {
-        let socket_addrs = server_addr
-            .socket_addrs()
-            .await
-            .map_err(|err| ConnectError::with_source(crate::ConnectErrorKind::Dns, err))?;
-
-        let mut last_err = None;
-        for socket_addr in socket_addrs {
-            match tokio::time::timeout(
-                self.options.connection_timeout,
-                self.try_connect_to(
-                    &socket_addr,
-                    server_addr.tls_required(),
-                    server_addr.clone(),
-                ),
-            )
-            .await
-            {
-                Ok(Ok((server_info, connection))) => {
-                    tracing::info!(
-                        server = %server_info.port,
-                        max_payload = %server_info.max_payload,
-                        "connected successfully"
-                    );
-                    self.attempts = 0;
-                    self.connect_stats.connects.add(1, Ordering::Relaxed);
-                    self.events_tx.try_send(Event::Connected).ok();
-                    self.state_tx.send(State::Connected).ok();
-                    self.max_payload.store(
-                        server_info.max_payload,
-                        std::sync::atomic::Ordering::Relaxed,
-                    );
-                    self.last_info = server_info.clone();
-
-                    // Update per-server state on success.
-                    if let Some(entry) = self.servers.iter_mut().find(|s| s.addr == *server_addr) {
-                        entry.did_connect = true;
-                        entry.failed_attempts = 0;
-                        entry.last_error = None;
-                    }
-
-                    return Ok((server_info, connection));
-                }
-
-                Ok(Err(inner)) => {
-                    // Update per-server state on failure.
-                    if let Some(entry) = self.servers.iter_mut().find(|s| s.addr == *server_addr) {
-                        entry.failed_attempts += 1;
-                        entry.last_error = Some(inner.to_string());
-                    }
-                    last_err = Some(inner);
-                }
-
-                Err(_) => {
-                    tracing::debug!(
-                        server = ?server_addr,
-                        "connection handshake timed out"
-                    );
-                    if let Some(entry) = self.servers.iter_mut().find(|s| s.addr == *server_addr) {
-                        entry.failed_attempts += 1;
-                        entry.last_error = Some("timed out".to_string());
-                    }
-                    last_err = Some(ConnectError::new(crate::ConnectErrorKind::TimedOut));
-                }
-            }
-        }
-
-        Err(last_err.unwrap_or_else(|| {
-            ConnectError::with_source(crate::ConnectErrorKind::Dns, "no addresses resolved")
-        }))
+        let deadline = Instant::now() + self.options.connection_timeout;
+        let attempt = async {
+            let socket_addrs: Vec<_> = server_addr
+                .socket_addrs()
+                .await
+                .map_err(|err| ConnectError::with_source(crate::ConnectErrorKind::Dns, err))?
+                .collect();
+
+            let mut last_err = None;
+            let mut socket_addrs = socket_addrs.into_iter();
+            while let Some(socket_addr) = socket_addrs.next() {
+                let now = Instant::now();
+                let remaining = deadline.saturating_duration_since(now);
+                let candidates = u32::try_from(socket_addrs.len() + 1).unwrap_or(u32::MAX);
+                let candidate_deadline = now + remaining / candidates;
+                match timeout_at(
+                    candidate_deadline,
+                    self.try_connect_to(
+                        &socket_addr,
+                        server_addr.tls_required(),
+                        server_addr.clone(),
+                    ),
+                )
+                .await
+                {
+                    Ok(Ok((server_info, connection))) => {
+                        tracing::info!(
+                            server = %server_info.port,
+                            max_payload = %server_info.max_payload,
+                            "connected successfully"
+                        );
+                        self.attempts = 0;
+                        self.connect_stats.connects.add(1, Ordering::Relaxed);
+                        self.events_tx.try_send(Event::Connected).ok();
+                        self.state_tx.send(State::Connected).ok();
+                        self.max_payload.store(
+                            server_info.max_payload,
+                            std::sync::atomic::Ordering::Relaxed,
+                        );
+                        self.last_info = server_info.clone();
+
+                        // Update per-server state on success.
+                        if let Some(entry) =
+                            self.servers.iter_mut().find(|s| s.addr == *server_addr)
+                        {
+                            entry.did_connect = true;
+                            entry.failed_attempts = 0;
+                            entry.last_error = None;
+                        }
+
+                        return Ok((server_info, connection));
+                    }
+
+                    Ok(Err(inner)) => {
+                        // Update per-server state on failure.
+                        if let Some(entry) =
+                            self.servers.iter_mut().find(|s| s.addr == *server_addr)
+                        {
+                            entry.failed_attempts += 1;
+                            entry.last_error = Some(inner.to_string());
+                        }
+                        last_err = Some(inner);
+                    }
+
+                    Err(_) => {
+                        tracing::debug!(
+                            server = ?server_addr,
+                            "connection handshake timed out"
+                        );
+                        if let Some(entry) =
+                            self.servers.iter_mut().find(|s| s.addr == *server_addr)
+                        {
+                            entry.failed_attempts += 1;
+                            entry.last_error = Some("timed out".to_string());
+                        }
+                        last_err = Some(ConnectError::new(crate::ConnectErrorKind::TimedOut));
+                    }
+                }
+            }
+
+            Err(last_err.unwrap_or_else(|| {
+                ConnectError::with_source(crate::ConnectErrorKind::Dns, "no addresses resolved")
+            }))
+        };
+
+        match timeout_at(deadline, attempt).await {
+            Ok(result) => result,
+            Err(_) => {
+                if let Some(entry) = self.servers.iter_mut().find(|s| s.addr == *server_addr) {
+                    entry.failed_attempts += 1;
+                    entry.last_error = Some("timed out".to_string());
+                }
+                Err(ConnectError::new(crate::ConnectErrorKind::TimedOut))
+            }
+        }
     }
 
     pub(crate) async fn try_connect_to(
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -689,8 +689,6 @@
                     debug!("reconnected");
                 }
                 ExitReason::Closed => {
-                    // Safe to ignore result as we're shutting down anyway
-                    self.connector.events_tx.try_send(Event::Closed).ok();
                     break;
                 }
                 ExitReason::ReconnectRequested => {
@@ -1088,10 +1086,17 @@
     let (info_sender, info_watcher) = tokio::sync::watch::channel(info.clone());
     let (sender, mut receiver) = mpsc::channel(options.sender_capacity);
 
+    let (close_sender, mut close_receiver) = tokio::sync::watch::channel(false);
+    let (closed_sender, closed_receiver) = tokio::sync::watch::channel(false);
+    let closed_events = connector.events_tx.clone();
+    let closed_state = connector.state_tx.clone();
+
     let client = Client::new(
         info_watcher,
         state_rx,
         sender,
+        close_sender,
+        closed_receiver,
         options.subscription_capacity,
         options.inbox_prefix,
         options.request_timeout,
@@ -1110,21 +1115,35 @@
     });
 
     task::spawn(async move {
-        if connection.is_none() && options.retry_on_initial_connect {
-            let (info, connection_ok) = match connector.connect().await {
-                Ok((info, connection)) => (info, connection),
-                Err(err) => {
-                    error!("connection closed: {}", err);
-                    return;
+        // The runner owns all transport work, including initial and later recovery.
+        // Dropping it releases the handler, socket, connector, and command queue.
+        {
+            let runner = async move {
+                if connection.is_none() && options.retry_on_initial_connect {
+                    let (info, connection_ok) = match connector.connect().await {
+                        Ok((info, connection)) => (info, connection),
+                        Err(err) => {
+                            error!("connection closed: {}", err);
+                            return;
+                        }
+                    };
+                    info_sender.send(Some(info)).ok();
+                    connection = Some(connection_ok);
                 }
+                let connection = connection.unwrap();
+                let mut connection_handler =
+                    ConnectionHandler::new(connection, connector, info_sender, ping_period);
+                connection_handler.process(&mut receiver).await
             };
-            info_sender.send(Some(info)).ok();
-            connection = Some(connection_ok);
-        }
-        let connection = connection.unwrap();
-        let mut connection_handler =
-            ConnectionHandler::new(connection, connector, info_sender, ping_period);
-        connection_handler.process(&mut receiver).await
+            tokio::select! {
+                biased;
+                _ = close_receiver.wait_for(|close| *close) => {}
+                _ = runner => {}
+            }
+        }
+        closed_state.send_replace(State::Disconnected);
+        closed_events.try_send(Event::Closed).ok();
+        closed_sender.send_replace(true);
     });
 
     Ok(client)
@@ -1290,18 +1309,21 @@
     sid: u64,
     receiver: mpsc::Receiver<Message>,
     sender: mpsc::Sender<Command>,
+    close_sender: tokio::sync::watch::Sender<bool>,
 }
 
 impl Subscriber {
     fn new(
         sid: u64,
         sender: mpsc::Sender<Command>,
+        close_sender: tokio::sync::watch::Sender<bool>,
         receiver: mpsc::Receiver<Message>,
     ) -> Subscriber {
         Subscriber {
             sid,
             sender,
             receiver,
+            close_sender,
         }
     }
 
@@ -1423,12 +1445,14 @@
         self.receiver.close();
         tokio::spawn({
             let sender = self.sender.clone();
+            let close_sender = self.close_sender.clone();
             let sid = self.sid;
             async move {
                 sender
                     .send(Command::Unsubscribe { sid, max: None })
                     .await
                     .ok();
+                drop(close_sender);
             }
         });
     }
@@ -1839,6 +1863,347 @@
 #[cfg(test)]
 mod tests {
     use super::*;
+
+    mod transport_resilience {
+        use super::*;
+        use futures_util::{FutureExt, StreamExt};
+        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
+        use tokio::net::{TcpListener, TcpStream};
+
+        const BOUND: Duration = Duration::from_secs(3);
+        const INFO: &[u8] = b"INFO {\"server_id\":\"fixture\",\"max_payload\":1048576}\r\n";
+
+        async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
+            tokio::time::timeout(BOUND, future)
+                .await
+                .expect("bounded native transport")
+        }
+
+        async fn line(peer: &mut BufReader<TcpStream>) -> String {
+            let mut line = String::new();
+            assert_ne!(within(peer.read_line(&mut line)).await.unwrap(), 0);
+            line
+        }
+
+        async fn command(peer: &mut BufReader<TcpStream>) -> String {
+            loop {
+                let command = line(peer).await;
+                if command == "PING\r\n" {
+                    within(peer.get_mut().write_all(b"PONG\r\n"))
+                        .await
+                        .unwrap();
+                } else {
+                    return command;
+                }
+            }
+        }
+
+        async fn handshake(stream: TcpStream) -> BufReader<TcpStream> {
+            let mut peer = BufReader::new(stream);
+            within(peer.get_mut().write_all(INFO)).await.unwrap();
+            assert!(line(&mut peer).await.starts_with("CONNECT "));
+            assert_eq!(line(&mut peer).await, "PING\r\n");
+            within(peer.get_mut().write_all(b"PONG\r\n")).await.unwrap();
+            peer
+        }
+
+        async fn connected(options: ConnectOptions) -> (TcpListener, Client, BufReader<TcpStream>) {
+            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+            let addr = listener.local_addr().unwrap().to_string();
+            let (client, peer) = within(async {
+                tokio::join!(options.connect(addr), async {
+                    handshake(listener.accept().await.unwrap().0).await
+                })
+            })
+            .await;
+            (listener, client.unwrap(), peer)
+        }
+
+        fn events() -> (ConnectOptions, mpsc::UnboundedReceiver<Event>) {
+            let (tx, rx) = mpsc::unbounded_channel();
+            (
+                ConnectOptions::new().event_callback(move |event| {
+                    let tx = tx.clone();
+                    async move {
+                        tx.send(event).ok();
+                    }
+                }),
+                rx,
+            )
+        }
+
+        async fn closed_events(mut events: mpsc::UnboundedReceiver<Event>) -> usize {
+            within(async move {
+                let mut closed = 0;
+                while let Some(event) = events.recv().await {
+                    closed += usize::from(event == Event::Closed);
+                }
+                closed
+            })
+            .await
+        }
+
+        // Occupy the runtime's only blocking thread so the real system resolver
+        // cannot finish. No replacement DNS implementation is involved.
+        async fn block_resolver() -> (std::sync::mpsc::Sender<()>, task::JoinHandle<()>) {
+            let (release, held) = std::sync::mpsc::channel();
+            let (started, ready) = tokio::sync::oneshot::channel();
+            let blocker = task::spawn_blocking(move || {
+                started.send(()).unwrap();
+                let _ = held.recv_timeout(BOUND * 2);
+            });
+            within(ready).await.unwrap();
+            (release, blocker)
+        }
+
+        #[test]
+        fn pending_system_dns_spends_the_server_attempt_budget() {
+            let runtime = tokio::runtime::Builder::new_current_thread()
+                .enable_all()
+                .max_blocking_threads(1)
+                .build()
+                .unwrap();
+            runtime.block_on(async {
+                let (release, blocker) = block_resolver().await;
+                let result = tokio::time::timeout(
+                    Duration::from_secs(1),
+                    ConnectOptions::new()
+                        .connection_timeout(Duration::from_millis(100))
+                        .connect("localhost:4222"),
+                )
+                .await;
+                release.send(()).unwrap();
+                within(blocker).await.unwrap();
+                assert_eq!(
+                    result.expect("native DNS deadline").unwrap_err().kind(),
+                    ConnectErrorKind::TimedOut
+                );
+            });
+        }
+
+        #[tokio::test]
+        async fn stalled_first_address_leaves_time_for_the_next_address() {
+            let addrs: Vec<_> = within(tokio::net::lookup_host(("localhost", 0)))
+                .await
+                .unwrap()
+                .collect();
+            let first_ip = addrs[0].ip();
+            let next_ip = addrs
+                .iter()
+                .find(|addr| addr.ip() != first_ip)
+                .expect("localhost fixture must resolve both loopback families")
+                .ip();
+            let first = TcpListener::bind((first_ip, 0)).await.unwrap();
+            let port = first.local_addr().unwrap().port();
+            let next = TcpListener::bind((next_ip, port)).await.unwrap();
+            let stalled = task::spawn(async move {
+                let (mut socket, _) = within(first.accept()).await.unwrap();
+                let mut bytes = Vec::new();
+                within(socket.read_to_end(&mut bytes)).await.unwrap();
+                assert!(bytes.is_empty(), "no CONNECT before INFO");
+            });
+            let healthy = task::spawn(async move {
+                let mut peer = handshake(within(next.accept()).await.unwrap().0).await;
+                let mut bytes = Vec::new();
+                within(peer.read_to_end(&mut bytes)).await.unwrap();
+            });
+            let client = tokio::time::timeout(
+                Duration::from_millis(500),
+                ConnectOptions::new()
+                    .connection_timeout(Duration::from_millis(600))
+                    .connect(format!("localhost:{port}")),
+            )
+            .await
+            .expect("later candidate must start before the entire server budget is spent")
+            .unwrap();
+            within(stalled).await.unwrap();
+            client.force_close();
+            assert!(within(client.wait_closed()).await);
+            within(healthy).await.unwrap();
+        }
+
+        #[tokio::test]
+        async fn pending_tls_handshake_spends_the_server_attempt_budget() {
+            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+            let addr = format!("tls://{}", listener.local_addr().unwrap());
+            let peer = task::spawn(async move {
+                let (mut socket, _) = within(listener.accept()).await.unwrap();
+                let mut header = [0; 5];
+                within(socket.read_exact(&mut header)).await.unwrap();
+                assert_eq!(header[0], 22, "client reached the TLS handshake");
+                let mut remainder = Vec::new();
+                within(socket.read_to_end(&mut remainder)).await.unwrap();
+            });
+            let error = within(
+                ConnectOptions::new()
+                    .tls_first()
+                    .add_root_certificates(
+                        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
+                            .join("tests/configs/certs/rootCA.pem"),
+                    )
+                    .connection_timeout(Duration::from_millis(200))
+                    .connect(addr),
+            )
+            .await
+            .unwrap_err();
+            assert_eq!(error.kind(), ConnectErrorKind::TimedOut);
+            within(peer).await.unwrap();
+        }
+
+        #[test]
+        fn same_subscriber_recovers_after_pending_system_dns() {
+            let runtime = tokio::runtime::Builder::new_current_thread()
+                .enable_all()
+                .max_blocking_threads(1)
+                .build()
+                .unwrap();
+            runtime.block_on(async {
+                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+                let addr = format!("localhost:{}", listener.local_addr().unwrap().port());
+                let (options, mut events) = events();
+                let (client, mut peer) = within(async {
+                    tokio::join!(
+                        options
+                            .connection_timeout(Duration::from_millis(100))
+                            .connect(addr),
+                        async { handshake(listener.accept().await.unwrap().0).await }
+                    )
+                })
+                .await;
+                let client = client.unwrap();
+                let mut subscriber = client.subscribe("retained").await.unwrap();
+                let original_sub = command(&mut peer).await;
+                assert!(original_sub.starts_with("SUB retained "));
+                let (release, blocker) = block_resolver().await;
+                drop(peer);
+                let timed_out = tokio::time::timeout(Duration::from_secs(1), async {
+                    loop {
+                        if let Event::ClientError(ClientError::Other(error)) =
+                            events.recv().await.expect("native runner events")
+                        {
+                            if error == "timed out" {
+                                break;
+                            }
+                        }
+                    }
+                })
+                .await;
+                release.send(()).unwrap();
+                within(blocker).await.unwrap();
+                timed_out.expect("reconnect must finish its pending DNS attempt");
+                let mut peer = handshake(within(listener.accept()).await.unwrap().0).await;
+                assert_eq!(command(&mut peer).await, original_sub);
+                let sid = original_sub.split_whitespace().last().unwrap();
+                let message = format!("MSG retained {sid} 2\r\nok\r\n");
+                within(peer.get_mut().write_all(message.as_bytes()))
+                    .await
+                    .unwrap();
+                assert_eq!(
+                    within(subscriber.next()).await.unwrap().payload.as_ref(),
+                    b"ok"
+                );
+                client.force_close();
+                assert!(within(client.wait_closed()).await);
+                assert!(within(subscriber.next()).await.is_none());
+                assert_eq!(closed_events(events).await, 1);
+            });
+        }
+
+        #[tokio::test]
+        async fn forced_close_finishes_recovery_and_drops_queued_work_before_receipt() {
+            let (options, events) = events();
+            let (listener, client, mut peer) = connected(options).await;
+            let mut subscriber = client.subscribe("retained").await.unwrap();
+            assert!(command(&mut peer).await.starts_with("SUB retained "));
+            drop(peer);
+            let (mut pending, _) = within(listener.accept()).await.unwrap();
+            let request = client.request("waiting", "body".into());
+            tokio::pin!(request);
+            assert!((&mut request).now_or_never().is_none());
+            client.force_close();
+            client.force_close();
+            assert!(
+                client.wait_closed().now_or_never().is_none(),
+                "a close request is not observed completion"
+            );
+            assert!(within(client.wait_closed()).await);
+            assert_eq!(client.connection_state(), State::Disconnected);
+            assert!(within(request).await.is_err());
+            assert!(within(subscriber.next()).await.is_none());
+            assert!(client.flush().await.is_err());
+            let mut byte = [0];
+            assert_eq!(within(pending.read(&mut byte)).await.unwrap(), 0);
+            assert_eq!(closed_events(events).await, 1);
+        }
+
+        #[tokio::test]
+        async fn raw_subscriber_retains_runner_after_last_client_is_dropped() {
+            let (options, events) = events();
+            let (_, client, mut peer) = connected(options).await;
+            let mut subscriber = client.subscribe("retained").await.unwrap();
+            let sub = command(&mut peer).await;
+            let sid = sub.split_whitespace().last().unwrap();
+            drop(client);
+            let message = format!("MSG retained {sid} 2\r\nok\r\n");
+            within(peer.get_mut().write_all(message.as_bytes()))
+                .await
+                .unwrap();
+            assert_eq!(
+                within(subscriber.next()).await.unwrap().payload.as_ref(),
+                b"ok"
+            );
+            subscriber.unsubscribe().await.unwrap();
+            assert_eq!(command(&mut peer).await, format!("UNSUB {sid}\r\n"));
+            drop(subscriber);
+            let mut remaining = Vec::new();
+            within(peer.read_to_end(&mut remaining)).await.unwrap();
+            assert_eq!(closed_events(events).await, 1);
+        }
+
+        #[tokio::test]
+        async fn last_owner_drop_terminates_background_initial_recovery() {
+            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+            let (options, events) = events();
+            let client = options
+                .retry_on_initial_connect()
+                .connect(listener.local_addr().unwrap().to_string())
+                .await
+                .unwrap();
+            let (mut pending, _) = within(listener.accept()).await.unwrap();
+            drop(client);
+            let mut byte = [0];
+            assert_eq!(within(pending.read(&mut byte)).await.unwrap(), 0);
+            assert_eq!(closed_events(events).await, 1);
+        }
+
+        #[tokio::test]
+        async fn graceful_close_reports_completion_and_one_closed_event() {
+            let (options, events) = events();
+            let (_, client, mut peer) = connected(options).await;
+            client.drain().await.unwrap();
+            let mut remaining = Vec::new();
+            within(peer.read_to_end(&mut remaining)).await.unwrap();
+            assert!(within(client.wait_closed()).await);
+            assert_eq!(closed_events(events).await, 1);
+        }
+
+        #[tokio::test]
+        async fn lost_runner_does_not_report_observed_completion() {
+            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+            let client = ConnectOptions::with_auth_callback(|_| async {
+                panic!("fixture terminates runner before its completion receipt")
+            })
+            .retry_on_initial_connect()
+            .connect(listener.local_addr().unwrap().to_string())
+            .await
+            .unwrap();
+            let (mut peer, _) = within(listener.accept()).await.unwrap();
+            within(peer.write_all(INFO)).await.unwrap();
+            assert!(!within(client.wait_closed()).await);
+            let mut byte = [0];
+            assert_eq!(within(peer.read(&mut byte)).await.unwrap(), 0);
+        }
+    }
 
     #[test]
     fn server_address_ipv6() {
--- a/src/tls.rs
+++ b/src/tls.rs
@@ -59,7 +59,8 @@
     let mut root_store = RootCertStore::empty();
     // load native system certs only if user did not specify them.
     if options.tls_client_config.is_some() || options.certificates.is_empty() {
-        let certs_result = rustls_native_certs::load_native_certs();
+        let certs_result =
+            tokio::task::spawn_blocking(rustls_native_certs::load_native_certs).await?;
         if !certs_result.errors.is_empty() {
             let errors = certs_result
                 .errors
--- a/README.md
+++ b/README.md
@@ -1,7 +1,3 @@
-<p align="center">
-  <img src="nats/logo/logo.svg">
-</p>
-
 <p align="center">
     A <a href="https://www.rust-lang.org/">Rust</a> client for the <a href="https://nats.io">NATS messaging system</a>.
 </p>
```
