# Operation budgets

An admitted operation carries one fixed deadline and a cancellation lineage.
Handlers pass that context to dependency calls; each dependency clamps it to its
existing local ceiling once, before preparation, and spends the same remaining
allowance through dispatch and completion. Waiting for authentication, a
connection or a provider does not start a fresh allowance.

The always-retained `operation-context` crate exports `OperationContext`,
`Deadline` and `Stopped`. It contains no provider, transport, retry policy,
runtime configuration, observation or spawned task. `Deadline` uses a monotonic
origin and duration, including finite durations too large to add to an instant.
`instant()` returns the exact cutoff when representable; `None` on that method
does not turn a finite deadline into an unbounded one.

## Carrying a context

`OperationContext::new` accepts an existing deadline and cancellation token.
`from_deadline` and `with_timeout` create standalone scopes. `child(ceiling)`
selects the smaller of the remaining parent budget and the local ceiling;
`child_context()` retains the existing deadline while deriving cancellation.
Callers derive a child once at admission and retain it for every later stage.
Clones denote the same logical scope. Cancelling a child cannot cancel its
parent or sibling. Cancelling a parent reaches all children.

`check()` reports `Stopped::Deadline` or `Stopped::Cancelled`; `wait_stopped()`
waits without spawning work. The current adapter maps that reason into its
existing error vocabulary. An explicitly unbounded context is appropriate for
a standalone parent before a finite adapter ceiling is applied, or for a named
response lifetime. It is never a substitute for an admitted opening budget.

Context-aware adapter methods use an explicit `*_with_context` name. Existing
standalone methods enter the same enforcement path with their existing ceiling.
Use an existing context when doing work on behalf of a caller; a fresh
standalone context would discard that caller's cancellation and remaining time.

## HTTP opening and response

The hardened HTTP chain fixes one opening context before authentication and
body handling. A handler extracts `infra_http::RequestContext` and accesses its
neutral value through `operation()`. Omitting the hardened chain produces a
sanitized internal rejection. No deadline is trusted from arbitrary HTTP
metadata.

`infra_http::ResponseContext` is a separate extractor for work deliberately
continued in the response. Its lifetime is explicitly unbounded unless the
route supplies its own finite owner. It shares call cancellation without
presenting the old opening deadline as a new response allowance.

The transport checks the original opening cutoff before committing headers,
including when an authentication failure becomes ready at that cutoff. Opening
expiry retains the existing 504 Problem. Successful headers transfer the
cancellation guard into the response body; body EOF, error or drop cancels the
call. There is no new generic HTTP response-body timer. A provider-owned stream
still enforces its own finite resource lifetime.

<!-- template:begin request-budget:docs-operation-request-deadline -->
`infra_http::RequestDeadline` remains a read-only projection of that same opening
cutoff for the retained idempotency and webhook surfaces. It adds neither a new
clock nor a new response reserve.
<!-- template:end request-budget:docs-operation-request-deadline -->

<!-- template:begin grpc:docs-operation-grpc -->
## gRPC opening and response

Inbound tonic request extensions contain the finite `OperationContext` for
opening and a distinct `infra_grpc::ResponseContext` for response lifetime.
Opening uses the smaller local cap and valid caller timeout. Response lifetime
uses the caller timeout only; missing or malformed timeout metadata retains the
existing uncapped generic stream lifetime. Long valid caller lifetimes can
therefore outlive the local opening cap.

Outbound `Client::prepare_call` fixes the concrete client's policy and original
entry before any composed credential work. Its opaque `PreparedCall` exposes
the opening context and mutable headers, then consumes itself in `send`.
`FullRpc(L)` applies the selected minimum through terminal response;
`OpeningOnly(L)` applies the local cap only through headers, retaining any
propagated parent or caller lifetime afterward. Local policy alone does not
invent a wire timeout. Cancellation ends response and upload custody even when
the response is retained without being polled. See [gRPC](grpc.md).
<!-- template:end grpc:docs-operation-grpc -->

<!-- template:begin authn:docs-operation-authn -->
## Authentication waits

