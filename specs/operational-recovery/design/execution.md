# Operational recovery: validation, cache and resources

This is the R4 mechanism selected by [system design](system.md), under the
current [Build Speed](../../../docs/build-speed.md),
[Validation Routing](../../../docs/validation-routing.md) and
[delivery policy](../../../docs/ci-cd-production-ready.md) owners.
No runtime/build observation is claimed by this design.

## E1: kernel exclusion plus generation custody

Keep `scripts/ci/validation-lock.sh -- command [args...]` as the public entry
point and existing Make call sites. Its implementation delegates to one
`scripts/ci/validation-lock.py` standard-library process. This is the current
lock's implementation, not a scheduling service. Python already belongs to
the verifier's prerequisites. Supported local platforms are the current macOS
workstation and Linux CI, on a local filesystem supporting `fcntl.flock`.
Do not silently substitute the old deletion protocol when that primitive or
filesystem guarantee is unavailable.

Use a permanent regular `codex/validation.lock.guard` file for `fcntl.flock`.
Never unlink or replace that guard inode. Every new-protocol acquisition,
generation change, reconciliation and release holds this short kernel mutex.
At the exact existing `codex/validation.lock` pathname, atomically create an
**ephemeral regular admission file** with `O_CREAT|O_EXCL`. It carries the
generation identity; update additional metadata atomically while holding the
guard. Release removes the matching admission file only after owned completion
and an under-guard token/inode check. The optional `VALIDATION_LOCK_DIR`
override retains its public name but denotes the admission pathname; the guard
is its sibling. Sidecars contain safe bounded metadata, not argv/environment.

This preserves normal legacy coexistence without permanently fencing older worktrees.
While a new generation owns the regular admission file, an old `mkdir` client
cannot enter and an old stale reclaimer's `rm validation.lock/owner` or
`rmdir validation.lock` cannot remove it. After normal release old clients can
acquire again. If a legacy directory exists, the new client only waits for its
normal removal within the current wait deadline; it never reclaims or converts
that directory. If old `mkdir` wins the next admission race, new `O_EXCL` fails;
if new `O_EXCL` wins, old `mkdir` fails. Dead/stuck legacy ownership is reported
as `legacy_protocol_unreconciled`; it does not authorize other-worktree edits,
interrupts or PID-based reclamation. Existing legacy-versus-legacy bugs are not
claimed repaired before those callers adopt the new entry point. Dynamic
compatibility proof uses isolated lock paths before exercising the shared path.

The supported protocol's exclusion covers all cooperating current entrypoints,
including stale recovery, owner change and nested invocations across worktrees.
Normal legacy admission interoperability is a separate, narrower claim. Two
unmodified legacy reclaimers can already erase a live legacy successor's
directory; pathname absence in that contested state is not proof of completion.
No new-only mechanism can repair that old race, and this design claims no such
retroactive guarantee. The continuation owner accepted this technical boundary
on 2026-10-06 after the source-backed consultation; R4's current-code safety is
not weakened and no global upgrade orchestration is introduced.

For this task, the final delivery owner admits its work through the candidate
entrypoint and read-only establishes completion/absence of already-admitted
legacy work before first shared-path use. Contested/unreconciled legacy state
prohibits shared activation, without interrupting other runners. Scoped coding,
isolated self-tests, PR delivery and required CI may continue; the precise local
shared-activation limitation is retained. It does not require proving that no
future legacy client can ever run, nor updating other checkouts. A subsequently
observed contested legacy state reopens only this activation decision.

### State and launch ordering

Each invocation has an unpredictable generation token, supervisor identity,
checkout identity, source/candidate fingerprint, safe command kind, phase and
monotonic start/wait instants. PID is diagnostic, never lock authority. The
public candidate for verify is its existing content/plan identity; direct
commands include HEAD and a dirty-source fingerprint, not HEAD alone.

