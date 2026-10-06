# Runtime template upgrades

Use `scripts/template-upgrade.sh` from an explicitly trusted template checkout
to update an initialized service's complete runtime baseline. It requires
Python 3.11+, Git with `merge-tree --write-tree --merge-base`, the selected
revisions' pinned Rust toolchains, and their already available locked offline
dependencies. Initialization choices stay fixed. [Portable synchronization](template-sync.md)
continues to own only its declared portable files.

The command always prepares a separate Git repository. It reads the consumer's
committed HEAD and records its original status; staged changes, unstaged
changes, ignored files, untracked files and the original branch stay in place.
Commit business work that should participate in the upgrade before preparing.
The original checkout may otherwise be dirty. Never substitute its current
mixed tree for a rendered baseline.

## Source admission and complete rendering

`--source` explicitly admits a local template repository whose selected code
will execute. Read and admit that source before using the command. A repository
URL or SHA in `template.lock` does not grant trust, fetch missing objects, or
grant provider access. Use a full exact commit ID for `--revision`; refs and
abbreviations are refused. Obtain unavailable source history separately.

Each baseline is rendered by that revision's own public `init-module.sh`,
initializer, inventory, state helper, pinned toolchain and Cargo.lock. This
includes locked offline Cargo metadata, pinned formatting, OpenAPI generation
and selected harness generation. Cheap profile projection is not a baseline.
Generation receives identity/profile values from the validated complete lock,
a scrubbed environment and local dependency/toolchain caches. Cargo credentials,
ambient Git configuration, hooks and publication/provider tokens are excluded.
`--cargo-target /absolute/cache/path` optionally reuses an explicit shared target;
cache contents never establish provenance. Do not run concurrent heavy builds.

The rendered tree records Git executable modes and the initializer's supported
generated skill links. Unsafe paths, case aliases, submodules, unsupported links,
contradictory locks or a generation failure are refusals. A new selector can
only inherit the existing lock owner's historical `none` normalization; this
command has no profile-migration route.

## Capture or adopt the first baseline

For a new service, commit the fully initialized output before adding business
work, and retain that commit. Later use its exact ID:

```sh
/trusted/template/scripts/template-upgrade.sh adopt \
  --consumer /work/service --source /trusted/template \
  --revision <original-template-commit> \
  --initial-commit <first-fully-initialized-consumer-commit> \
  --destination /work/service-adoption
```

The tool reconstructs the entire historical render and requires exact tree
equality with that initialization commit. It creates an adoption candidate
containing every committed consumer file, without changing runtime content.

For a legacy service which has only an unambiguous complete `template.lock`, use
`--reconstruct` in place of `--initial-commit`. The accepted record labels this
baseline **reconstructed**. Review the reconstructed baseline and its complete
diff to consumer HEAD, classifying all differences as consumer work. This does
not claim that an unavailable historical generated output was compared.
Missing choices, contradictory evidence and unreproducible source cannot be
overridden. Both adoption routes require explicit review and acceptance below.
The reported `adoption_diff` points to the complete binary-capable baseline-to-
consumer patch kept beside the attempt record.

## Prepare and resolve an update

After integrating the accepted adoption commit, prepare a later source:

```sh
/trusted/template/scripts/template-upgrade.sh prepare \
  --consumer /work/service --source /trusted/template \
  --revision <later-template-commit> --destination /work/service-upgrade
/trusted/template/scripts/template-upgrade.sh status \
  --candidate /work/service-upgrade
```

The tool copies complete committed Git history without alternates or hardlinks,
renders B1 with the same identity/profiles, and uses native
`git merge-tree --write-tree --merge-base=B0 C B1`. Git's exit status, conflict
stages and messages are retained. The resulting tree alone never establishes a
clean merge. Original `template.lock` and accepted `template.upgrade.json` are
deliberately preserved as consumer control files during the merge; B1 retains
its own generated lock for provenance.

Inspect `git status`, `git diff <consumer_commit>` and `git ls-files --unmerged`
inside the candidate. The command's JSON identifies `consumer_commit`, rendered
baseline, attempt record and native conflict paths. Its adjacent `merge-output`
file preserves Git's NUL-delimited messages, including directory conflicts
which may have no index stages. Resolve rename, binary, mode, add/add and
modify/delete conflicts explicitly and commit the resolution. Even a directory
conflict with no staged changes needs a resolution commit (use
`git commit --allow-empty` only after reviewing its messages and actual files).
`prepared_diff` retains the initial complete patch; `complete_diff_command`
prints the exact Git command for inspecting the current resolved delta.
There is no blanket `ours`/`theirs` resolution or automatic deletion override.

