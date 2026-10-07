# T4 — Command-scoped cache, resources and receipt truth

Outcome:
Existing Make/verifier execution gains supported explicit task-local compiler
caching and truthful resource/context decisions, so incompatible or unknown
execution cannot silently reuse a passing receipt.

Consumes:
- T3 integrated generation-custody and current Make/verifier output; implementation
  dependency only, not an acceptance or passing-check gate.
- [E2](../design/execution.md#e2-explicit-local-compiler-cache),
  [E3](../design/execution.md#e3-storage-observations-and-execution-identity),
  [Ownership B/V/P/G](../design/ownership.md) and [R4](../spec.md#r4-delivery-and-evidence-economy).

Provides:
- Narrow `build-context.py` projection/execution helper integrated with existing
  Cargo leaves and sole verifier receipt writer; explicit pinned sccache mode,
  safe resource classes and exact reusable execution identity.

Boundary:
B/V remain one outcome because cache/output inputs are unsafe for reuse without
matching verifier identity and resource/custody disposition. Keep Cargo as
compiler/configuration authority; support default inherit mode and fail closed
for explicit invalid cache selection. Retain unknown inherit context as unknown
without an exact-context pass. Foreground task-local server/client custody uses
T3. No global install/config/security changes, default daemon/remote cache,
shared targets, deletion/pruning, guessed free-space reserve or extra runner.

Mutable owners:
- New `scripts/ci/build-context.py`; `make/template.mk` current Cargo leaves;
  `scripts/ci/verify.sh` context/identity/resource admission and existing relevant
  self-tests; `tools/versions.env` optional `SCCACHE_VERSION=0.18.0` only.
- Existing profile/projection/classifier metadata and self-test owners plus
  `template-owned.paths` only where required for these paths; build on the
  integrated T3 bytes and preserve C profile wiring.
- `docs/build-speed.md`, `docs/build-test-and-development-commands.md`,
  `docs/validation-routing.md`, `docs/validation/delivery.md` for supported
  opt-in inputs, task-local provision, resource evidence and receipt semantics.
  Preserve inherited workstation policy without performing global mutation.

Exclusive locks:
- Build context/Make/verifier receipt owner after T3 relinquishes it.
- Existing profile/projection/classifier bundle, serial with T2/T3.
- Worktree-owned target and task-local tool/cache/server scope during final
  observations; all expensive commands remain serial at Completion.

Final validation:
- Claim: Explicit valid cache uses the supported pinned tool under joined local
  custody; invalid/conflicting inputs refuse before expensive work; inherit
  semantics remain truthful; filesystem observations and resource failures are
  distinct from code assertions; changed context cannot borrow an incompatible pass.
- Checks: Existing verifier/lock/classifier checks and accepted native cache
  observation at assembled Completion. Implementation chooses cases/commands;
  provisioning is an explicit final-delivery action under E2, not an install
  side effect of ordinary Make/tools-check/CI.
- Observable: Actual wrapper identity/version/mode and bounded native stats,
  output/cache role fingerprints, current capacity or unavailable observation,
  preserved partial attempt/pending step on failure. No predicted hit/speedup.
  Use E2's verified official task-local archive/digest and fresh capped cache;
  re-observe capacity before download/build. Unsupported provisioning leaves
  cache observation explicitly incomplete without blocking independent uncached
  code, proof or delivery actions.

Reopen if:
Supported Cargo/native-cache behavior cannot be projected safely, server custody
cannot be joined, or receipt identity cannot represent the relevant inputs.
System Design owns that mechanism; routine parser/test fixes stay here.

## Implementation handoff — 2026-10-06

```text
unit: T4
verdict: Implemented
candidate: codex/operational-recovery-20261006 at base 699887b18594088a59bcc23a049d290d089f6da1 plus the scoped working-tree bytes below
provides: unverified Make build-context execution, native-cache custody, resource admission and verifier receipt integration
next_owner: one Delivery Lead for the fully assembled T1–T4 candidate
```

The resumed owner preserved integrated T1–T3 and the joined T4 lanes. The helper
resolves supported Cargo configuration and includes, canonical output/cache
roles, compiler/tool identities, and explicit resource policy. Unknown inherited
context executes with its supplied arguments/environment and cannot publish or
reuse an exact-context receipt. Explicit sccache selection uses the optional
0.18.0 pin, a task-owned cache, private foreground server and client-side compiler
execution. Native stats require a live owned server and reachable private socket
both before and after the native query; disconnected synthetic zero stats are
not counted as observed cache evidence.

The resumed edits finish exception-path cleanup so a failed shutdown probe still
reaches server wait, retain a safe refusal for malformed task-owner metadata and
invalid input, and classify unexpected native stats as unavailable. The existing
verifier self-test now contains helper-boundary cases for inherited wrappers,
configuration/includes, capacity policy, unavailable/conflicting tools, malformed
owner metadata and stats, failed command, cancellation, and a Cargo executable
that disappears after preflight. Its native-command stand-in exercises process
and socket custody; it is not evidence of native compiler-cache compatibility or
benefit. The fixture server has a bounded idle lifetime, so a missing helper
cleanup fails the observation instead of leaving an unbounded test daemon.

Coding feedback actually run after these edits:

- Python `ast.parse` passed for `scripts/ci/build-context.py`, consumed
  `scripts/lib/template_init.py`, and both embedded Python blocks in
  `scripts/ci/verify.sh`.
- `bash -n scripts/ci/verify.sh scripts/ci/changed-surfaces.sh` passed.
- No build, behavioral suite, compiler-cache provisioning/download/start,
  container or runtime probe ran in this resumed unit. These syntax observations
  are coding feedback, not task acceptance or a reusable proof receipt.

Final validation must exercise the written cases and the accepted native-cache
observation under one assembled candidate and serial expensive execution. It
must re-observe capacity before provisioning/building; the inherited report of
6.7 GiB free is neither a fresh observation by this unit nor a fit guarantee.
No cache-hit count, speedup, full-build capacity, final review or CI result is
claimed here. Optional cache unavailability keeps its observation incomplete
without blocking independent uncached work, as E2 already requires.

No descendants were spawned by the resumed owner; previous T4 lanes were joined
before resume. This handoff relinquishes all T4 mutable scope. No commit, push,
PR or acceptance operation was performed.

Scoped candidate SHA-256:
`a0566ae03b5e79cbbdc9711d5e42fa6b71224c8b265537322414962d54e73675`.
It hashes compact sorted-key JSON of ordered records `{path, mode, sha256}`;
`mode` uses the `0o644`/`0o755` spelling. Shared files include their preserved T3
changes. The packet itself and previously integrated C profile metadata are not
part of this T4 source scope.

| Path | Mode | SHA-256 |
| --- | --- | --- |
| `scripts/ci/build-context.py` | `0o644` | `de78878c20ca47168b880ae2b4f48ff5453ec7d2b310d607acd42a9c71fc0d03` |
| `make/template.mk` | `0o644` | `a315de9ed7ac5553ef3496e277f41803e6bd3bd1c9ceb551d6c4bcc5307cc3fd` |
| `scripts/ci/verify.sh` | `0o755` | `1881410c1a158a8782d7f0cbd5edb453d4a1c4ac20546b9bf99eef1937b004f4` |
| `tools/versions.env` | `0o644` | `4b6696c99ed852526fb9ce22fff7e7d8a3fe3679856f1353bd5e95d11e9a2c4c` |
| `scripts/ci/changed-surfaces.sh` | `0o755` | `646d6c5d3f584c0a4703d976973e149c51daf8b9bc4d382fa1260bbcf197f9fe` |
| `template-owned.paths` | `0o644` | `2d057fbf27c51391d1e9c3e69f32d5a1e960dbd962e6bfad844c47a7cbaa4171` |
| `docs/build-speed.md` | `0o644` | `bfce0ea92d662fdda583ae56135cee8e20f3293d00363bef7d7d3fb7bde9ba26` |
| `docs/build-test-and-development-commands.md` | `0o644` | `e58bde47b327a64ed4ee86e3776d9eea41040d75637b887cc2c7308ae3539db6` |
| `docs/validation-routing.md` | `0o644` | `ba3e4ebe86c9ed5201569e4fa52c0c0a8b3b42723cd9fbed20b2254f9533677c` |
| `docs/validation/delivery.md` | `0o644` | `aba4d64ce2070cec205be99a222abd48075506af25e14cac0d5d3ff888721aff` |
