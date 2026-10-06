# T3 — Real Valkey authentication during file replacement

Outcome:
Extend the existing Valkey target so replacement/rejected/pending file
credentials are demonstrated at real maintained-connection AUTH and subsequent
connection authentication, rather than inferred from an old usable session.

Consumes:
- T1 implemented cache telemetry and released cache guide ownership.
- [Specification R3](../spec.md#r3-real-authentication-during-nats-and-valkey-file-rotation)
  — accepted Valkey outcomes and reusable mock scope.
- [Design Valkey fixture](../design/design.md#valkey-scoped-acl-state-on-the-existing-server)
  — isolated named ACL user, scoped keys and existing server/cleanup ownership.

Provides:
- Regression coverage inside the existing `valkey` integration target and
  accurate guide text for its authenticated scope and fixture privileges.

Boundary:
Use the current pinned service and existing runner. Own only synthetic user,
keys, temporary files, clients and any fixture-local forwarding resources.
Actual AUTH acceptance is required; an old session or fake AUTH reply is
insufficient. Keep unrelated users/default ACL/server configuration untouched.
Reuse sufficient protocol mocks for malformed inputs and precise timing;
the executor chooses the smallest proving scenarios. Add no production seam,
Valkey expiry policy, runner or standalone infrastructure.

Mutable owners:
- `crates/infra-cache/tests/valkey.rs` and test-local helpers owned by that target.
- `docs/cache.md`: local-run/proof and disposable fixture rights guidance,
  preserving T1 signal documentation.

Exclusive locks:
- Valkey integration fixture owner and cache guide. At actual execution, owned
  ACL user/key namespace and disposable `CACHE_URL` fixture resources are held
  under the existing integration harness; no shared/global ACL mutation.

Final validation:
- Claim: The Valkey R3 outcomes, including old-password new-auth refusal,
  accepted replacement, rejection without success telemetry and valid pending
  recovery, actually execute on the real authenticated server boundary.
- Checks: Accepted R3 requires one matching real run in the existing local/CI
  path. Existing CI `cache_integration` owns its selected heavy route; absent
  optional local Docker is not a coding gate or authority to provision.
  Concrete cases/assertions/commands are executor choices recorded for the
  one assembled delivery owner.
- Observable: Fixture outcomes identify real authentication on the adapter's
  connection, subsequent new authentication and cleanup of only owned state.
  Skipped, zero-selected, compile-only or mock-only receipts do not prove R3.

Reopen if:
Design owns infeasible isolation/cleanup or a necessary changed production
flow; Specification owns changed authentication/recovery policy. Missing final
server evidence stays an incomplete proof scope, not a new implementation task.
