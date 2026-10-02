# Intent: close out outbound OAuth reliability and evidence gaps

Status: ready

## Problem

The user requested an analysis of `infra-oauth2-client-credentials` and related integration, then authorized fixing the necessary issues completely and opening one separate pull request. The profile's current failure, reuse and resource-retention behavior and its library rationale need a coherent closeout.

## Desired outcome

A focused, reviewed repair of the existing outbound OAuth profile, with behavior, tests, operational guidance and dependency rationale in agreement, delivered in one separate PR. Complete the scoped findings rather than leaving known defects for a follow-up.

## Affected actors and systems

Derived-service developers, HTTP and gRPC callers using `Credentials`, the configured authorization server, downstream resource servers, and operators interpreting acquisition failures and cache limits.

## Scope and non-goals

Cover the OAuth adapter, its existing HTTP/gRPC bindings, relevant configuration/profile integration, regression proof and documentation. Assess existing libraries fairly before choosing retained or replacement mechanisms. No authorization-server migration, inbound authentication redesign, DPoP rollout, platform migration, merge or deployment.

## Constraints

Preserve private_key_jwt, the two supported grants, tenant/subject isolation, fixed egress, sensitive-data redaction, caller deadlines and absence of automatic resource replay. Preserve unrelated work. Code/docs/tests repair, non-destructive validation, branch push and one separate PR are authorized; merge and deployment are not.

Assumption: existing supported providers and the public composition API remain the compatibility baseline. Reopen if completing a necessary fix would require a provider/platform migration or a new user-visible capability.

## Success signal

The selected behavior is implemented and independently reviewed; relevant local validation passes; changed CI-owned surfaces have an exact-candidate result or an explicitly pending result; a separate PR describes the changes and their limits without claiming deployment proof.
