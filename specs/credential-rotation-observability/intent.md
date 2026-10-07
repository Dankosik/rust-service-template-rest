# Intent: observable credential refresh and authenticated rotation proof

## Problem

The refresh-hardening work in PR #247 leaves two useful follow-ups: operators
cannot consistently observe credential refresh outcomes and the age of the last
usable JWKS fetch; existing NATS/Valkey real-server tests do not demonstrate
authentication with replacement file credentials.

## Desired outcome

Deliver these two improvements together in a separate PR, with accurate,
secret-free operational signals and regression coverage that proves the real
authentication boundary. Preserve the existing rotation and failure policies.
The requester accepted this recommendation with “Хорошо, давай сделаем то, что
ты рекомендуешь”.

## Affected actors and systems

Operators reading diagnostics; maintainers changing the PostgreSQL, Valkey,
NATS and OIDC/JWKS adapters; the existing local and CI integration suites.
External credential publishers and identity providers retain their roles.

## Scope and non-goals

Include credential/JWKS refresh observability and the missing NATS/Valkey real
authentication coverage. Reuse adequate PostgreSQL rotation and LISTEN reconnect
coverage. TLS certificate/CA hot reload and bounded JWKS staleness are separate
capabilities/policies. Secret-manager/cloud SDK adoption, a universal credential
framework, unrelated PR work, dashboards, deployment and live rotation are out.

## Constraints

Start from main `699887b18594088a59bcc23a049d290d089f6da1`; PR #247 is reference
evidence, not an implementation dependency or authority to import its changes.
No new availability/security defaults, readiness gates or revocation promises.
Never emit secret values, fingerprints or unbounded labels.

The accepted delivery authority covers scoped local code/tests/docs, matching
validation, commit/push and a new PR. It excludes merge, deployment, real
credential/configuration/infrastructure changes and unrelated ongoing work.
Synthetic test material and bounded fixture changes within existing local/CI
test harnesses are normal test work, not authority over live systems.

## Success signal

An operator can distinguish refresh/read progress from installation and server
authentication, and see when usable JWKS material was last obtained. The
NATS/Valkey regression coverage cannot pass merely because authentication is
disabled, an old connection remains usable, or a file was reread. The separate
PR reports exactly which local/CI boundaries actually ran.
