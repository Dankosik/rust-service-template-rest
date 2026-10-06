# Delivery Validation

Select these commands for an explicit verification requirement or a bounded
diagnostic. Existing CI and publication gates keep their own admission scope.

| Claim | Command | Notes |
| --- | --- | --- |
| Workflow syntax | `make actionlint` | `go run` of the pinned actionlint; the host shellcheck and pyflakes integrations are off so local and CI agree |
| Workflow security | `make zizmor` | see [Security](security.md) |
| Shell scripts | `make shellcheck` | every tracked `*.sh` through the pinned container; `SHELL_FILES='a.sh b.sh'` scopes it, which CI does on diff events |
| Tool pins | `make tools-check` | manifest shape, image digests, each Cargo tool reports its pin, Dockerfile `ARG` defaults and `FROM` tag agree with `tools/versions.env` and `rust-toolchain.toml` |
| Dockerfile | `make dockerfile-check` | BuildKit's built-in checks (`docker buildx build --check`) |
| Publication naming and promotion | `make publish-image-metadata-check` | self-test of `scripts/ci/publish-image-metadata.sh`: both modes, refused SHAs and tags, a release tag that disagrees with the crate version, partial promotion receipts |
| Validation routing | `make changed-surfaces-check`, `make affected-crates-check`, `make validation-lock-self-test`, `make verify-check` | the scripts' self-tests; `make verify` selects them when a routing file changes |

Every tool version is pinned once in `tools/versions.env`. The Cargo tools
build once per version into `<git-common-dir>/tools/<crate>-<version>` the
first time a target needs them; CI installs the same versions as prebuilt
binaries and resolves them from `PATH` because `CI=true`. A version bump is a
deliberate commit to the manifest (and to the Dockerfile `ARG` default for
the tools built inside the image); `make tools-check` refuses drift.

`SCCACHE_VERSION` pins the optional task-local compiler-cache executable. It is
not a Cargo tool and `make tools-check` never installs it. `BUILD_CACHE=inherit`
preserves the caller's direct Cargo, wrapper and output context; a context the
helper cannot safely project remains unknown and cannot reuse or publish an
exact-context receipt. `BUILD_CACHE=sccache` requires a matching pinned binary,
a task-owned `BUILD_CACHE_DIR`, and a foreground private server joined to the
validation generation. `BUILD_CACHE_SIZE` defaults to `1G` for that private
cache; `BUILD_MIN_FREE_BYTES` supplies an optional explicit byte threshold.
There is no default daemon, remote backend, global Cargo mutation, shared target
directory, cache cleanup, or automatic provisioning.

A merge-readiness or release claim also needs the CI evidence named in
[CI/CD Production Readiness](../ci-cd-production-ready.md); local analyzers
do not prove platform state.

The current validation entrypoint uses a permanent sibling flock guard and a
generation-specific regular admission file. It retains the admitted command,
process group, and finite Docker/Compose tickets until their owners establish
terminal completion. Container owners register before creation and acknowledge
only after scoped cleanup and positive absence readback. Build owners require
native successful completion; interrupted or ambiguous builds remain pending.
An empty readback alone does not resolve a submission interrupted before its
native acknowledgement or created identity. Foreground container owners retain
their native CID file; Compose owners retain whether startup was confirmed.
Unknown admission stays pending even after best-effort scoped cleanup.
Failed cleanup or missing acknowledgement returns incomplete custody (exit 74),
keeps the generation quarantined, and cannot produce a verifier receipt.

Nested calls verify the originating absolute lock path, generation token and
admitted process group. This includes initializer representatives in another
Git root; `VALIDATION_LOCK_HELD=1` alone grants no admission. Waits retain the
900-second default deadline and exit 75 on timeout. A timed-out or cancelled
waiter never launches its command.

Normal admission interoperates with older directory-based clients: current
clients wait for legacy directories to disappear and never reclaim them.
Legacy-versus-legacy stale-reclaimer races remain outside this guarantee.
Before activating this protocol at a shared Git-common path, the delivery owner
must establish completion or absence of already-admitted legacy work. Contested
legacy custody prevents shared activation; isolated proof and CI can continue.
Unknown current generations also remain quarantined until their authorized
owner supplies token-specific command/resource terminal evidence. PID absence,
elapsed time, or deleting a pathname is not a recovery procedure. Recovery
never stops unrelated containers, builders, processes, or caches.

Reconciliation uses the same entrypoint, `validation-lock.sh --reconcile TOKEN
--evidence OWNER_TERMINAL_JSON`, with the originating `VALIDATION_LOCK_DIR`.
The named work owner must first join its command/group and establish native
terminal evidence for every pending resource. The evidence JSON contains that
exact `token`, `decision: "owner-confirmed-terminal"`, `command: "joined"`,
`process_group: "joined"`, and a bounded non-secret `reference`. Its `tickets`
object is keyed by each pending nonce and contains the matching `owner`, `scope`,
terminal `evidence` class and `reference`; an empty object applies only when no
ticket remains pending. The helper rejects a surviving group, missing ticket,
different scope, or stale token. It preserves the generation record and owner
reference when retiring admission. There is no force-reclaim flag.

An ordinary failing command releases custody after confirmed completion while
retaining its exit status. A BuildKit failure without authoritative terminal
evidence remains pending conservatively, even when the CLI has returned: its
owner must reconcile that invocation before another validation can enter. No
shared builder is adopted or stopped to make this result look complete.