| Generation state | Admission/recovery rule |
| --- | --- |
| Preparing | No payload has permission to execute yet |
| Active | Payload admission was issued; custody must reach terminal evidence |
| Completed | Original owner observed command and owned completion; next holder may retire this generation |
| Interrupted/unknown | Preserve evidence and refuse new payload until token-specific custody reconciliation |

The supervisor starts a small internal pre-exec gate child in its own session/
process group. It records Active and its group identity atomically before
granting GO over a private pipe. EOF without GO means no payload execution.
Cancellation checked before admission, a timed-out waiter, failed metadata
write, or dead supervisor before GO cannot run the queued command. If failure
falls between Active and GO, a conservative interrupted generation is allowed;
false admission is not. The gate is an internal mode of the same Python helper,
not a new daemon or reusable execution framework.

While alive, the supervisor owns the direct child, the group, signal forwarding,
completion, and release. It handles INT/TERM/HUP by stopping admission and
forwarding cancellation only to its currently owned group, then waits for
completion. It does not use an EXIT trap to delete ownership while work survives.
Group signaling is permitted only while the supervisor retains the unreaped
direct-child identity; after reaping, numeric group existence is observation,
not authority to signal potentially reused identifiers. Surviving/unidentified
work then requires its named cleanup owner and remains quarantined if unresolved.
No new blanket kill timeout is added: if its work will not finish, the lock
remains occupied with visible cancellation-pending state. Existing bounded
resource owners remain responsible for their actual operations and cleanup.

Normal completion requires both the direct child's reaped status and completion
of the governed group/explicitly registered subordinate work; observing only
the shell exit is insufficient. A child intentionally detaching work or a
daemon-owned workload must have a preexisting resource-specific join/cleanup
owner supplying terminal evidence. Docker/compose proof retains its existing
runner cleanup owner, but that owner must now supply the generation-bound
acknowledgement below. Existing swallowed cleanup failures are not adequate
evidence. The lock cannot infer container termination from the Docker CLI's PID.
Newly supported sccache runs foreground as described in E2.

### Existing daemon-work completion seam

The first Technical Design Review found TD-1: current
`scripts/lib/compose-postgres.sh::compose_postgres_down` and
`scripts/ci/runtime-image-check.sh::cleanup` suppress removal failure. Extend
the current generation metadata with a finite set of pending external-operation
tickets, not a resource-discovery/cleanup service or separate ledger. Two narrow
internal operations on `validation-lock.py` serve its existing script callers:

- Before an operation can escape the admitted process group, its existing owner
  records a pending ticket under the guard. It names the current generation,
  a fresh operation nonce, a fixed owner label and a non-secret unique scope
  (Compose project, container name/ID, or build invocation). If this write fails,
  the external operation is not admitted. Begin always precedes `up`, `run`,
  `create` or a build request, including partial-startup cleanup paths.
- Only that existing owner acknowledges the ticket after native completion or
  cleanup/readback establishes the named work is terminal. Completion validates
  the generation and ticket under the same guard and retains a bounded evidence
  class/reference. It cannot clear a different ticket or generation. Repeated
  acknowledgements for the same terminal ticket are idempotent.

The internal ticket commands require verified inherited custody and work only
on the current generation. They do not accept arbitrary executable cleanup
callbacks, credentials, raw command arguments or new resource types. Supported
owners are the finite current Docker/Compose validation paths listed below.
They continue to own actual handles, cleanup bounds and errors. A fixture script
invoked outside a validation generation keeps its normal owner-local cleanup
and reports incomplete cleanup; it does not create a hidden second lock/runner.

