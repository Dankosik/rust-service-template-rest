# Rust Ownership Review: HTTP idempotency

Adapter: [Rust Ownership Review](../../../docs/spec-first-workflow/rubrics/rust-ownership-review.md)
under shared [Review](../../../docs/spec-first-workflow/shared/review.md), over
the [ownership map](ownership.md) and the [system design](system.md).

The panel was triggered for three reasons:

- several crates and ownership boundaries change: a new provider crate, a new
  `infra-http` module, and the config, service, test, and initializer owners;
- generated/manual containment is non-obvious, with two marker profiles, one
  of them derived;
- the first panel's reviewers disputed the candidate's reading of the
  portable instructions.

Every lane was a native `reviewer-agent` carrier selected with the Opus model
field, with fresh history and a read-only boundary. Each lane received:

- the fixed candidate hashes, which it verified before and after reading;
- its one lens from the rubric;
- the accepted Definition inputs;
- the rule that no network request may carry the user's email address or any
  other personal data.

No lane edited a file, built, tested, started a container, or made a network
request. One panel-3 lane recomputed the section 8 vectors with local Python.

## Candidates

| Candidate | `system.md` SHA256 | `ownership.md` SHA256 |
| --- | --- | --- |
| c1 | `4dff17b6f8197ab57f590ccaaebe80efada3f02c78b5dbc2b5011a863070af03` | `fdac1c639e9e4da5971233464ddb054c1748c86554b7f67ce976807dbf7cd008` |
| c2 | `0082ddd04d96a529c61a9cf3efd8a369b41c8bb7d08dfb6c96df4c739ddb187d` | `fdf7ad4ecfcb308c24dd7a23171e5c3ab2ed0e81458cd1f2b9fc1b20df63cd91` |
| c3 | `69a9fac2c92e362e2dcb49ffdcde4ddef09a0249d42e1dc8847c51db7d334208` | `2e6f60ea0d6438029abace9e59aed338f0c392c11fe6b57afb8da99167e10da7` |
| c4 | `afddfa6c72376b5c2122190797eee7e877ac5a660693b7f517c7341cd9721047` | `e0f767cf07095940369c1ae99d0f877ae59b4c0f5f312c1fec420b0f74e5b4df` |
| c5 | `6778132a3ee8cf2d7fb96129423d38ad31a7c433419d966574235d402450b8e8` | `1ccc52d799f6a5cbad2ecca1a3a3444eebcf92d89ecb539e3bbeed59c9712575` |
| c6 | `1e8685998422be444ebd44f5e38d8bcacfbb99b8d84380d0e6f72cc6d6127ac3` | `5684eb9f0e65a3d33a35743af3c5f60e68bb35637898d3ee584ac314dcde630a` |
| c7 | `b198ac00aa5c8b81507a0d42b3591e7afcfbf101b6cc51b52b8a6611b9c1e620` | `b2ae411d1f344c83a6757f0c678ec2403469512ff0c700a359296a808c1d76ab` |
| c8 | `468c103bd7b129cf1fda214b7827927ceb9a35c812a197e2a57fa1c853dde80a` | `8a50f29c94e76d59944c3054707e019715a69dac482ae55c5b647df97022972c` |
| ready (c8 plus the `Status` lines) | `47cf5fe908bc790a0dae2425a05e3e1fd3cae9ef0c1aa81bce547b8b9964510f` | `80818a74412e3ff55be24086d41df3d22d6e3a4f353d06c7ce8f17e7ade7414e` |

## Panel 1 on c1

