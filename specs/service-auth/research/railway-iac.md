# Railway Infrastructure as Code

Read 2026-10-17 from docs.railway.com (`infrastructure-as-code`, its
reference, `cli/config`, `variables`, `builds/dockerfiles`, `environments`),
the CLI source (`railwayapp/cli` `src/iac/eval.rs`, CLI 5.70.0) and the SDK
package `railway` 3.16.0 (`dist/index-*.d.ts`). Evidence for
[B9](../spec.md#b9-railway-iac-and-build).

- Railway Config as Code (`railway.toml`/`railway.json`) stops being read on
  2026-12-01; new services cannot opt into it. IaC is `.railway/railway.ts`
  (also `.py`, `.go`), evaluated by the CLI and applied explicitly with
  `railway config plan|apply`; Railway does not read `.railway/` during a
  deploy.
- Evaluation: the CLI runs `node --experimental-strip-types --input-type=module`
  (Node ≥ 22.6; the SDK declares `node >=22`) and imports the file; the file's
  own `import "railway/iac"` resolves from the nearest `node_modules` above it,
  so `.railway/package.json` with `npm ci --prefix .railway` serves it. The
  SDK is not bundled with the CLI. Only erasable TypeScript syntax works.
- `service(name, config)`: `source: github("owner/repo", { branch })`,
  `build: { builder: "DOCKERFILE", dockerfilePath, watchPatterns }`,
  `deploy: { healthcheckPath, healthcheckTimeout, restartPolicyType,
  restartPolicyMaxRetries, overlapSeconds, drainingSeconds, region, … }`,
  `env: { NAME: value | ctx.shared.NAME | preserve() }`. `ctx.environment`
  and `ctx.projectName` come from the linked project.
- Variables: a service with `env` gets every undeclared remote variable
  planned for deletion (CLI ≥ 5.64.0); `preserve()` keeps a value set out of
  band and is never written; `ctx.shared.NAME` compiles to `${{shared.NAME}}`
  and does not manage the shared variable. Destructive changes need
  confirmation (`--confirm-destructive` when non-interactive).
- `export const partial = "<name>"` lets each repository own its slice of a
  multi-repository project; once one file in an environment names a partial,
  every file must.
- Environments: a plan or apply targets one environment; PR environments are
  a project setting outside IaC and are off unless enabled.
- Dockerfile builds receive service variables only through an `ARG` of the
  same name; a sealed variable still reaches such an `ARG`. Railway's
  Dockerfile builds have no secret mounts.