| Existing owner / path | Admission and terminal input |
| --- | --- |
| `scripts/lib/compose-postgres.sh`, consumed by `test-integration-db.sh`, `sqlx-prepare.sh` and `migration-validate.sh` | Ticket precedes this run's unique project startup; it closes only after successful scoped `down` and positive readback that owned project containers are absent. Caller-managed Compose (`INTEGRATION_COMPOSE_MANAGED=1`) is not adopted or deleted. |
| `scripts/ci/test-integration-messaging.sh`, `test-integration-cache.sh`, `test-integration-object-storage.sh` | Their existing unique Compose-project owners use the same pending/terminal contract. Failed/unknown cleanup stays pending instead of becoming `true`. |
| `scripts/ci/test-integration-oauth.sh`, `runtime-image-check.sh`, and the extra container path in `migration-validate.sh` | Record a preselected unique container name before creation, then ID when obtained. Existing cleanup operates only on that scope and confirms removal/absence through a responsive Docker daemon; a failed inspect alone is not absence. |
| Foreground `docker run --rm` leaves in `make/template.mk` (shellcheck, docs-check, container scan/SBOM) | Make keeps command ownership and provides a unique name or CID file for the operation. Native terminal result plus scoped absence/cleanup acknowledges the ticket, including a failed tool result; CLI death alone does not. No image/cache pruning. |
| `scripts/ci/runtime-image-build.sh`, Make's Dockerfile check, and the image-build portion of `runtime-progress-proof.sh` | Record the build invocation before submitting it. Successful native build completion closes it. Interrupted/disconnected/otherwise ambiguous build leaves it pending; recovery uses that invocation's native build evidence, never a global builder stop or prune. Failure without authoritative terminal evidence conservatively remains unknown. |
| `scripts/ci/runtime-progress-proof.sh` external workload | Its existing unique run/image-scoped cleanup remains the owner; register before the driver can start containers and acknowledge only after its current scoped terminal/cleanup readback. Retain all frozen workload inputs/oracles and historical results. |

Where a foreground Docker leaf needs a few lines to retain its unique identity
and cleanup result, place that shell composition at the existing Make/script
owner; the lock helper only records tickets. Do not grow a generic Docker API,
plugin registry or resource manager. All relevant entrypoint paths are known;
an additional path escaping process-group custody reopens this narrow list.

Normal release requires no pending ticket in addition to process completion.
A killed owner, swallowed/failed cleanup, unreachable daemon or missing
acknowledgement leaves the ticket pending and quarantines the generation, even
if the shell returned zero. A failed test with confirmed cleanup can release
custody while keeping its failed validation result. Cleanup never changes that
failure into a pass. The verifier sees incomplete custody as a failed/incomplete
attempt, with the safe owner/scope and remaining step, and writes no receipt.
Readback/refusal is scoped to the run's resources; unrelated containers, runners,
builders and caches are not touched. Existing resource time budgets remain
bounded; expiration preserves unknown custody instead of extending a wait.

