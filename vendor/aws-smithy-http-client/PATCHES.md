# aws-smithy-http-client 1.4.2 TCP timeout propagation

Source: the published `aws-smithy-http-client-1.4.2.crate` archive, verified
before extraction against the registry checksum in Cargo.lock:
`7bd25384a4e437aa8d8f339afad4b69e786b936a7cb10db668a7aaf66717b1a8`.
The published archive contains no `.cargo_vcs_info.json`; an exact upstream
revision is unavailable. Upstream is
[smithy-rs](https://github.com/smithy-lang/smithy-rs).
Published license files remain unchanged. Runtime package versions stay pinned;
the standalone manifest additionally selects the shared Hyper path patch, and
its lock is deliberately Cargo-authored to match the exercised production graph.

The accepted native repair passes the existing configured connect timeout to
Hyper's TCP connector in `src/client.rs::base_connector_with_resolver`.
The shared Hyper repair races enabled same-family candidates under their
configured TCP bounds. Smithy's outer connect timer still bounds the complete
DNS/TCP/TLS dial.
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

The root and standalone locks were deliberately authored with constrained
Cargo commands for this composition. Every later metadata, graph, build and
test invocation is locked. The standalone manifest selects the same local
Hyper source as production. `make native-transport-regressions` verifies normal/
build package source/checksum identities and relevant features for the actual
target, and lists separate test-only additions. Its Smithy feature selection
includes `hyper-rustls/aws-lc-rs`, matching the production backend union.

The object-storage profile owns the root patch/exclusion, vendor removal,
Docker context and cooked-layer source copy. The existing classifier selects
Rust/dependency, object-storage integration, runtime-image and initializer
proof. Initializer assertions reject an orphaned vendor or a registry-source
fallback in a retained profile. `make native-transport-regressions` carries the native regression through the
existing quality job, serially with other vendored transport filters.

## Native regression and exact source delta

`client::test::same_family_candidate_fallback_and_inner_timeout_classification`
uses a loopback listener whose queue is filled with retained sockets, and
first confirms a genuine pending TCP connect. The native resolver returns
two same-family socket addresses. The healthy second destination must answer
within the unchanged outer timer; a refused second destination preserves the
first native TCP timeout as an I/O failure. The refusal fixture releases a
fresh loopback port and verifies `ConnectionRefused` immediately before use;
a bound but non-listening socket can leave TCP pending on macOS. The existing
`http_connect_timeout_works` covers the distinct outer timeout classification.
The loopback queue fixture is selected on Linux/macOS. All listeners, sockets
and futures are test-owned; no external blackhole or production hook is added.

The fixed input PR's negative control predates the shared Hyper race: without
inner timeout propagation its healthy later candidate reached the outer timeout.
That historical result is not proof for this composition. Here, the refused
later-candidate branch also discriminates missing native timeout propagation:
a native I/O timeout must finish before Smithy's outer timer. Actual native
execution against the aligned graph is recorded only by the final receipt.

The certificate-fixture regeneration script also qualifies its cleanup globs
with `./` so the existing ShellCheck gate accepts the imported source. Other source/fixture bytes remain unchanged; `Cargo.toml` adds the shared Hyper
path patch and `Cargo.lock` intentionally aligns the standalone graph. `PATCHES.md` is the
sole added file.

| File | Published SHA256 | Patched SHA256 |
| --- | --- | --- |
| `src/client.rs` | `609df0b07b555808ff1692567ec3efb41a64d95633d5b50c1c6d77e05633d105` | `6a56f0515c6d69933cf7aef6f81550b50b3c0d26e0b40b6bfeca50cfc1ec78fd` |
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
@@ -1318,6 +1323,124 @@
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
+                // A bound, non-listening socket can leave TCP pending on macOS.
+                // Release a fresh loopback port and verify refusal before using it.
+                let socket = TcpSocket::new_v4().unwrap();
+                socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
+                let address = socket.local_addr().unwrap();
+                drop(socket);
+                let refusal = tokio::time::timeout(probe_timeout, TcpStream::connect(address))
+                    .await
+                    .expect("second candidate must promptly refuse TCP")
+                    .expect_err("second candidate must not accept TCP");
+                assert_eq!(refusal.kind(), std::io::ErrorKind::ConnectionRefused);
+                address
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

## Current standalone lock identity

- Before this composition: `6aedb743839fbb2c162f7fced17fc9111bcd22f305991fcc2263e20818845b79`.
- Current standalone lock: `53a94a1e3a3e9d6f9d34c6f43a5fd4c08225c76c970ba6c4152a40f28e97ab69`.
