// Railway infrastructure as code for this service. `npm ci --prefix .railway`
// installs the SDK pinned in package.json; `railway config plan` and
// `railway config apply` from the linked checkout evaluate this file against
// the linked project and environment. Railway does not read it during a
// deploy, and an apply changes the linked project: review its plan first.
//
// A declared `env` plans the deletion of every variable it does not name, so
// declare or `preserve()` each variable the service reads before an apply
// (docs/railway-deployment-profile.md).
import { defineRailway, github, preserve, project, service } from "railway/iac";

// This repository's slice of a project that several repositories share.
const name = "service";
export const partial = name;

export default defineRailway((ctx) => {
  const web = service(name, {
    source: github("Dankosik/rust-service-template-rest"),
    build: {
      builder: "DOCKERFILE",
      dockerfilePath: "build/docker/Dockerfile",
      watchPatterns: [
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "LICENSE", "deny.toml",
        "crates/**", "migrations/**", ".sqlx/**", "vendor/**",
        "test/Cargo.toml", "test/src/**", "test/tests/**", "test/fixtures/**",
        "env/config/**", "build/docker/**", ".dockerignore",
      ],
    },
    deploy: {
      healthcheckPath: "/health/ready",
      healthcheckTimeout: 180,
      restartPolicyType: "ON_FAILURE",
      restartPolicyMaxRetries: 5,
      overlapSeconds: 45,
      // The process may use all of `http.grace_period` (45 s) after SIGTERM;
      // the margin keeps its exit, and its exit code, ahead of SIGKILL.
      drainingSeconds: 50,
      // template:begin postgres:railway-postgres-migrate
      // Runs in a container of the new image before it starts; a failure
      // keeps the previous deployment serving. Railway sets no timeout of its
      // own; this one covers postgres.migration_deadline (5 min) plus connect.
      preDeployCommand: ["/migrate"],
      preDeployTimeoutSeconds: 360,
      // template:end postgres:railway-postgres-migrate
    },
    env: {
      // Railway health-checks the port named by PORT; the service reads only
      // http.addr. `[::]` also accepts IPv4, and private DNS in environments
      // created before 2025-10-16 resolves to IPv6 only.
      PORT: "8080",
      APP__HTTP__ADDR: "[::]:8080",
      APP__APP__ENV: ctx.environment ?? "production",
      // Unset, the runtime starts one worker per vCPU of the plan limit (24)
      // and holds about twice the memory after a burst. Raise this to the
      // replica's CPU allocation.
      APP__RUNTIME__WORKER_THREADS: "2",
      // template:begin postgres:railway-postgres-env
      // The migrator refuses a disabled profile, and the DSN is set out of band.
      APP__POSTGRES__ENABLED: "true",
      APP__POSTGRES__DSN: preserve(),
      APP__POSTGRES__MIGRATION_DSN: preserve(),
      // template:end postgres:railway-postgres-env
      // template:begin jobs:railway-postgres-notifications
      APP__POSTGRES__NOTIFICATIONS: preserve(),
      // template:end jobs:railway-postgres-notifications
      // template:begin service-auth:railway-service-auth-env
      // The token type and the trust registry are one value for every service
      // of the environment, kept as shared variables.
      APP__SERVICE_AUTH__MODE: "jwt",
      APP__SERVICE_AUTH__NAME: name,
      APP__SERVICE_AUTH__JWT_TYPE: ctx.shared.SERVICE_AUTH_JWT_TYPE,
      APP__SERVICE_AUTH__TRUST: ctx.shared.SERVICE_AUTH_TRUST,
      // Set out of band, the key sealed; a service that calls no other drops both.
      APP__SERVICE_AUTH__KEY_ID: preserve(),
      APP__SERVICE_AUTH__PRIVATE_KEY: preserve(),
      // template:end service-auth:railway-service-auth-env
    },
  });
  return project(ctx.projectName ?? name, { resources: [web] });
});
