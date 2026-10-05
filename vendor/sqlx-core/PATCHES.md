# sqlx-core 0.9.0: bounded native return and concurrent TCP candidates

The template dependency owner carries two independent runtime repairs.
The package is copied from the [published crates.io archive](https://static.crates.io/crates/sqlx-core/sqlx-core-0.9.0.crate),
verified before extraction with SHA256 `05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb`.
The archive contains 110 files (648,948 bytes); all remain byte-identical except
`src/pool/connection.rs` and `src/net/socket/mod.rs` (which also contains the
TCP regression tests). This record is the only additional file. Upstream
license files and the published normalized manifest are unchanged.
The archive's `.cargo_vcs_info.json` identifies upstream revision
`003b698e99e024f3621b8043a2426fde5b741171`.

## Whole-return patch

This source is byte-identical to `sqlx-core/src/pool/connection.rs` at
[PR 4407 head c3735ab](https://github.com/transact-rs/sqlx/blob/c3735abf034689bb1fab9d7bf9f040e492de28cf/sqlx-core/src/pool/connection.rs).
[PR 4350 head 60772eb](https://github.com/transact-rs/sqlx/blob/60772eb39474136adb15c6ffd68ded307649bc35/sqlx-core/src/pool/connection.rs)
uses the same whole-return timeout. These are reference revisions, not a released
upstream fix. No other changes from either PR are included.

The five-second timeout covers the owned native return future, including
callbacks, ping and graceful-close branches. Expiry drops the raw connection
and size guard before native minimum-connection maintenance. Healthy reuse,
close-on-drop and transaction semantics remain upstream-owned.

- Pristine source SHA256: `94269a532ef60aaa31321e43af17cba2424f919a36501d862328ed05958a08de`.
- Patched source SHA256: `ad99491e0ca834b125da83c65506f70e9f4188d13107368cd66d17b53cd2339c`.

```diff
--- a/src/pool/connection.rs
+++ b/src/pool/connection.rs
@@ -14,6 +14,7 @@
 use crate::pool::options::PoolConnectionMetadata;
 
 const CLOSE_ON_DROP_TIMEOUT: Duration = Duration::from_secs(5);
+const RETURN_TO_POOL_TIMEOUT: Duration = Duration::from_secs(5);
 
 /// A connection managed by a [`Pool`][crate::pool::Pool].
 ///
@@ -143,7 +144,16 @@
 
         async move {
             let returned_to_pool = if let Some(floating) = floating {
-                floating.return_to_pool().await
+                // Bound the whole return, including callbacks and connection shutdown.
+                // On timeout, dropping the future drops the connection and its size guard,
+                // releasing the permit without awaiting any further connection I/O.
+                match crate::rt::timeout(RETURN_TO_POOL_TIMEOUT, floating.return_to_pool()).await {
+                    Ok(returned) => returned,
+                    Err(_) => {
+                        tracing::warn!("timed out while returning a connection to the pool");
+                        false
+                    }
+                }
             } else {
                 false
             };
```

## Tokio TCP candidate patch

The template-owned change to `src/net/socket/mod.rs` resolves through Tokio's
existing `lookup_host`, concurrently drives owned `TcpStream::connect` futures,
and returns the first successful stream. All other attempts are dropped before
`WithSocket` can start TLS or PostgreSQL opening. The options host remains the
TLS identity. No timeout, application retry, spawned task, protocol fallback or
async-io change is added. The accepted cost is O(N) concurrent TCP attempts and
loss of resolver-order preference among successful addresses.

No-address resolution retains Tokio's `InvalidInput`. If every address fails,
the retained error belongs to the last resolver-order address, regardless of
completion order; preserving `ConnectionRefused` matters to SQLx's pool opening
retry branch. The caller's existing deadline owns DNS/TCP/protocol opening.
This patch does not change the independent five-second whole-return bound.

- Pristine socket source SHA256: `54756a5db25c045e7b2b4c92b5be7d19ff030ed33517d3097fe01f5930e0ac9c`.
- Patched socket source SHA256 (including native tests): `e262d1062861f0da009a3f94d8cb313472ec6950fe3db268c554f1c89f2b6bee`.

Exact runtime delta (the same file's `cfg(test)` module adds real loopback
candidate-progress, empty-resolution and resolver-order failure regressions):

```diff
--- a/src/net/socket/mod.rs
+++ b/src/net/socket/mod.rs
@@ -5 +5 @@
-use std::task::{ready, Context, Poll};
+use std::task::{Context, Poll, ready};
@@ -191,3 +191,3 @@
-        return Ok(with_socket
-            .with_socket(tokio::net::TcpStream::connect((host, port)).await?)
-            .await);
+        let addresses = tokio::net::lookup_host((host, port)).await?;
+        let stream = connect_tcp_tokio(addresses).await?;
+        return Ok(with_socket.with_socket(stream).await);
@@ -202,0 +203,43 @@
+}
+
+// Keep candidate ownership below the protocol continuation: only the winner
+// can perform TLS or database opening, and cancellation drops every attempt.
+#[cfg(feature = "_rt-tokio")]
+async fn connect_tcp_tokio(
+    addresses: impl IntoIterator<Item = std::net::SocketAddr>,
+) -> io::Result<tokio::net::TcpStream> {
+    use futures_util::stream::{FuturesUnordered, StreamExt};
+
+    let mut attempts: FuturesUnordered<_> =
+        addresses
+            .into_iter()
+            .enumerate()
+            .map(|(index, address)| async move {
+                (index, tokio::net::TcpStream::connect(address).await)
+            })
+            .collect();
+    let mut last_error = None;
+    while let Some((index, result)) = attempts.next().await {
+        match result {
+            Ok(stream) => {
+                drop(attempts);
+                return Ok(stream);
+            }
+            Err(error) => {
+                // Match Tokio's serial connect error, regardless of which
+                // candidate finishes last in this concurrent race.
+                if last_error
+                    .as_ref()
+                    .is_none_or(|(last_index, _)| index > *last_index)
+                {
+                    last_error = Some((index, error));
+                }
+            }
+        }
+    }
+    Err(last_error.map(|(_, error)| error).unwrap_or_else(|| {
+        io::Error::new(
+            io::ErrorKind::InvalidInput,
+            "could not resolve to any addresses",
+        )
+    }))
```

The private native tests use the existing dependency harness, selected alongside
the assembled workspace proof with `cargo test --locked -p infra-postgres -p sqlx-core --lib
net::socket::tests` (the provider selects its production Tokio/TLS features). The PostgreSQL integration suite adds an IPv6 loopback relay
to its existing real-database harness; admission tests cover all admitted TLS
modes and password-file identity preservation, and an IPv6 loopback peer
observes the native TLS ClientHello (without claiming certificate acceptance). These are authored regressions,
not passing proof until run on the assembled candidate.

## Dependency and delivery custody

The root Cargo patch selects this excluded non-workspace dependency. Only
sqlx-core's registry source/checksum fields were projected out of Cargo.lock;
its version/dependency vector and every other package byte were preserved.
The fail-closed projection parsed both versions, required the published identity
and asserted the two-field semantic difference.

- Lock before source projection: `9273481496362e1684634bf50e6e6c2df2354404ccaa6ca3621dc30ff58f9a1f`.
- Lock after source projection: `9e69f8f01f80d13644f14ca77df6435b4a7950234f6bff713dcc6ed8edd7b398`.

All Cargo commands remain locked. Cargo graph/build acceptance is recorded with
the delivery evidence, not inferred from these hashes. The Docker context and
cooked dependency layer carry these real sources; the PostgreSQL profile owns
removal of the vendor, patch, exclusion and copy together. The dependency,
integration, image and initializer gates retain their existing scope.

## Retirement

Retire the whole-return patch when a published, otherwise acceptable SQLx
release includes an equivalent whole-return bound and passes cancellation,
reuse and finality regressions. Retire the TCP patch independently when a
published native equivalent (or supported setting) supplies candidate progress,
loser cleanup and equivalent last-resolver-address failure classification.
Retiring either patch must preserve the other until its own condition is met.

The dependency owner upgrades through the ordinary locked process. Remove this
package, root patch/exclusion and PostgreSQL-specific Docker/profile/classifier
entries together only after both patches can retire. Keep the behavioral
regressions. A version change without equivalent ownership/proof reopens the
source choice.

## Historical whole-return verification and provenance

The following evidence predates the TCP candidate repair and does not verify
that repair or its assembled candidate.

The accepted source tree is `e114d7998992419a67e258144a6ef9b4c7fc8b96`,
recorded with full validation and fresh independent review in local commit
`8643ccb74681dd9bb9c0694ef692d87c159dc1eb` on 2026-10-04. Matching build,
804 workspace unit tests and 34 PostgreSQL tests passed on PostgreSQL 18.6
and PgBouncer 1.26.0. The verified unpatched source failed the local-slot
reclamation regression; exact patch restoration passed the identical test.
Locked metadata preserved all 588 package versions/features; the only other
intentional graph delta was the existing `integration-tests -> tracing` dev edge.
Retained/absent PostgreSQL projections, dependency policy, source routing,
ShellCheck, documentation and Dockerfile checks passed. Runtime-image,
full-initializer and other CI gates were not executed locally.

Git retains the complete evidence and review records after execution cleanup:

```bash
git show 8643ccb74681dd9bb9c0694ef692d87c159dc1eb:specs/postgres-pool-resilience/completion.md
git show 8643ccb74681dd9bb9c0694ef692d87c159dc1eb:specs/postgres-pool-resilience/implementation-review.md
```

These observations establish the local backport outcome, not production
capacity, immediate termination of unreachable server sessions, or a known
COMMIT result after cancellation.
