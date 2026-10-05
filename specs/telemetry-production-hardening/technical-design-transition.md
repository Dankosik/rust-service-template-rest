# Technical Design transition

status: ready

owner: Technical Design (System / Integration Design and Rust Ownership)

result: `design/technical-design.md`, `design/ownership.md`,
`design/component-evidence.md`, beneath `specs/telemetry-production-hardening/`

review: `technical-design-review.md`, PASS; consumed
`design/ownership-review.md`, all applicable lenses PASS

movement_evidence: every accepted S1-S4 mechanism, fixed limit, diagnostic
boundary, resource owner, actual consumer and exit mapping is selected.
Independent review closed the no-runtime migration drain and isolated hook-test
mapping findings. No new user-owned input or implementation-policy decision
remains. Only draft-to-ready status lines changed after the passing fixed-candidate
review; its semantic scope is unchanged.

reopen_owner: none

next_owner: Planning

## Exact ready identity

| Artifact | SHA256 |
| --- | --- |
| `spec.md` (unchanged accepted authority) | `d3bb54400a9f9ffb9634d2653ddbeb64ce179dea1700ad622c66fe747470e8f2` |
| `design/technical-design.md` | `00227eb490a86eaa2e4cb71c952fa279e9ea5ddf4bf25dc39f155093ecd4c9e9` |
| `design/ownership.md` | `87ec056d877e58deda362835fb408c0c5372db24571febc85275af53689063a6` |
| `design/component-evidence.md` | `ef27e864f03ea93f5c7cc9d0ad67ed2367808baaf12eef709ca373f34c861ae4` |
| `design/ownership-review.md` | `75f352676a2cb2414db90ab29e2d951ad2e43046ad27a8de82685652c97c5636` |
| `technical-design-review.md` | `163cd36fdf0d11a0f8bb9df2455eac745e1346ff63c2a546d9c018e49af7d767` |

Worktree:
`/Users/daniil/.codex/worktrees/telemetry-production-hardening/rust-service-template-rest`.
Branch `codex/telemetry-production-hardening-20261005`, HEAD/base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, verified in this phase.

## Planning input and proof boundary

Use the standard bounded channel and one OS writer thread; no crate, feature,
manifest or lockfile change is selected. Fixed logger contracts are 16 KiB per
complete record, 512 queue records/8 MiB payload, 32 formatting callbacks with
two 16 KiB buffers each and 128 field entries, 1024 cached local spans with
4 KiB/64 fields each, one 16 KiB in-flight writer, and bounded metadata/library
overhead. The source-owned Debug/Display/SDK/registry/RSS exclusions and overflow
semantics remain those in Design, not a process-memory guarantee.

The existing 5s telemetry tail is shared: trace receives at most 4s including
its deducted join slack, logger the remainder with 1s reserved when available.
The existing 17s aggregate tail and 1s final runtime cap are not enlarged.
Logger closure itself is runtime-independent. Service and ordinary worker map
final telemetry incompleteness to 3 unless a primary error requires 1; migrate
retains its primary 0/1 result, including committed success, and its existing 1s
cleanup allowance covers runtime termination plus logger closure.

Planning can order the exact ownership map without selecting a new mechanism.
Implementation owns concrete test cases at the existing formatter/exporter,
HTTP/gRPC, isolated panic-hook, service/worker process and migration validation
owners. Required evidence includes bounded stopped/failing-sink behavior and
retention, real boundary privacy at local JSON/text and OTLP outputs, in-flight
and later-success final-drain errors, truthful terminal records/exits, and
successful correlation/HTTP/TLS export compatibility. Applicable final build,
tests, independent implementation review and CI remain outstanding. Historical
benchmarks remain historical; no new performance campaign or remote host is a
completion gate. No live backend/durability claim follows from this design.

The root coordinator continues Planning under the current workflow/harness
owners read at the base above. This actor stops here. No production code,
dependency, original-checkout, commit, push, PR, deployment or infrastructure
change was made. No Rust build/test was run. `make docs-check` passed after the
behavioral repair: 1275 links, zero errors; subsequent changes were status and
receipt-only Markdown with no added relative links. Static consistency and
independent review are document/design proof only.

Reopen the smallest owner named in Design if implementation evidence invalidates
a bound, API/lifecycle assumption, source privacy or accepted exit behavior;
otherwise preserve those decisions through Planning and Implementation.
