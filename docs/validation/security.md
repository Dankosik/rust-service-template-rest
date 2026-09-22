# Security Validation

Select these commands for an explicit verification requirement or a bounded
diagnostic. The domain label alone adds no local gate. The service's CI policy
owns security and image-scan admission scope, including range versus
full-history secret scanning and missing-base handling.

| Claim | Command | Observes |
| --- | --- | --- |
| Dependency advisories, licenses, bans, sources | `make deny` | the locked graph for the two Linux gnu targets under `deny.toml` |
| Declared dependency no crate uses | `make unused-deps` | every `Cargo.toml` against the source (cargo-shear) |
| Current reviewable secret exposure | `make secret-scan BASE_REF=origin/main` | the worktree and the commits since the base (Gitleaks, `.gitleaks.toml`) |
| Full history secret exposure | `ALLOW_HEAVY=1 make secret-scan-history` | every commit on every branch |
| Workflow security weaknesses | `GH_TOKEN=$(gh auth token) make zizmor` | `.github/workflows` and `.github/actions`; the token enables the online audits |
| Runtime image vulnerabilities | `ALLOW_HEAVY=1 make container-security CONTAINER_IMAGE=<tag>` | Debian packages and the `cargo-auditable` Rust list inside the image; fixable HIGH and CRITICAL fail |

An advisory ignore in `deny.toml` names the crate, why it is acceptable, and
what reopens it; the current one is `paste` through `utoipa-axum 0.2.0`,
reopened by the next `utoipa-axum` release. A license the graph does not use
is not listed: cargo-deny warns on an unmatched allowance, and a crate that
brings a new license adds its line in the same change.

Full-history proof applies only when the claim spans repository history, such
as a tag or manual run. Dependency Review runs in CI only, on pull requests,
and needs the repository's dependency graph (an operator setting). CodeQL for
Rust and Actions runs in `codeql.yml` and has no local command.

Security validation supplements the negative-path behavior proof at each
trust boundary (the rejected oversized body, the refused ambient credential);
it does not replace it.
