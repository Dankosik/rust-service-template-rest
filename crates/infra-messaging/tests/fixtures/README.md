# Synthetic NATS credential fixture

`synthetic-account.seed` is a deliberately public test-only NKey. Its account is
preloaded by [`credential-rotation.conf`](../../../../env/nats/credential-rotation.conf),
under a dedicated synthetic operator and a separate system account. Never trust
these identities in a deployment. The account has 16 MiB memory and disk
JetStream limits, ten streams and ten consumers, and disallows bearer tokens.

The fixed trust claims were generated once with local `nsc version 0.0.0-dev`
using `-H` for an isolated task-created temporary store. Every invocation used
that store; no real nsc store or global machine configuration was touched.
This tool is not a test or CI dependency. No operator or
system-account private key is needed or retained in the repository. The
`credential_rotation` target uses the declared Rust NKey, JSON and base64
libraries to create nonce-bearing expiring users at runtime. It checks compact
framing and NKey verification against the existing upstream TestUser reference,
then lets the pinned NATS server independently validate its generated claims.
