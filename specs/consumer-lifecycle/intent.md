# Intent: A derived service can release, upgrade, recover, and validate efficiently

Status: ready

## Problem

The template has extensive source, generated-profile, runtime, and CI proof,
but that does not establish the complete operating life of a real derived
repository. Runtime updates do not yet have a supported consumer-preserving
path, recovery obligations remain largely instructions, and the image gate in
the initializer-admission candidate takes about 41 minutes.

## Desired outcome

The user accepted all four improvements:

1. Complete a first real derived-service release cycle: clone, initialize,
   build, publish, verify signatures and SBOM, run the published digest, and
   roll back to a previously verified digest.
2. Provide a supported runtime upgrade path that preserves the service's
   business code and local decisions, with a reviewable diff.
3. Establish practical mixed-version and recovery evidence for the
   template-owned PostgreSQL, jobs, and JetStream mechanisms, using existing
   harnesses and native backup tools. RPO, RTO, and reconciliation policy
   belong to the service.
4. Measure and reduce the native image CI critical path while preserving
   release proof and avoiding a profile × harness × infrastructure matrix.

## Affected actors and systems

Template maintainers, derived-service maintainers, release operators and
reviewers; the initializer and portable instruction sync; service source,
locked dependencies and generated contracts; native GitHub CI/GHCR publication;
and the existing PostgreSQL/jobs/JetStream integration and lifecycle surfaces.

## Scope and non-goals

This outcome covers reusable template support plus one real non-production
consumer rehearsal. It preserves all four outcomes until their actual proof
exists. Local development, generated-tree validation, CI, registry publication,
and observed runtime/recovery are separate completion claims.

Assumption A1: the first consumer is a deliberately named non-production
demonstrator containing synthetic data, because no business service or
production target was identified. A named existing consumer or production
expectation reopens this assumption before dependent work.

This is not a replacement queue, migration framework, deployment platform,
backup product, or business feature. It does not change the established refusal
to migrate an initialized service between profiles, broaden portable-sync
ownership, or duplicate unrelated open runtime fixes. The separate PR250
documentation correction is outside this task.

## Constraints

Use existing Git, Cargo, image/publication, integration, and provider-native
capabilities before adding a carrier. Preserve consumer edits and service-owned
policy. Keep immutable source, generated output, image, and runtime identities
distinct. Do not weaken release checks or accept a skipped selected gate.

No numeric CI improvement, RPO, or RTO was supplied. No production-data scope,
remote consumer repository/visibility, hosting target, or spending envelope
was supplied. The agent owns technical preparation and must make the proposed
external operation concrete before the coordinator requests missing authority.
That boundary does not block independent local implementation.

## Success signal

A maintainer can initialize a real consumer, review and accept a later runtime
upgrade without losing its own work, publish and verify a particular image,
run it by digest, and recover to an admitted prior version with the applicable
durable-state evidence. The delivery record identifies the exercised service,
versions, environment and limits. Comparable native CI evidence shows the
critical-path improvement and its runner/cache cost with the same required
proof. Any outstanding external action remains explicit rather than counted
as completed.
