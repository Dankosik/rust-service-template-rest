# Container Validation

Use only for a matching explicit verification requirement or a bounded
diagnostic. A changed file or an available Docker daemon does not create a
local image gate; `ALLOW_HEAVY=1` is the deliberate opt-in.

Build with `ALLOW_HEAVY=1 make runtime-image-build RUNTIME_IMAGE=service:ci`.
The script passes the manifest tool versions, `HEAD` as `VCS_REF`, the crate
version, and the commit time as `SOURCE_DATE_EPOCH`; `RUNTIME_IMAGE_CACHE_FROM`
and `RUNTIME_IMAGE_CACHE_TO` accept buildx cache specs. A successful build is
not a runtime observation.

Prove the lifecycle with the same tag when it is required:

```bash
ALLOW_HEAVY=1 make runtime-image-check RUNTIME_IMAGE=service:ci RUNTIME_EXPECTED_COMMIT="$(git rev-parse HEAD)"
```

The check starts the container `--read-only --cap-drop=ALL
--security-opt=no-new-privileges`, polls `/health/ready` from the host
(distroless has no shell or curl), asserts `app.commit` in the
`service_starting` record, and requires exit `0` from `docker stop --time 45`.
With the default configuration the stop takes about 15 s (the readiness
propagation delay) inside the 45 s grace the
[runtime budget policy](../configuration-source-policy.md) derives; a longer
stop or a non-zero exit is the finding.

Reuse that image for the scan and the SBOM only when those claims are
required:

```bash
ALLOW_HEAVY=1 make container-security CONTAINER_IMAGE=service:ci
ALLOW_HEAVY=1 make container-sbom CONTAINER_IMAGE=service:ci SBOM_OUTPUT=sbom.cdx.json
```

Trivy reports two targets: the Debian packages and the `rustbinary` list
that `cargo auditable build` embedded (178 crates for the health-only
service). A plain `cargo build` binary yields zero Rust packages; the empty
target, not a clean scan, is the signal.

`make verify` on a `runtime_image` change plans exactly this sequence on one
shared `service:verify` tag and refuses to run without `ALLOW_HEAVY=1`.
Preserve layer caches; do not use `--no-cache` or broad pruning as iteration.
An unavailable optional container check is a disclosed gap, not a reason to
provision an environment before local completion.
