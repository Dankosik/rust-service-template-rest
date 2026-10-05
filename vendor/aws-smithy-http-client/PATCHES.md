# aws-smithy-http-client 1.4.2 TCP timeout propagation

Source: the published `aws-smithy-http-client-1.4.2.crate` archive, verified
before extraction against the registry checksum in Cargo.lock:
`7bd25384a4e437aa8d8f339afad4b69e786b936a7cb10db668a7aaf66717b1a8`.
The published archive contains no `.cargo_vcs_info.json`; an exact upstream
revision is unavailable. Upstream is
[smithy-rs](https://github.com/smithy-lang/smithy-rs).
Published manifests, lockfile and license files remain unchanged. No dependency
version or feature is upgraded.

The accepted native repair passes the existing configured connect timeout to
Hyper's TCP connector in `src/client.rs::base_connector_with_resolver`.
Hyper then divides the budget across same-family address candidates while
Smithy's existing outer connect timer still bounds the whole DNS/TCP/TLS dial.
The SDK owns retries, operation deadlines and response classification. This
also applies to credential HTTP consumers of this client when configured.
No application transport, retry layer, streaming cap or timeout setting is added.

Runtime source changes are confined to `src/client.rs`. The exact diff and native
test inventory follow below.

Retire the patch when a published native equivalent propagates this timeout
with equivalent candidate fallback, SDK classification and mutation finality.
Remove the root path patch/exclusion, vendor source, Docker context/cooked-layer
copy and object-storage profile removal entries together.

## Dependency and delivery custody

The initializer's guarded `_lock_records` parser verified the exact registry
identity and checksum before generating the path-source selection. Only this
package's source/checksum fields were removed: every version and dependency
vector, every other key, and every other package byte were preserved. Parsed
before/after data admitted only the two-field delta before an atomic write.
No dependency resolution or unlocked Cargo invocation was used.

- Root lock before projection: `1479c2eb8bfbce28c192deed1ae54809e06e69f0ce1535ca39f96cbe6bad0085`.
- Root lock after projection: `6dfd78f5969cd6e08682671f00bb5ea2c9fee80bae2c71bb3825fddb9991968a`.

The object-storage profile owns the root patch/exclusion, vendor removal,
Docker context and cooked-layer source copy. The existing classifier selects
Rust/dependency, object-storage integration, runtime-image and initializer
proof. Initializer assertions reject an orphaned vendor or a registry-source
fallback in a retained profile. `make test` carries the native regression
through the existing selected CI workspace-test owner, serially with other
vendored transport regressions.

## Native regression and exact source delta

`client::test::same_family_candidate_fallback_and_inner_timeout_classification`
uses a loopback listener whose queue is filled with retained sockets, and
first confirms a genuine pending TCP connect. The native resolver returns
two same-family socket addresses. The healthy second destination must answer
within the unchanged outer timer; a refused second destination preserves the
first native TCP timeout as an I/O failure. The existing
`http_connect_timeout_works` covers the distinct outer timeout classification.
The loopback queue fixture is selected on Linux/macOS. All listeners, sockets
and futures are test-owned; no external blackhole or production hook is added.

The existing `make test` recipe runs this one new native test through the
package's retained published lockfile and `rustls-aws-lc` feature. This standalone
lock is distinct from the production workspace lock; production build/adapter
proof still uses the workspace graph. Locked metadata availability does not
claim successful compilation or execution.

The certificate-fixture regeneration script also qualifies its cleanup globs
with `./` so the existing ShellCheck gate accepts the imported source. All other
archive files except `src/client.rs` remain byte-identical. `PATCHES.md` is the
sole added file.

| File | Published SHA256 | Patched SHA256 |
| --- | --- | --- |
| `src/client.rs` | `609df0b07b555808ff1692567ec3efb41a64d95633d5b50c1c6d77e05633d105` | `7823e1802d565bfd16aa15738f50c65686f49a8254576b7d3b581a31f3ae5ef2` |
| `tests/regen-certificates.sh` | `8d3d8299ffde64c4bc9b706c44ac5de252a0b2736c31f733a3a99c52f2a1837c` | `99a9f35c949d758d7ffeaba334dc3e1e18a9bfcd7975eb934ad555177203374e` |

```diff
--- a/src/client.rs
+++ b/src/client.rs
@@ -268,6 +268,11 @@
     fn base_connector_with_resolver<R>(&self, resolver: R) -> HyperHttpConnector<R> {
         let mut conn = HyperHttpConnector::new_with_resolver(resolver);
         conn.set_nodelay(self.enable_tcp_nodelay);
+        conn.set_connect_timeout(
+            self.connector_settings
+                .as_ref()
+                .and_then(HttpConnectorSettings::connect_timeout),
+        );
         #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
         if let Some(interface) = &self.interface {
             conn.set_interface(interface);
@@ -1318,6 +1323,116 @@
         assert_elapsed!(now, Duration::from_secs(1));
     }
 
+    #[cfg(any(target_os = "linux", target_os = "macos"))]
+    #[tokio::test]
+    async fn same_family_candidate_fallback_and_inner_timeout_classification() {
+        use hyper_util::client::legacy::connect::dns::Name;
+        use std::net::SocketAddr;
+        use tokio::io::{AsyncReadExt, AsyncWriteExt};
+        use tokio::net::{TcpListener, TcpSocket, TcpStream};
+
+        #[derive(Clone)]
+        struct TestResolver(Vec<SocketAddr>);
+
+        impl tower::Service<Name> for TestResolver {
+            type Response = std::vec::IntoIter<SocketAddr>;
+            type Error = std::io::Error;
+            type Future = std::future::Ready<Result<Self::Response, Self::Error>>;
+
+            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
+                Poll::Ready(Ok(()))
+            }
+
+            fn call(&mut self, _name: Name) -> Self::Future {
+                std::future::ready(Ok(self.0.clone().into_iter()))
+            }
+        }
+
+        // Retain every completed connection without accepting it, until the kernel
+        // queue is full. Real time is required: a paused Tokio clock cannot drive TCP.
+        let socket = TcpSocket::new_v4().unwrap();
+        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
+        let stalled_listener = socket.listen(1).unwrap();
+        let stalled_addr = stalled_listener.local_addr().unwrap();
+        let probe_timeout = Duration::from_millis(100);
+        let mut fills = Vec::new();
+        let mut saturated = false;
+        for _ in 0..32 {
+            match tokio::time::timeout(probe_timeout, TcpStream::connect(stalled_addr)).await {
+                Ok(Ok(stream)) => fills.push(stream),
+                Ok(Err(err)) => panic!("listen queue setup failed: {err}"),
+                Err(_) => {
+                    saturated = true;
+                    break;
+                }
+            }
+        }
+        assert!(saturated, "could not establish a pending TCP candidate");
+
+        let healthy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
+        // A bound, non-listening socket reserves a second port that refuses TCP.
+        let refused_socket = TcpSocket::new_v4().unwrap();
+        refused_socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
+        let connect_timeout = Duration::from_secs(1);
+
+        for healthy in [true, false] {
+            // Recheck the first candidate immediately before each request. A
+            // refused connection would not distinguish the missing inner timer.
+            assert!(
+                tokio::time::timeout(probe_timeout, TcpStream::connect(stalled_addr))
+                    .await
+                    .is_err(),
+                "first candidate must remain pending"
+            );
+            let later_addr = if healthy {
+                healthy_listener.local_addr().unwrap()
+            } else {
+                refused_socket.local_addr().unwrap()
+            };
+            let builder = Connector::builder()
+                .connector_settings(
+                    HttpConnectorSettings::builder()
+                        .connect_timeout(connect_timeout)
+                        .build(),
+                )
+                .sleep_impl(SharedAsyncSleep::new(TokioSleep::new()));
+            let tcp =
+                builder.base_connector_with_resolver(TestResolver(vec![stalled_addr, later_addr]));
+            let adapter = builder.wrap_connector(tcp).adapter;
+            // No explicit URI port: Hyper retains each resolver-supplied port.
+            let request = adapter.call(HttpRequest::get("http://candidate-fallback.test").unwrap());
+            tokio::time::timeout(Duration::from_secs(2), async {
+                if healthy {
+                    let server = async {
+                        let (mut stream, _) = healthy_listener.accept().await.unwrap();
+                        let mut request = Vec::new();
+                        while !request.ends_with(b"\r\n\r\n") {
+                            request.push(stream.read_u8().await.unwrap());
+                        }
+                        stream
+                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
+                            .await
+                            .unwrap();
+                        Ok::<_, ConnectorError>(())
+                    };
+                    let (response, ()) = tokio::try_join!(request, server)
+                        .expect("healthy later TCP candidate must receive an opportunity");
+                    assert_eq!(response.status().as_u16(), 200);
+                } else {
+                    let err = request.await.unwrap_err();
+                    assert!(err.is_io(), "inner TCP expiry must remain I/O: {err:?}");
+                    assert!(!err.is_timeout(), "outer timer must not win: {err:?}");
+                    assert_eq!(
+                        find_source::<std::io::Error>(&err).unwrap().kind(),
+                        std::io::ErrorKind::TimedOut
+                    );
+                }
+            })
+            .await
+            .expect("candidate request and server must finish within the test bound");
+        }
+    }
+
     #[tokio::test]
     async fn http_read_timeout_works() {
         let tcp_connector = crate::client::timeout::test::NeverReplies;
--- a/tests/regen-certificates.sh
+++ b/tests/regen-certificates.sh
@@ -69,4 +69,4 @@
             -extensions v3_end -extfile openssl.cnf
 
 cat server.cert inter.cert ca.cert > server.pem
-rm *.key *.cert *.req
+rm ./*.key ./*.cert ./*.req
```
