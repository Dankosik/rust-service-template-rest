# Planning: transfer the admitted webhook body

Status: ready. Independent [Task Review / Readiness](planning-review.md): PASS.
Authority: [Intent](intent.md), [reviewed Specification](spec.md),
[ready Technical Design](technical-design-transition.md), and [design.md](design.md).

## One fixed implementation unit

**Outcome:** the mounted HTTP webhook path transfers its collected `Bytes`
through existing receipt arbitration into the winning durable payload without
copying the body. Existing slice callers remain supported and copy only on a
receipt win. All observable and durable behavior in the Specification stays
unchanged. This unit has no predecessor implementation task and no separately
consumable layers; the HTTP caller and inbound representation/core belong in
one assembled change. No ledger, waves, or separately scheduled proof tasks
are needed.

**Current-to-target delta:** HTTP currently lends its collected buffer to
`Receiver::receive`, and `Incoming::new` copies it into a `Vec<u8>` after the
receipt insert wins. Implement the reviewed additive `receive_bytes` entry
point and one private admission core with a private borrowed-or-owned carrier.
Both public entry points retain the same arguments/result except for body
ownership. The HTTP adapter moves the original collected handle into the owned
entry point. Verification borrows the exact original bytes; the winning branch
consumes the carrier inside `Incoming::new`. Owned bytes move, borrowed bytes
use `Bytes::copy_from_slice`. Duplicates and rejected input gain no body copy,
serialization, or preparation. Keep construction and preparation measurement
boundaries intact so helper work cannot escape the comparison.

Change only the private `Incoming.body` representation to `Bytes`. Keep its
Base64 Serde attributes, field order, other fields, public slice accessors,
redacted Debug, job kind, version behavior, and consumer/processor contracts.
Move the current admission implementation into the shared core and remove the
superseded inline path; do not retain duplicate validation or SQL logic. The
borrowed public API remains supported, without deprecation or a removal plan.

## Accepted inputs and mutable owners

