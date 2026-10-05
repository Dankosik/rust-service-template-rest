# Intent: predictable credential refresh and honest rotation guidance

Status: ready.

## Problem

The credential and trust research identified places where fleet refreshes can
coincide and where an operator could mistake rereading a file, refreshing a
token, or renewing a connection for immediate revocation. Recommendations also
overlap protections already present in remote main.

## Desired outcome

Evaluate every research recommendation, implement the ones necessary for this
template, preserve adequate existing behavior, and deliver the result in one
separate pull request. Explain which material can change in a running process,
when a new connection observes it, and which changes require restart.

## Affected actors and systems

Template adopters and operators; PostgreSQL, Redis, NATS, outbound OAuth,
inbound JWT/JWKS, TLS clients/listeners, and the external owner publishing
mounted credentials.

## Scope and non-goals

Scoped hardening and operational documentation on a clean remote-main base.
This does not request universal hot reload, a new identity platform, a new
secret-management framework, or a deployment. Unrelated local hotpath work is
outside this candidate.

## Constraints

Local edits, validation, commit, push, and creation of one separate PR are
authorized. Merge, deployment, infrastructure mutation, real credential or
configuration mutation, and reading or outputting real secret values are not.
Retain existing protocol, authority, expiry, deadline, concurrency, lifecycle,
and redaction guarantees. Technical choices belong to the phase owners.

Assumption: restart remains an acceptable adoption mechanism for material
whose existing owner admits an immutable snapshot. Reopen if a real consumer
requires continuity across such a rotation without restarting.

## Success signal

The separate PR contains justified changes with the repository's matching
validation and review, accurate rotation/revocation guidance, and an explicit
disposition and reopen condition for each research recommendation. The final
report distinguishes local proof, CI, and publication from runtime deployment.
