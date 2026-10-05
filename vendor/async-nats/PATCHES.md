# async-nats 0.50.0: bounded native request and ACK ownership

The template dependency owner carries one temporary same-version native lifetime
repair. The package is copied from the [published crates.io archive](https://static.crates.io/crates/async-nats/async-nats-0.50.0.crate),
verified before extraction with SHA256
`d83a251fa1a4c9d0fe6e816b7acd60549e473e08d14f27a1d992c2675abff05f`.
The archive contains 118 files (1,712,200 bytes). Every package file is retained;
only `src/lib.rs` and `src/jetstream/context.rs` differ. This record is the only
additional file. The licenses, normalized manifest, published Cargo.lock,
client.rs, native transport, framing and parsing remain unchanged.
The published `.cargo_vcs_info.json` identifies revision
`9b382a2a01b5404cd66bee6c2b4f0c82c9943063`.

## Selected repair

The handler-only adaptive pruning is taken from
[upstream draft PR 1629 at fixed commit 7db17cf](https://github.com/nats-io/nats.rs/blob/7db17cf15830a1a65e7ba73cecda65aee72b1ea7/async-nats/src/lib.rs).
This is a reference revision, not a released fix. It adds the 256-entry pruning
floor, Multiplexer threshold/insert/remove/prune methods and their existing
handler call sites. Only those production changes and four adjacent multiplexer
regressions are selected; unrelated Subscriber changes from that commit are
excluded. [Issue 1617](https://github.com/nats-io/nats.rs/issues/1617) is provenance
for abandoned native request registrations.

The additional context.rs repair retains the native ACK receiver while its
future is polled. Dropping a polled caller transfers that receiver and its native
permit through the existing bounded acker; terminal ACK/error/timeout drops the
receiver before capacity is released and cannot schedule another full ACK wait.
The acker explicitly drops its receiver before its permit on ACK or expiry.
Native error classification and ACK parsing are unchanged. No task, channel,
receiver wrapper, manager, version, dependency or feature is added by the repair.

Live publication ownership is bounded by the adapter's configured P permits.
Pruning bounds abandoned request history separately: the map retains at most
max(256, 2 × peak live native requests) entries, including separately owned
control calls. An idle expired cohort may leave finite stale metadata until
later insertion or handler destruction. Map allocation is not the entry count;
the upstream adaptive shrink policy returns excess burst capacity. No immediate
metadata-expiry or hard RSS claim is made.

The four native multiplexer regressions cover repeated already-closed insertions,
live receiver delivery amid abandonment, post-burst pruning and excess capacity
reclamation. Adjacent ACK regressions cover first-poll cancellation through both
ACK and cleanup expiry, receiver closure before capacity return, native timeout
without a second cleanup handoff, and terminal ACK/error field parity. These
cases require no production exports or custom runner. Their execution and
pre-fix failure demonstration belong to assembled delivery validation.

## Exact source custody

The implementation comparison checked every archive member and found only the
two selected source files changed. Hashes and the full patch below include the
adjacent regressions as well as production code.

| File | Pristine SHA256 | Patched SHA256 |
| --- | --- | --- |
| `src/lib.rs` | `90e270319d172fa339ba822ec92ab4295c32a881bee393394c7f8b511a553ec1` | `ebdc96537331f293c65f6daf37f5a1f91c83963efac86865b3d842a4d637cea1` |
| `src/jetstream/context.rs` | `14ae2603ef34156a268337df140be064e2cfbc74f55819406bfab7043139bc67` | `ce174476163a7ddcd64ac6ca7824164026245db292fa35f0a2ed81b29bd2a577` |

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -234,6 +234,7 @@
 const LANG: &str = "rust";
 const MAX_PENDING_PINGS: usize = 2;
 const MULTIPLEXER_SID: u64 = 0;
+const MULTIPLEXER_PRUNE_MIN: usize = 256;
 pub(crate) const DEFAULT_SERVER_MAX_PAYLOAD: usize = 1024 * 1024;
 
 /// A re-export of the `rustls` crate used in this crate,
@@ -455,6 +456,48 @@
     subject: Subject,
     prefix: Subject,
     senders: HashMap<String, oneshot::Sender<Message>>,
+    /// Size of `senders` at which senders of abandoned requests are pruned: twice the smallest
+    /// size since the last prune, and at least [`MULTIPLEXER_PRUNE_MIN`].
+    prune_at: usize,
+}
+
+impl Multiplexer {
+    fn insert(&mut self, token: String, sender: oneshot::Sender<Message>) {
+        if self.senders.len() >= self.prune_at {
+            self.prune();
+        }
+        self.senders.insert(token, sender);
+    }
+
+    fn remove(&mut self, token: &str) -> Option<oneshot::Sender<Message>> {
+        let sender = self.senders.remove(token)?;
+        // Follow the map down as replies drain it, so that abandoned requests cannot pile up to
+        // the size of a past burst before the next prune.
+        self.prune_at = self
+            .prune_at
+            .min((self.senders.len() * 2).max(MULTIPLEXER_PRUNE_MIN));
+        Some(sender)
+    }
+
+    /// Removes the senders of requests whose callers stopped waiting, e.g. after a timeout.
+    ///
+    /// Such a request dropped its receiver and will never read a reply, so nothing else would
+    /// ever remove its sender. At least half of `prune_at` requests are inserted between two
+    /// prunes, which keeps pruning amortized O(1) per request.
+    fn prune(&mut self) {
+        let len = self.senders.len();
+        self.senders.retain(|_, pending| !pending.is_closed());
+        self.prune_at = (self.senders.len() * 2).max(MULTIPLEXER_PRUNE_MIN);
+        // `retain` visits every bucket, so give back the capacity a past burst left behind.
+        if self.senders.capacity() > 4 * self.prune_at {
+            self.senders.shrink_to(self.prune_at);
+        }
+        debug!(
+            "pruned {} abandoned requests, {} pending",
+            len - self.senders.len(),
+            self.senders.len()
+        );
+    }
 }
 
 /// A connection handler which facilitates communication from channels to a single shared connection.
@@ -787,7 +830,7 @@
                             subject.strip_prefix(multiplexer.prefix.as_ref()).to_owned();
 
                         if let Some(token) = maybe_token {
-                            if let Some(sender) = multiplexer.senders.remove(token) {
+                            if let Some(sender) = multiplexer.remove(token) {
                                 debug!("forwarding message to request with token {}", token);
                                 let message = Message {
                                     subject,
@@ -911,6 +954,7 @@
                         subject,
                         prefix,
                         senders: HashMap::new(),
+                        prune_at: MULTIPLEXER_PRUNE_MIN,
                     })
                 };
                 self.connector
@@ -918,7 +962,7 @@
                     .out_messages
                     .add(1, Ordering::Relaxed);
 
-                multiplexer.senders.insert(token.to_owned(), sender);
+                multiplexer.insert(token.to_owned(), sender);
 
                 let respond: Subject = format!("{}{}", multiplexer.prefix, token).into();
 
@@ -1929,4 +1973,112 @@
             assert_eq!(addrs_iter.next().unwrap().port(), expected_port);
         }
     }
-}
+    fn multiplexer() -> Multiplexer {
+        Multiplexer {
+            subject: Subject::from_static("_INBOX.mux.*"),
+            prefix: Subject::from_static("_INBOX.mux."),
+            senders: HashMap::new(),
+            prune_at: MULTIPLEXER_PRUNE_MIN,
+        }
+    }
+
+    fn reply() -> Message {
+        Message {
+            subject: Subject::from_static("_INBOX.mux.reply"),
+            reply: None,
+            payload: Bytes::from_static(b"reply"),
+            headers: None,
+            status: None,
+            description: None,
+            length: 5,
+        }
+    }
+
+    #[test]
+    fn multiplexer_prunes_abandoned_requests() {
+        let mut multiplexer = multiplexer();
+
+        // Dropping the receiver is what a request does when its caller stops waiting.
+        for i in 0..10_000 {
+            let (sender, receiver) = oneshot::channel();
+            drop(receiver);
+            multiplexer.insert(format!("abandoned{i}"), sender);
+        }
+
+        assert!(multiplexer.senders.len() <= MULTIPLEXER_PRUNE_MIN);
+    }
+
+    #[test]
+    fn multiplexer_keeps_pending_requests() {
+        let mut multiplexer = multiplexer();
+        let mut pending = Vec::new();
+
+        for i in 0..1_000 {
+            let (sender, receiver) = oneshot::channel();
+            multiplexer.insert(format!("pending{i}"), sender);
+            pending.push((format!("pending{i}"), receiver));
+
+            for j in 0..10 {
+                let (sender, receiver) = oneshot::channel();
+                drop(receiver);
+                multiplexer.insert(format!("abandoned{i}.{j}"), sender);
+            }
+        }
+
+        // Pruning keeps the map within twice the number of pending requests.
+        assert!(multiplexer.senders.len() <= 2_000);
+
+        for (token, mut receiver) in pending {
+            let sender = multiplexer
+                .remove(&token)
+                .expect("pending request was pruned");
+            sender.send(reply()).unwrap();
+            assert_eq!(receiver.try_recv().unwrap().payload, "reply");
+        }
+    }
+
+    /// Fills the multiplexer with `count` pending requests, then answers all of them.
+    fn answer_burst(multiplexer: &mut Multiplexer, count: usize) {
+        let mut pending = Vec::new();
+        for i in 0..count {
+            let (sender, receiver) = oneshot::channel();
+            multiplexer.insert(format!("burst{i}"), sender);
+            pending.push((format!("burst{i}"), receiver));
+        }
+        for (token, mut receiver) in pending {
+            let sender = multiplexer
+                .remove(&token)
+                .expect("pending request was pruned");
+            sender.send(reply()).unwrap();
+            assert!(receiver.try_recv().is_ok());
+        }
+    }
+
+    #[test]
+    fn multiplexer_prunes_abandoned_requests_after_burst() {
+        let mut multiplexer = multiplexer();
+        answer_burst(&mut multiplexer, 10_000);
+
+        for i in 0..10_000 {
+            let (sender, receiver) = oneshot::channel();
+            drop(receiver);
+            multiplexer.insert(format!("abandoned{i}"), sender);
+        }
+
+        assert!(multiplexer.senders.len() <= MULTIPLEXER_PRUNE_MIN);
+    }
+
+    #[test]
+    fn multiplexer_releases_capacity_after_burst() {
+        let mut multiplexer = multiplexer();
+        answer_burst(&mut multiplexer, 100_000);
+
+        for i in 0..1_000 {
+            let (sender, receiver) = oneshot::channel();
+            drop(receiver);
+            multiplexer.insert(format!("abandoned{i}"), sender);
+        }
+
+        assert!(multiplexer.senders.capacity() <= 4 * MULTIPLEXER_PRUNE_MIN);
+    }
+}
--- a/src/jetstream/context.rs
+++ b/src/jetstream/context.rs
@@ -127,8 +127,11 @@
     concurrency: Option<usize>,
 ) -> tokio::task::JoinHandle<()> {
     tokio::spawn(async move {
-        rx.for_each_concurrent(concurrency, |(subscription, permit)| async move {
-            tokio::time::timeout(ack_timeout, subscription).await.ok();
+        rx.for_each_concurrent(concurrency, |(mut subscription, permit)| async move {
+            tokio::time::timeout(ack_timeout, &mut subscription)
+                .await
+                .ok();
+            drop(subscription);
             drop(permit);
         })
         .await;
@@ -1881,9 +1884,11 @@
 
 impl PublishAckFuture {
     async fn next_with_timeout(mut self) -> Result<PublishAck, PublishError> {
-        let next = tokio::time::timeout(self.timeout, self.subscription.take().unwrap())
-            .await
-            .map_err(|_| PublishError::new(PublishErrorKind::TimedOut))?;
+        let next = tokio::time::timeout(self.timeout, self.subscription.as_mut().unwrap()).await;
+        // Cancellation leaves the receiver in self for Drop's bounded acker handoff.
+        // A terminal result closes it before releasing capacity or propagating errors.
+        drop(self.subscription.take());
+        let next = next.map_err(|_| PublishError::new(PublishErrorKind::TimedOut))?;
         next.map_or_else(
             |_| Err(PublishError::new(PublishErrorKind::BrokenPipe)),
             |m| {
@@ -1919,6 +1924,165 @@
         Box::pin(std::future::IntoFuture::into_future(
             self.next_with_timeout(),
         ))
+    }
+}
+
+#[cfg(test)]
+mod publish_ack_tests {
+    use super::*;
+    use tokio::sync::Semaphore;
+
+    fn pending_ack(
+        semaphore: &Arc<Semaphore>,
+        tx: &mpsc::Sender<(oneshot::Receiver<Message>, OwnedSemaphorePermit)>,
+        timeout: Duration,
+    ) -> (oneshot::Sender<Message>, PublishAckFuture) {
+        let (sender, receiver) = oneshot::channel();
+        (
+            sender,
+            PublishAckFuture {
+                timeout,
+                subscription: Some(receiver),
+                permit: Some(semaphore.clone().try_acquire_owned().unwrap()),
+                tx: tx.clone(),
+            },
+        )
+    }
+
+    fn ack_message(payload: &'static [u8], status: Option<StatusCode>) -> Message {
+        Message {
+            subject: "_INBOX.ack".into(),
+            reply: None,
+            payload: Bytes::from_static(payload),
+            headers: None,
+            status,
+            description: None,
+            length: payload.len(),
+        }
+    }
+
+    #[tokio::test]
+    async fn polled_ack_cancellation_retains_capacity_until_ack_or_cleanup_expiry() {
+        for receive_ack in [true, false] {
+            let semaphore = Arc::new(Semaphore::new(1));
+            let (tx, rx) = mpsc::channel(1);
+            let (sender, ack) = pending_ack(&semaphore, &tx, Duration::from_secs(60));
+            let mut sender = Some(sender);
+            let mut ack = ack.into_future();
+            assert!(futures_util::poll!(&mut ack).is_pending());
+            drop(ack);
+            assert!(!sender.as_ref().unwrap().is_closed());
+            assert!(semaphore.clone().try_acquire_owned().is_err());
+
+            let cleanup_timeout = if receive_ack {
+                Duration::from_secs(60)
+            } else {
+                Duration::ZERO
+            };
+            let acker = spawn_acker(ReceiverStream::new(rx), cleanup_timeout, None);
+            drop(tx);
+            if receive_ack {
+                sender
+                    .take()
+                    .unwrap()
+                    .send(ack_message(br#"{"stream":"events","seq":1}"#, None))
+                    .unwrap();
+            }
+            let permit =
+                tokio::time::timeout(Duration::from_secs(1), semaphore.clone().acquire_owned())
+                    .await
+                    .unwrap()
+                    .unwrap();
+            if let Some(sender) = sender {
+                assert!(
+                    sender.is_closed(),
+                    "receiver must close before capacity returns"
+                );
+            }
+            tokio::time::timeout(Duration::from_secs(1), acker)
+                .await
+                .unwrap()
+                .unwrap();
+            drop(permit);
+            assert_eq!(semaphore.available_permits(), 1);
+        }
+    }
+
+    #[tokio::test]
+    async fn native_ack_timeout_closes_receiver_without_another_cleanup_wait() {
+        let semaphore = Arc::new(Semaphore::new(1));
+        let (tx, mut rx) = mpsc::channel(1);
+        let (sender, ack) = pending_ack(&semaphore, &tx, Duration::ZERO);
+        let error = tokio::time::timeout(Duration::from_secs(1), ack.into_future())
+            .await
+            .unwrap()
+            .unwrap_err();
+        assert_eq!(error.kind(), PublishErrorKind::TimedOut);
+        assert!(sender.is_closed());
+        assert_eq!(semaphore.available_permits(), 1);
+        assert!(matches!(
+            rx.try_recv(),
+            Err(mpsc::error::TryRecvError::Empty)
+        ));
+    }
+
+    #[tokio::test]
+    async fn terminal_ack_results_release_capacity_without_cleanup_handoff() {
+        let cases: &[(Option<&'static [u8]>, Option<StatusCode>, Option<PublishErrorKind>)] = &[
+            (
+                Some(br#"{"stream":"events","seq":7,"domain":"test","duplicate":true,"val":"9"}"#),
+                None,
+                None,
+            ),
+            (
+                Some(b""),
+                Some(StatusCode::NO_RESPONDERS),
+                Some(PublishErrorKind::StreamNotFound),
+            ),
+            (
+                Some(b"invalid json"),
+                None,
+                Some(PublishErrorKind::Other),
+            ),
+            (
+                Some(br#"{"error":{"code":400,"err_code":10070,"description":"wrong last message id"}}"#),
+                None,
+                Some(PublishErrorKind::WrongLastMessageId),
+            ),
+            (
+                Some(br#"{"error":{"code":400,"err_code":10071,"description":"wrong last sequence"}}"#),
+                None,
+                Some(PublishErrorKind::WrongLastSequence),
+            ),
+            (None, None, Some(PublishErrorKind::BrokenPipe)),
+        ];
+        for &(payload, status, error) in cases {
+            let semaphore = Arc::new(Semaphore::new(1));
+            let (tx, mut rx) = mpsc::channel(1);
+            let (sender, ack) = pending_ack(&semaphore, &tx, Duration::from_secs(60));
+            match payload {
+                Some(payload) => sender.send(ack_message(payload, status)).unwrap(),
+                None => drop(sender),
+            }
+            let result = tokio::time::timeout(Duration::from_secs(1), ack.into_future())
+                .await
+                .unwrap();
+            if let Some(error) = error {
+                assert_eq!(result.unwrap_err().kind(), error);
+            } else {
+                let ack = result.unwrap();
+                assert_eq!(ack.stream, "events");
+                assert_eq!(ack.sequence, 7);
+                assert_eq!(ack.domain, "test");
+                assert!(ack.duplicate);
+                assert_eq!(ack.value.as_deref(), Some("9"));
+            }
+            assert_eq!(semaphore.available_permits(), 1);
+            assert!(matches!(
+                rx.try_recv(),
+                Err(mpsc::error::TryRecvError::Empty)
+            ));
+        }
     }
 }
 
```

## Dependency and delivery custody

The root Cargo patch selects this excluded non-workspace dependency. The existing
workspace declaration remains exactly async-nats =0.50.0 with default features
disabled; resolved workspace features remain aws-lc-rs, jetstream and nkeys.
The published normalized manifest and dependency vector are byte-identical.

One deliberate source-resolution change was made with Cargo's supported
`cargo update --workspace --offline`. Targeted package update attempts were
rejected because they also changed tempfile's getrandom edge; the complete
saved lockfile was restored before the final supported resolution. Comparing
parsed lockfiles establishes that only async-nats's registry source/checksum
fields were removed. All package versions and dependency vectors are preserved.
Subsequent `cargo metadata --locked --offline --format-version 1` selected the
intended path source without changing the lockfile.

- Root lock before source resolution SHA256:
  `9507b2d057a2f72dc929f802923e2ee947ef406b9d268e1ec06c48bf45401b14`.
- Root lock after source resolution SHA256:
  `dd9502f6bdc0b585980c94b7c6ddb79ebc381bf4353aae1d9c63074b90634e7c`.

The Docker context and cooked dependency layer carry these real sources. The
messaging profile removes this package, its root patch/exclusion and both Docker
entries together through existing declarative markers. The classifier routes
vendor changes to Rust/dependency, messaging integration, image and initializer
gates. The messaging-owned root lychee.toml excludes only the upstream README's
logo URL, whose image is absent from the published archive; all other links and
the pristine README remain unchanged. Messaging-owned Gitleaks rules name only
the published TLS/JWT/nkey test fixture paths and their reported rule IDs, whose
bytes match the verified archive. They do not exempt other source or keys.
No release gate or advisory policy is waived. Profile projections,
dependency policy, native regressions, adapter parity and image acceptance remain
with the assembled delivery owner; source hashes and locked metadata alone do
not establish those outcomes.

## Retirement

Retire this repair when a maintained published async-nats release provides both
equivalent adaptive abandoned-request reclamation and ACK/permit ownership, and
passes cancellation, expiry, healthy reuse, native ACK parity and adapter/DLQ
regressions. The dependency owner upgrades through ordinary locked resolution
and removes this package, root patch/exclusion, messaging profile removal entry,
Docker context/cooked-source markers and dedicated classifier cases together.
Remove the messaging-owned lychee.toml exception with the copied README.
Remove the messaging-owned public-fixture Gitleaks rules with those fixtures.
Retain necessary behavior proof at the surviving native or adapter owner. A
version change without equivalent ownership and proof reopens the source choice.
