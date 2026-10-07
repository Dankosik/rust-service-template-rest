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
| Candidate history secret exposure | `ALLOW_HEAVY=1 make secret-scan-history` | `HEAD` and every reachable ancestor, including merged-parent ancestry |
| Workflow security weaknesses | `GH_TOKEN=$(gh auth token) make zizmor` | `.github/workflows` and `.github/actions`; the token enables the online audits |
| Runtime image vulnerabilities | `ALLOW_HEAVY=1 make container-security CONTAINER_IMAGE=<tag>` | Debian packages and the `cargo-auditable` Rust list inside the image; fixable HIGH and CRITICAL fail |

An advisory ignore in `deny.toml` names the crate, why it is acceptable, and
what reopens it; there is none at present. A license the graph does not use
is not listed: cargo-deny warns on an unmatched allowance, and a crate that
brings a new license adds its line in the same change.

Candidate-history proof applies when admission requires the candidate's full
ancestry, such as a tag or manual run. The existing target uses native
`--log-opts=HEAD` and refuses with exit 2 before scanning if Git cannot establish
a non-shallow repository and a readable `HEAD` commit. CI retains `fetch-depth: 0`.
Unrelated fetched refs do not change the candidate's admission input.

A repository-wide audit is a separate explicit action against all locally
available refs. From a complete checkout, use the pinned scanner with the same
configuration, ignore file, redaction and failing result:

```sh
. ./tools/versions.env
go run github.com/zricethezav/gitleaks/v8@v"${GITLEAKS_VERSION}" git \
  --no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml \
  --log-opts=--all .
```

This audit does not fetch additional refs or add a candidate admission gate.
Dependency Review runs in CI only, on pull requests, and needs the repository's
dependency graph (an operator setting). CodeQL for Rust and Actions runs in
`codeql.yml` and has no local command.

Security validation supplements the negative-path behavior proof at each
trust boundary (the rejected oversized body, the refused ambient credential);
it does not replace it.
