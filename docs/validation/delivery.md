# Delivery Validation

Select these commands for an explicit verification requirement or a bounded
diagnostic. Existing CI and publication gates keep their own admission scope.

| Claim | Command | Notes |
| --- | --- | --- |
| Workflow syntax | `make actionlint` | `go run` of the pinned actionlint; the host shellcheck and pyflakes integrations are off so local and CI agree |
| Workflow security | `make zizmor` | see [Security](security.md) |
| Shell scripts | `make shellcheck` | every tracked `*.sh` through the pinned container; `SHELL_FILES='a.sh b.sh'` scopes it, which CI does on diff events |
| Python scripts | `make python-lint` | `ruff check` at the pinned version (`uvx`, or `pipx` when uv is absent) with the defect rules in `ruff.toml`; no formatter gate |
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

`SCCACHE_VERSION` pins the optional task-local compiler cache (`BUILD_CACHE=sccache`
with `BUILD_CACHE_BIN` and `BUILD_CACHE_DIR`); `BUILD_CACHE=inherit` is the
default ([Build Speed](../build-speed.md#command-scoped-compiler-cache)).

A merge-readiness or release claim also needs the CI evidence named in
[CI/CD Production Readiness](../ci-cd-production-ready.md); local analyzers
do not prove platform state.

The shared validation queue ([`scripts/ci/validation-lock.py`](../../scripts/ci/validation-lock.py))
owns lock admission, container custody, and quarantine. When a run enters
quarantine, use `--status` and `--reconcile`; never delete the gate, force an
unlock, or stop unrelated processes, containers, or caches to clear custody.
