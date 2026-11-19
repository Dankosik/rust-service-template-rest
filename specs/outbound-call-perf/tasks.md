# Goal

status: done

Completion Result: Accepted on f6c99a10 (shipping source b06f10f7). Full review and both experiment repairs passed. The [comparison results](evidence/comparison-results.md) preserve 204 matched timed pairs, four matched behavior pairs, the five conditional decisions and exclusions. [Usage and verified exports](evidence/digitalocean-usage.json) and [deletion receipt](evidence/digitalocean-lifecycle.json) close the external effect.

Base: d101ce385552e29267d6e8ed42f50163582a30ad

Execution: T1–T5 assigned to fresh Sol 6.2 medium leads t1–t5 in isolated worktrees.

Completion: Public authenticated HTTP and gRPC calls exercise D1–D6 with the R1–R6 behavior and G1–G5 guarantees. Record a supported disposition for all five R7 alternatives, save matched baseline and final-candidate R8 evidence, and verify deletion of the one authorized Droplet within the total US$5 ceiling.

Global constraints:

- [Specification](spec.md) and [technical design](design/design.md) own behavior and architecture. Keep public APIs, workspace dependencies and features, configuration, shared context and origin rules, generated contracts, and vendored code outside writable scope. Preserve the accepted repository decisions linked in [G5](spec.md#preserved-guarantees).
- Implementation validates the assembled source under the repository validation budget and obtains full final review for the credential concurrency change. Local correctness checks are in scope. Local performance measurement, push, release, and deployment are outside scope.
- The Delivery Owner owns the [remote comparison and teardown](design/comparison.md) after preparation and review of the assembled D1–D6 candidate. Freeze the original baseline at `d101ce385552e29267d6e8ed42f50163582a30ad`, the D1–D6 reference, and the shared experiment source. Use isolated variants rather than permanent production switches. Freeze the final selection before its comparison with the original baseline. Integrate any evidence-supported product change through the affected owner and re-pin, revalidate, and re-review its changed scope before final acceptance.
- The comparison uses one new CPU-optimized DigitalOcean Droplet, with existing Droplets outside scope. Before create, verify the live rate, availability, total spend including cleanup margin, and off-host recovery and deletion capability under [Cost and teardown](design/comparison.md#cost-and-teardown). Save raw evidence and the recovery and deletion receipts under `specs/outbound-call-perf/evidence/`. An unavailable prerequisite or broader authority reopens Intake. Retain valid local and partial remote receipts if the external outcome remains open.
- Ready Frontier is T1–T5. The tasks own disjoint writable surfaces and require no shared mutation locks. Remote execution and optional product adoption follow preparation; they are Delivery Owner completion work.

## Tasks

- [x] T1: Reuse eligible service tokens before waiter setup and eliminate duplicate representable cutoff checks.
  - Implemented: 8a0e7b9f. Budgeted service hit shares final activity and expiry arbitration; synthetic cutoff remains distinct. Assembled validation pending.
  - Depends on: none
  - Provides: Credential acquisition fast path with unchanged stop and reuse semantics
  - Packet: tasks/1-credential-acquisition.md
- [x] T2: Reduce destination ownership and origin/framing work in prepared HTTP calls.
  - Implemented: 770161da. Borrow destination through emission; keep recorder-owned labels and wire framing. Final runtime checks pending.
  - Depends on: none
  - Provides: Prepared HTTP path with borrowed destination and equivalent admission and framing
  - Packet: tasks/2-prepared-http.md
- [x] T3: Reuse verified gRPC method labels and prepare diagnostic values on demand.
  - Implemented: b06f10f7. Verified warm labels, saturated missing paths and deferred diagnostic ownership; wire propagation unchanged. Final checks pending.
  - Depends on: none
  - Provides: Bounded gRPC observation fast path with unchanged labels and wire context
  - Packet: tasks/3-grpc-observation.md
- [x] T4: Finalize already-terminal gRPC responses before allocating shared stream state.
  - Implemented: 3267bcf4. Return empty only after terminal completion, preserve staged trailers. Assembled checks pending.
  - Depends on: none
  - Provides: Immediate gRPC terminal completion with unchanged transport and decoder ownership
  - Packet: tasks/4-grpc-terminal-response.md
- [x] T5: Prepare a baseline-compatible comparison driver and off-host evidence workflow.
  - Implemented: 1538d6ca. Shared baseline-compatible synthetic loopback overlay and isolated variants. Remote measurements and deletion pending.
  - Depends on: none
  - Provides: Reusable comparison overlay for matched remote proof and bounded teardown
  - Packet: tasks/5-remote-comparison.md