| Lens | Reviewer | Verdict | Findings |
| --- | --- | --- | --- |
| 1. Responsibility and execution paths | `a8962715f15f9d8e9` | PASS | Notes N1–N5. For other lenses: X1, the `contract()` caller at `api.rs:157` inside an authentication region; X2, incomplete allowed dependencies; X3, a reverse-edge rule that conflicts with Persistence. |
| 2. Placement, direction, visibility, containment | `a6a2e37431e35e601` | FAIL | F1: the reconciled rule left the SQL that runs inside `Tx` no legal crate. F2: a feature depending on a crate that owns SQL contradicted the portable instructions, which would trigger the spec's conditional reopen. F3: the `api.rs:157` caller. F4: P9-only dependencies unassigned. F5: allowed-dependency precision. |
| 3. Cohesion, naming, grouping, tests | `a0424d1b9becb5820` | FAIL | F1: the `api.rs:157` caller. F2: no home for P9-only dependencies and fixtures. F3: a store/cleanup drain with two owners. F4: no homes for the attempt type, the outcome enum, and the constants. F5: accessor naming and reuse of `occupied_string`. |

Falsifiers that survived:

- the new crate's deletion test;
- acyclic and necessary edges;
- `sha2` feature unification;
- the `none` contract and lock projections;
- marker removal, including nested removal and a sub-profile marker inside a
  removed path;
- the proof layers;
- portable placement.

The owner did not reopen the Specification for lens 2's F2. The design was
restructured instead, so the retained profile adds no feature-to-provider
edge and the conditional reopen is not triggered. The new design follows the
authentication split: inbound composition in `infra-http`, and the mechanism
in the new provider crate `infra-idempotency-store`. The owner also repaired
every other finding. The restructure changed interfaces and the risk surface,
so the next panel used fresh reviewers.

## Panel 2 on c2

| Lens | Reviewer | Verdict | Findings |
| --- | --- | --- | --- |
| 1 | `a487a38c06a4be148` | PASS | Notes: N1, the wiring guard's claim; N2, the rule for choosing the real or inert store; N3, parity between the runtime key grammar and the declared pattern; N4, a decode after commit with no mapping row. For other lenses: X1, response-component placement; X2, `Activation::Active` constructible outside `agree`; X3, the composition root offered as the SQL home; X4, the section 3 reading and its anchors. |
| 2 | `ab46a62aa0e5f6a16` | FAIL | F1 (blocking): section 3 offered the composition root as a legal home for the operation's SQL, against Boundaries and Integration. F2: thin citations and alignment for the section 3 reading. F3: an incomplete `mounted.rs` dependency list. |
| 3 | `abd4c8d2b1a61da48` | FAIL | F1: new response components outside their documented owner, unreconciled. F2: the lock-key vector assigned to tests that cannot observe it. F3: an incomplete mounted-only dev-dependency list. |

Repairs in c3:

- the adapter lives in a `crates/infra-<provider>` crate, behind an
  `async-trait` port in the feature's HTTP module;
- a "Response components" decision, with the pack-owned-family rule added to
  three documents;
- the lock-key literal tested in the store's `attempt.rs`;
- `utoipa` and `utoipa-axum` added to the mounted set;
- `execute` takes `Result<Fingerprint, FingerprintError>` and renders the
  fingerprint 500 with the request id;
- `#[non_exhaustive]` on `Activation::Active`;
- `stored.rs` owns capture, and the first response reuses the captured form;
- corrected anchors and wording.

## Same-reviewer rechecks on c3

| Lens | Reviewer | Verdict | Findings |
| --- | --- | --- | --- |
| 1 | `a487a38c06a4be148` | PASS | N5: the bootstrap test cannot construct `Active`. N6: a stale "exactly one outcome" sentence. |
| 2 | `ab46a62aa0e5f6a16` | PASS | N1: one marker id covered two regions. N2: unmarked prose must stay pack-neutral. N3: `async-trait` missing from the mounted set. |
| 3 | `abd4c8d2b1a61da48` | CONCERNS | F3-residual: `async-trait` missing from the mounted-only set. Notes on the bootstrap test path, the parity test's dependency, the moved comment, and wording. |

