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
