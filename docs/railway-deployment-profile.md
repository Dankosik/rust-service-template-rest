# Railway Deployment Profile

This template ships deployment policy for derived services as IaC in
[`.railway/railway.ts`](../.railway/railway.ts), without connecting to any live
Railway project, environment, domain, or secret. `.railway/railway.ts`,
`build/docker/Dockerfile`, `.dockerignore`, and `crates/config` own exact ports,
timeouts, watch patterns, and environment variable names; this document records
only Railway platform behavior, operator obligations, and rollback rules.

## Infrastructure as code

Railway deprecated Config as Code (`railway.toml` / `railway.json`) in favor of
TypeScript IaC evaluated locally via `npm ci --prefix .railway` and applied with
`railway config plan` / `railway config apply` (Railway never reads `.railway/`
automatically during deploys):

- `export const partial` scopes IaC to this repository's service slice in a
  shared project; once one service uses partials in an environment, all must.
- Declaring `env` makes `.railway/railway.ts` authoritative: `apply` deletes any
  service variable omitted from `env` unless wrapped in `preserve()`. Review
  `railway config plan` before applying.
- `APP__HTTP__ADDR` binds `[::]:8080`, so one dual-stack listener serves both
  IPv4 and legacy IPv6-only Railway private DNS. The service never reads
  `PORT`; it is set to the same port only for Railway's health check.
<!-- template:begin service-auth:docs-railway-service-auth -->
- With `SERVICE_AUTH=jwt`, create environment-level shared variables for token
  type and trust registry before the first deploy, and seal per-service signing
  key variables out of band (`preserve()`).
<!-- template:end service-auth:docs-railway-service-auth -->
<!-- template:begin postgres:docs-railway-postgres-migrate -->
- With PostgreSQL retained, `/migrate` runs as `preDeployCommand` in the new
  image before cutover while the old deployment keeps serving. Because Railway
  sets no default pre-deploy timeout, keep `preDeployTimeoutSeconds` in
  `.railway/railway.ts` above `postgres.migration_deadline` plus connection
  overhead, and author expand-then-contract migrations so the old binary
  tolerates the new schema during build-to-cutover overlap.
<!-- template:end postgres:docs-railway-postgres-migrate -->

`make docs-check` (`scripts/ci/image-inputs-check.py`) verifies that
`build.watchPatterns` in `.railway/railway.ts` covers `Dockerfile`,
`.dockerignore`, and `rust-toolchain.toml` inputs so docs/CI/agent edits do not
trigger redeployments.

## Delivery paths

Choose one delivery path per derived service and keep its provenance distinct:

- **Railway source build:** Railway builds `build/docker/Dockerfile` directly
  and injects `RAILWAY_GIT_COMMIT_SHA` (winning over `VCS_REF` as `app.commit`).
  GHCR signatures from `cd.yml` do not cover source-built Railway images.
- **Railway CLI upload (`railway up <dir> --path-as-root`):** Requires explicit
  [Authority](../AGENTS.md#authority); pass the commit SHA into `VCS_REF` since
  Railway sets no `RAILWAY_GIT_COMMIT_SHA` on uploads ([Build
  Speed](build-speed.md#deployment-builds)).
- **Published GHCR image:** Deploy the verified `image@sha256:…` digest produced
  by `cd.yml` ([CI/CD Production Readiness](ci-cd-production-ready.md)).

## The build

Builds hold no credentials (`make dockerfile-check` refuses secret-named `ARG` /
`ENV` declarations; vendor private crates via `vendor-private/` as described in
[Build Speed](build-speed.md#deployment-builds)). `ARG IMAGE_TESTS` defaults to
`none` when CI runs tests; CI-less repositories (`REPOSITORY_CI = none`) must
set `IMAGE_TESTS=unit` in service variables and `.railway/railway.ts`. Keep
Railway "Wait for CI" off unless promotion must wait for post-merge workflows.

## Rollout

Railway routes traffic once `/health/ready` returns `2xx` and **never re-polls
readiness afterward**, signalling `SIGTERM` after `overlapSeconds` and `SIGKILL`
after `drainingSeconds`. On Railway, `overlapSeconds` (not
`http.readiness_propagation_delay`) is what prevents cutover drops; the
propagation delay remains for platforms that continuously poll `/health/ready`.

Keep `deploy.drainingSeconds` in `.railway/railway.ts` (and container/orchestrator
stop grace elsewhere) at or above `http.grace_period` validated by
`crates/service/src/bootstrap`, rerunning `ALLOW_HEAVY=1 make runtime-image-check`
when lifecycle budgets move.

## Operator-owned configuration

- **Set `APP__RUNTIME__WORKER_THREADS` explicitly** (e.g., `4` sized to actual
  CPU quota): unthrottled Railway plans expose 24+ host vCPUs (`crates/config`
  ignores `TOKIO_WORKER_THREADS` and reads cgroup parallelism when unset),
  wasting idle threads and RSS on low-CPU services.
- **Isolate diagnostics:** Keep the separate metrics listener unreachable from
  public networks (override its bind address to `[::]:<port>` if scraping over
  IPv6 private networking).

## Rollback

- **Source deployments:** Use [Railway rollback](https://docs.railway.com/deployments/deployment-actions)
  within image retention rather than Redeploy (which triggers a fresh build with
  mutable upstream inputs).
- **Image deployments:** Roll back by verified immutable `sha256` digest, never
  mutable tags, paired with compatible configuration (older binaries reject
  unknown config keys).
- **State compatibility:** Confirm schema and job/event payload compatibility
  under [Production Contract](production-contract.md#operation-and-recovery)
  before rolling back, then verify `/health/ready`, `app.version`, and
  `app.commit` in `service_starting`.
