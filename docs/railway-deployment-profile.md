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
copy-ready `service()` snippet instead. The initializer stage may generate
`.railway/` once it knows the service identity.

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
| `build.watchPatterns` | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/**`, `build/docker/**`, `.dockerignore`, `docs/railway-deployment-profile.md` | documentation, tests, CI, and agent-only changes do not start a deployment; add a path here in the same pull request that makes it affect the image |
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
        "crates/**", "build/docker/**", ".dockerignore",
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

## Grace budget

The [runtime budget policy](configuration-source-policy.md) owns the
derivation: `http.shutdown_timeout` (`25s`, including the `15s` readiness
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

For a source build, select a previously successful Railway deployment and
verify its source commit. For an image deployment, restore the previously
accepted immutable digest, not a mutable tag. After any rollback, verify
readiness, `app.version` and `app.commit` in the `service_starting` record,
and the user path the derived service owns.

## Change proof for this profile

1. review the diff of this file and the Dockerfile for policy alignment;
2. run `ALLOW_HEAVY=1 make runtime-image-build` and
   `ALLOW_HEAVY=1 make runtime-image-check` (`make verify` selects both on an
   image change) and confirm the clean stop inside 45 s;
3. leave project-specific settings and live deployment evidence to the
   derived service's operator.
