# PostgreSQL connection diagnostics

Upstream reviewed: 0.9.1

The published `sqlx-postgres-0.9.0.crate` has SHA-256
`87a2bdd6e83f6b3ea525ca9fee568030508b58355a43d0b2c1674d5f79dcd65e`
and records upstream revision `003b698e99e024f3621b8043a2426fde5b741171`.
The two license files contain the full texts from the matching `sqlx-0.9.0.crate`,
which the PostgreSQL package references by relative path.

The client-key documentation example reads a mounted file to keep embedded
key markers out of the source secret scan. Import normalization removes trailing
horizontal whitespace from five upstream files.

`src/options/mod.rs` limits `PgConnectOptions` debug output to the host, port,
database and SSL mode. The pool exposes these options after initial connection
and password rotation. Both 0.9.0 and 0.9.1 derive `Debug` for the whole struct,
including its password and TLS key material.

Retire the patch and its PostgreSQL profile, Docker and validation routing when
a compatible release protects these fields and the public pool diagnostics
regression in `crates/infra-postgres/src/credentials.rs` passes with it.
