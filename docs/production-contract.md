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
- [HTTP idempotency decisions](decisions/http-idempotency-decisions.md) give
  the reasons for the replay policy and when a derived service should revisit it.
- For mounted idempotent HTTP operations, publish the client retry window and
  how to look up the original effect in the service's public API documentation.
  Measure the window from the client's first send to the database check on
  retry. Include transit, request processing and pool wait in the client's
  budget. Set `http_idempotency.retention` on every writer replica to at least
  the published window. The database sets expiry when it writes the outcome,
  before commit and delivery. Publish a longer window only after every writer
  supports it, with an effective time for new requests. Earlier requests keep
  their original window. When shortening retention or rolling back, honor
  requests first sent under the earlier promise until that window expires.
  Keep the set of declared request headers used in the fingerprint stable on
  each method and URL through rolling updates and rollbacks. When that set
  changes, publish a versioned URL outside the old route's path pattern and
  keep serving the previous contract through the latest retry deadline for
  requests first sent to the old URL.
  Clients retry on the original URL with the original key and use fresh keys
  for new requests to the versioned URL.
  Supply the absolute HTTP(S) URI of this policy when registering the operation
  (`idempotent_route!(handler, docs = "...")` or OpenAPI `externalDocs.url`).
  The service checks this at registration and returns that link on key errors.
  Before enabling `FreshCommand::ALLOW_TERMINAL_REFUSALS`, apply the
  migrations for stored 409 and 422 responses and deploy readers for those
  responses on every replica. Use `Rollback::terminal(problem)` for the
  definitive business decisions `Conflict`, `AlreadyExists` or
  `UnprocessableContent`. When rolling back, stop creating new terminal
  records first. Keep capable readers on the route until all existing terminal
  records expire. An older reader returns an integrity error on a matching
  live 4xx record and cannot replay its body.
<!-- template:end http-idempotency:production-contract-retry-window -->
<!-- template:begin inbound-webhooks:production-contract-receipt-recovery -->
- In the PR that adds the service's first inbound webhook handler, include
  tests for business duplicates and out-of-order events. Deliver the same
  business operation with two distinct `webhook-id` values and assert that
  its business effect occurs once. Deliver two events for the same entity in
  reverse order and assert the expected business result. For versioned state,
  verify that the older event cannot overwrite the newer state. For
  order-independent operations, verify the same expected result in both orders.
- Before relying on a late webhook redelivery, retire every receiver running
  the old 14-day receipt cleanup, including rollback replicas and scheduled
  maintenance. Receipts that still exist protect their delivery IDs. Reconcile
  older events against business records before replaying them: an ID pruned
  before the cutover cannot be recovered from the receipts table. Restore
  receipts alongside the processing jobs and their business effects.
- Budget PostgreSQL storage and backup capacity for one retained receipt per
  distinct delivery on each endpoint. Keep endpoint IDs stable across
  deployments. An irreversible effect outside the processing transaction needs
  its own idempotency key at the recipient.
<!-- template:end inbound-webhooks:production-contract-receipt-recovery -->
- RPO, RTO, backup/key custody, and restore-compatible versions: Unresolved.
- Cache staleness, TTL, invalidation, and token/key revocation lag: N/A for the
  health-only scaffold; reopen when cached state or authentication is enabled.

## Edge and trust

