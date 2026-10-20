# Railway Deployment Profile

This repository is a service template. It carries the deployment policy a
derived service applies on Railway, as values and as
[`.railway/railway.ts`](../.railway/railway.ts), and the image tooling to build
it, but it never connects to a Railway project, service, environment, domain,
or secret.

## Infrastructure as code

Railway deprecated Config as Code (`railway.toml` / `railway.json`) in favour
of Infrastructure as Code: new services cannot opt into Config as Code, and
existing files stop being read on 2026-12-01. The Railway CLI evaluates
`.railway/railway.ts` with Node.js 22.6 or newer and the `railway` SDK pinned
in `.railway/package.json` and its lockfile (`npm ci --prefix .railway`), then
plans and applies it against the linked project and environment with
`railway config plan` and `railway config apply`. Railway does not read
`.railway/` during a deploy, and initialization creates no project, link or
deployment. After initialization the file is the service's own: the
initializer writes the service name and repository into it.

- `export const partial` names this repository's slice, so several
  repositories can each own their services in one project; once one file in an
  environment names a partial, every file must.
- The project name comes from the linked context (`ctx.projectName`), and
  `APP__APP__ENV` takes the environment's name (`ctx.environment`), so one
  file serves every environment. Only `local` and `development` admit the
  developer escape hatches; name a Railway environment accordingly.
- `env` makes the file authoritative for the service's variables: a plan
  deletes every variable the file does not name. Before the first apply,
  declare each variable the service reads, or keep a value set out of band with
  `preserve()`, as the file does for `APP__RUNTIME__WORKER_THREADS`, and review
  the plan's destructive changes.
<!-- template:begin service-auth:docs-railway-service-auth -->
- With `SERVICE_AUTH=jwt` the file sets `APP__SERVICE_AUTH__MODE` and the
  service's caller name, takes the token type and the trust registry from the
  shared variables `SERVICE_AUTH_JWT_TYPE` and `SERVICE_AUTH_TRUST`, one value
  for every service of the environment, and preserves the key id and the
  signing key, which an operator sets per service and seals. A shared
  reference does not create the shared variable; set it before the first
  deploy. [Service authentication](service-authentication.md) owns the values.
<!-- template:end service-auth:docs-railway-service-auth -->

## Independent delivery paths

Choose one path per derived service and keep its evidence separate.

**Railway source build.** Connect the derived repository and branch; Railway
builds `build/docker/Dockerfile` from source. Railway passes a service variable
only to a Dockerfile `ARG` of the same name, so every `ARG` has a default, and
it sets `RAILWAY_GIT_COMMIT_SHA`, which the Dockerfile bakes as `app.commit`
(it wins over `VCS_REF`). `app.version` is the Cargo package version. The GHCR
image published by `cd.yml` is a different build; its signature and
attestations do not prove what a Railway source build runs.

