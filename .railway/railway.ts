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
        "crates/**", "migrations/**", ".sqlx/**", "vendor/**",
        "test/Cargo.toml", "test/src/**", "test/tests/**", "test/fixtures/**",
        "env/config/**", "build/docker/**", ".dockerignore",
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
    env: {
      // The Dockerfile's unit-test gate. Railway passes a service variable to
      // the ARG of the same name; at run time nothing reads it.
      IMAGE_TESTS: "unit",
      // Only `local` and `development` admit the developer escape hatches.
      APP__APP__ENV: ctx.environment ?? "production",
      // Sized by the operator to each replica's CPU allocation.
      APP__RUNTIME__WORKER_THREADS: preserve(),
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
