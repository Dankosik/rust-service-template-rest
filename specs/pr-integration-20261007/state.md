# Integration of open pull requests

## Accepted scope and authority

Integrate all 18 open pull requests captured on 2026-10-07 into one candidate based on main `5ba71a07dc1bc37fb8259e78e46ea3d195a3ffe7`. The user authorized all changes and technical conflict resolution, integration branch publication and eventual main integration. The root continuation owner retains the final main merge. Existing PR specifications own accepted behavior; this integration adds no capability or roadmap decision.

## Fixed inputs

| PR | Head | Accepted intent |
| --- | --- | --- |
| [237](https://github.com/Dankosik/rust-service-template-rest/pull/237) | `856b16d0e567cc966f30ddde19595ced67cc88b2` | ci(initializer): lint every runtime graph |
| [239](https://github.com/Dankosik/rust-service-template-rest/pull/239) | `7223ea877f031d440842d3df6876857e91492ec2` | Harden messaging durability admission and recovery proof |
| [240](https://github.com/Dankosik/rust-service-template-rest/pull/240) | `cf0f1b7a816fd63e6fc019aa77b1a3eb45fcbc4b` | fix(jobs): close attempt custody and rehearse durable effect recovery |
| [241](https://github.com/Dankosik/rust-service-template-rest/pull/241) | `f0454c9b6fb3088443dfb3bea7c59a2cc38e347e` | Observe PostgreSQL maintenance and qualify sustained cleanup |
| [242](https://github.com/Dankosik/rust-service-template-rest/pull/242) | `b0f9899dd90ca1e1002e4dbf7575e3799a381e08` | fix: preserve deadline, authentication and cache time bounds |
| [243](https://github.com/Dankosik/rust-service-template-rest/pull/243) | `d200ef09ee88014995e2b07515a16340d049459b` | fix(health): reject stale recovery and bound pool admission |
| [246](https://github.com/Dankosik/rust-service-template-rest/pull/246) | `e41034e6fc2626f3619503944f27cbb38798d856` | fix: bound outbound connection recovery and preserve destination trust |
| [247](https://github.com/Dankosik/rust-service-template-rest/pull/247) | `208760df3fc10c721985953abb2b650998881372` | fix: stagger credential refresh and clarify rotation semantics |
| [250](https://github.com/Dankosik/rust-service-template-rest/pull/250) | `2cb871895b9edd018205fc98223477e269fce2e9` | fix: verify template inputs and derived release artifacts |
| [251](https://github.com/Dankosik/rust-service-template-rest/pull/251) | `f481a206f83a95c7aa3d24126503095d770cb84d` | Release failed download bodies and document cancellation ownership |
| [252](https://github.com/Dankosik/rust-service-template-rest/pull/252) | `3bf1b5ffa293896d164b6fe7b9771175d118f244` | Propagate fixed operation budgets across transport and provider boundaries |
| [253](https://github.com/Dankosik/rust-service-template-rest/pull/253) | `d43796fe736959bb412ffc2165d1fe356b3cc34d` | fix: bound gRPC openings and S3 download lifetimes |
| [257](https://github.com/Dankosik/rust-service-template-rest/pull/257) | `b13ef89dc94619e5121f1073097900e2f26b5017` | Strengthen messaging admission and add recovery rehearsals |
| [258](https://github.com/Dankosik/rust-service-template-rest/pull/258) | `727861b031522c5f7f0a77453225c8ad702d4687` | feat: observe credential refresh and prove authenticated rotation |
| [259](https://github.com/Dankosik/rust-service-template-rest/pull/259) | `a13a28a2b624047c0635c1a9f912c25f88bc9299` | fix(runtime): fail stalled readiness and retain validation custody |
| [260](https://github.com/Dankosik/rust-service-template-rest/pull/260) | `3cc1e29e008fcacd38d1e7af98db552f400533de` | Keep validation fair and support safe child cancellation |
| [261](https://github.com/Dankosik/rust-service-template-rest/pull/261) | `20d8f0e98acf836ddb815ff2487b86377cfc9716` | feat: support runtime upgrades and native consumer recovery |
| [263](https://github.com/Dankosik/rust-service-template-rest/pull/263) | `cd8040d5069eca52734c295984ab8c0da47d00de` | Compose transport recovery with fixed operation deadlines |

## Single implementation unit

- Unit: integration.
- Owner: integration lead, exclusive mutation of this worktree.
- Branch: `codex/integrate-open-prs-20261007`.
- State: Implementing.
- Output: one assembled candidate with all fixed heads retained as ancestors where compatible.
- Constraints: preserve main's later invariants, existing required CI gates and opt-in experiment boundaries. PR 241 retains baseline cleanup policy; unfinished measurements are not delivered measurements. Historical proof remains attached to its original candidate.
- Remaining: assemble merges, record semantic conflict dispositions, hand off Implemented, run one assembled validation plan, resolve independent final review and CI, root merges only after required evidence.

## Integration dispositions

Entries below record only conflicts or equivalence decisions. Final proof belongs to the assembled candidate, not individual merge commits.

- #263: composed transport implementation merged unchanged from its fixed head. Known original CI failures remain delivery work, not passing evidence.
- #261 (also #250 ancestry): retain runtime upgrades, candidate HEAD ancestry admission, source/derived image proof and native recovery. Combine exact public-fixture Gitleaks matches from #263 with #261's exact receipt digests; do not restore its broader fixture-path exclusions. Profile markers and image-input assertions are additive. Git batch reads use temporary request and response files, retaining both deadlock avoidance and bounded duplicate memory.

- #241 (also #260 ancestry): add dated population observation and queue ownership without selecting a new cleanup policy. Preserve main's 5-second statement bound; all experimental budget/pacing/spread constants stay disabled (P0). Keep #263's fully joined worker stdout capture and add the bounded failure diagnostic. Combine source-only profile pruning, monitoring rules, image identities and queue custody. Historical replay patches remain bound to their original immutable inputs.

- #259: retain progress-loss shutdown in both process roots, fresh useful-work and two-instance recovery proof, plus context-aware validation receipts and opt-in private compiler cache. Queue v3 remains the owner; obsolete custody calls are adapted to its authenticated API. PostgreSQL TLS fixture dependencies remain whenever PostgreSQL is retained, including unauthenticated PostgreSQL-only projections. Worker capture keeps joined full stdout and the same tuple order for all callers. Lockfile reconciliation is deferred until every merged manifest is present.

- #237: retain Clippy over every initializer runtime graph and full graph lint; union its additional jobs-profile pruning markers with current worker preparation/pool markers.

- #257: compose stricter messaging topology/transfer admission, durable-effect/DLQ/R3 recovery and opt-in capacity rehearsal with #263 transport recovery. Keep fair v3 validation custody instead of restoring the old directory lock or manual unlink recovery. Retain exact historical public-fixture fingerprints and keep them in derived services whose reachable accepted history includes those commits; #261's candidate-ancestry scan owns that boundary. Native regression workflow markers remain #263's production-graph carrier rather than the superseded single-provider job.

- Inherited #263 CI repair: PostgreSQL trust-rotation assertions now require native `InvalidCertificate(UnknownIssuer)` through either the SQLx TLS wrapper or rustls's `io::ErrorKind::InvalidData` handshake wrapper. Other I/O failures do not satisfy the oracle. The existing relay handshake body is extracted to satisfy the pinned nesting lint without changing the exchange. Runtime reproduction remains final validation work.

- Inherited #263 initializer repair: S3-only projections retain the `rcgen/pem` and `aws-lc-rs/untrusted` edges selected by the S3 TLS fixtures. Existing locked/offline profile checks remain the proving surface; no registry upgrades or unguarded fallback resolution were added.

- Inherited #263 quota-runner diagnosis: compile the external offer driver with the existing release profile, matching the optimized specimen. Input rates, capacity, occupancy threshold and every assertion are unchanged. The original dev driver showed material dispatch delay; this is a causal repair hypothesis until the unchanged three-run CI scenario observes it.

- Inherited main quality-check repair: the blocking HTTP lint fixture now consumes the exact locked reqwest `compiler-artifact` reported by the existing root lint invocation. It no longer chooses foreign/stale feature metadata by global fingerprint mtime. The positive dependency-load control and all negative policy cases remain; no extra build/resolve environment was added.

- #257 native regression composition: preserve #263's fresh source-stream probe and richer #257 transfer admission; deduplicate ACK-loss/storage scenarios already covered by the expanded owner test. Adapt Batch fixtures to current native close-channel constructors and add all four pull completion regressions to the canonical exact-name native runner.

- #239: all production and test intents are already preserved by #257/#263; retain exact original head ancestry without restoring its obsolete storage helper or duplicate ACK-loss test. Keep distinct MaxAckPending/operator/DLQ-coordinate and cross-store restore guidance, with readiness described by fresh stream metadata.

- #243: freshness metrics and stale-failure rules are already retained by main/#259. Keep later terminal progress-loss precedence and the stronger prepare/retain/admit pool lifecycle: its five-second admission includes acquire, and roots retain cleanup ownership before awaiting. Do not restore the older sequential 13-second connect path or loosen the existing 12-second rejection test bound. Preserve original specification/evidence history and compatible explanatory guidance.

- #257/#260 recovery bridge: automatic demo/rehearse/measure sessions register their exact Compose project and private canonical rendered configuration before effects, use the queue's native observer, and retain that input for guardian cleanup. Manual start/inspect/redrive/stop sessions keep their intentionally persistent operator lifetime. The queue admits only named foreground `compose run --rm` under the already registered project/files; no second guardian or arbitrary command exception was added. Existing test owners contain normal/controller-termination regressions; execution remains final validation.

- #242: compose fallible authentication clocks, per-waiter calendar eligibility and fresh/retained provenance with #263 caller contexts. Moka keeps its native cancelled-initializer re-election; no background fill owner is introduced. Cache SET validates TTL before context/admission/observation and returns typed `SetError`; Retry-After rounds upward across the full Duration range. Outbound success is fixed after the last await against both original contexts, before its one terminal observation. Preserve both branches' distinct temporal/context regressions.

- #240: retain panic-contained attempt custody and reading-counter business-effect recovery. The jobs reference runs once inside the canonical owned database runner while its PostgreSQL/NATS resources remain alive; retain artifact export without resurrecting shared fixed-port CI services. Inbound documentation keeps the 14-day elapsed retention and current cleanup cadence, while removing the false claim that it covers all sender retries. Source native completion and final messaging callback submission are both required before successful close acknowledgement.

- #246: retain #263/#257 native transport and source-custody supersets, root Hyper path patch and current locked production-graph runner. Preserve the distinct provider stalled-TLS/same-client-redial regression alongside newer IP/trust replacement proof. Merge precise platform trust, resolver, discovery and TCP lifetime guidance while keeping current five-second cache setup, complete S3 lifetime, fixed gRPC receive windows and authentication contexts. Do not restore pre-custody messaging startup or remove final callback acknowledgement.

- #247: credential refresh jitter/runtime is retained in the composed source. Restore the distinct periodic-refresh/unknown-key and admitted-token scheduling regressions, plus precise rotation guidance (coherent mTLS generations, fixed cache username, existing-session limits and issued-token validity). Keep current TLS-first admission/close receipt and the five-second cache setup budget. Historical research and completion evidence remains tied to its original source.

- #258: add bounded credential-file challenge/refresh observations and latest usable JWKS acquisition time without changing rotation or authentication policy. Preserve #242's unusable-clock refusal instead of restoring the older sentinel-time explanation. Both static and expiring-credential broker proofs run in separately owned sessions. New fixture JWT exceptions match exact public bytes and exact path, alongside retained prior source custody.

- #240/#263/#261 derived proof boundary: pin only the historical scoped jobs runtime patch to #240 `cf0f1b7a816fd63e6fc019aa77b1a3eb45fcbc4b`, with raw immutable-blob SHA-256 comparisons and a separate `runtime_adoption_source` receipt field. Current source scenarios and portable sync retain the actual assembled candidate identity. Do not silently widen the historical service-owned Cargo/business preservation contract to a full #261 runtime upgrade; its separate rehearsal owns that broader claim.