**Railway CLI upload.** `railway up <dir> --path-as-root` builds
`build/docker/Dockerfile` from a prepared directory on Railway's builders and
deploys the result, so it needs [Authority](../AGENTS.md#authority), and the
build holds no credential ([The build](#the-build)). [Build
Speed](build-speed.md#deployment-builds) owns its commit identity, skip marker
and private dependencies.

**Published image.** With `ENABLE_GHCR_PUBLISH=true` the CD workflow publishes
a signed, attested `image@sha256:…` reference ([CI/CD Production
Readiness](ci-cd-production-ready.md)). Deploy that digest after verifying it.

## Repository-owned policy

The values a derived service applies, and the IaC form that carries them:

| Setting | Value | Why |
| --- | --- | --- |
| `build.builder` | `DOCKERFILE` | the template's image is the deployment unit |
| `build.dockerfilePath` | `build/docker/Dockerfile` | |
| `build.watchPatterns` | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/**`, `migrations/**`, `.sqlx/**`, `vendor/**`, `test/Cargo.toml`, `test/src/**`, `test/tests/**`, `test/fixtures/**`, `env/config/**`, `build/docker/**`, `.dockerignore`, `docs/railway-deployment-profile.md` | covers admitted image inputs and build controls; other documentation, CI, and agent-only changes do not start a deployment |
| `deploy.healthcheckPath` | `/health/ready` | the cached readiness verdict, not liveness |
| `deploy.healthcheckTimeout` | `180` | startup and dependency probes settle well inside it |
| `deploy.restartPolicyType` / `restartPolicyMaxRetries` | `ON_FAILURE` / `5` | a clean `SIGTERM` exit (`0`) is not restarted; exit `3` (degraded shutdown) and `1` are |
| `deploy.overlapSeconds` | `45` | the new deployment serves before the old one is stopped |
| `deploy.drainingSeconds` | `45` | see the budget below |

`IMAGE_TESTS` is `unit` in the IaC file; see [the build](#the-build).

The source build uses the repository root as its context. `make docs-check`
checks the table and `.railway/railway.ts` against the Dockerfile and root
`.dockerignore` allowlist; a repository initialized before the file existed
keeps a `ts` snippet in this document, which the check reads instead and
names. Every admitted file or directory family requires coverage, including
new families before files exist; exclusions may reduce build inputs but do not
reduce this conservative watch set. `rust-toolchain.toml` is an additional
trigger coupled to the builder pin, although it stays outside the context.
The workspace test manifest, `test/src/` and `test/tests/` enter the context.
Cargo and cargo-chef must discover real targets after initialization can remove
the optional helper library and worker fixture binary. The retained utility
tests keep that workspace member valid; package/bin release builds do not compile
those test targets. Their source changes therefore conservatively trigger the
source build too.

Update the table and the IaC file when admitting an input. The check refuses unsupported
inclusion/watch syntax, alternate build contexts and Dockerfile-specific ignore
overrides with the responsible path. Extend the coverage model before adopting
those forms. This checks policy data only; it does not apply IaC or verify live
Railway settings.

## The build

The image build holds no credential. BuildKit prints the value of an `ARG`
that a `RUN` line references in the step name the build log keeps, and
Railway's Dockerfile builds have no secret mounts, so no `ARG` may carry a
secret; `make dockerfile-check` runs BuildKit's checks, which refuse a
secret-named `ARG` or `ENV`. A private dependency enters as its locked release
instead of a fetch with a token, from `vendor-private/` with its source in
`.cargo/config.toml`; [Build Speed](build-speed.md#deployment-builds) owns the
steps, including both watch-pattern forms above. Public crates still come from
crates.io under the `Cargo.lock` checksums.

With the build argument `IMAGE_TESTS=unit` the build runs the workspace's
unit tests (`cargo test --workspace --lib --bins`) in a debug stage of their
own that the runtime stage depends on: a failing test fails the build, and
Railway keeps serving the previous deployment. They need no database, Docker
or network beyond loopback; integration suites and process tests stay in
`make test`, the database proof and CI. The default, `none`, skips the work,
and any other value fails the build. The first test build compiles the
dependency graph a second time, in the debug profile; later builds reuse that
layer. `IMAGE_TESTS=unit ALLOW_HEAVY=1 make runtime-image-build` runs the same
gate locally.

## Grace budget

The [runtime budget policy](configuration-source-policy.md) owns the
derivation: `http.drain_timeout` (`25s`, including the `15s` readiness
propagation delay) plus the `18.5s` teardown tail (diagnostics, background
join, dependency close, shared trace/logger cleanup, 0.5 s SDK join slack, and 1 s
runtime shutdown) requires `43.5s`, and `http.grace_period` (`45s`) is the platform window that must cover it. The
`45s` `drainingSeconds` leaves 1.5 seconds beyond that bound before `SIGKILL`.
If a derived service changes the application budget, rederive the platform grace from that
owner and rerun `make runtime-image-check`, which sends `docker stop --time
45` and requires exit `0`. Other platforms need the equivalent explicit stop
grace (`docker stop --time 45`, Compose `stop_grace_period: 45s`, Kubernetes
`terminationGracePeriodSeconds: 50`); a shorter grace can `SIGKILL` the
service mid-drain.

## Operator-owned configuration

Set `APP__RUNTIME__WORKER_THREADS` explicitly for each process, for example
`4`, sized to the replica's CPU allocation and workload. Railway containers
can expose every host core; one Tokio worker per visible core adds idle
thread and memory overhead. The typed [runtime setting](configuration-source-policy.md#runtime)
uses cgroup-aware available parallelism when unset and always sets the count
on the runtime builder, so `TOKIO_WORKER_THREADS` is not read. Check the
effective `runtime.worker_threads` in the process's startup record.

The template deliberately does not choose: the Railway project, environment,
service, branch, domain, or region; secrets and connection references;
replica count, CPU, memory, autoscaling, or spend; private reachability for
the separate metrics listener, whose shipped `:9090` address binds every
interface; alerting or an external uptime check; release approval, rollback
authority, and retention.

For source builds, enable Railway "Wait for CI" when promotion must wait for
the repository's push checks. Treat the deployment healthcheck as a
startup and promotion gate, not continuous monitoring.

## Rollback

For source deployment, retain the successful Railway deployment identity and
source revision, and verify that its image is still within the provider's
retention window. [Railway rollback](https://docs.railway.com/deployments/deployment-actions)
restores that retained image and its custom variables. Rebuilding old source
through Redeploy is a new build, with the [mutable input limits](ci-cd-production-ready.md#runtime-image)
that implies; it is not proof of the previously accepted artifact.

For image deployment, retain and verify the accepted immutable digest as the
primary rollback identity. A mutable tag or old source commit cannot replace it.
Pair either artifact with compatible configuration, including overlays, external
references and secret custody; [unknown configuration fields](configuration-source-policy.md#source-of-truth)
can make an older binary refuse startup.

Before rollback, establish the service's schema/data and event/job payload
compatibility window under its [Production Contract](production-contract.md#operation-and-recovery).
Newer successful migration history does not prove old SQL compatibility.
After rollback, verify readiness, `app.version` and `app.commit` in the
`service_starting` record and the service's user path. Those observations do not
establish mixed-version compatibility or restore/reconciliation success.

## Change proof for this profile

1. review the diff of this file, `.railway/railway.ts` and the Dockerfile for
   policy alignment, and run `make docs-check`;
2. evaluate the IaC file with the pinned SDK (`npm ci --prefix .railway`,
   then `railway config plan` from the linked checkout) and read the plan's
   variable changes before any apply;
3. take `make runtime-image-build` and `make runtime-image-check` from CI's
   `image` job (`make verify` leaves both to CI on an image change;
   `ALLOW_HEAVY=1` runs them locally) and confirm the clean stop inside 45 s;
4. leave project-specific settings and live deployment evidence to the
   derived service's operator.