All three reviewers asked whether a same-reviewer recheck still fit, because
the c3 repair changed public interfaces (`execute`'s signature,
`FingerprintError`, and `Activation::Active`'s visibility). The rubric re-runs
each materially affected lens in fresh context, and shared Review asks for a
fresh reviewer when a repair introduces a new interface. All three lenses were
affected, so the rechecks are kept as evidence only. The owner repaired their
notes (c4), and panel 3 re-ran every lens in fresh context.

## Panel 3 on c4, fresh context

| Lens | Reviewer | Verdict | Findings |
| --- | --- | --- | --- |
| 1 | `a46629774ab465401` | PASS | None. Notes O1–O4: the reserve wording, the combination rule written in two places, the inert store's other methods, and the unused record in `Committed`. |
| 2 | `a738bb8d5d1c8e69b` | CONCERNS | F1: the design claimed features cannot use `Tx::connection()`, but `sqlx-postgres` 0.9.0's `PgConnection` has inherent `copy_in_raw` and `copy_out_raw`, which need no import. A feature holding a `Tx` could therefore read or write any table. |
| 3 | `a4a6488bea985c5af` | PASS | None. Notes: the commit proxy cannot join on drop; the mapping tests cannot reach two section 9 rows; bootstrap's tests could sit in its existing `mod tests`; naming nits; no named cancel test for `run_cleanup`. |

Falsifiers that survived:

- **Lens 1:**
  - store selection agrees with the retention rule;
  - `Active` cannot be built outside `agree`;
  - cleanup cannot outlive a failed startup;
  - moving contract assembly is safe;
  - there is no anonymous fallback;
  - outcomes are counted once;
  - 504 precedence holds;
  - the readback stays within its bound;
  - the first response matches replays;
  - there is no success without the seam;
  - the cleanup join and budget fit;
  - refusals come before any write.
- **Lens 2:**
  - the new crate and the module survive deletion;
  - the edges are acyclic;
  - the SQL has a legal home;
  - no `contract()` caller is missed;
  - marked regions can be removed;
  - the `none` contract and lock are unchanged;
  - generated files stay derived;
  - portable placement;
  - the section 3 reading holds, apart from F1.
- **Lens 3:**
  - every file survives deletion;
  - names follow the authentication precedent;
  - every shared declaration has a home;
  - the recomputed vectors reproduce;
  - `TxError` classification is feasible;
  - the proof layers are right;
  - marker and path removal work, including nested removal;
  - the dependency split holds in every graph.

Repairs in c5:

- **F1.** `Tx` becomes opaque: a private field, no methods, and neither
  `Deref` nor a conversion trait. The store's free function
  `connection(&mut Tx)` is the only accessor, and `infra-http` does not
  re-export it. Probe 1's fourth run (`visprobe/`) measured this. The adapter
  compiled. From a crate that depends on the seam only, `tx.connection()`
  fails with E0599, `store::connection` with E0433, `as_mut` with E0599, and a
  dereference with E0614.
- **Lens 1 notes applied:**
  - O1: the reserve is its own constant in `execute.rs`.
  - O2: one predicate, `http_idempotency_requirement`, owns the combination
    rule, and a lock-refusal test case is added.
  - O3: the inert store's methods are specified.
  - The `Outcome` visibility wording is fixed.
  - O4 is left as is: the reviewer found it resolved.
- **Lens 3 notes applied:**
  - The commit proxy stops its tasks on drop, and each test awaits a bounded
    join.
  - The mapping tests claim only the `Attempted` and `ReadBack` rows. The
    key-rejection Problem is factored out, and P9 names budget expiry.
  - Bootstrap's tests move to a region inside its `mod tests`.
  - `idempotency_composer` is renamed `prepare_http_idempotency`.
  - A note covers the `Digest` alias.
  - A `run_cleanup` cancel test is added.
  - `maintenance.rs` keeps its name.
- **Lens 2 observations applied:**
  - the adapter-construction rule in the guide;
  - the unmarked migration sentences in `migrations/README.md` and
    `persistence.md` reworded so they stay true with the pack;
  - the `Tx` join path in the persistence region;
  - `openapi.rs` owns the expected-value constants, and a test pins the
    derive's pattern literal to its constant;
  - the `problem.rs` file row;
  - the `Cargo.lock` wording.
- **Lens 3 observations applied:**
  - `tokio-rustls` features;
  - P9 prose that states its introspection condition.

## Session restart and c6

A fresh whole-lens-2 re-run was dispatched on c5 (reviewer
`ae71be9f21c026715`). The coordinator session then restarted, which stopped
that reviewer before it returned a verdict and cleared the design scratchpad.
The re-run is treated as lost, and it carries no verdict.

The owner then:

- rebuilt c4 from c5 by reversing the recorded c4-to-c5 edits, and matched
  both c4 hashes;
- recreated `visprobe/` from its recorded source and re-ran it. It
  reproduced the zero-warning adapter check and the four refusals
  (E0599, E0433, E0599, E0614);
- recomputed the section 8 vectors with a fresh implementation. All six
  literals matched.

A full re-read of both files after the restart produced c6. Its changes from c5:

- the section 14 evidence locator now says which probe sources were lost and
  which results were re-established, and adds probe 7;
- the section 8 provenance sentence is updated;
- the "Expected values" paragraph moves below the component table it follows
  from;
- the opaque-handle bullet is rewrapped;
- the key-layer proof cell names the factored key-rejection test;
- the store facade row names its `connection` re-export.

None of these changes placement, ownership, visibility, or proof location.

## Lens 2 on c6, fresh reviewer for the unavailable one

| Lens | Reviewer | Verdict | Findings |
| --- | --- | --- | --- |
| 2 | `a93c41d9fe82c286a` | PASS | None. Scope: the c4-to-c6 delta (SHA256 `15e7fbd842fbfb61116df941ce85452b6382be8c4a62289a0e49e91b2a6a720b`, 23 hunks) and the proof it invalidated. |

The reviewer:

- verified both hashes before and after reading;
- confirmed that the reconstructed c4 files match the c4 hashes;
- confirmed that the delta is the complete c4-to-c6 change.

It found that the repair narrows the risk surface rather than widening it,
so it did not re-review the unchanged candidate. Before the repair, any
holder of `Tx` could reach the connection. Now only crates that already
depend on the store can.

Falsifiers that survived:

- **F1 is closed.** The recreated probe matches the design, and the lock
  shows no feature-to-store edge. The reviewer also closed the paths the probe
  does not cover:
  - an `extern crate` of a transitive dependency;
  - an associated function or a `From`/`Into` conversion, which the design's
    "only accessor" already forbids;
  - access to the private field;
  - `transmute`, which the workspace's `unsafe_code = "forbid"` blocks;
  - `Store` and the plain data types;
  - the extractor's private attempt;
  - re-export chains;
  - a feature depending on its own adapter, which would be a cycle.
- **The public surface is minimal.** `connection` must be public, and must be
  a free function: `Tx`'s path is re-exported, so any associated item would
  reach features.
- **The new edge is sound.** `declaration.rs -> openapi.rs` is a sibling edge
  inside the removable directory, with the tightest visibility, and the
  contract test stays an independent oracle.
- **The combination predicate belongs in `template_state.py`.** That library
  owns the choices and `validate_profiles`, and `template_init.py` imports
  from it, never the reverse.
- **The reworded unmarked sentences stay true in every output.** This covers
  `migrations/README.md`, `persistence.md`, and the `problem.rs` doc.
- **Nothing downstream breaks.** Marker removal, the `none` contract and lock
  projections, and generated authority are intact. The bootstrap test region
  sits inside an unmarked `mod tests`, and the mounted `tokio-rustls` region
  is always paired with its root declaration.

Cross-lens check: no hunk in the delta invalidates the lens 1 or lens 3
reasoning. The reviewer checked the inert-store paragraph, P9 running under
the hardened chain, and the commit-proxy join precedent.

Non-blocking observations and the owner's disposition (c7):

1. Section 3 named only "a method on `Tx`" as the excluded form. The
   exclusion now also names associated functions, `Deref`, and conversions,
   and the `attempt.rs` Files row forbids them.
2. A `compile_fail` doctest could guard the opaque boundary against
   regression. Not adopted. The specification's proof list does not require
   it, and a `compile_fail` doctest passes on any compile error, so its oracle
   is weak. The rule lives in one type definition with one owner, whose row
   now forbids any accessor.
3. The key's length bounds are also literal-only in utoipa 5.5.0. They are
   added to the expected-value constants, and the pin test covers the
   generated schema.
4. The P9 documentation rule now says P9 runs where the introspection engine
   is retained: the source template and `AUTHN=oidc-introspection` outputs.
5. The `persistence.md` line reference is corrected to 199-201. The roadmap's
   stage history is left as history.
6. `Tx` derives only `Debug`, which the workspace's
   `missing_debug_implementations` lint requires. `PgConnection`'s `Debug`
   prints only its type name.

The owner also recorded one enforcement statement. Like an `in_tx` closure
today, the adapter is bound by contract, not by type, never to end the
transaction with transaction-control SQL and never to name the profile table.
The guide states both.

Candidate c7:

- `system.md`: `b198ac00aa5c8b81507a0d42b3591e7afcfbf101b6cc51b52b8a6611b9c1e620`
- `ownership.md`: `b2ae411d1f344c83a6757f0c678ec2403469512ff0c700a359296a808c1d76ab`
- c6-to-c7 delta: 9 hunks, SHA256
  `1535d0df3a4493ccb984ee720b16a4cefeb4092281d579daf63b6479ccc38aae`

## Technical Design Review repair (c7 to c8)

The Technical Design Review of c7 consumed this receipt and found it
compatible with c7. Its repair (c8) touches these ownership-map rows:

- The record store's `lib.rs` keeps whole microseconds of the retention. The
  store row, the retention row, and the `lib.rs` Files row name the new
  inline, configuration, and database-suite cases.
- The region rule now covers only service-owned files. Template-owned files
  (`template-owned.paths`) carry no markers and, like today's authentication
  and outbound rows, may name profile values and pack paths. This states what
  lens 2 already verified ("no markers go into template-owned files"), and it
  removes a contradiction with the classifier rows the map already required.
- The validation items for `.github/workflows/ci.yml`,
  `scripts/ci/changed-surfaces.sh`, and `scripts/ci/verify.sh` record the
  review's CI observations. So does the adopter-guide row, for the conversion
  and rollback case.

None of these moves a responsibility, a file, a dependency, a visibility
boundary, or a proof layer. Under Transition's unchanged semantic-scope rule,
the panel verdicts therefore stand for c8. The same review's bounded delta
recheck covers the whole c7-to-c8 delta.

## Synthesis

All three lenses pass for the current semantic scope:

- Lens 1 (`a46629774ab465401`) and lens 3 (`a4a6488bea985c5af`) passed on c4.
  Their receipts are kept because the fresh lens-2 reviewer checked the whole
  c4-to-c6 delta and found no hunk that invalidates their reasoning. The
  delta's lens-1 and lens-3 items apply their own notes in the direction they
  proposed.
- Lens 2 (`a93c41d9fe82c286a`) passed on c6.

The c6-to-c7 delta applies only lens 2's non-blocking observations and the
enforcement statement above. It changes no placement, ownership, dependency,
visibility boundary, or proof location. Under Transition's unchanged
semantic-scope rule, the verdicts therefore stand for c7. They also stand
for c8, as the repair section records, and for the ready files, which differ
from c8 only in their `Status` lines.

No ownership finding remains unresolved, and Technical Design Review
consumes this receipt. It is design evidence, not implementation or runtime
proof.
