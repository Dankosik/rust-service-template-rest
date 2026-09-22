# Technical Design: initializer and portable synchronization

Status: ready. Owns mechanism, not behavior or execution sequencing. Inputs:
[Specification](../spec.md), [research](../research/synthesis.md), current
`81bbd16b320c90d430522e69fc2d52a2aaa049de`. Detailed scope is in
[inventory](inventory.md) and [ownership](ownership.md).

## Decisions and alternatives

Use existing Bash entrypoints, Python 3.11+ standard library policy code, Git
object plumbing, and the existing harness generators. No Rust runtime crate,
new dependency, renderer, templating language, Git library, or network lookup is
introduced. `scripts/init-module.sh` and `scripts/template-sync.sh` only locate
their Python owner and forward argv. `scripts/lib/template_init.py` owns the
one-shot transformation; `scripts/lib/template_sync.py` owns synchronization;
`scripts/lib/template_state.py` owns shared JSON lock/admission/path plans and
Git snapshot materialization. `scripts/lib/template_profiles.json` is the
initializer's closed source inventory, not executable configuration. It is
source/service-owned and never adopted by sync. Avoid a general template engine.

Git supplies bytes and identity; Python supplies the repository-specific safety
policy. Copier and cargo-generate do not remove the exact-manifest ownership,
selective dirty refusal, or one-shot physical-pruning policy gap. A maintained
renderer would add state and hooks without replacing this gap. Git archive is
rejected because export attributes can omit or substitute committed content.
Whole-file JSON serialization is rejected for consumer settings because it
changes unrelated bytes. The small managed-leaf lexical edit is justified by
that exact-byte contract. Reopen these choices if a maintained mechanism meets
the same admission and preservation contract with less lifecycle cost.

## Inputs, identity, and marker language

Both entries use one Python argparse/validation path. For the template-init
goal, the root Makefile freezes every input before any include or use, using
separate literal assignments such as `override DESCRIPTION := $(value DESCRIPTION)`
followed by `export DESCRIPTION` (not a combined override/export directive).
Use the same explicit pair for SERVICE_NAME, REPOSITORY, CODEOWNER, DATABASE,
and AGENT_HARNESS; apply `DATABASE ?= none` and `AGENT_HARNESS ?= all` before
freezing those defaults. The simply-expanded variables hold raw Make input
without interpreting nested dollar expressions; no eval/foreach-generated
assignments. The recipe invokes only the fixed script path and transports data
in those environment variables, never recipe interpolation. A harmless probe
on current Make preserved literal `$(info ...)`, `$(shell ...)`, dollar,
quotes, backtick and hash bytes without evaluating them. The
direct CLI uses named `--service-name`, `--repository`, `--description`,
`--codeowner`, `--database`, `--agent-harness`; duplicate, unknown, conflicting
CLI/environment values refuse. The source path and target are argv data.

SERVICE_NAME follows the specification regex and length; compare both exact
package spelling and hyphen-to-underscore spelling against all retained package
names, all auxiliary binaries (`openapi`, `migrate`), and the pinned edition's
strict/reserved Rust keywords. Reject underscore spellings `self`, `Self`,
`super`, `crate`, and reserved `gen` too. Repository and owner grammars follow
the specification; DESCRIPTION Unicode control-category characters and line
separators are refused. JSON encoding is not shell escaping: argv/subprocess
arrays are the only subprocess transport. TOML/Rust string destinations use
separate tested literal encoders; Markdown text escapes active formatting and
HTML, and URL destinations use the already admitted URL grammar.

Source profile blocks use whole-line comments `template:begin postgres:<id>`
and `template:end postgres:<id>` with the host comment wrapper (`#`, `//`, or
`<!-- ... -->`). `<id>` is an ASCII lower-case hyphenated identifier registered
once with its exact path in template_profiles.json. No nesting, unknown labels,
duplicate pairs, mismatch, missing expected pair, or marker outside a registered
surface is allowed. All source blocks compile before transformation. Retaining
PostgreSQL drops delimiters; removing it drops the block. Whole-file packs are
removed by inventory, not marked. Adapter selection uses whole-file pack
ownership, never conditional Rust. No marker may enter a manifest-owned file.
Move DB recipes out of portable files first. If a Rust expression needs a
non-DB default, keep the common neutral expression outside the block (for
example readiness probes start as an empty collection and the selected block
adds the PostgreSQL probe). No generated source is a marker authority.

