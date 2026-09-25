# Container Validation

Use only for a matching explicit verification requirement or a bounded
diagnostic. A changed file or an available Docker daemon does not create a
local image gate; `ALLOW_HEAVY=1` is the deliberate opt-in.

Build with `ALLOW_HEAVY=1 make runtime-image-build`; the image tag comes from
`make/service.mk`.
The script passes the manifest tool versions, `HEAD` as `VCS_REF`, the crate
version, and the commit time as `SOURCE_DATE_EPOCH`; `RUNTIME_IMAGE_CACHE_FROM`
and `RUNTIME_IMAGE_CACHE_TO` accept buildx cache specs. A successful build is
not a runtime observation.

Prove the lifecycle with the same tag when it is required:

```bash
ALLOW_HEAVY=1 make runtime-image-check RUNTIME_EXPECTED_COMMIT="$(git rev-parse HEAD)"
```

The check starts the container `--read-only --cap-drop=ALL
--security-opt=no-new-privileges`, polls `/health/ready` from the host
(distroless has no shell or curl), asserts `app.commit` in the
`service_starting` record, and requires exit `0` from `docker stop --time 45`.
With the default configuration the stop takes about 15 s (the readiness
propagation delay) inside the 45 s grace the
[runtime budget policy](../configuration-source-policy.md) derives; a longer
stop or a non-zero exit is the finding.
<!-- template:begin jobs:docs-containers-jobs-worker-check -->

The check then runs a `/jobs-worker` step whose expectation comes from the
repository's jobs selection
(`scripts/lib/template_state.py profile --field jobs`: `postgres` in the
template source while `crates/infra-jobs` exists, otherwise the complete
`template.lock`'s value). Where the pack is retained the image must carry
`/jobs-worker`; the step starts it with `--entrypoint /jobs-worker`, the same
hardened flags, `--network none`, and the default configuration, and requires
exit `1` with a startup refusal before any database I/O: exactly
`no job kind is registered` in the template source (no `template.lock`), and
either that or `postgres.enabled must be true to run the jobs worker` in a
derived service, which may have registered kinds. Where the pack is not
retained, an image that carries `/jobs-worker` fails. The step ignores
`RUNTIME_IMAGE_NETWORK` and `RUNTIME_IMAGE_POSTGRES_DSN`, so the migration
rehearsal sees the same result.
<!-- template:end jobs:docs-containers-jobs-worker-check -->

Reuse that image for the scan and the SBOM only when those claims are
required:

```bash
ALLOW_HEAVY=1 make container-security
ALLOW_HEAVY=1 make container-sbom SBOM_OUTPUT=sbom.cdx.json
```

Trivy reports two targets: the Debian packages and the `rustbinary` list
that `cargo auditable build` embedded (178 crates for the health-only
service). A plain `cargo build` binary yields zero Rust packages; the empty
target, not a clean scan, is the signal.

`make verify` on a `runtime_image` change plans exactly this sequence on one
shared verification tag (overridable with `VERIFY_RUNTIME_IMAGE`) and leaves it
to CI's `image` job unless `ALLOW_HEAVY=1` keeps it local.
Preserve layer caches; do not use `--no-cache` or broad pruning as iteration.
An unavailable optional container check is a disclosed gap, not a reason to
provision an environment before local completion.
