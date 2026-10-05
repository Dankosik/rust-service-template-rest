# Railway Deployment Profile

This repository is a service template. It carries the deployment policy a
derived service applies on Railway and the image tooling to build it, but it
never connects to a Railway project, service, environment, domain, or secret.

## Why there is no `railway.toml`

Railway deprecated Config as Code (`railway.toml` / `railway.json`) in favour
of Infrastructure as Code (`.railway/railway.ts`, the `railway` npm package):
new services cannot opt into Config as Code, and existing files stop being
read on 2026-12-01. IaC is evaluated by the Railway CLI against a linked
project and applied explicitly; Railway does not read `.railway/` during a
deploy. A template owns neither a `package.json` with the `railway`
dependency nor a linked project, so it ships the policy as values and a
copy-ready `service()` snippet instead. Initialization does not create a
Railway project, linked configuration or deployment input.

## Independent delivery paths

Choose one path per derived service and keep its evidence separate.

**Railway source build.** Connect the derived repository and branch; Railway
builds `build/docker/Dockerfile` from source. Railway passes no build
arguments, so every Dockerfile `ARG` has a default, and it does set
`RAILWAY_GIT_COMMIT_SHA`, which the Dockerfile bakes as `app.commit` (it wins
over `VCS_REF`). `app.version` is the Cargo package version. The GHCR image
published by `cd.yml` is a different build; its signature and attestations do
not prove what a Railway source build runs.

**Published image.** With `ENABLE_GHCR_PUBLISH=true` the CD workflow publishes
a signed, attested `image@sha256:…` reference ([CI/CD Production
Readiness](ci-cd-production-ready.md)). Deploy that digest after verifying it.

## Repository-owned policy

The values a derived service applies, and the IaC form that carries them:

| Setting | Value | Why |
| --- | --- | --- |
| `build.builder` | `DOCKERFILE` | the template's image is the deployment unit |
| `build.dockerfilePath` | `build/docker/Dockerfile` | |
| `build.watchPatterns` | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/**`, `migrations/**`, `.sqlx/**`, `vendor/**`, `test/Cargo.toml`, `test/src/**`, `test/tests/**`, `build/docker/**`, `.dockerignore`, `docs/railway-deployment-profile.md` | covers admitted image inputs and build controls; other documentation, unadmitted fixtures, CI, and agent-only changes do not start a deployment |
| `deploy.healthcheckPath` | `/health/ready` | the cached readiness verdict, not liveness |
| `deploy.healthcheckTimeout` | `180` | startup and dependency probes settle well inside it |
| `deploy.restartPolicyType` / `restartPolicyMaxRetries` | `ON_FAILURE` / `5` | a clean `SIGTERM` exit (`0`) is not restarted; exit `3` (degraded shutdown) and `1` are |
| `deploy.overlapSeconds` | `45` | the new deployment serves before the old one is stopped |
| `deploy.drainingSeconds` | `45` | see the budget below |

```ts
// .railway/railway.ts in a derived repository (`railway` npm package, IaC
// entrypoint; `railway config plan` / `railway config apply` from the linked
// directory). Project and service names, the GitHub source, and every
// secret are the derived service's own.
import { defineRailway, github, project, service } from "railway/iac";

export default defineRailway(() => {
  const web = service("service", {
    source: github("<owner>/<repo>"),
    build: {
      builder: "DOCKERFILE",
      dockerfilePath: "build/docker/Dockerfile",
      watchPatterns: [
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "crates/**", "migrations/**", ".sqlx/**", "vendor/**",
        "test/Cargo.toml", "test/src/**", "test/tests/**",
        "build/docker/**", ".dockerignore",
        "docs/railway-deployment-profile.md",
      ],
    },
    deploy: {
      healthcheckPath: "/health/ready",
      healthcheckTimeout: 180,
      restartPolicyType: "ON_FAILURE",
      restartPolicyMaxRetries: 5,
      overlapSeconds: 45,
      drainingSeconds: 45,
    },
  });
  return project("<project>", { resources: [web] });
});
```

The source build uses the repository root as its context. `make docs-check`
checks both watch forms against the Dockerfile and root `.dockerignore`
allowlist. Every admitted file or directory family requires coverage, including
new families before files exist; exclusions may reduce build inputs but do not
reduce this conservative watch set. `rust-toolchain.toml` is an additional
trigger coupled to the builder pin, although it stays outside the context.
The workspace test manifest, `test/src/` and `test/tests/` enter the context.
Cargo and cargo-chef must discover real targets after initialization can remove
the optional helper library and worker fixture binary. The retained utility
tests keep that workspace member valid; package/bin release builds do not compile
those test targets. Their source changes therefore conservatively trigger the
source build too.

Update both forms when admitting an input. The check refuses unsupported
inclusion/watch syntax, alternate build contexts and Dockerfile-specific ignore
overrides with the responsible path. Extend the coverage model before adopting
those forms. This checks policy data only; it does not apply IaC or verify live
Railway settings.

## Grace budget

The [runtime budget policy](configuration-source-policy.md) owns the
derivation: `http.drain_timeout` (`25s`, including the `15s` readiness
propagation delay) plus the `17s` teardown tail (diagnostics, background
join, dependency close, telemetry flush) is a `42s` worst case, and
`http.grace_period` (`45s`) is the platform window that must cover it. The
`45s` `drainingSeconds` leaves three seconds before `SIGKILL`. If a derived
service changes the application budget, rederive the platform grace from that
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

1. review the diff of this file and the Dockerfile for policy alignment;
2. take `make runtime-image-build` and `make runtime-image-check` from CI's
   `image` job (`make verify` leaves both to CI on an image change;
   `ALLOW_HEAVY=1` runs them locally) and confirm the clean stop inside 45 s;
3. leave project-specific settings and live deployment evidence to the
   derived service's operator.
