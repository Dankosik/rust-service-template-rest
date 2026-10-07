# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/credential-rotation-observability/design/design.md
review: specs/credential-rotation-observability/design-review.md — PASS
movement_evidence: finite truthful metric schemas, synchronous owner updates, supported synthetic authentication fixtures and exclusive existing-harness lifecycle/CI routing are closed; fresh independent Technical Design Review found no missing mechanism, ownership or feasibility edge
reopen_owner: none
next_owner: Planning
```

## Candidate and authoritative locators

Checkout:
`/Users/daniil/.codex/worktrees/credential-rotation-observability/rust-service-template-rest`.
Branch: `codex/credential-rotation-observability-20261006`.
Base/HEAD: `699887b18594088a59bcc23a049d290d089f6da1`.

| Artifact | Current SHA256 |
| --- | --- |
| [Intent](intent.md) | `08812c2e6c27c4a793f6b70712157e4afbbad37f5709f9b600e5ac1e194cd222` |
| [Specification](spec.md) | `ba8c0acc93bbc666b24104750302a4a9bd36c3cd5d69bf42c5201bf4b5851201` |
| [Ready Design](design/design.md) | `e54d324706b45ffb6ea57d6ccd8046683b6d0ed628ab3e048f01747f83879d3d` |
| [Design evidence](design/evidence.md) | `d5dc09a5cc22d106e919f45af615537655cdeddad57b4802b07fdf10bf5bec29` |
| [Technical Design Review](design-review.md) | `468caade4cf92b17a34ff42c4cbc95530cf7cfd513c41f435fd177ae16bba74c` |

Definition inputs are unchanged by this phase. The coordinator's narrow R3
correction was consumed before fixing Design and review. This phase wrote only
`design/design.md`, `design/evidence.md`, `design-review.md` and this transition.
The ready promotion changed only Design lifecycle text after PASS. Runtime
source and main checkout remain untouched; no commit or push was made.

Current owners read from this checkout:

| Owner | SHA256 |
| --- | --- |
| [AGENTS.md](../../AGENTS.md) | `23e288567b372c69aa50a272c583e82c40ea868cc8e2141be51d191e7dbbb00b` |
| [Workflow router](../../docs/spec-first-workflow.md) | `2f4b5d255e5c960d4b7fed12b679d9f7c5a69d0758a3d4f32a4b1bf04b923295` |
| [Agent Harness](../../docs/agent-harness.md) | `1aa1029cd9fecf08fd65f3a313b12522ce9d3c190287b5fbf9553b634358d7a9` |
| [Codex adapter](../../docs/agent-harness/codex.md) | `23441c0c44bcd591172588d68e2c7224b867065e9bc609d61117c5b1555d41f9` |
| [System / Integration Design](../../docs/spec-first-workflow/phases/system-integration-design.md) | `4a5d00df22f1542c5b0af16fa12e4ca4b4a7b8e6a4508a90e98dafdcc348da2e` |
| [Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md) | `0459e327fc5f4babbb58fe4c4093dca4aea8ff7eaeca1ed2274170a845e5b7e2` |

## Closed decisions and next action

Three adapter-local counter families plus one unlabelled JWKS gauge add at most
sixteen series. PostgreSQL observes options installation, NATS observes prepared
challenges and Valkey observes accepted maintenance AUTH. JWKS records usable
local acquisition time, including unchanged keys; no signal changes policy or
misrepresents issuer freshness. Existing owners mechanically determine runtime
placement, and the Design file map closes fixture/runner ownership.

Valkey uses one synthetic ACL user and owned keys/files inside the existing
suite. NATS uses a separate authenticated integration target under the existing
runner, with one serialized operator-JWT configuration segment of the same
pinned disposable service. Explicit Compose project ownership, endpoint match,
absence of parallel consumers, restoration/failure propagation and CI selection
are mandatory mechanism constraints. External unmanaged endpoints are never
reconfigured. Installed signing/JSON/base64 support suffices for fixture-only
expiring NATS users; no SDK, new crate or generator runner is selected.

The existing root continues as coordinator and can dispatch a fresh Planning
actor with these artifacts. No further requester decision or permission is
needed for the already-authorized next phase. This Design actor stops here;
it performed no Planning or Implementation. Concrete tests/assertions/commands
and the smallest proving scenarios remain executor-owned.

## Proof boundary and reopening

Source/official-contract inspection, one supporting read-only NATS specialist
and fresh independent Technical Design Review support mechanism readiness.
The scoped static check found no missing relative file links or trailing
whitespace. The full repository `make docs-check` was not run because it starts
a Docker container and this phase prohibits environment/runtime execution.
No build, product test, real-server authentication, CI, release or deployment
result is claimed.

Downstream proof must actually execute the authenticated NATS/Valkey cases
under the current local/CI owner, including the accepted old/expired/recovery
outcomes. Mocks may cover malformed input and exact timing but cannot replace
the required real authentication boundary. Existing PostgreSQL/LISTEN and
protocol coverage is reused at its scope. A missing/zero-selected/skipped or
compile-only scenario is not a pass; optional local unavailability does not
authorize a new environment.

Reopen Research for changed base/pins/provider facts or conflicting #247 deltas;
Design for fixture isolation/restoration or owner-flow incompatibility;
Specification for observable semantics/policy; Intake only for requester meaning
or effect authority. Preserve unaffected decisions. No TLS reload, stale-key
cutoff, new readiness rule, broad #247 import, live credential mutation, merge
or deployment is included.

Both child lanes completed: `/root/credential_followup_design/nats_fixture`
and `/root/credential_followup_design/design_review`. They were read-only; no
descendant retains writable ownership or pending runtime work.
