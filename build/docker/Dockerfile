# syntax=docker/dockerfile:1

# Production image for the service binary. Measurements and the reasons for
# each choice: docs/ci-cd-production-ready.md (Decisions Recorded Here).
#
# - The builder tag is the toolchain authority for the image; .dockerignore
#   keeps rust-toolchain.toml out of the context so rustup does not download
#   components in every stage. `make tools-check` asserts the tag equals the
#   channel in rust-toolchain.toml.
# - Builder and runtime share one Debian release (trixie / debian13): a
#   trixie-built glibc binary does not start on an older cc-debian image.
# - cargo-chef cooks the dependency layer as a plain layer (no cache mounts),
#   so BuildKit exports it to the GitHub Actions cache and a source-only
#   change rebuilds only the final layer.
# - cargo-auditable embeds each binary's selected Rust dependency graph.
#   Image scan/SBOM admission requires every retained entrypoint inventory;
#   OS packages alone cannot satisfy it. Source revision and vendor custody
#   records identify local patches beyond auditable names and versions.
# - Every ARG has a default because Railway passes only the service variables
#   an ARG names; `make tools-check` asserts the tool defaults equal
#   tools/versions.env. Railway sets RAILWAY_GIT_COMMIT_SHA, which wins over
#   VCS_REF. No ARG carries a credential: BuildKit prints a referenced ARG in
#   the build log, and Railway's builds have no secret mounts.
# - CARGO_INCREMENTAL=0, fixed /src, stripping and SOURCE_DATE_EPOCH bounded
#   the historical byte-equality experiment documented in delivery policy.
#   Locked resolution is not a future binary/image equality guarantee: APT
#   inputs, this frontend tag and tool/package availability can change.

# Declared once for the stages that cook and build the service package.
ARG SERVICE_PACKAGE=service

FROM rust:1.99.0-slim-trixie@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581 AS chef
WORKDIR /src
# jemalloc (the service allocator) builds with configure and make; slim has gcc only.
RUN apt-get update && apt-get install -y --no-install-recommends make \
 && rm -rf /var/lib/apt/lists/*
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_AUDITABLE_VERSION=0.7.6
RUN cargo install --locked cargo-chef --version "${CARGO_CHEF_VERSION}" \
 && cargo install --locked cargo-auditable --version "${CARGO_AUDITABLE_VERSION}"

# Temporary security overlay until distroless carries Debian's fixed OpenSSL.
# Keep the package version and dpkg metadata together so Trivy observes it.
# The image maintainer removes this overlay once the pinned base has the fix
# and the combined image passes its gates; old APT availability is not assured.
FROM chef AS runtime-openssl
RUN apt-get update \
 && mkdir -p /patched/var/lib/dpkg/status.d \
 && cd /tmp \
 && apt-get download libssl3t64=3.5.7-1~deb13u3 \
 && dpkg-deb --extract libssl3t64_*.deb /patched \
 && dpkg-deb --control libssl3t64_*.deb /tmp/openssl-control \
 && cp /tmp/openssl-control/md5sums /patched/var/lib/dpkg/status.d/libssl3t64.md5sums \
 && dpkg-deb --field libssl3t64_*.deb > /patched/var/lib/dpkg/status.d/libssl3t64 \
 && printf 'Status: install ok installed\n' >> /patched/var/lib/dpkg/status.d/libssl3t64

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The three binaries build in parallel stages over one cooked layer: each is
# one fat-LTO link on one codegen unit, so serial builds left three of four
# cores idle. Separate cargo invocations keep each binary's own feature
# resolution, as the separate cooks do.
FROM chef AS cooked
ENV CARGO_INCREMENTAL=0
COPY --from=planner /src/recipe.json recipe.json
# Optional profiling dependency must be visible even during a default cook.
COPY --from=planner /src/vendor/hotpath /src/vendor/hotpath
# template:begin postgres:docker-postgres-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/sqlx-core /src/vendor/sqlx-core
# template:end postgres:docker-postgres-vendor
# template:begin messaging:docker-messaging-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/async-nats /src/vendor/async-nats
# template:end messaging:docker-messaging-vendor
# template:begin object-storage:docker-object-storage-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/aws-smithy-http-client /src/vendor/aws-smithy-http-client
# template:end object-storage:docker-object-storage-vendor
# Shared HTTP clients, including telemetry, use the same native connector.
COPY --from=planner /src/vendor/hyper-util /src/vendor/hyper-util
ARG SERVICE_PACKAGE
RUN cargo chef cook --release --locked --recipe-path recipe.json -p "${SERVICE_PACKAGE}"
# template:begin postgres:docker-migrator-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p migrate
# template:end postgres:docker-migrator-build
# template:begin worker:docker-jobs-worker-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p jobs-worker
# template:end worker:docker-jobs-worker-build

# IMAGE_TESTS=unit runs the workspace's unit tests, which need no database
# or network beyond loopback, in their own debug build. The runtime stage
# copies this stage's empty marker, so a failing test fails the image build
# and a platform keeps its previous deployment. The default skips the work.
FROM chef AS unit-tests
ARG IMAGE_TESTS=none
ENV CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=line-tables-only
COPY --from=planner /src/recipe.json recipe.json
COPY --from=planner /src/vendor /src/vendor
# procps supplies the kill(1) the signal tests send to their own process.
RUN case "${IMAGE_TESTS}" in \
      none) ;; \
      unit) apt-get update && apt-get install -y --no-install-recommends procps \
        && rm -rf /var/lib/apt/lists/* \
        && cargo chef cook --tests --workspace --locked --recipe-path recipe.json ;; \
      *) echo "IMAGE_TESTS is none or unit" >&2; exit 1 ;; \
    esac
COPY . .
# BuildKit points OTEL_* and TRACEPARENT at its own trace socket inside RUN,
# and the tests read the process environment as the service does.
RUN if [ "${IMAGE_TESTS}" = unit ]; then \
      for name in $(env | sed -n 's/^\(OTEL_[A-Za-z0-9_]*\)=.*/\1/p') TRACEPARENT; do unset "${name}"; done; \
      cargo test --workspace --lib --bins --locked --no-fail-fast; \
    fi \
 && mkdir /unit-tests

