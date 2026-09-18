## Summary

- What changed and why?

## Scope

- [ ] Runtime behavior changed (`crates/`)
- [ ] Dependencies or toolchain changed (`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`)
- [ ] CI/CD workflow or quality gates changed (`.github/workflows/`, `Makefile`, `make/`)
- [ ] Documentation or agent instructions changed (`docs/`, `AGENTS.md`, `README.md`)
- [ ] Roadmap stage delivered or re-scoped (`docs/roadmap.md`)

## Test Evidence

- [ ] `make build` and the relevant tests passed, or `make verify` recorded a passing receipt for the changed surfaces
- [ ] `ALLOW_FULL=1 make check` passed when the change spans the full repository
- [ ] Unverified remainder named, or none

Commands/output summary:

```text
paste concise command output or links to CI evidence
```

## Security Impact

- [ ] No security-sensitive changes
- [ ] Security-sensitive changes included (authn/authz/input validation/secrets/logging)

Notes:

## Rollback Notes

- [ ] Not needed (low risk)
- [ ] Required (describe rollback or mitigation path)

Rollback plan:
