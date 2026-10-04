# Intent: cache reliability closeout

Status: ready. Definition baseline: `67be869`, branch
`codex/cache-reliability-closeout-20261002`, 2026-10-02.

## Problem

The optional Redis/Valkey cache has confirmed recovery, background-lifetime,
password-rotation, and documentation gaps. A bounded caller timeout does not
by itself recover an established connection that stops answering. Background
connection work and rejected live re-authentication can outlive their useful
owner or recovery path; dependency diagnostics can bypass sanitization.

## Desired outcome

Completely fix the justified cache review findings and deliver one separate
pull request with appropriate local and CI evidence. Operators can retain the
optional cache without permanently stuck connections, abandoned background
work, or a password rotation that silently stops following the file.

## Affected actors and systems

Feature authors using cached bytes; service operators managing availability,
credentials and shutdown; `infra-cache`, service bootstrap, and cache adoption
and architecture documentation.

## Scope and non-goals

Close established-connection stalls, resource/task lifetime, password-file
re-authentication recovery, diagnostic sanitization, and cache guidance
inconsistencies. Preserve bytes-only get/set/delete, feature-owned
serialization/TTL/fallback, and the optional profile. No new cache capability,
Redis topology, product feature, benchmark project, merge, or deployment.

## Constraints

Keep resolved redis-rs 1.7.1 and Valkey. Reuse the existing configuration,
runtime budgets and proof infrastructure. Preserve unrelated changes. Local
edits and appropriate validation plus one separate PR are authorized; merge
and deployment are not. Mechanism and code ownership are Technical Design
decisions.

## Success signal

The repaired implementation demonstrates recovery from the confirmed failures,
bounded owned work, credential-error redaction, and consistent adoption
guidance. Required review and selected validation are closed for the PR;
local proof and CI results are reported separately.
