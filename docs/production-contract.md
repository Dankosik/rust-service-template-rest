# Production Contract

Status: **Unresolved — production promotion is blocked until the owning
service accepts every applicable field.** Use `N/A` only with a concrete
reason. This file becomes service-owned with the first production feature;
`crates/config`, `crates/service/src/bootstrap`, `api/openapi/service.yaml`,
and each retained `crates/infra-*` adapter own the template's runtime defaults,
limits, probes, and exit codes.

## Service scope

- Business capabilities: Unresolved.
- Authoritative facts and data owners: Unresolved.
- Public and internal interfaces: Unresolved (`api/openapi/service.yaml` and
  `crates/service/src/bootstrap` define the scaffold's application and private
  diagnostics listeners).

## Dependency contract

| Dependency | Criticality | Readiness | Deadline | Concurrency | Retry owner | Identity | Ambiguous outcome | Recovery |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved |

Register a readiness probe only for dependencies whose loss makes the replica
unable to serve, and budget every downstream deadline inside
`http.request_timeout` (or the caller's gRPC deadline).

## Capacity and resource envelope

- Expected and burst arrival rate: Unresolved.
- Service time, payload bounds, and expected concurrency: Unresolved (size
  `crates/config` listener, body, header, and in-flight limits from workload
  measurements rather than scaffold defaults).
- First capacity ceiling and required headroom: Unresolved.
- Database connections, worker concurrency, and queue-age objective: N/A for
  the health-only scaffold; when retained, budget across peak API/worker
  replicas, rolling overlap, dedicated `LISTEN` sessions, migrator/admin
  reserves, and pooler front/back limits.
- Surviving capacity after one failure-domain loss: Unresolved.
- Cold/unavailable cache source capacity (loaders, waiters, replicas): N/A for
  the health-only scaffold; reopen when cached state is adopted.
- Streaming, background, and blocking work: bound body lifetimes, durable queue
  backlog/retention, and CPU/blocking task permits through actual execution
  completion rather than request-head admission alone.

## Consistency and durability

- Transaction and read guarantees: Unresolved.
- Asynchronous propagation, replay, deduplication, and retention: Unresolved
  (for each durable job, event, or webhook handler, define logical effect
  identity, idempotency/reconciliation boundary, and retention horizon).
<!-- template:begin http-idempotency:production-contract-retry-window -->
- For mounted idempotent HTTP operations, publish the client retry window and
  how to look up the original effect in the service's public API documentation.
  Measure the window from the client's first send to the database check on
  retry. Include transit, request processing and pool wait in the client's
  budget. Set `http_idempotency.retention` on every writer replica to at least
  the published window. The database sets expiry when it writes the success,
  before commit and delivery. Publish a longer window only after every writer
  supports it, with an effective time for new requests. Earlier requests keep
  their original window. When shortening retention or rolling back, honor
  requests first sent under the earlier promise until that window expires.
<!-- template:end http-idempotency:production-contract-retry-window -->
- RPO, RTO, backup/key custody, and restore-compatible versions: Unresolved.
- Cache staleness, TTL, invalidation, and token/key revocation lag: N/A for the
  health-only scaffold; reopen when cached state or authentication is enabled.

## Edge and trust

- TLS termination and trusted proxy topology: Unresolved.
- Gateway retry and fleet rate-limit owners: Unresolved (the service sheds
  in-flight overload per replica rather than enforcing fleet rate limits).
- Diagnostics/metrics listener network isolation: Unresolved (keep the
  diagnostics port unreachable from untrusted networks).
- Egress, identity, and authorization authorities (trusted issuers, audiences,
  callers, and destination origins): Unresolved.
- For any connection carrying credentials over application-level plaintext,
  identify what authenticates the intended peer and protects the whole route,
  including the hop after any TLS termination. Use verified TLS when the
  deployment cannot establish those properties.
- For each rotating credential, record the issuer and consumers, how a new
  authentication with the replacement is confirmed, when the old credential
  is revoked, and how existing sessions are handled during emergency revocation.
  Set the permitted revocation delay from the service's threat model.
  Grant each process access only to the credentials it uses. A role-specific
  configuration snapshot cannot restrict access to a shared volume or environment.

<!-- template:begin authn:production-bearer-delegation -->
When migrating a handler that used `VerifiedPrincipal::access_token()`, keep
identity-only handlers on `VerifiedPrincipal`. For each HTTP operation that
exchanges the caller's token, declare `x-bearer-delegation: true` alongside
its bearer security requirement. The handler can then read
`infra_http::DelegationCredential` from request extensions. Record the
authorized downstream audience for that exchange.
<!-- template:end authn:production-bearer-delegation -->
<!-- template:begin grpc-authn:production-rpc-delegation -->
In bearer mode, declare a scope rule for every registered business RPC. Use
`require_scopes(path, &[])` when any verified principal may call the method.
Register an RPC that performs token exchange with
`require_scopes_for_delegation` and its required scopes. Read
`infra_grpc::DelegationCredential` from the Tonic request extensions after
admission.
<!-- template:end grpc-authn:production-rpc-delegation -->

<!-- template:begin oidc-jwt:production-jwt-issuer-contract -->
For each JWT issuer, qualify the access-token profile against the provider's
actual configuration. Verify an access token for this API succeeds and an ID
token from the same issuer is refused on each enabled transport. For a
`resource-server` integration, record how the issuer separates these token
kinds, for example by reserving a distinct audience for the API. Confirm that
each signing key is bound to one algorithm, including during rotation.

Choose `authn.jwks_max_age` from the maximum acceptable delay in observing a
withdrawn key and the provider outage the service must survive. Allow for the
normal refresh interval and acquisition time. Exercise an outage across the
chosen limit, then restore the endpoint and confirm authentication recovers.
For a discovered JWKS endpoint, rehearse moving `jwks_uri` while the previous
endpoint still serves its old keys. Verify the replica accepts the new signing
key and rejects the withdrawn key without restarting.
<!-- template:end oidc-jwt:production-jwt-issuer-contract -->

<!-- template:begin oidc-introspection:production-introspection-issuer-contract -->
Qualify the introspection endpoint with the credentials assigned to this API.
Confirm a valid access token is admitted and that a token for another resource
and a refresh token are refused, including when sent with the access-token
hint. The hint assists lookup. The issuer must supply enough evidence to
distinguish tokens usable at this resource. If positive caching is enabled,
record the revocation delay the service accepts.
<!-- template:end oidc-introspection:production-introspection-issuer-contract -->

## Operation and recovery

- SLI, SLO, and alert thresholds: Unresolved for service-specific routes and
  objectives ([`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml)
  and [Alerting](alerting.md#runbooks) own shipped defaults).
- Rollback authority and mixed-version compatibility window: Unresolved
  ([Railway Deployment Profile](railway-deployment-profile.md#rollback)).
- Platform termination grace: must equal or exceed `http.grace_period` validated
  by `crates/service/src/bootstrap`.
- Reconciliation policy, missing-data disposition, and observed restore proof:
  Unresolved.

Independent stores (PostgreSQL, JetStream, object storage) have no coordinated
snapshot guarantee. Any multi-store recovery runbook must:

1. Fence producers, worker claims, retention owners, and external effects.
2. Restore the verified artifact, compatible configuration, and stores into an
   isolated environment while preserving migration history, sequences, and
   consumer state.
3. Reconcile business identities, applied effects, deduplication records, and
   object digests before lifting the fence.
4. Invalidate pre-restore tokens, operator commands, and receipts, re-inspecting
   live state before resuming writes.
5. Record observed recovery timing and reconciled gaps.

<!-- template:begin jobs:production-contract-jobs-rollout -->
### First HTTP operation that enqueues a job

When a business crate first connects HTTP and jobs, register its operation
and job handler together in the service's composition code. Check the operation
in the generated OpenAPI document, send a command through the shipped HTTP
entry point, and observe the corresponding business effect after the shipped
worker processes that command. With only this handler omitted and another kind
still registered, the missing effect must fail the check even if the worker is
ready.

### Rolling out exact background-job payloads

For a service that uses PostgreSQL jobs, inventory every worker, outbox
publisher, operator and rollback image that reads the jobs table. Before
enabling a producer that writes only `payload_text`, move those readers to a
release that prefers `payload_text` and accepts historical `payload` JSONB.
Include tooling that compares a live duplicate, and exercise a retained failed
job through redrive. If an older JSONB-only reader is still in the rollout or
rollback window, deploy the dual-writing, dual-reading release first.

Apply the forward migration that permits either representation before starting
text-only producers. During overlap, exercise an old producer with a new worker
and a new producer with the oldest supported worker. Roll back the application
to the dual-reading release while keeping the expanded schema. Leave historical
JSONB-only rows in their original format: reconstructing text from JSONB cannot
recover the original encoding and changes how live duplicates are compared.

For an incompatible handler payload, register the new kind under a new name
before switching producers. Keep the old handler available until pending,
running and recoverable failed jobs of the old kind have a disposition.

Before accepting a payload containing an escaped U+0000 in a key or value,
upgrade every producer that may compare a live duplicate, including rollback
images. Older producers can refuse that comparison even with a compatible
text reader.

### Promoting a PostgreSQL replica with active jobs

Apply the additive claim migration before upgrading workers. While an older
worker can still finish an attempt, stop those processes and prevent their
connections to the promoted primary before resuming processing. Apply the
same rule when rolling back to an older worker. Once every active attempt
uses the new claim identity, a previous owner cannot acknowledge a replacement
claim even when the promoted replica reuses a sequence value.

Choose a writable endpoint and confirm that the worker loses readiness when
that endpoint is read-only and regains it after promotion. Record the RPO of
the promotion target. An asynchronous replica may lack a committed enqueue
or completion. Reconcile effects at external recipients by their stable IDs
before replaying ambiguous work, including webhooks and outbox publications.

For a PgBouncer endpoint using transaction pooling, set
`APP__POSTGRES__NOTIFICATIONS=false` on workers and producers that use that
endpoint. Allow for the polling interval when setting a pickup latency target.
Use a direct or session-pooled connection when the target requires prompt
notification after commit.
<!-- template:end jobs:production-contract-jobs-rollout -->