Identity rewrites use one registered anchor per semantic destination, with
declared expected cardinality, context, and old source value. Parse Cargo TOML
before and after lexical anchor edits; never replace arbitrary occurrences of
`service`. Keep library target spelling `service` explicitly through `[lib]
`name = "service"` while renaming the package and main binary. This preserves
existing `service::api` source imports without inventing another crate. Rename
the lifecycle test executable environment key. Auxiliary `openapi` stays named
`openapi`. API info title/description remain Rust annotation authority and are
regenerated by the existing locked `openapi` binary in the staging tree.

## Initialization material flow and finality

1. Parse and validate arguments; resolve the supplied/current Git root and HEAD.
   Admit only a real working repository root. A matching complete lock takes
   the replay path below before requiring a pristine source.
2. Enumerate tracked changes using porcelain-v1 `-z`, index entries using
   `ls-files --stage -z`, untracked and ignored files using NUL-delimited
   ls-files calls. Tracked dirt anywhere refuses first initialization. Build
   the entire write/removal set, including lock, generated output, and parents;
   untracked/ignored overlap, unsafe parents or unsupported types refuse.
3. Materialize the tracked clean source into a private temporary tree using
   Git objects. Validate inventories, source shapes, marker counts, identity,
   tools, and transformations. Transform only there, deliberately transform
   Cargo.lock, run locked metadata and the existing OpenAPI generator, then
   generate/check selected harness projections in that tree. A missing tool,
   offline dependency, failed helper or postcondition refuses before target
   writes. This is preflight generation, not a claim that build/tests passed.
4. Compute a sorted immutable operation plan: replacement bytes + Git executable
   mode, removals, allowed generated link target, and necessary directories.
   Admit every operation and destination, including new absent paths, before
   starting writes. No helper executes against the real target.
5. Write `template.lock` with `state = "incomplete"` first, through a same-parent
   temporary file and atomic replace. Apply planned regular files using the
   same method; remove only planned files and empty owned directories. Copy no
   Git metadata. Apply generated links last. Verify output matches the plan.
   Only then atomically replace the incomplete lock with the complete lock.
   Never print success before this replacement and readback succeed.

Every deterministic refusal precedes step 5 and preserves target bytes/index.
Unexpected I/O after admission may leave only initialization-produced changes;
retain incomplete state, nonzero status, operation/path diagnostic, and explicit
diff/fresh-checkout recovery. No reset, stash, stage, clean, commit, rollback,
automatic retry, or profile migration is attempted. A failed first lock write
leaves no complete lock. Interruptions follow the same incomplete-state boundary.
Exclusive access to selected paths is a documented caller precondition; initial
checks are not transactional isolation or protection from concurrent writers.

Replay parses a complete lock, compares all requested fields, verifies required
package/main binary identity, chosen packs' presence/absence, valid manifest,
selected generated-view ownership shape, and absence of executable profile
markers. It does not compare ordinary service-owned bytes against initializer
output or restore edits. Inconsistent identity/profile structure refuses with
recovery guidance. An exact matching replay is a no-write success, even when
unrelated service content has evolved. Incomplete/unsupported lock never resumes.

## Lock representation

`template.lock` is UTF-8 JSON, schema version integer 1, with exactly these keys:

```json
{
  "schema_version": 1,
  "state": "complete",
  "identity": {"service_name": "catalog-api", "repository": "https://github.com/example/catalog-api", "description": "Catalog API", "codeowner": "@example/platform"},
  "profiles": {"database": "none", "agent_harness": "claude"},
  "source": {"repository": "https://github.com/Dankosik/rust-service-template-rest", "checkout_revision": "81bbd16b320c90d430522e69fc2d52a2aaa049de", "provenance": "local-checkout"}
}
```

Reject duplicate/unknown/missing keys, non-finite numbers, wrong types (including
bool for integer), unknown version/state/choices, invalid identity, and invalid
Git object-id spelling. The source repository is the template's explicit
provenance constant; checkout_revision is the actual admitted local HEAD,
never a fetched or verified upstream assertion. Source object format determines
40/64-hex admission. An incomplete lock has the same fields with incomplete
state. No timestamps, machine paths, credentials, or mutable sync revision.
Sync leaves this service-owned lock unchanged and reports the adopted source
HEAD in its result. Lock complete denotes initialization only.

Cargo.lock remains Cargo's format, not template.lock data. Parse source lock
version 4 with tomllib and retain each original package block. Match local
workspace records by name/version and absence of registry/Git source; ambiguous
identity refuses. Apply the known local package rename and removed direct
dependency edges (including DB edges in retained integration-tests), then seed
all retained workspace packages and traverse exact lock dependency identities
name/version/source. The no-DB profile inventory also closes these current-source
feature edges: bitflags 2.13.2 loses serde_core; either 1.18.0 loses serde;
hashbrown 0.16.1 loses allocator-api2 and equivalent; smallvec 1.16.1 loses serde.
Guard each named record/version/source and original dependency edge before
editing; these are explicit source-shape anchors, not guessed feature resolution.
Remove unreachable package blocks after both local and these declared edge
edits (including allocator-api2 0.2.21). Retain every external name/version/source
and checksum and all nondeclared fields/edges; compare this invariant before
admitting the output. Preserve the header/format. The complete current no-DB
projection was accepted by full locked offline metadata with utility package
retained; evidence is in the research supplement. Future lock/source updates
must refresh the guarded inventory and pass the source matrix or refuse init.
Do not reimplement Cargo feature resolution or edit arbitrary external records.
As the admission oracle, run `cargo metadata --locked --offline --format-version
1` without `--no-deps` on the staged transformed manifests/lock. A resolution
mismatch, unknown lock shape or missing cached input refuses before target
writes and reports the precise category. The full matrix later proves locked
build/check; metadata alone is only resolution evidence. There is no unlocked
Cargo call, online update, new version selection or hidden lockfile repair.

## Committed-source synchronization

SOURCE and TARGET resolve to distinct physical Git roots. Reject symlink root
spellings and nested/overlapping roots; validate Git object type and read SOURCE
HEAD once. Use `git ls-tree -r -z --full-tree <oid>` and `git cat-file --batch`
with validated blob object IDs to materialize the fixed commit into a private
temporary tree. Parse NUL records, modes and declared blob lengths, never shell
quoted paths. Reject gitlinks (160000), ordinary symlinks (120000), unsupported
modes, case-fold aliases, backslash, newline/control path components, `.git`,
absolute paths, empty/dot/dot-dot components and file/directory collisions.
Only generated skill links in canonical shape are the explicit symlink exception.
Do not use archive, checkout filters, textconv, eval, target hooks, or target
Makefile execution. A source HEAD change after capture does not change the plan.

The committed manifest controls full ownership. Validate it and the source's
canonical/projection/helper closure before reading target bytes. Applicable
source dirty paths include selected manifest owners, selected generated views,
settings leaves/files, and every actual generator input/helper; unrelated source
dirt is excluded. Do not execute dirty source helpers. Use `GIT_OPTIONAL_LOCKS=0`
for read-only Git calls; no index refresh or writes in check. Source-generated
projections must already match the canonical source, not merely be regenerable.

Load target's complete lock, validate local authority presence and the selected
adapter. Materialize expected selected ownership from snapshot into a second
private staging tree. Overlay only admitted service-owned skills and settings
needed by generators, using data copies without execution. Source helpers run
against this staged target; capture safe diagnostics and never emit settings
values. Source projection check and target projection rendering are different
steps: target-local marked skills can legitimately change its generated links.

Manifest directory entries end in `/`; entries have no globs. Reject duplicate
and ancestor-overlapping entries. Removing a manifest entry simply relinquishes
ownership. Compare the source inventory against target descendants and plan
deletion of target-only content inside currently owned directories, except
valid marked skills. Standalone scripts are individual entries. Dirty/staged,
untracked and ignored overlap with selected copy/remove/generated/prune paths
refuses both check and apply even when bytes already match; valid marked skill
trees and their canonical discovery links are the sole dirty exemption. Check
ignored status also for absent destination names. Refuse type collisions and
all symlink parents; use lstat, not follow-links recursion. Differences include
executable mode. Print escaped safe paths and reasons, never JSON content.

After all source/target checks and staged helpers succeed, `--check` returns 0
for parity, 1 for drift, 2 for refusal; no target writes. `--apply` uses the
same plan/write machinery as init but never writes the lock, and verifies the
selected final plan before success. Unexpected I/O/helper failure is exit 2,
explicitly reported as a partial sync if writes started. There is no
destructive rollback. An already-current clean target is a no-op. Apply leaves
changes uncommitted; a check before their commit refuses owned dirt by design.

`--instructions-only` derives its fixed subset from the inventory, does not
consult target Makefiles, and leaves manifest, tool pins, all scripts/Make,
receipts and unselected adapters untouched. Generator scripts are executed from
the committed staging source. Full mode copies portable tooling and prunes
unselected adapter generated paths; it never copies removed DB packs. The
source/target admission sets are derived from the chosen mode, not the entire
repository, so dirty unselected tooling cannot block instruction parity.

## Harness and consumer preservation

Canonical roles, contracts and skills remain shared. Existing agent-roles and
Codex generators gain explicit selected-adapter input from lock (the source
template without a lock is all). No selected command relies on absence alone
to infer a different requested adapter. Dedicated adapter targets refuse when
unselected; aggregate check-instructions checks canonical sources plus only
the selected views. Preserve existing role bodies, effort mappings, nested
execution and generated config behavior.

`.service-owned` is an empty regular file directly inside a real canonical
skill directory with a real SKILL.md and valid metadata. It reserves that whole
directory, including dirt. Any marker in a source skill, collision with source
skill name, symlink marker/skill, missing SKILL, or malformed metadata refuses.
The existing Claude/Qwen relative link form is retained. Validate every link
before mutation; no real file/directory may be erased from generated-link roots.
Unmarked target-only skills are owned drift. Sync can remove them only after
their dirt admission passes. No copying service skills back into source.

For Claude/Qwen settings, first parse strict JSON with duplicate-key detection
and rejected constants. A lexical JSON token scanner records source character
spans for object members. Claude owns a decimal-digit JSON string token
(`"3"` currently); Qwen owns a positive JSON integer token (`5` currently).
Validate the committed source against those per-adapter types. Replace only the
managed leaf's complete JSON token with the canonical source token/type; a
different existing managed-leaf type is managed drift, while nonobject parent
still refuses. When absent insert that leaf, or the
missing parent object, immediately before its closing brace with the necessary
comma. Preserve all pre-existing other characters byte-for-byte. Validate the
rendered document again. Missing file creates a minimal object. Nonobject root
or managed parent refuses. No generic JSON merge, reindent, key reordering, or
consumer-value output. Settings files are special leaf owners, excluded from
ordinary whole-file manifest ownership.

Codex config remains the exact generated project view from
`.agents/codex-project.toml` and generated role registrations, with stronger
ordered/nonoverlapping marker admission. There is no consumer-project JSON
exception for Codex; machine settings stay outside project config.

## CI and proof boundaries

`make template-init-check` owns one serial 2 × 8 matrix in
`scripts/ci/template-init-check.sh`, plus source-only safety fixtures. Each output
is a fresh isolated Git repository from one fixed tracked candidate, initialized,
committed, then runs `make build` and `ALLOW_FULL=1 ALLOW_HEAVY=1 make check`.
The actual constituent list is the generated service's standard check; it is
never substituted with a cheap assertion. Ordinary check in the source also
retains existing gates. Source-only `make/source.mk` owns matrix/purity/sync
fixtures; the initializer removes that file and fixtures from outputs, leaving
the same portable optional include in Makefile.
The generated standard aggregate therefore cannot recursively invoke matrix.
No bypass flag, environment skip, recursive call or swallowed missing tool.

The matrix takes the existing validation lock once and runs cells sequentially;
children inherit the existing lock-held protocol. It does not run Cargo-heavy
cells concurrently. CI `module_initializer` selects one `initializer` job that
runs the full 16-cell command, with all normal required tools installed and
Docker available. This is deliberately one serial job, not 16 overlapping
heavy jobs. Add job output to changes and job to required.needs. When
module_initializer is true, required asserts initializer.result == success;
skipped/cancelled/failure are failures. Existing other gates are unchanged.
Local verify adds the same matrix command with full/heavy admission. The
classifier's source-only selection is disabled by absence of source.mk in
derived services, so changed generic tooling cannot resurrect matrix work.

Select module_initializer for initializer/sync/profile metadata, manifest,
source fixtures/source.mk, all identity/profile inventory files and canonical
harness generator inputs, Cargo/toolchain/lock changes, Docker/CI/validation
wiring, and source docs that the initializer transforms. Unknown source paths
retain existing fail-closed behavior. Pin the routing in existing classifier
and verify self-tests. Record every cell choice, candidate, command, status,
and output revision; reuse sequential caches but never clear shared caches.

Executor chooses fixtures during implementation. Required behavior families
are the specification's identity/profile closure, failure-before-write snapshot
equality, complete/incomplete replay, object snapshot and dirty-path admission,
manifest deletion, modes, settings/skill preservation, selected adapters,
instruction-only independence, purity and actual matrix/canary completion.
Include the composition in the specification as the canary. PostgreSQL-on
outputs retain existing real-DB gate semantics; no-DB outputs contain no active
DB gate. Static instruction fixtures contrast changed routing/stop decisions
and do not claim measured agent behavior. Final stage completion requires main
evidence; phase design creates no build/CI/deployment claim.

## Reopen

Reopen Research for a failed mechanism probe or changed API/lock semantics;
System Design for an unsupported source shape or portable/local split that
would require another truth owner; Specification for changed supported behavior
or preservation; Planning owns dependency order and execution carriers. No new
runtime profile, `.railway` deployment generation, or universal discipline is
introduced. Carry the first real dispatched-ledger obligation into Planning.
