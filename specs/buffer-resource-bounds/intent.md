# Intent: bounded buffer and resource ownership

Status: ready. Definition owner: `/root/definition`.

## Problem

The preceding buffer/streaming audit identified oversized temporary allocations,
resource retention after the apparent end of a call, and dependency queues whose
limits are not clear from the template's public contract.

## Desired outcome

Implement every audit recommendation actually necessary for this template in one
separate PR. Fix concrete adapter defects, configure adequate existing mechanisms,
and explain feature-owned limits without introducing a general memory manager.

## Affected actors and systems

Template adopters and operators; callers of HTTP/gRPC, object storage, cache,
messaging and jobs; feature authors retaining byte buffers or decoded values.

## Scope and non-goals

The Rust template and its documentation, including relevant existing proofs.
No new business routes, global memory accounting, blanket byte copies, dependency
upgrade, compression enablement, merge, deployment or infrastructure changes.

## Constraints

Preserve framing, error identity and fields, checksum validation, cancellation,
and ambiguous mutation outcomes. Use resolved dependencies and supported extension
points first. Preserve unrelated work. Commit, push and open one separate PR are
authorized; production effects are not. Phase actors stop at their reviewed boundary.

## Success signal

The separate PR fixes the necessary defects, gives each remaining recommendation
an explicit grounded disposition, and carries the repository-required local and CI
proof at its actual scope. A payload/count limit must not be described as an RSS
ceiling or as ownership of a returned value after its admission slot is released.
