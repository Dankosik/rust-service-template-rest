# Stage 9 research synthesis

Status: ready for Specification. Evidence read on 2026-09-20.

## Questions and stop boundary

Decide which existing capabilities can be selected, which maintained mechanisms
fit in-place initialization and safe portable sync, and which Go constraints
survive Rust's architecture. Stop once the strongest alternatives and their
material gaps are known; implementation details belong to Technical Design.
No performance, runtime, or generated-output claim is made here.

## Current authorities

Rust source: `81bbd16b320c90d430522e69fc2d52a2aaa049de`.
[Roadmap](../../../docs/roadmap.md#stage-9-template-initializer-profiles-and-sync),
[boundaries](../../../docs/architecture/boundaries.md), Cargo manifests,
make/template.mk, scripts/ci/changed-surfaces.sh, and canonical agent helpers
show HTTP core plus optional PostgreSQL. Stage 10 capabilities do not exist.
PostgreSQL is currently compiled and defaults to disabled; removal must close
manifests, configuration, lifecycle, tests, image, and validation together.
There are six existing harness adapters (Codex, Claude, Qwen, Cursor, Grok,
OpenCode), their canonical role registry, and generated views. No initializer,
lock, ownership manifest, or evals directory exists at this baseline.

Go source inspected read-only at `1ae75302507afb58b08ff3b439d08a00d782864e`:
`scripts/init-module.sh`, `scripts/template-sync.sh`, `template-owned.paths`,
`docs/template-sync.md`, and `docs/universal-disciplines/README.md` under
`/Users/daniil/Projects/Opensource/go-service-template-rest`.
These provide the rationale for physical profile pruning, explicit local
initialization provenance, committed-source sync, scoped dirty-path refusal,
service-owned authority exclusions, generated harness projections, narrow JSON
settings ownership, and preserved marked service skills. Their live versions
are evidence, not instruction authority or code to translate literally.

## Maintained alternatives

| Mechanism | Primary evidence and observed version | Decision-changing fit and gap |
| --- | --- | --- |
| cargo-generate | [v0.25.0 release](https://github.com/cargo-generate/cargo-generate/releases/tag/v0.25.0), published 2026-09-18 07:55:06 UTC, verified from GitHub releases/latest API; [official guide](https://cargo-generate.github.io/cargo-generate/) | Actively released Rust project generation with Liquid and Rhai hooks. Useful for new rendered projects; keeping this checkout buildable and applying a manifest-limited committed update still requires repository policy and hooks. Does not eliminate the safety mechanism needed here. |
| Copier | [v9.18.2 release](https://github.com/copier-org/copier/releases/tag/v9.18.2), published 2026-09-07 13:19:00 UTC, GitHub API; [update contract](https://copier.readthedocs.io/en/stable/updating/) | Active generation and lifecycle update tool; answers plus old/new rendering and conflict reconciliation support broad project evolution. Strongest alternative, but merge/conflict update semantics do not implement exact portable-file ownership or selective dirty-content refusal. Adopting it still needs the bounded synchronizer policy and adds a renderer/state format. |
| Existing Git CLI | [ls-files](https://git-scm.com/docs/git-ls-files), [archive](https://git-scm.com/docs/git-archive) | Already required; supplies committed object/index identity and tracked/ignored/untracked enumeration. Use supported Git mechanisms rather than a new Git crate. Repository policy must reject unsafe paths and separate source snapshot from target state. Archive attributes can alter contents, so Design must choose a snapshot method that preserves the committed owner bytes. |
| Existing Python standard library and shell | [tomllib](https://docs.python.org/3/library/tomllib.html) | Python is already required by check-skills; standard filesystem, JSON, process, and parsing facilities cover bounded local policy without a new runtime crate. tomllib reads TOML but does not write it; exact lexical rewrites or a writer decision remain Design work. No language choice requires changing the runtime crate graph. |
| Existing Cargo CLI | [generate-lockfile](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html) | Owns dependency resolution. Locked validation cannot silently repair a renamed/pruned graph; initializer must deliberately produce a compatible lock as part of its declared output while preserving retained dependency versions. Do not regenerate all versions to newest as a side effect. |

Freshness conflict resolved: indexed cargo-generate changelog showed 0.23.14 and
the browser's cached latest redirect showed 0.24.0; the current primary GitHub
API returned 0.25.0. No dependency is adopted or upgraded by this research.
Versions are external facts, not installed-version or runtime compatibility proof.

## Conclusions and Rust deviations

The supported capability axis is `DATABASE=none|postgres`; HTTP, health,
configuration, telemetry, and the hardened lifecycle are mandatory core.
The adapter axis is `AGENT_HARNESS=core|codex|claude|qwen|cursor|grok|opencode|all`.
This is 16 supported outputs, not the Go template's unimplemented profiles.
Retaining PostgreSQL still defaults to inert runtime configuration.

Keep Cargo package names as identity while `crates/service` remains the
composition-root directory; other owner crate identities do not follow the
service rename. Update the Rust-generated OpenAPI through its source owner.
Keep dependency locks deterministic and retained versions fixed. A dedicated
runtime crate for templating is unnecessary; custom code is justified only for
the named marker/identity transformation and Git/path/ownership admission gap,
with canonical existing helpers reused for their generated surfaces.

Whole-file portable ownership cannot coexist with profile markers or service
identity. Split service-local data from portable method before adding it to the
manifest; in particular AGENTS.md, Make variables, and Rust skills currently
mention repository facts. Portability review must follow those references,
not merely match a banned service name. The two JSON depth leaves are the Go
model's explicit exception, preserving unrelated consumer settings.

## Stage-7 and harness obligations

No stage-10 profile skill is reached. Existing delivery, configuration,
security, error, SQLx, and verification methods cover this work. If portability
changes a skill, add positive/negative evaluation fixtures for its actual
changed decision and check neighboring triggers. The Go universal index routes
principal/tenant policy, distributed arbitration, schema design, provider
integration, jobs, caches, production incidents, and messaging; local file
admission/profile removal reaches none of these new decisions. Do not bulk-port
that catalog or introduce a filesystem skill solely for stage 9. Reopen this
disposition if Design introduces one of those pressures. Instruction ownership
and sync preservation need focused contrasting static fixtures; they do not
prove measured model behavior.

Stage 8 left the first dispatched ledger unproved. This work crosses several
mutable owners and phase actors; Planning must record and exercise that existing
harness obligation if it selects a ledger. It is not satisfied by these
Definition/reviewer dispatches alone.

## Unknowns and reopen conditions

No generated service or sync run has been executed. Exact writable inventories,
marker grammar, recovery implementation, lock schema, and helper placement are
Technical Design decisions within the behavioral contract. Reopen Research if
source capabilities or maintained alternatives change the selected mechanism,
or if physical profile removal cannot preserve retained behavior. Reopen
Specification if that would change supported selections or preservation semantics.

## Technical Design mechanism supplement

Design evidence refreshed on 2026-09-20; behavioral Definition remains unchanged.
[Git ls-tree](https://git-scm.com/docs/git-ls-tree) supplies mode/type/object/path
records and NUL termination; [cat-file](https://git-scm.com/docs/git-cat-file)
supplies object bytes with lengths in batch mode. Selecting blob IDs and omitting
filters/textconv avoids archive attributes and worktree conversion. These are
existing Git facilities, not a new Git abstraction. Design additionally rejects
unsafe paths/modes; Git alone does not enforce the repository's ownership policy.

[Python JSON](https://docs.python.org/3/library/json.html) supplies
object_pairs_hook, parse_constant and decoder positions needed for strict
semantic validation. It does not preserve unrelated lexical bytes through
serialization, so the chosen implementation retains original text and replaces
only the managed leaf's token/insertion span. [os.replace](https://docs.python.org/3/library/os.html#os.replace)
supplies atomic same-filesystem replacement; it does not make a multi-file plan
transactional. Design therefore stages/preflights everything, writes the complete
lock last, and reports partial I/O failure honestly. No crash-durability or
concurrent-writer isolation claim is made.

Current source inspection confirms `test/` now includes independent stage-6
utility recipes. Only its DB library, postgres test and migration fixtures are
removed for no-DB output. Earlier broad Go-style test-pack removal would lose
accepted Rust functionality. Explicit `[lib] name = "service"` retains existing
service::api consumers while Cargo package/main binary receive service identity.
No source/helper or runtime profile has been implemented by this design work.

Cargo lock falsifier: a temporary manifest-only no-DB projection retained the
integration-tests utility package and removed its DB edges. Simple local
rename + transitive lock reachability retained 446/485 packages, but full
`cargo metadata --locked --offline --format-version 1` exited 101 with a lock
mismatch. Earlier `--no-deps` success was rejected as insufficient evidence.
Resolver trace from that same locked command exposed five surplus external
edges: bitflags 2.13.2 to serde_core, either 1.18.0 to serde, hashbrown 0.16.1 to
allocator-api2 and equivalent, and smallvec 1.16.1 to serde. Those features were
selected only by removed DB paths. Removing the declared edges and unreachable
allocator-api2 0.2.21 gave 445 records and full locked offline metadata exit 0
(2,311,388 bytes of metadata; empty stderr). No unlocked Cargo call ran.
The disposable evidence is `/tmp/cargo-lock-probe.CSPuxs-repo/`:
`resolver-trace.log`, transformed `Cargo.lock`, `corrected-metadata.json` and
`corrected-metadata.stderr`. It proves resolution for this manifest/lock
projection only, not source compilation, runtime, or the 16-output matrix.
The small exact edge inventory is therefore selected over a feature resolver
or whole alternate lockfile. Source version/edge guards and full locked
admission fail closed on future drift. Cargo's
[locked contract](https://doc.rust-lang.org/cargo/commands/cargo-update.html#manifest-options)
remains the validation oracle; debug output is diagnostic evidence only and is
not a runtime interface the initializer parses.

Technical-review repair evidence: plain Make export evaluates recursive command
line identity values before Python sees them. On the installed Make, separate
`override DESCRIPTION := $(value DESCRIPTION)` and `export DESCRIPTION`
preserved literal info/shell expressions and quote/hash/dollar/backtick text in
three harmless stdin-Make probes. Combined `override export` did not preserve
them and is explicitly excluded. Source settings differ in token type:
`.claude/settings.json` owns string `"3"`, while `.qwen/settings.json` owns
integer `5`; lexical updates preserve the respective canonical token type.
Current `.github/dependabot.yml` and CONTRIBUTING.md add the missing retained
owners reached by PostgreSQL removal. No behavior or authority was expanded.
