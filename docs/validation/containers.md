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
<!-- template:begin worker:docs-containers-jobs-worker-check -->

Jobs or messaging retention requires `/jobs-worker`; pruning both requires its
absence. The lifecycle check derives that expectation from repository selection,
then runs the retained worker with `--entrypoint /jobs-worker`, hardened flags,
`--network none` and default configuration. It requires the selection's expected
startup refusal before provider I/O, rather than certifying a configured worker.
The step ignores `RUNTIME_IMAGE_NETWORK` and `RUNTIME_IMAGE_POSTGRES_DSN`, so a
migration rehearsal does not turn it into worker integration proof.
<!-- template:end worker:docs-containers-jobs-worker-check -->

Reuse that image for the scan and the SBOM only when those claims are
required:

```bash
ALLOW_HEAVY=1 make container-security
ALLOW_HEAVY=1 make container-sbom SBOM_OUTPUT=sbom.cdx.json
```

Each invocation resolves the image tag to one immutable local image ID. The
native Trivy report must contain a readable `rustbinary` inventory for every
retained entrypoint: `/service` and, when selected, `/migrate` and `/jobs-worker`.
Repository manifests/profile selection determine that set independently of scan
output. Admission checks each root package/version and reachable dependency graph;
missing binaries, empty inventories, ambiguous identities and broken graph edges
refuse before the vulnerability verdict or SBOM conversion. The SBOM preserves
each binary's application component and dependency graph. No fixed package count
or clean OS-package scan substitutes for these checks.

This is structural proof under the trusted `cargo auditable build` path, not an
independent reconstruction from machine code or detection of a coherent producer
omission. Names/versions alone do not establish upstream patch provenance: retain
the source revision and vendored patch custody records. Locked resolution also
does not promise identical future binaries/images; [delivery policy](../ci-cd-production-ready.md#runtime-image)
bounds the historical experiment and mutable inputs. A native output or published
artifact is proven only by the run that actually consumes it.

`make verify` on a `runtime_image` change plans exactly this sequence on one
shared verification tag (overridable with `VERIFY_RUNTIME_IMAGE`) and leaves it
to CI's `image` job unless `ALLOW_HEAVY=1` keeps it local.
Preserve layer caches; do not use `--no-cache` or broad pruning as iteration.
An unavailable optional container check is a disclosed gap, not a reason to
provision an environment before local completion.
