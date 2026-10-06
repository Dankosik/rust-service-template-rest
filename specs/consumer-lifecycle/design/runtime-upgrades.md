# Runtime upgrade mechanism

Status: ready. Implements [U1–U4](../spec.md#u1-upgrade-inputs-and-support-boundary)
under [System Design](system.md). Portable sync and `template.lock` retain their
existing meanings.

## Inputs and complete baselines

The operation takes explicit local trusted template repository paths and exact
commit IDs, the consumer repository path and committed HEAD, and normalized
identity/profile inputs from the complete initialization lock. A URL or SHA
string in that lock establishes neither trust nor local object availability.
Only the template repository already admitted by this task, or a later explicitly
admitted source, may execute generation code. No fetch, hook, credential reuse,
source replacement or new network trust is implicit in `prepare`.

Render each required revision by materializing that source into an empty
disposable repository and invoking **that revision's** `scripts/init-module.sh`
with its `scripts/lib/template_init.py`, `template_state.py`,
`template_profiles.json`, pinned toolchain and locked graph. Complete the public
initializer's locked metadata, pinned formatting, OpenAPI and selected harness
generation. Preserve regular-file/executable modes and allowed generated links.
Record source/helper/inventory/toolchain/lock and full output tree identities.
An explicitly shared absolute Cargo target may accelerate generation without
changing those inputs. Cache contents are not baseline authority.

The cheap `_project_staged` output is not admitted as B0 or B1. There is no
assumed equivalence with full initialization, no formatting bypass and no
current-helper-on-old-source shortcut. Failure to reproduce an old input gives
an actionable refusal rather than a fabricated pristine tree. Generation uses
a scrubbed environment with required local tool/cache paths; publication tokens,
cloud credentials and arbitrary Git configuration/hooks are excluded.

For a new service, retain its first committed, fully initialized tree before
business edits. For a legacy service, `adopt` prepares an isolated adoption
candidate from the complete lock and a fully reconstructed old render. Prefer
an original initialization commit and verify its tree equality with the render.
When only source/choices survive, the adoption receipt explicitly identifies a
reconstructed baseline; review the reconstructed tree and its complete diff to
current consumer C. The reviewer must establish that the recorded historical
source/helper/choices are unambiguous and classify the difference as consumer
work, rather than claim a historical output comparison that was unavailable.
Contradictory choices, unavailable source/generation, or unexplained evidence
cannot be overridden by `adopt`. Adoption changes no runtime files; it records
the reviewed baseline and keeps every existing consumer file intact.

## Durable representation

Use native Git commits/trees as the rendered-baseline store. A committed
`template.upgrade.json` in the consumer is a small schema-1 **accepted** record:

- accepted upstream repository identity and commit;
- rendered baseline commit/tree plus SHA-256 of its canonical render manifest;
- normalized initialization choices and the original `template.lock` digest;
- rendering helper/inventory/toolchain/lock identities and whether the first
  baseline was captured or explicitly reconstructed/adopted;
- accepted candidate's content-tree identity excluding this control record,
  and immutable review/validation evidence locators with their hashes.

Use the existing strict JSON and identity/path validation in `template_state.py`;
the new command owns its schema and lifecycle. Reject unknown shapes, duplicate
fields, contradictory inputs, missing objects or an unreachable baseline.
This record is upgrade bookkeeping, not an attestation or a new source of
service policy. Original `template.lock` remains the initialization record and
is not advanced by runtime upgrades.

Baseline B0 is the exact complete initialized tree. Each later B1 is a complete
new render with the same choices, parented to B0. The accepted consumer commit
has the resolved candidate as its first parent and B1 as its second parent.
Therefore ordinary full clones retain the required baseline objects without a
special refspec, remote side branch, LFS object or a full-tree archive in every
working tree. Shallow clones that omit the baseline must explicitly obtain the
needed history before use. Garbage collection cannot remove a reachable
baseline. Compare-and-swap ref updates prevent overwriting a concurrently
changed candidate. Creation of a baseline commit never makes it accepted.

Do not embed a commit's own hash in its tree. Evidence binds the resolved
content tree excluding `template.upgrade.json`; the final acceptance commit
adds only the reviewed metadata and Git parent edge. Validation of runtime
bytes stays valid because those bytes cannot change during this sealing step.
The next real CI/release result still names the final full commit.

Attempt records live under the attempt repository's Git common directory,
`codex/template-upgrade/<attempt>/`, outside the tracked content. They bind C,
B0, target source and B1, merge outcome, candidate revision, generated-output
dispositions, evidence hashes and state. They contain no secrets. An interrupted
record is evidence of an incomplete attempt, never evidence of acceptance.

## Prepare, resolve, validate, accept

1. **Admit and isolate.** Read the original committed consumer HEAD C and record
   its branch/index/worktree status. Preparation is allowed with dirty source
   because it copies committed objects into a separate repository using native
   Git, without hardlinks/alternates or inherited local hooks/filter/merge-driver
   configuration. Original index, branch, tracked dirt, ignored and untracked
   contents are untouched. No stash/reset/clean operation is part of this path.
   A path that cannot be represented safely by Git fails before materialization.
2. **Recover B0 and render B1.** Verify accepted metadata, reachability and recipe.
   If no accepted metadata exists, run the separate adoption result first.
   Execute the selected trusted new public initializer in its own stage. Refuse
   a different service identity/profile; an introduced selector can only take
   its canonical historical `none` interpretation when the current lock owner
   already defines that normalization. Otherwise reopen profile migration.
3. **Merge natively.** Use `git merge-tree --write-tree --merge-base=B0 C B1`
   inside the isolated candidate repository. Treat its return status and
   conflict records as authority; do not infer success from the existence of a
   result tree. Git owns rename, add/add, modify/delete, mode and directory/file
   collision semantics. Consumer-only content persists. Keep the original
   initialization lock and accepted control record out of the template update
   by explicitly preserving C's versions; generated B1 retains its own lock for
   provenance. Record both deliberate control-file dispositions in the diff.
4. **Present the complete change.** Materialize the result only in the isolated
   candidate and retain native conflict stages/paths for resolution. Show the
   full consumer delta and all outstanding conflicts; binary/unresolved rename
   or delete collisions require explicit resolution. There is no global
   `ours`, `theirs`, automatic deletion override, or partial-apply success.
   Ordinary Git edits/commits resolve the candidate. Original C stays intact.
5. **Restore generated authority.** Merge source owners first. Regenerate
   OpenAPI and retained harness carriers from the resolved source, then run their
   existing drift checks. Generated files never independently select business
   behavior. Cargo.lock may merge cleanly but is still validated with locked
   metadata/build. A graph conflict needs a deliberate reviewed lockfile change
   under the dependency owner, never automatic broad `cargo update`.
6. **Admit migrations and compatibility.** Preserve consumer migrations. The
   existing append-only checker and runtime history remain authoritative.
   A newly imported template migration below the consumer's already-admitted
   maximum, a colliding version, or a changed applied checksum prevents
   acceptance; do not renumber or edit it automatically. The service migration
   owner must supply a reviewed forward migration/reconciliation disposition
   before that candidate can be accepted. A clean Git merge does not waive
   schema/wire/runtime compatibility admission.
7. **Validate and review R.** Select proof from the actual consumer diff using
   its current repository owners. Bind results to the resolved content tree and
   original accepted baseline. A subsequent content change invalidates them.
   The responsible maintainer/reviewer owns semantic resolutions and evidence
   admission; the CLI checks identities, does not manufacture a review verdict.
8. **Seal and adopt.** An explicit `accept` verifies unchanged C/base/target,
   no unresolved conflicts and matching admitted review/validation. It creates
   the metadata-only sealing commit with first parent R and second parent B1 in
   the isolated candidate. Re-read metadata, parent reachability and content
   identity. A maintainer may then fast-forward a clean original branch from C,
   after checking all worktree/untracked/ignored overlap; otherwise retain the
   accepted candidate for later ordinary Git integration. The tool never moves
   or cleans the dirty original checkout. Any integration changing runtime
   content requires new applicable evidence before becoming accepted there.

The command surface is `adopt`, `prepare`, `status`, `accept`, `abort` in
`scripts/template-upgrade.sh` / `scripts/lib/template_upgrade.py`. The wrapper
loads code from its explicitly trusted checkout, not a helper opportunistically
found in a consumer. `status` reports no attempt, prepared/conflicted, resolved
awaiting proof, or accepted, and the exact identities. `abort` retires only its
own isolated attempt and preserves original C; it does not remove user content
or evidence. Candidate cleanup follows the existing checkout lifecycle owner.
No custom Git merge implementation or generic transaction journal is added.

## Distribution and link closure

The authoritative updater CLI and library live in the explicitly admitted
template-tool checkout. Add `scripts/template-upgrade.sh` and
`scripts/lib/template_upgrade.py` to the initializer's source-only inventory,
alongside the already source-only updater proof. This enforces the selected
execution owner rather than leaving an independently edited consumer copy as
an implied tool authority. The consumer argument still names the separately
owned target; full historical rendering still executes each historical source's
own helper. No new fetch or execution trust is implied by distribution.

Keep `docs/template-upgrade.md` in initialized consumers, outside portable-sync
ownership. It is their supported operational route and explains how to select
the trusted tool checkout. Retained README, sync, command and structure
documentation may link to that retained guide. Guide examples invoke an
explicit path such as `<trusted-template>/scripts/template-upgrade.sh`, not a
missing `./scripts/template-upgrade.sh` inside the consumer. Any browseable
reference to removed CLI/library/proof or source-only design uses a source
repository URL or explanatory code text, never a broken consumer-relative
Markdown link. Initialization must retain every guide-relative link target.

Do not add CLI, library, guide or updater tests to `template-owned.paths`.
Already-portable documents/Makefiles/scripts cannot acquire an unconditional
relative link or executable dependency on these new non-portable files: a
portable-only update of an older consumer must remain closed. Keep the new
route in its retained service-document owners and the explicit trusted source
invocation. The existing source-only inventory and generated-output proof own
these containment checks; there is no new updater distribution system.

## Idempotency and recovery

An accepted metadata target equal to the requested target and matching choices
returns a no-op before rendering. New consumer commits after an accepted
upgrade are ordinary service work; the next preparation uses the same accepted
B1 and current C. An attempt fingerprint contains current C, B0, requested
source and normalized inputs. Repeating an unfinished attempt reports its
existing candidate and state; it does not overwrite conflict resolution.

Render/merge failure leaves the original untouched. A crash during candidate
record creation yields either an atomically readable record or an explicitly
incomplete attempt. A crash during acceptance is reconciled from the candidate
ref, sealing commit parents and manifest readback; mismatches refuse rather
than create a second inferred success. Parent and metadata checks are
mandatory before treating any imported Git commit as an accepted upgrade.

Proof must establish recoverable capture/adoption, actual full historical and
new rendering, consumer preservation including dirt and local deletes,
conflict visibility, no early baseline advance, interruption/retry behavior,
reachable baselines after a fresh clone, and a second upgrade after business
edits. Executor-selected tests reuse existing initializer/sync fixture patterns.
The real A-to-B consumer exercise supplies end-to-end usage, while focused
synthetic cases exercise refusal and conflict edges without multiplying image
or provider runs.
