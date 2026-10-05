# Intent: Tokio progress under ordinary application work

Status: ready

## Problem

The template uses Tokio for concurrent requests. Synchronous work performed
inside a task can keep other requests from progressing, even when the surrounding
function is async. The requester first asked for a read-only audit, then
authorized the recommendations judged necessary to fix in a separate PR.

## Desired outcome

Remove concrete template-owned progress hazards and make the rules for adding
blocking or CPU-heavy business work explicit, so ordinary application code does
not silently stall unrelated requests.

## Affected actors and systems

Service authors, request handlers, optional object-storage uploads, the shared
logger and its service/jobs-worker/migration consumers, and operators observing
overload and process shutdown.

## Scope and non-goals

Fix necessary findings in this Rust template, with scoped local changes,
validation, a pushed branch and a separate PR. Do not merge or deploy. Do not
add a speculative CPU executor, retune worker counts from the development host,
or move every serialization/cryptography operation without evidence.

## Constraints

Preserve existing request, object-storage integrity, logging-content and process
lifecycle contracts except for the explicitly accepted progress delta. Preserve
unrelated work. Technical mechanism and implementation choices belong to the
agent; no additional requester decision is currently missing.

## Success signal

Concrete ready-input and stalled-output hazards have bounded behavior and
focused regression evidence; contributor guidance makes admission, cancellation,
actual execution completion and shutdown ownership unambiguous. The assembled
candidate passes the repository's applicable checks and review and is published
as a separate PR with its exact-head CI result reported.
