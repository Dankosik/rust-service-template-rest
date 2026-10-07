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

The [shared validation queue](../build-test-and-development-commands.md#shared-validation-queue)
owns generation admission, ordinary process scopes, native Docker resource
custody, quarantine and reconciliation. Context-aware verification uses that
same queue and does not introduce a second ticket or recovery protocol.

Before publishing a reusable result, the verifier calls the authenticated
`validation-lock.sh --assert-complete`. It observes completed registered
resources, absent protected observers and stopped other child scopes; it does
not stop work, clean resources or release the root. The calling child remains
owned until it returns. Only the outer supervisor's successful terminal result
then permits receipt publication. Nested results remain pending custody.
Receipts record `custody_protocol: queue-v3-context-v1`; receipts from the older
ticket protocol are not reusable under the composed queue.

Unknown native completion retains the queue's quarantine and cannot yield a
passing receipt. Use its typed reconciliation procedure; a diagnostic status,
PID absence, elapsed time or deletion of an admission pathname does not establish
completion. No unrelated process, container, builder or cache is stopped to
resolve a task's custody.