<!-- template:begin webhooks-common:production-webhook-key-custody -->
Provision a separate HMAC key for each webhook endpoint, direction and
environment. During rotation, keep an endpoint's predecessor exclusive to
that endpoint. Before upgrading a deployment that shared secrets, coordinate
new keys with the affected partners and retire shared predecessors before
starting the new replicas. Reconcile deliveries missed during cutover by ID.
The API and worker load separate secret sets, so cross-process ownership
belongs to provisioning.
[Standard Webhooks signing keys](https://github.com/standard-webhooks/standard-webhooks/blob/3d65b1824b6697f05a5a4584e7062bd23512fc8e/spec/standard-webhooks.md#signature-scheme).
<!-- template:end webhooks-common:production-webhook-key-custody -->

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

<!-- template:begin webhooks:production-contract-worker-egress -->
For outbound webhooks, record the network policy enforced on the delivery
worker, its owner, the partner destinations, and any private dependencies
that the worker must reach. Restrict outgoing connections to those destinations
on IPv4 and IPv6. A configured HTTPS origin authenticates the peer after the
connection starts. It cannot restrict the IP selected by DNS.
If the worker also needs internal HTTPS services outside the partner allowlist,
isolate the webhook delivery path so it cannot use those permissions.

Before promotion, verify the deployed policy from the worker's network
identity. Confirm a signed delivery to a controlled, permitted HTTPS receiver
and successful access to required dependencies. On fresh connections, confirm
that the network denies access to controlled destinations in prohibited
subnets, including after the test name changes address. Record the policy
revision and the observed permits and denials. Repeat when worker networking
or the destination policy changes.
<!-- template:end webhooks:production-contract-worker-egress -->

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

<!-- template:begin outbound-auth:production-keycloak-registration -->
### Registering an outbound OAuth client with Keycloak

Start with the HTTPS realm issuer supplied by the identity administrator. At
provisioning, qualify the exact issuer and token endpoint that will be passed
to the synchronous registration callback. The check requires only those URLs.

```sh
cargo run --locked -p infra-oauth2-client-credentials --example keycloak_preflight -- "$ISSUER" "$TOKEN_ENDPOINT"
```

Install the checked pair as `assertion_audience` and `token_url` after a
successful exit. Repeat on changes to either value. An integration with async
setup can instead call `Registration::prepare_keycloak(options).await` to run
the same check before it creates a registration. Discovery must return the
configured issuer exactly and the configured HTTPS token endpoint. The
endpoint can have a different host from the issuer. On failure, correct the
configuration or restore connectivity before installing it. Keep the current
registration during a planned replacement until the new one passes. The
resource origin and recipient scope are configured separately for each
downstream service.
[Keycloak OIDC endpoints](https://www.keycloak.org/securing-apps/oidc-layers)

### Rotating the Keycloak client's signing key

Keep the same confidential client ID, grant and recipient scope. Generate a
new key pair with a distinct `kid`. During planned rotation, register both
public keys on that client, verify a fresh grant with the replacement, switch
the signers and remove the retired public key. During emergency withdrawal
remove the compromised key immediately. Preserve the client's existing
scopes, audience mappers and authentication settings when updating its keys.

Verify two independent outcomes in a rehearsal with controlled keys. First,
a fresh client assertion signed with
the withdrawn key receives `invalid_client` and a fresh assertion with the
replacement obtains a token. Use an acquisition path with an empty token
cache. Second, introspect the original, previously active access token as
its intended recipient. Repeat until it becomes inactive, using an active
replacement token as a positive control. Record the remaining access window
and the provider and resource revocation policies. If the incident requires
immediate denial at the resource, invoke its access-token revocation or
isolation procedure and verify it there.

On 2026-11-18 two isolated Keycloak 26.8.0 runs kept one ES256 client, its
service account and its catalog scope throughout each rotation. Both keys
obtained fresh access tokens during overlap. Times below start at the admin
request that removed the old JWK. The original catalog bearer had a
client-specific 20 s lifetime.

| Run | Key update acknowledged by | Fresh old assertion returned `invalid_client` by | Last active probe started | First inactive reply received by |
| --- | ---: | ---: | ---: | ---: |
| 1 | 4 ms | 8 ms | 19.875 s | 20.155 s |
| 2 | 5 ms | 9 ms | 20.306 s | 20.584 s |

Catalog still accepted a freshly issued new-key token after the old bearer
became inactive in both runs. The final two columns bracket the observed
transition with 250 ms between probes. Client-key withdrawal closed issuance
while the previously issued access token remained usable until around its own expiry.
Repeat for each deployment's token lifetime, resource validation policy and
cache settings.

```sh
ALLOW_HEAVY=1 REQUIRE_DOCKER=1 RUST_TEST_NOCAPTURE=1 make test-integration-oauth
```
<!-- template:end outbound-auth:production-keycloak-registration -->

<!-- template:begin service-auth:production-service-key-rotation -->
### Rotating a caller's gRPC signing key

Name the caller, every receiver that trusts it, and the TLS address of every
serving replica, including old deployments still draining. Record the accepted
time to revoke new calls and who can take a stale replica out of service.
Keep private keys in their secret store. Use distinct key material and
`kid` values for the old and replacement keys.

Save a trusted copy of the previous public registry. A receiver's new
registry must have a unique SHA-256 JWK thumbprint for each public key,
including keys assigned to different callers. Distinct callers may use
the same `kid` for different keys. Before retiring a key, check that its
material is absent throughout the candidate. The comparison accepts aliases
in a historical registry so operators can remove them during an upgrade.
[RFC 7638 §3](https://www.rfc-editor.org/rfc/rfc7638.html#section-3)

```sh
cargo run --locked -p infra-serviceauth --example check_key_withdrawal -- "$PREVIOUS_REGISTRY" "$CANDIDATE_REGISTRY" "$CALLER" "$OLD_KID"
```

Omit the final argument to withdraw all of that caller's previous keys. Use
the same check for rollback and future replica configurations to prevent a
retired key from returning under another name.

1. For a planned rotation, publish both public keys under the caller's existing
   issuer. Roll the receivers and confirm that each replica accepts a
   correctly signed call using either key for the same authorized method.
2. Switch the caller's signing key and `kid` together. Confirm every signer
   has switched and the new key works on every receiver. Allow outstanding
   old tokens to pass their latest expiry plus the receiver's clock allowance.
   Then remove the old public key, roll the receivers again and confirm that
   only the replacement key grants new calls.
3. For emergency withdrawal with a trusted standby, disable the old signer and
   remove its public key from the authoritative trust registry immediately.
   Start the revocation clock when withdrawal is ordered, before distributing the change. Roll
   receivers from the revised registry and retire every old process. A replica
   that has loaded the previous registry can still accept fresh signatures.
   Isolate any replica that cannot be replaced within the service's budget.
   Preserve the revoked key's exclusion across rollback.
4. Rehearse with controlled keys. Address each replica directly, including any
   old instance that can still serve during rollout. Count a replica as revoked
   when it rejects a still-valid token under the withdrawn key with
   `UNAUTHENTICATED` and accepts the same call under the replacement key.
   Record both observations, replica identity, and elapsed monotonic time.
   A timeout, connection failure or drained instance alone cannot establish
   that the replacement enforces the new registry. Stop the fleet clock after
   the last pair of observations and the retirement of all old processes.

The result bounds admission of new RPCs. Already admitted streams have their
own lifetime and drain policy. Record the deployment's measured result and
accepted budget together, including configuration propagation, rollout overlap
and draining.

From the repository root, run the drill against three independent TLS receivers.

```sh
cargo run --release --locked --manifest-path crates/service/Cargo.toml --example service_auth_rotation
```

The receivers use loopback TLS and ephemeral signing keys. The drill verifies
old-token reuse, overlap, caller cutover and retirement. Its sole synthetic caller accounts
for every in-flight RPC. In the separate emergency trial the standby key is
already trusted. The clock starts before launching the first replacement and
ends after the final sweep of all three. The receipt gives cumulative upper
bounds for each replica and the fleet. Repeat on the deployment platform to
include secret distribution, scheduling and traffic cutover in its budget.

On 2026-11-18, all three release-build trials on macOS arm64 passed the
planned rotation. In each emergency trial a stale process still accepted a
fresh old-key token while its replacement rejected those same bytes. Every
replacement also accepted the new key. Observed time since withdrawal, rounded
up to the next millisecond:

| Trial | Replica 1 | Replica 2 | Replica 3 | Final fleet confirmation |
| --- | ---: | ---: | ---: | ---: |
| 1 | 542 ms | 1,081 ms | 1,620 ms | 1,621 ms |
| 2 | 682 ms | 1,399 ms | 1,967 ms | 1,970 ms |
| 3 | 537 ms | 1,075 ms | 1,624 ms | 1,626 ms |

The local rollout replaced one receiver at a time with a prebuilt Rust 1.100.0
binary, one runtime worker per receiver, a 300 ms readiness propagation delay
and unary probe traffic only. Input changes were passed directly to child
processes. A production revocation budget remains a service-owned decision.

### Emergency withdrawal without a standby

When removing the last trusted key leaves the receiver without a usable
registry, fence all receivers that trust the compromised key. Prevent the
deployment controller from recreating the old configuration. Start the clock
at the withdrawal order. Confirm that every old process has exited or is
isolated from all callers, including clients with existing connections.
Record the elapsed time when no old instance can admit a call. During this
interval the protected service can be unavailable.

Generate the replacement, distribute the new public registry, install the
signer and restore the receivers. Record a second elapsed time from the same
withdrawal order when all intended replicas are reachable, accept a valid
new-key call and refuse a valid old-key call with `UNAUTHENTICATED`. Track
the containment and restoration budgets separately.

```sh
cargo run --release --locked --manifest-path crates/service/Cargo.toml --example service_auth_rotation -- --cold
```

On 2026-11-18 three release-build trials on macOS arm64 started with only
the old key trusted. The local operator sent `SIGKILL` to the three owned
receivers. At containment all three processes had exited, old calls on
established channels returned `UNAVAILABLE` and every direct TCP endpoint
refused connections. At restoration three healthy TLS replacements rejected
fresh old-key calls with `UNAUTHENTICATED` and accepted new-key calls.

| Trial | Contained, 0 of 3 serving | Restored, 3 of 3 serving |
| --- | ---: | ---: |
| 1 | 13 ms | 692 ms |
| 2 | 14 ms | 695 ms |
| 3 | 12 ms | 691 ms |

Both columns are cumulative upper bounds from withdrawal, rounded up to the
next millisecond. Replacement signing material was generated only after
containment. Recovery included starting three prebuilt receivers, readiness,
TLS and direct unary probes. Production traffic switching, an external
secret manager and operator response time were outside this loopback drill.
<!-- template:end service-auth:production-service-key-rotation -->

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
4. Invalidate pre-restore tokens, operator commands, and their receipts, re-inspecting
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