These are application-owned completion acknowledgements, not claims inferred
from OS process enumeration. Native resource behavior follows current official
[Docker container run](https://docs.docker.com/reference/cli/docker/container/run/),
[container removal](https://docs.docker.com/reference/cli/docker/container/rm/),
[Compose down](https://docs.docker.com/reference/cli/docker/compose/down/) and
[Buildx build](https://docs.docker.com/reference/cli/docker/buildx/build/)
contracts, inspected 2026-10-06. This source-level seam is required to satisfy
the existing no-premature-release invariant; it adds no new runtime scenario
or CI gate. Implementation chooses the fault/control cases at these owners.

If the supervisor is killed or loses custody, its kernel lock may release but
the Active admission file and generation sidecar prevent successor admission. Dead PID, absent
group, recycled PID/PGID, elapsed time and missing metadata do not certify that
all governed work ended. They produce bounded diagnostics, never a kill or
reclaim decision. The permanent guard serializes competing reclaimers; only its
current flock owner may retire the exact token it inspected, preserving its
evidence before publishing a successor. Unknown metadata fails closed.

Recovery is a token-checked reconciliation mode of the same entry point, under
the same guard flock. It requires the original runner's retained terminal evidence
for its named command/resource scope, or an explicit scoped recovery decision
from that work's authorized owner after joining its known work. It never
accepts PID absence alone or a blanket `force` flag. Preserve the interrupted
record and recovery evidence; a stale token cannot retire a successor. When
completion cannot be established with available authority, return the exact
unresolved generation/resource and keep new work blocked. This is the specified
uncertain-custody case, not permission to interrupt someone else's runner.

### Nested execution and waiting

Replace the bare `VALIDATION_LOCK_HELD=1` bypass with verified inherited custody:
the originating absolute lock path and generation token must match the active
or quarantined generation, and the invocation must still belong to that
generation's admitted session/group. The originating path remains authoritative
for initializer representatives with different Git roots. A boolean alone grants
nothing. A quarantined generation can finish already admitted nested work but
cannot admit an unrelated generation. The nested command borrows the same
custody and never retires its parent's generation. Update `template-init-check.sh`'s direct boolean branch
to the same verification; self-tests clear inherited custody and use isolated
temporary lock paths. No second lock is taken for a verified nested call.

Use a monotonic wait deadline with the existing default/override of 900 seconds.
Print a safe owner/checkout/candidate summary on first contention and each owner
change, then elapsed waiting at a fixed bounded cadence (10 seconds), with a
final timeout/cancellation disposition. Keep observed unknown/initializing
states explicit. No raw argv, DSN, environment dump or wrapper option is logged.
Timeout keeps exit 75; usage/configuration failures retain a distinct nonzero
refusal. Cancellation runs no queued payload and releases no other token.
FIFO remains unspecified: no starvation evidence justifies a queue.

### Alternatives and reopen conditions

Bare `mkdir` plus PID checks cannot serialize stale reclaim/release. A sidecar
guard alone cannot protect a directory from old deletion clients. A permanent
nonempty directory still has an initialization race with delayed legacy
reclaimers. A permanent regular admission file would indefinitely block older
worktrees; instead the regular admission file is temporary and all new-protocol
create/delete transitions use the permanent sibling guard. Legacy clients cannot
delete that regular file. Kernel `flock` alone releases on supervisor death and
does not establish child/resource completion, so the generation record remains
required. Python's standard `fcntl`, `subprocess`, signals
and pipe are sufficient; external lock libraries would not own command-tree
custody or legacy coexistence. Native `flock(1)` is not installed uniformly on
the supported hosts. A portable queue/daemon or OS-specific cgroup manager would
add lifecycle/platform costs beyond this requirement.

Reopen E1 if a current command launches unowned detached effects, a supported
host cannot provide local-file flock, a legacy owner cannot be reconciled, or
actual starvation requires scheduling policy. The current owner resolves the
smallest affected execution boundary; no proof claim is made by a dead PID.

## E2: explicit local compiler cache

Select the maintained native sccache tool through Cargo's supported
`RUSTC_WRAPPER` interface. Pin optional `SCCACHE_VERSION=0.18.0` in
`tools/versions.env`; no Cargo dependency, toolchain upgrade or global Cargo
configuration is required. The ordinary inherited/uncached path remains valid.
The existing Make/verify entry points gain a command-scoped `BUILD_CACHE` mode:

| Mode | Contract |
| --- | --- |
| `inherit` (default) | Preserve caller-selected wrapper/output settings; no cache installation or start is implied. Record the resolved execution context. |
| `sccache` | Require a supported executable at `BUILD_CACHE_BIN` or PATH, the pinned version, a task-owned local `BUILD_CACHE_DIR`, and a usable private server endpoint before expensive work. Refuse missing/invalid/conflicting settings with a safe cause. |

`scripts/ci/build-context.py` owns the narrow preparation and execution-context
projection used by `make/template.mk`'s Cargo build/test/lint leaves and the
verifier. It is not a compiler wrapper and never changes compiler semantics.
Before selecting sccache, it resolves supported wrapper inputs and refuses a
different caller-selected rustc or workspace wrapper instead of replacing it.
An already selected identical sccache path can be retained. Configuration
sources are read only; use Python's TOML parser and the documented Cargo lookup
and precedence for the small set of wrapper/output keys. Resolve referenced
includes and explicit `--config` inputs when supported by this helper; unsupported
dynamic/ambiguous inputs refuse explicit cache mode before work and cannot reuse
a passing receipt. No nightly `cargo config get`, `RUSTC_BOOTSTRAP`, compiler
interposition or full Cargo reimplementation is selected.

The supported execution boundary is the repository's direct Cargo executable
and explicit wrapper/output configuration, not arbitrary shell snippets in
`CARGO` or `CARGO_FLAGS`. Preserve ordinary caller execution when context cannot
be fully projected in inherit mode, but mark context unknown and disable receipt
reuse/publication of an exact-context pass. Never call an unknown wrapper
`uncached`. Known wrapper identities include executable content/version and
configuration fingerprints; only bounded safe labels are displayed. Cargo
continues to own actual compilation and cache correctness.

Selected sccache mode uses a generated task-local, local-disk-only configuration,
sanitized supported SCCACHE variables, a private Unix socket, and foreground
`SCCACHE_START_SERVER=1 SCCACHE_NO_DAEMON=1`. `SCCACHE_CLIENT_SIDE=1` keeps actual
compiler execution in the command's client custody. No default shared daemon,
remote backend, inherited credentials or silent server-I/O fallback is selected.
Reject conflicting cache settings by safe key name before creating the context;
do not print their values. The helper owns the foreground server, verifies
readiness, executes the Cargo command, obtains bounded native stats, requests
shutdown and joins it before returning. A server that outlives cleanup remains
owned/unresolved under E1, never falsely completed.

Keep `CARGO_TARGET_DIR`/Cargo output directories per worktree. Default outputs
are that checkout's `target/`; an explicit output must be exclusively assigned
to that worktree and cannot alias another worktree's output after canonical path
resolution. The cache is a separate task-owned location reusable across that
task's repeated commands. Leave incremental, debug/profile flags, linker and
native cache-key behavior unchanged. No path normalization or forced incremental
disable is needed to claim the feature works; cache hits are observational.

### Provisioning and current capability

The supplied machine snapshot had no active wrapper or sccache; this design has
not installed or executed one. Provisioning is explicit task-local delivery
work, never a side effect of `make build` or missing-tool preflight. Prefer the
official prebuilt release to a resource-intensive source installation. For this
macOS ARM64 task, use `sccache-v0.18.0-aarch64-apple-darwin.tar.gz` (7,997,637
bytes), expected SHA-256
`308184519b646f5125289e8515b36f6ca65a13a041923994aebe702348674e8e`.
The official release API supplied that digest on 2026-10-06. Verify before
extracting only the expected executable into a task-owned tool directory; retain
URL, digest and actual `--version` in local evidence. No Homebrew install,
quarantine/security change, global config or shared-cache cleanup is permitted.
An unsupported host, unavailable archive or digest/version mismatch leaves
explicit cache mode unavailable while ordinary uncached work remains usable.

Use a fresh cache directory dedicated to this task and an explicit local cache
size cap of 1 GiB for its first observation, given the last supplied free-space
snapshot of about 11 GiB. This limits cache growth; it is neither a universal
build reserve nor evidence a full build will fit. Native LRU operation inside
this new task-owned cache is the tool's ordinary cache lifecycle. Never prune
an existing shared cache. Implementation re-observes current capacity under E3
before downloads/builds and can adjust this task-owned cap from actual evidence.

The optional pin is checked when that mode is requested; ordinary tools-check
and CI do not install an optional cache or gain a new mandatory cache gate.
The Linux release can be provisioned through the same version/checksum method
when a cache observation is needed there; no cross-platform hit is promised.

### Selection evidence and limits

Official upstream contracts inspected on 2026-10-06:
[Cargo wrapper/output variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html),
[Cargo configuration](https://doc.rust-lang.org/cargo/reference/config.html),
[sccache 0.18.0](https://github.com/mozilla/sccache/releases/tag/v0.18.0),
[pinned usage](https://github.com/mozilla/sccache/blob/v0.18.0/README.md),
[pinned configuration](https://github.com/mozilla/sccache/blob/v0.18.0/docs/Configuration.md),
and [Rust limits](https://github.com/mozilla/sccache/blob/v0.18.0/docs/Rust.md).
The release's MSRV is 1.91, below this task's existing 1.99 toolchain. Native
non-cacheable units and zero hits remain valid outcomes; a first build is not
claimed faster. Check native stats only within a private server invocation and
state actual observed hits/misses/uncacheable work, not a predicted speedup.

Retaining uncached builds is the cheapest fallback but does not implement the
requested opt-in capability. A global installation/config conflicts with task
authority. A custom artifact cache would duplicate maintained compiler-key
logic. Source-building sccache spends unnecessary local capacity when an
official verified binary exists. A permanently shared server obscures both
configuration identity and command completion custody. Reopen E2 on unsupported
host/version behavior, an unavoidable caller-wrapper conflict, or measured
cache overhead/storage pressure; preserve uncached correctness.

## E3: storage observations and execution identity

The build-context helper observes the selected target, intermediate output
where separately configured, Cargo cache and selected compiler-cache filesystems
before expensive work. Walk to an existing ancestor for an output not created
yet, use `statvfs` available bytes, deduplicate filesystem observations, and
record the measured path role, filesystem identity and timestamp. Distinguish
unavailable observation from zero space. Private or unrecognized path strings
are represented by role plus fingerprint rather than copied into logs.

`BUILD_MIN_FREE_BYTES` is an optional explicit caller requirement applied to
selected output/cache filesystems. Validate it as a nonnegative byte count;
when supplied, unavailable measurement or insufficient capacity refuses the
pending expensive step. Without it, print diagnostics and reject actual
unwritable/exhausted required locations, but do not invent a universal reserve.
A previous approximately 11-GiB observation is not current capacity or a fit
guarantee. No automatic clean, prune, target move or build retry follows failure.

During a nonzero command, retain a bounded failure classification from positive
storage evidence: OS ENOSPC/EDQUOT from owned I/O or a canonical child storage
diagnostic. Record `resource_exhausted`, affected role when known, failed step,
exit and remaining plan; preserve partial output/build artifacts and attempts.
A test merely printing those words is not proof of real ENOSPC. Unknown causes
remain command failures with storage observation attached, not asserted resource
failures. The wrapper streams normal command output but new diagnostics retain
only bounded classes/fields, not extra raw logs or environment dumps.

`verify.sh` remains the sole receipt writer and source/plan owner. Resolve context
before an expensive step and again after lock admission, so time spent waiting
cannot reuse a stale capacity/configuration decision. Its environment identity
must include a schema/version for this context, effective target/build/cache
roles and canonical path fingerprints, wrapper identities/content/version,
selected cache mode/configuration, compiler and Cargo versions, and relevant
Cargo build configuration fingerprints. Changes invalidate exact receipt reuse.
Temporary UDS names, measured free-space values, counter totals and elapsed
waiting are evidence, not cache-key material; their variation must not force
rebuilds. A caller resource requirement is retained and rechecked even on a
receipt hit, not treated as a timeless observation.

Keep current source/content/mode/HEAD/plan identity, candidate-change invalidation,
partial attempts and CI-owned step reporting. Unknown execution context,
interruption, failed resource admission or incomplete cache/command custody
writes no passing receipt. Relevant configuration fingerprints cannot include
raw secrets; unexpected secret-bearing settings get safe key/category refusal.
The selected helper/tool input bytes join the relevant candidate identity so
old verifier receipts cannot silently prove the new protocol.

Direct build leaves use the same preparation/classification but do not invent
independent passing receipts. Make still chooses the command and verifier still
chooses/records the validation plan. Extend the existing classifier and
validation self-tests for the helper paths; no new CI gate or full-build matrix
is created solely for these scripts. Proof owners are existing lock/verify
self-tests and the selected actual compiler-cache observation. Implementation
chooses the discriminating cases, including concurrency and interrupted custody.
