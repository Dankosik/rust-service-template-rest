# Evidence proposed for Git and external custody

This is an inclusion proposal, not a commit or a new proof result. Nothing has
been staged, removed, moved or hidden with ignore rules. Existing source,
consumer and native evidence identities remain unchanged. The three frozen
tracked CI receipts and `tasks.md` remain under root ownership.

`evidence-curation.json` records the initial snapshot and refreshed reports,
each original path, SHA-256, byte count, recommendation and independent local
copy. The initial snapshot captured 227 records totaling 2,079,571 bytes. Current
counts and bytes are in the index's `current_totals`; the index excludes its own
size/hash to avoid a self-reference. Changed reports receive separate timestamped
copies. All original SHA-bound locators and historical copies remain intact.

## Keep in Git

- Completion, the complete independent-review sequence and the local handoff.
  Keep the initial FAIL results and their bounded closures, not only the last
  local PASS / global NEEDS_PARENT.
- Scalar command-result JSON, including nonzero attempts, aborted adoption and
  superseded upgrade records. `local-attempt-index.json` distinguishes observed
  failures from explicitly expected counterexamples; original records still own
  their scope.
- Compact A/B identity, full-render, review, validation, seal and integration
  metadata, plus exact object-equality evidence used to reuse B checks. These
  records do not publish the separately owned consumers' full source.
- Native recovery command/summary metadata and hashes. The compact
  `native-recovery-summary.json` points to full state and archives externally.
- Actual CI control/readback/cache metadata, current final-source object maps,
  the reviewable same-builder scheduling patch, native artifact bindings,
  execution history and eventual qualified C2 comparison. Keep every executed
  candidate/run/attempt distinct.

The current final-source object maps are about 250 KiB together and make the
comparison input check reviewable. Superseded maps remain externally preserved.
They are not silently rewritten to the latest commit.

## Keep outside Git

Full logs/ZIPs, SBOMs, native PostgreSQL/JetStream archives, complete runtime-state
snapshots, executable actors and initialized actor repositories remain in named
task-owned directories. Their existing records retain content hashes and native
artifact IDs/digests where available. Cargo targets and caches are rebuildable
working data; no completed proof is inferred from their presence.

Raw consumer source patches and repository snapshots remain local while the
consumer's external authority is pending. They must not be published indirectly
through the template PR. The accepted evidence records are not rewritten to
replace their original locators; the curation index maps those paths and hashes
to the independent local copies.

Large native roots currently include:

- `native-recovery-ffb33186-20261007`: successful actors, archives and native state.
- `native-recovery-f4ba-20261007`: failed attempt and preserved evidence/resources.
- `ci-initial-cancelled-control-7761090`: initial cancelled run and redacted scan.
- `ci-old-control-proof-8cf53b`: successful superseded diagnostic.
- `ci-failed-final-serial-proof-4ba`: failed whole-gate serial with successful image proof.
- `ci-replacement-serial-proof-fb33186`: final serial source/derived proof, API data
  and full logs ZIP.
- `ci-final-split-proof-fb33186`: terminal failed Split, successful image proof,
  API data, full logs ZIP and redacted failure log.
- `ci-pair-analysis-fb33186`: complete joined observations; compact Git metadata
  binds this external report by hash.
- `history-scope-native-20261007` and `history-scope-native-20261007-v2`:
  initial incomplete-fixture setup failure and successful canonical-target proof.
  Only scalar result/Make/log hashes are proposed for Git; repositories and RSA
  fixture bytes remain external.

These directories are under
`/Users/daniil/Projects/consumer-lifecycle/20261006`. The record snapshot is
`completion-evidence-20261006T233620Z` there. No retention expiry or deletion is
performed by this curation.

## Preserve the failures and limits

All five completed C2-related runs account for 43,271 observed assigned
runner-seconds: initial cancelled control 3,691; old diagnostic 9,465;
failed-docs serial 10,100; replacement serial 9,894; terminal Split 10,121.
The final pair totals 20,015. These are not charges or all development CI.
Split failed `secrets` and `required`; successful image proof cannot substitute.
The actual runner-image and six Cargo restore differences preserve C2 as
incomplete. The separately repaired candidate-history gate has local native
proof, with focused review and final native CI still owned by root.

Split's history-scan failure is retained. Root's evidence owner established
that both finding commits are outside the source/control ancestry, belong to
`refs/heads/codex/transport-recovery-20261006`, and entered between the serial's
451-commit scan and split's 456-commit scan. Record only ancestry/ref/fetch/count
evidence and hashes, not JWT or credential contents. Root owns the explicit
candidate-history policy correction and its proof; no older run becomes a pass.

Source `VCS_REF` changes embedded binary commit metadata. Runner-image rollout
was mixed. Raw timestamps, cache outcomes and artifact hashes remain attached to
their actual run. Future report commits gain no implied execution proof, and
image-only timing cannot close a failed whole selected-gate C2 outcome.