FROM cooked AS source
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
# vergen emits VERGEN_GIT_SHA verbatim when the build has no .git (see
# crates/config/build.rs); this is what the binary logs as app.commit.
ENV VERGEN_GIT_SHA=${RAILWAY_GIT_COMMIT_SHA:-${VCS_REF}}
COPY . .

FROM source AS service-build
ARG SERVICE_PACKAGE
ARG SERVICE_BIN=service
ARG SOURCE_DATE_EPOCH=0
RUN cargo auditable build --release --locked -p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}" \
 && mkdir -p /out && cp "target/release/${SERVICE_BIN}" "/out/${SERVICE_BIN}" \
 && touch -d "@${SOURCE_DATE_EPOCH}" "/out/${SERVICE_BIN}"
# template:begin postgres:docker-migrator-output

FROM source AS migrate-build
ARG SOURCE_DATE_EPOCH=0
RUN cargo auditable build --release --locked -p migrate --bin migrate \
 && mkdir -p /out && cp target/release/migrate /out/migrate \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/migrate
# template:end postgres:docker-migrator-output
# template:begin worker:docker-jobs-worker-output

FROM source AS jobs-worker-build
ARG SOURCE_DATE_EPOCH=0
RUN cargo auditable build --release --locked -p jobs-worker --bin jobs-worker \
 && mkdir -p /out && cp target/release/jobs-worker /out/jobs-worker \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/jobs-worker
# template:end worker:docker-jobs-worker-output

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
COPY --from=runtime-openssl /patched/ /
# Empty: the dependency alone makes the unit-test gate part of every build.
COPY --from=unit-tests /unit-tests/ /
# The version is the Cargo package version; scripts/ci/runtime-image-build.sh
# passes it from the workspace manifest.
ARG APP_VERSION=unknown
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
ARG SOURCE_URL=""
LABEL org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${RAILWAY_GIT_COMMIT_SHA:-$VCS_REF}" \
      org.opencontainers.image.source="${SOURCE_URL}"
ARG SERVICE_BIN=service
COPY --from=service-build "/out/${SERVICE_BIN}" /service
# The migration job runs the same image with `--entrypoint /migrate`
# (docs/architecture/persistence.md); it needs no migration directory.
# template:begin postgres:docker-migrator-runtime
COPY --from=migrate-build /out/migrate /migrate
# template:end postgres:docker-migrator-runtime
# template:begin worker:docker-jobs-worker-runtime
# The jobs worker runs the same image with --entrypoint /jobs-worker (docs/background-jobs.md).
COPY --from=jobs-worker-build /out/jobs-worker /jobs-worker
# template:end worker:docker-jobs-worker-runtime
EXPOSE 8080
STOPSIGNAL SIGTERM
USER nonroot:nonroot
ENTRYPOINT ["/service"]