| Input | Ready authority / consumption |
| --- | --- |
| Meaning, constraints and unchanged behavior | [intent.md](intent.md), blob `b5a507c1d02f634a96fd77e6b08dba4890b25191`; [spec.md](spec.md), blob `467b750b9a141c6c10e3455a9200c21407de58e3`; consumed before source edits |
| Mechanism, file placement and proof claims | [design.md](design.md), blob `6865b530187045c4ee7bb61422282835bd5c4fd3`, with [PASS review](technical-design-review.md); consumed before source edits |
| Dirty source baseline | The four source blob identities in [Design](design.md#decision-and-evidence) match current files at Planning. Preserve their profiling and unrelated edits; HEAD alone is not this baseline. Root owns immutable snapshot custody before optimization mutation. |
| Execution host and snapshot custody | [operations.md](operations.md), maintained by root; host availability and fixed baseline/final manifests are consumed by final remote proof/comparison, not prerequisites for independent code authoring once baseline bytes are preserved |

Root has preserved the pre-edit dirty baseline at remote
`/root/optimization/baseline`, archived as
`/root/optimization/evidence/baseline-source.tar.gz` with SHA-256
`03989134f24224b2ab95709881ee5ec5d89f7c8e3dabe06ad0b778bccf23d82f`.
The captured allow-list is [source-files.nul](../hotpath-profiling/source-files.nul).
Root owns verification and readback of this custody report and the eventual
`/root/optimization/evidence/baseline-binaries.sha256` manifest. Baseline build
progress does not gate the source handoff, and Planning claims no build result.

One Acceptance-Unit Lead owns the following source/test delta serially in the
existing checkout. Root owns infrastructure, snapshot custody and operations;
these responsibilities do not authorize overlapping source edits.

| Writable surface | Required delta / boundary |
| --- | --- |
| `crates/infra-webhooks/src/inbound.rs` | Owned entry point, private shared core/carrier, private `Bytes` storage and constructor conversion. Extend existing adjacent serialization proof only where needed. |
| `crates/infra-http/src/webhooks.rs` | Move collected `Bytes` into the owned entry point; preserve body collection, endpoint lookup, route, Problem mapping, middleware and observations. Extend existing adjacent proof only where needed. |
| `test/tests/webhooks/inbound.rs`, only for a real coverage gap | Adapt or extend existing admission parity proof using existing fixtures and dependencies. Exact tests and assertions are Implementation decisions. |

Existing HTTP/inbound source remains the implementation authority; there is no
generated-source update or new directory, module, crate, feature, dependency,
public body abstraction, or runtime owner. `infra-jobs::enqueue` and its shared
preparation/validation/SQL stay unchanged. Do not change pool settings,
telemetry, schema, worker behavior, transaction topology, durability, storage,
allocator, release profile, limits, configuration or unrelated dirty work.
The default PostgreSQL pool remains four connections.

## Completion and dependency timing

Source implementation is executable from the closed inputs above. The executor
chooses exact tests, fixtures, commands and relevant profile coverage while
coding, reusing adequate existing proof. No test inventory, execution receipt
or review is a prerequisite for returning the assembled code as `Implemented`.
Use [Implementation](../../docs/spec-first-workflow/phases/implementation.md)
and the matching existing Rust methods; follow its allowed feedback and final
validation timing within the remote-only authority.

After this complete unit is assembled and writers have joined, its delivery
owner performs one consolidated final validation/comparison and the required
final independent Implementation review. The following are Completion claims,
not implementation tasks or additional review phases:

- Relevant build and passing behavior proof under repository validation owners,
  preserving optional-profile boundaries and locked dependencies. Full
  pre-normalization JSON-byte parity, original/decoded body bytes and metadata
  must remain compatible; decoded-value equality alone cannot prove the wire
  contract. Reuse existing admission proof for the affected borrowed and owned
  paths, including real PostgreSQL proof wherever durable behavior is claimed.
  [Design proof map](design.md#proof-map-and-continuation) and
  [Specification invariants](spec.md#deliberately-unchanged-behavior) own the
  required behavior, not a new Planning test matrix.
- A fresh fixed-baseline/fixed-final comparison on the same approved droplet.
  Match toolchain, resolved dependencies, features, release profile, allocator,
  CPU placement, database, load generator, fixtures, pool size, warmup and
  instrumentation within each comparison. Root retains source identities and
  manifests; historical results cannot substitute for this new control.
- At least one actual full large-fixture body length less measured allocated
  bytes per newly admitted delivery over the combined `Incoming::new` and
  `enqueue::prepare` boundaries. Include carrier/helper costs in that scope,
  confirm HTTP reaches the moved-buffer path, and report allocation counts
  separately. The retained nominal 64 KiB fixture has a slightly larger body;
  use its measured full length, not a rounded threshold.
- Ordinary release observations, separate from instrumented allocation runs,
  for new small/large deliveries, duplicates and retained mixed traffic.
  Report repeated samples/spread of CPU, latency percentiles, successful useful
  throughput, failures, dropped work and peak RSS. No reproducible regression
  beyond observed variation is allowed. Equal pool settings are mandatory;
  any 16-connection synthetic control is labeled and changes no default.
- A retained new comparison report with actual commands, source/environment
  identities, results, variance, limitations and final review disposition.
  The delivery owner selects evidence/report files under this existing task
  bundle and the coordinator records their locators in its handoff. These
  evidence artifacts do not create source implementation units or a new ledger.

Root supplies remote execution and evidence transport under
[operations.md](operations.md). No local cargo, make, Docker, tests, services,
load, hotpath, Python or Node processing is authorized. All resource work is
restricted to droplet `606044304` (`159.89.101.4`), 8 AMD vCPU / 16 GiB,
`fra1`, at $0.16667/hour, total at most 8 hours/about $1.34. Root manages and
removes that exact host after evidence retrieval. The executor requests remote
commands through root; this plan grants no independent SSH/provisioning or
provider mutation. Use synthetic data/providers only, no production data or
credentials. No deployment, push, PR, real provider or Railway mutation is
selected. Expiry or unavailable required remote proof prevents verified
Completion, while implementable source work remains valid.

## Readiness walkthrough and reopen

The Lead first consumes the ready Specification and Design and obtains the
root-held pre-edit baseline custody locator. It edits the existing inbound
owner and HTTP call together, preserving current fixture/profiling work, and
writes only the missing tests inside existing owners. No additional product,
API compatibility, runtime ownership, failure, rollout or dependency choice is
needed. After the assembled implementation is returned, remote final proof
consumes fixed source snapshots, followed by one final independent review.
Passing Planning means this sequence is executable, not that it has run.

Reopen Technical Design for a mechanism, lifetime, representation/API conflict,
missed allocation target or reproducible regression requiring a different
mechanism. Reopen Definition for changed behavior, scope or success meaning.
Return unavailable infrastructure, snapshot custody or external authority to
root. Routine coding and test repairs stay with Implementation. Do not add
streaming Base64, capacity hints, shared-job changes or pool tuning as an
unreviewed fallback. No user-owned decision remains open.
