# Initialization and portable updates

Initialization gives a clean template checkout its service identity and selects
its optional capability and agent-harness profiles. Later synchronization adopts
portable tooling and instructions from a committed template checkout.
[`template-owned.paths`](../template-owned.paths) is the full-sync copy
authority; runtime crates, Cargo manifests, vendor patches, `.railway/`, CI
activation, and local policy documents remain service-owned.

## Initialize a service

Start with a clean Git checkout, Python 3.11+, Git, the pinned Rust toolchain,
and fetched lockfile dependencies (`cargo fetch --locked`):

```sh
cargo fetch --locked
make template-init \
  SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' \
  CODEOWNER=@example/platform \
  AGENT_HARNESS=claude
```

[`Makefile`](../Makefile) and [`scripts/template/state.py`](../scripts/template/state.py)
own all profile flags, allowed values, defaults, and cross-profile prerequisites
(`python3 scripts/template/init.py --repo .` accepts the same flags in
`--kebab-case`; unset ambient shell `JOBS=<n>` before running `make
template-init` unless selecting the `JOBS` profile).

The initializer validates inputs before mutating the checkout, renames the
service package and main binary (keeping `crates/service/`, the library target,
and `openapi`), prunes unselected packs, `source_only` paths, and unused
`[workspace.dependencies]`, prunes `Cargo.lock` offline via `cargo metadata
--offline` (verifying remaining package checksums against the template lock),
formats Rust code, regenerates OpenAPI, materializes selected harness adapters,
removes the one-shot initializer (`scripts/template/init.py` and
`scripts/template/profiles.json`), and writes `template.lock` without staging or
committing.

`template.lock` records initialization postconditions (`Cargo.toml`,
`Cargo.lock`, code, and typed config remain runtime truth). Re-running from a
template checkout (`python3 /path/to/template/scripts/template/init.py --repo . ...`)
with identical inputs verifies identity and structure without touching service
edits; changing profiles after initialization is refused.

## Adopt a committed source

SOURCE and TARGET must be separate, nonoverlapping Git roots. Sync reads
committed `HEAD` blobs from SOURCE (refusing dirty owned paths). Point `--from`
at the template's `green` branch so post-merge CI and CodeQL have already passed
([Red main](ci-cd-production-ready.md#red-main)):

```sh
python3 scripts/template/sync.py --check --from /path/to/template --repo /path/to/service
python3 scripts/template/sync.py --apply --from /path/to/template --repo /path/to/service
```

`--check` exits `0` on parity, `1` on drift, and `2` on refusal. `--apply`
leaves changes uncommitted; commit before re-checking. `--instructions-only`
syncs only bootstrap, workflow/harness docs, canonical skills/roles, and
selected adapters without touching Makefiles, scripts, tool pins, or the
manifest.

## Ownership and extension hooks

- **Manifest rules:** File entries replace whole files; directory entries ending
  in `/` own all descendants (deleting target-only files). Profile-marked
  sections render against `template.lock` so pruned capabilities never return.
- **Makefiles:** Keep service recipes in [`make/service.mk`](../make/service.mk)
  (`INTEGRATION_LINT_FEATURES += --features <crate>/integration` adds feature
  lint coverage); sync refuses root `Makefile` edits or overrides of
  `TEMPLATE_STANDARD_TARGETS`.
- **Service script hooks:** Portable scripts under `scripts/verify/`,
  `scripts/image/`, and `scripts/profiles/` optionally read service-owned
  `scripts/service/changed-surfaces.sh`, `scripts/service/database.sh`,
  `scripts/service/runtime-image.sh`, and `scripts/service/runtime-binaries.json`;
  inspect each caller for its hook interface.
- **Local skills and harness settings:** Place an empty regular `.service-owned`
  file beside `.agents/skills/<name>/SKILL.md` to preserve a service-local skill
  across sync. Claude and Qwen settings merge only their managed subagent-depth
  key when drifted, leaving unrelated keys untouched.

## Design choices

Initializer and sync (`scripts/template/init.py` and `scripts/template/sync.py`)
are stdlib-only Python 3.11+ entrypoints over Git plumbing
(`scripts/template/profiles.json` stays source-only). Copier and `cargo-generate`
were rejected for lacking exact-manifest ownership, selective dirty refusal, and
one-shot physical pruning; `git archive` was rejected because export attributes
can alter committed bytes. Reopen if a maintained tool meets these constraints
at lower maintenance cost.
