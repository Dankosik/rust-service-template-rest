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
- Leave `APP__APP__ALLOW_INSECURE_DEVELOPMENT` unset on Railway. Its default
  keeps hosted environments strict even when named `development`. Omitting
  the key also keeps the environment readable by older binaries during rollback.
  A binary that trusts the environment name alone needs a strict
  `APP__APP__ENV` in its effective rollback configuration. If rollback would
  restore a permissive label, deploy the verified older image with a strict
  label and verified TLS.
  For an existing local setup that relies on a development exception, set
  `APP__APP__ALLOW_INSECURE_DEVELOPMENT=true` alongside `APP__APP__ENV=local`
  or `development`. The shipped local TOML selects both.
- `APP__HTTP__ADDR` binds `[::]:8080`, so one dual-stack listener serves both
  IPv4 and legacy IPv6-only Railway private DNS. The service never reads
  `PORT`; it is set to the same port only for Railway's health check.
<!-- template:begin service-auth:docs-railway-service-auth -->
- With `SERVICE_AUTH=jwt`, create environment-level shared variables for token
  type and trust registry before the first deploy, and seal per-service signing
  key variables out of band (`preserve()`).
<!-- template:end service-auth:docs-railway-service-auth -->
<!-- template:begin postgres:docs-railway-postgres-migrate -->
- For a direct deployment, set `APP__POSTGRES__DSN` to the PostgreSQL URL and
  leave `APP__POSTGRES__MIGRATION_DSN` unset. The API and `/migrate` then
  share the configured endpoint and password source.
- To put the API behind PgBouncer in transaction mode, set the following
  variables on the Railway API service. Substitute the credentials and the
  certificate hostnames for your endpoints. Both URLs must reach the same
  writable database. The pre-deploy command holds a session-level advisory
  lock across migrations and needs the direct connection.

  | Railway variable | Example value |
  | --- | --- |
  | `APP__POSTGRES__DSN` | `postgres://api:<password>@pgbouncer.railway.internal:6432/app?sslmode=verify-full` |
  | `APP__POSTGRES__MIGRATION_DSN` | `postgres://migrator:<password>@postgres.railway.internal:5432/app?sslmode=verify-full` |

  Store the URLs as service secrets. Railway exposes both to the API service
  and its pre-deploy command. The migration override supplies its own password.
  An explicitly empty or invalid override stops the migration.
  When using separate database roles, grant the API role read access to
  `_sqlx_migrations` and the permissions its runtime queries need.
  Configure the pooler to forward the startup settings in the
  [PgBouncer example](../env/pgbouncer/pgbouncer.ini).
- Use a hostname covered by each endpoint's certificate and
  `sslmode=verify-full`. For a private CA, package its public trust anchor
  in the derived runtime image and set `sslrootcert` to its absolute path in the DSN. The
  pre-deploy migrator runs in a separate container without the service's
  mounted volumes. [Railway pre-deploy commands](https://docs.railway.com/deployments/pre-deploy-command)
- With PostgreSQL retained, `/migrate` runs as `preDeployCommand` in the new
  image before cutover while the old deployment keeps serving. Because Railway
  sets no default pre-deploy timeout, keep `preDeployTimeoutSeconds` in
  `.railway/railway.ts` above `postgres.migration_deadline` plus connection
  overhead, and author expand-then-contract migrations so the old binary
  tolerates the new schema during build-to-cutover overlap.
<!-- template:end postgres:docs-railway-postgres-migrate -->
<!-- template:begin jobs:docs-railway-postgres-notifications -->
- With transaction pooling, set `APP__POSTGRES__NOTIFICATIONS=false` on job
  producers and workers using that endpoint. Workers pick up jobs by polling.
<!-- template:end jobs:docs-railway-postgres-notifications -->

Keep `build.watchPatterns` in `.railway/railway.ts` aligned with `Dockerfile`,
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
set `IMAGE_TESTS=unit` in service variables and `.railway/railway.ts`. `ARG PGO`
defaults to `none`; `PGO=train`, set the same way, builds the service a second
time against the requests in `build/docker/pgo-train.sh`, trading build time for
CPU per request ([measurements](ci-cd-production-ready.md#profile-guided-build-and-compiler-flags)). Keep
Railway "Wait for CI" off unless promotion must wait for post-merge workflows.

## Rollout

Railway routes traffic once `/health/ready` returns `2xx` and **never re-polls
readiness afterward**, signalling `SIGTERM` after `overlapSeconds` and `SIGKILL`
after `drainingSeconds`. On Railway, `overlapSeconds` (not
`http.readiness_propagation_delay`) is what prevents cutover drops; the
propagation delay remains for platforms that continuously poll `/health/ready`.

Keep `deploy.drainingSeconds` in `.railway/railway.ts` (and container/orchestrator
stop grace elsewhere) at least 5 s above `http.grace_period` validated by
`crates/service/src/bootstrap`: a teardown that spends its whole budget exits at
the end of the grace period, and an equal platform limit can `SIGKILL` it first,
replacing exit code `3` with `137`. Rerun `make image-build && make image-check`
when lifecycle budgets move.

## Operator-owned configuration

- Keep inbound trace trust disabled on public listeners. Enable
  `APP__HTTP__TRUST_TRACE_CONTEXT` only on a listener reachable exclusively by
  trusted peers, or behind a gateway that starts a fresh trace for every
  untrusted request. The gateway must strip trace response headers before
  forwarding responses to public clients.
<!-- template:begin grpc:railway-grpc-trace-trust -->
- Enable `APP__GRPC__TRUST_TRACE_CONTEXT` only when the gRPC listener has the
  same trusted network boundary.
<!-- template:end grpc:railway-grpc-trace-trust -->
- **Size `APP__RUNTIME__WORKER_THREADS` to the replica's CPU allocation:**
  `.railway/railway.ts` sets `2`. Unset, `crates/config` reads cgroup
  parallelism (it ignores `TOKIO_WORKER_THREADS`), and an unthrottled Railway
  plan exposes 24 vCPUs. On two CPUs, 24 workers kept 65.4 MB resident 90 s
  after a 20 s burst of 64 KiB JSON against 31.1 MB for two workers, with
  1.2 MB between them before any load (DigitalOcean c-4, host THP `madvise`).
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
<!-- template:begin postgres:docs-railway-postgres-rollback -->
- When rolling back to a binary that predates the migration override, restore
  the direct PostgreSQL URL in `APP__POSTGRES__DSN` and remove
  `APP__POSTGRES__MIGRATION_DSN` in the same configuration change.
<!-- template:end postgres:docs-railway-postgres-rollback -->
- **State compatibility:** Confirm schema and job/event payload compatibility
  under [Production Contract](production-contract.md#operation-and-recovery)
  before rolling back, then verify `/health/ready`, `app.version`, and
  `app.commit` in `service_starting`.
