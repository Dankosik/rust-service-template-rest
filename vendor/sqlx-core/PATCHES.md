# sqlx-core 0.9.0: bounded native return

The template dependency owner carries one temporary runtime backport.
The package is copied from the [published crates.io archive](https://static.crates.io/crates/sqlx-core/sqlx-core-0.9.0.crate),
verified before extraction with SHA256 `05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb`.
The archive contains 110 files (648,948 bytes); all remain byte-identical except
`src/pool/connection.rs`. This record is the only additional file. Upstream
license files and the published normalized manifest are unchanged.
The archive's `.cargo_vcs_info.json` identifies upstream revision
`003b698e99e024f3621b8043a2426fde5b741171`.

## Isolated patch

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

Retire this backport when a published, otherwise acceptable SQLx release includes
an equivalent whole-return bound and passes the cancellation, reuse and finality
regressions. The dependency owner upgrades through the ordinary locked process
and removes this package, the root patch/exclusion and their PostgreSQL-specific
Docker/profile/classifier entries together. Keep the behavioral regression.
A version change without equivalent ownership/proof reopens the source choice.
