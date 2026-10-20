# Goal
status: done
Completion: the three outcomes below are implemented on `claude/service-auth-profile`, their final validation passes under the template's Validation budget, the independent final review (authorization behaviour) has no open blocking finding, and the work is committed on that branch; pushing, a pull request and PR #274 stay with the owner.
Global constraints: [AGENTS.md](../../AGENTS.md) Engineering and Validation budget; [spec](spec.md); [decision record](../../docs/service-authentication-decisions.md); every Cargo command `--locked`; existing profiles keep their behaviour.
Result: Accepted: Completion; evidence: `make build`, `make lint`, `make test` and `make fmt-check`; the local steps of the default `make verify` route except `make deny`, which fails on RUSTSEC-2026-0346..0349 for the unchanged rustls 0.23.45 of `c0b4f320`; canonical projections, runtime graphs 66–68 and the initializer source suites; the `IMAGE_TESTS=unit` image build and `make runtime-image-check`; a `REPOSITORY_CI = none` receipt in an initialized service; final independent review PASS after repairs. Steps `make verify` leaves to CI await a CI run; candidate: the single commit of `claude/service-auth-profile` over `c0b4f320`.

## Tasks

- [x] TA: `SERVICE_AUTH=none|jwt` profile — crate, configuration, gRPC admission and binding, bootstrap, initializer support, tests and documentation (B1–B7)
  - Depends on: none
  - Provides: caller-signed service tokens on gRPC; profile selectable at initialization
  - Packet: tasks/TA-service-auth-profile.md
  - Result: Accepted: TA; evidence: the Completion result above; candidate: the single commit of `claude/service-auth-profile`
- [x] TB: no-CI verification mode — `REPOSITORY_CI`, plan, guards, receipt, targets for CI-only steps, self-tests and documentation (B8)
  - Depends on: none
  - Provides: `make verify` that owns every CI step when `REPOSITORY_CI = none`
  - Packet: tasks/TB-no-ci-verify.md
  - Result: Accepted: TB; evidence: the Completion result above; candidate: the single commit of `claude/service-auth-profile`
- [x] TC: Railway IaC and secret-free build — `.railway/` files, initializer identity and markers, policy check, surface classification, `IMAGE_TESTS` stage, documentation (B9)
  - Depends on: TA for the service-auth variables and markers it declares
  - Provides: a derived repository with Railway IaC whose image builds without credentials and runs unit tests
  - Packet: tasks/TC-railway-iac.md
  - Result: Accepted: TC; evidence: the Completion result above; candidate: the single commit of `claude/service-auth-profile`