Resolve authoritative source first. Regenerate OpenAPI and retained harness
carriers with the consumer's current commands, then run their drift checks.
Validate Cargo.lock through locked metadata/build; resolve dependency conflicts
under the dependency owner, without broad `cargo update`. Select all further
checks from the actual consumer diff and its [validation owner](validation-routing.md).

Preserve consumer migrations. Run its existing append-only checker and required
real-database/runtime-history proof. The updater additionally refuses changed
or removed existing migration bytes/modes, colliding versions and new versions
below the consumer maximum. `migration_issues` names imported history/checksum
obligations even when Git merges cleanly. Resolve these through a reviewed
forward migration or reconciliation, with a concrete disposition for every
issue in both evidence records. It never renumbers or edits migrations for you.
Schema, wire and runtime compatibility remain service-owned admission decisions.

## Review, validate and accept

Commit the resolved candidate. `status` returns `resolved-awaiting-proof` and
the exact `content_tree` excluding only `template.upgrade.json`. Obtain review
and validation records for those bytes. The CLI checks identities and required
evidence fields; the responsible maintainer and reviewer own the truth and
adequacy of each result. Do not manufacture passing evidence from command exit
status alone or from these examples.

Both evidence files use this shape, with actual IDs and concrete results:

```json
{
  "schema_version": 1,
  "kind": "review",
  "verdict": "pass",
  "consumer_commit": "<C>",
  "baseline_commit": "<B0, or null for adoption>",
  "target_revision": "<exact selected template revision>",
  "content_tree": "<status content_tree>",
  "summary": "<responsible reviewer and immutable evidence locations>",
  "checks": {
    "resolutions": "<review of complete delta and native conflicts>",
    "baseline_adoption": "<captured equality or reconstructed baseline review>"
  },
  "migration_dispositions": {}
}
```

For validation use `"kind": "validation"` and exactly these `checks` keys:
`generated`, `locked_graph`, `migration_history`, `compatibility`. Each value
records actual commands/results and immutable evidence locations, or the
concrete reason a check is inapplicable. For adoption, document the unchanged
consumer tree and historical render evidence. `baseline_commit` is JSON `null`
for adoption, not the string `"null"`. `migration_dispositions` maps every
reported issue string to its reviewed disposition; it is `{}` with no issues.
Keep these files outside the candidate's tracked/worktree content.

```sh
/trusted/template/scripts/template-upgrade.sh accept \
  --candidate /work/service-upgrade \
  --review /evidence/review.json --validation /evidence/validation.json
```

Acceptance requires unchanged original C, clean committed resolutions, original
initialization lock bytes, unchanged prior accepted record and matching evidence.
Any content edit invalidates both records. The tool adds only accepted metadata
and a native second parent: resolved candidate R is first parent, complete B1 is
second parent. B1 itself is parented to B0. Compact evidence records are embedded
in metadata under their SHA-256 locators, so full ordinary clones retain both
the evidence and baseline ancestry without a special refspec or remote branch.
Metadata stores the render manifest digest and source/helper/inventory/toolchain/
lock IDs; the manifest is reproducible from that recipe and the baseline tree.
This is bookkeeping, not a signed attestation. `template.lock` always remains
the original initialization record.

Run any requested CI/release proof on the final full acceptance commit. The
metadata seal preserves validated runtime bytes but does not replace that proof.
Integrate with ordinary Git only after checking the original branch is still at
C and resolving all dirty/untracked/ignored overlap. A clean unchanged original
can fetch the explicit local candidate and fast-forward to its accepted commit.
The updater never performs that integration. Any integration changing runtime
content needs fresh applicable review/validation before accepted use.

## Retry and recovery

Repeating `prepare` for the same original C, accepted baseline, target and choices
at the same destination reports the existing attempt without overwriting edits.
An already accepted target returns `no-op` before rendering. New consumer commits
after acceptance remain ordinary business work; the next upgrade still uses the
stored pristine baseline. Do not replace an attempt's source or original checkout
while it is in progress.

`status` distinguishes no attempt, incomplete, prepared, conflicted,
resolved-awaiting-proof, sealing, accepted and aborted. An incomplete render or
copy never advances custody. Preserve its evidence and prepare into a fresh
destination after repairing the reported capability/input. A crash while sealing
is reconciled by repeating `accept`: the stored seal, parent edges, exact content
and compare-and-swap ref update must agree. A moved candidate or changed content
refuses; recover the recorded candidate without discarding later work.

```sh
/trusted/template/scripts/template-upgrade.sh abort --candidate /work/service-upgrade
```

Abort marks only the isolated attempt retired. It keeps all files, edits,
commits and evidence, and cannot retire an accepted/sealing result. Dispose of
an unused candidate through your normal checkout lifecycle after preserving
needed work. A shallow clone missing the baseline must obtain its full history
before preparation; copied metadata without the required parents is refused.
Image rollback and durable-state recovery remain separate service operations.
