# Intent: caller-signed service tokens, no-CI verification, Railway IaC

Source: derived services that run on Railway without an authorization server
and without GitHub CI. The template owner approved the service-to-service
rule amendment on 2026-10-13: a caller-signed short JWT with a key pinned to
that caller, and platform tokens, alongside the authorization-server path, as
a new profile `service_auth` separate from `authn`. A derived repository may
run without GitHub CI, with Railway building and deploying it.

## Problem

Derived Rust services that call each other over gRPC have no caller
verification unless an authorization server exists: any process in the
environment can call any method. The template offers service-to-service
authentication only through an authorization server, which such a system
does not have. Repositories created without GitHub CI also lose every gate
the template delegates to CI, and the template ships no Railway
Infrastructure-as-Code file.

## Desired outcome

1. Profile `SERVICE_AUTH` (`none` or `jwt`), independent of `AUTHN`. With
   `jwt` every gRPC business call carries a short ES256 JWT that the caller
   signs with its own key: `typ` names the trust domain's service-token
   type (for example `example-svc+jwt`), `iss` and `sub` are the caller's
   name, exactly one audience, at most 60 seconds. The receiver looks the key
   up only among that caller's keys, admits callers per method from a table in
   its own code with deny by default, refuses to start when a method has no
   rule, and hands the verified caller name to the handler. The token never
   reaches logs or traces. Outbound calls get a token source. On Kubernetes
   the same contract is later carried by service-account tokens, and only two
   adapters change: where a caller gets its token and where the receiver gets
   keys.
2. A no-CI mode in which `make verify` runs every step the template now
   delegates to CI (real PostgreSQL, migration rehearsal, image checks), and
   whose receipt is never `partially_verified`.
3. `.railway/railway.ts` and a Railway build that needs no secret, following
   the rules of the open template PR #274; the build runs the fast tests that
   need no database.

## Affected actors and systems

Derived services on Railway now and on Kubernetes later, their operators, the
agents that verify before pushing, Go and TypeScript callers that will sign
the same token, the template initializer, sync and CI.

## Scope and non-goals

- In scope: the profile, its configuration, gRPC enforcement, the outbound
  token source for gRPC and HTTP clients, initializer support, tests, docs,
  the no-CI verify mode, the Railway IaC file and secret-free build.
- Non-goals: the production trust registry, the key rotation script and the
  key-age warning, which belong to each derived system; a Go token package;
  Kubernetes adapters; accepting service tokens on HTTP operations; merging
  PR #274, whose rules are carried over instead.

## Constraints

- Follow the template's AGENTS.md: validation budget, `--locked`, accepted
  decision records, profile markers, inert defaults.
- Do not change behaviour of existing profiles (`AUTHN`, `OUTBOUND_AUTH`).
- The template is shared by unrelated organizations: no organization names in
  code; organization values are configuration.

## Success signal

Tests refuse a call without a token, with a foreign audience, with an expired
token, with another issuer's key, and to a method outside the table; a service
without a rule for a method does not start; the token does not appear in logs
or traces; the no-CI mode passes locally; a service initialized from the
template builds without secrets; the decision is recorded in the template's
decision records.