Inbound authentication spends the opening context and checks it again before
protected dispatch. Request cancellation bounds that caller's wait without
cancelling the process-owned JWKS refresh worker or another caller. Uncached
introspection clamps its provider bound to the caller; cached introspection
keeps native Moka coalescing and initializer takeover. Dropping the initiating
future can let another live waiter initialize again, so this contract does not
promise exactly one physical exchange across cancellation. See
[Authentication](authentication.md).
<!-- template:end authn:docs-operation-authn -->

<!-- template:begin outbound-http:docs-operation-outbound -->
## Outbound HTTP

`execute_with_context` bounds preparation, dispatch and complete buffered EOF.
The existing `execute(request, deadline)` also honors an `OperationContext`
request extension. No successful complete buffered response is delivered from
an unfinished expired exchange. Timeout or cancellation does not prove a remote
effect was rolled back, and this layer never replays it. See
[Outbound HTTP](outbound-http.md).
<!-- template:end outbound-http:docs-operation-outbound -->

<!-- template:begin outbound-auth:docs-operation-oauth -->
Authenticated HTTP retains one resource allowance across credentials and the
resource exchange. Each credential waiter has its own stop bound; process
refresh and shared cache ownership remain independent. Token rejection only
invalidates future reuse and never replays the resource request. See
[Machine authentication](outbound-machine-authentication.md).
<!-- template:end outbound-auth:docs-operation-oauth -->

<!-- template:begin outbound-auth-grpc:docs-operation-oauth-grpc -->
Authenticated gRPC prepares the concrete resource call before either the
cached-token or fetched-token path.
<!-- template:end outbound-auth-grpc:docs-operation-oauth-grpc -->

<!-- template:begin cache:docs-operation-cache -->
## Cache commands

The context-aware namespace operations retain one cutoff through key/command
preparation, connection acquisition and one command dispatch. The connection
supervisor's recovery remains process-owned. An unfinished stopped command
retains `Unavailable`; a mutation may already have landed and is not replayed.
See [Cache](cache.md).
<!-- template:end cache:docs-operation-cache -->

<!-- template:begin object-storage:docs-operation-storage -->
## Object storage

Context-aware put, streamed put, get, head, delete and presigned GET share one
finite operation path. SDK operation/attempt limits spend the remaining budget;
read retries and the existing single mutation attempt stay with the SDK owner.
Signature expiry for a presigned URL remains a separate parameter.

A `Download` retains its original cutoff and cancellation through confirmed
EOF. Its weak expiry task removes the SDK body, withheld final chunk, permit
and observation guard even if the caller never polls it again. There is no
producer queue or extra background read. Body drop synchronously releases those
resources and aborts the timer.

A mutation whose final SDK poll began live and returns a definitive result
retains that result even if synchronous work crossed the cutoff. A still-pending
mutation stopped after dispatch remains `OutcomeUnknown`; it is not polled or
dispatched again. The outer HTTP/gRPC/job/message terminal owner separately
enforces its own deadline. Confirmation of a mutation never authorizes a late
successful terminal response, and cancellation never establishes rollback.
See [Object storage](object-storage.md).
<!-- template:end object-storage:docs-operation-storage -->

<!-- template:begin messaging:docs-operation-messaging -->
## Message handlers

Typed registry handlers receive an `OperationContext` derived at the existing
handler start. That same cutoff governs timeout and the final success check;
a late ready success cannot ACK an expired invocation. The context is cancelled
when the invocation returns, panics, times out or drains. Broker settlement,
redelivery and shutdown keep their current owners. See
[Durable messaging](durable-messaging.md).
<!-- template:end messaging:docs-operation-messaging -->

<!-- template:begin jobs:docs-operation-jobs -->
## Job attempts

`Job::context()` returns a child view of the existing attempt deadline and token.
It does not start another attempt allowance or cancel the engine when the child
ends. Existing deadline/cancellation accessors remain available. Claims, leases,
retry identities and settlement remain with the job engine. See
[Background jobs](background-jobs.md).
<!-- template:end jobs:docs-operation-jobs -->

<!-- template:begin postgres:docs-operation-postgres -->
PostgreSQL retains its existing terminal reserve and native SQLx transaction,
commit and cancellation custody. This context introduces no universal reserve
or transaction replay policy.
<!-- template:end postgres:docs-operation-postgres -->
