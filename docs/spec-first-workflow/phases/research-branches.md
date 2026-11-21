# Research Branches

Load only the branch selected by [Research](research.md).

## Current-State Or Semantic Baseline

Matching names or historical intent do not prove semantic equivalence or runtime
use. Inspect only the surfaces that can change the decision (code, contracts,
runtime callers, persisted/deployed state, mixed versions), tracing grain,
identifiers, units, absence/error states, version authority, and lossy
transformations. Record any unobservable required surface as an unknown with its
decision effect.

## Current External Contract

Verify each decision-changing external claim against official provider
contracts, applicable standards, or maintainer source pinned to the actual
repository version (`Cargo.lock`, toolchain, or deployed service). Add local
operational evidence only when fit or failure behavior remains uncertain.

## Solution Discovery Evidence

Derive vendor-neutral terms from required behavior, invariants, workload,
failure/recovery, and hard constraints. Classify each candidate as a substitute,
prerequisite, complement, or defense-in-depth mechanism, comparing only
same-level substitutes (eliminate candidates contradicted by primary authority
or hard constraints; leave mechanism selection to [Spec And
Design](spec-and-design.md)).

Scan only relevant rungs (`repo reuse -> Rust std -> native platform -> approved
infra -> managed service -> mature OSS -> custom`). For each material precedent,
note which assumptions hold here and where the analogy breaks. For a surviving
external option, record only approval-changing evidence: ownership/support,
availability/quota/SLA, compatibility, security/data custody, pricing unit at
accepted workload, migration, and exit/failback (plus maintenance, license,
vulnerabilities, API stability, and transitive cost for external code).

## Empirical Claim Or Probe

Record version/config, exact command or method, environment, representative
workload or sample, timestamp, result, and limits—plus a comparable baseline for
performance, capacity, reliability, or cost claims. Run the smallest authorized
discriminating probe when static authority cannot resolve applicability;
otherwise carry the exact proof gap.

## Conflict Or Freshness

For a material authority conflict or freshness-sensitive one-way-door choice,
obtain an independent challenge before downstream use and record `valid as of`
plus an objective refresh trigger. Unresolved authority conflicts block or
return to their evidence owner.

## Downstream Input Closure

Record the input owner, authoritative source, required shape, availability, and
earliest checkpoint—or for a proof implication, the observable surface, setup
availability, and missing-proof owner.
