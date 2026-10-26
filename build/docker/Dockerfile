# syntax=docker/dockerfile:1
# check=error=true

# Production image for the service binary. Measurements and the reasons for
# each choice: docs/ci-cd-production-ready.md (Decisions Recorded Here).
#
# - The builder tag is the toolchain authority for the image; .dockerignore
#   keeps rust-toolchain.toml out of the context so rustup does not download
#   components in every stage. `make tools-check` asserts the tag equals the
#   channel in rust-toolchain.toml.
# - Builder and runtime share one Debian release (trixie / debian13): a
#   trixie-built glibc binary does not start on an older cc-debian image.
#   A static musl image was 31% smaller but spent up to twice the CPU per
#   request in musl's memcpy (delivery policy, Runtime image).
# - cargo-chef cooks the dependency layer as a plain layer (no cache mounts),
#   so BuildKit exports it to publication's registry cache and a source-only
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
# jemalloc (the service allocator) builds with configure and make; slim has
# gcc only. curl and xz fetch and unpack the build tools below.
RUN apt-get update && apt-get install -y --no-install-recommends make curl xz-utils \
 && rm -rf /var/lib/apt/lists/*
# The upstream static release binaries, checked against the digests in
# tools/versions.env: compiling both tools took 67 s of a 728 s cold build,
# and CI image builds start without a layer cache.
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_CHEF_X86_64_SHA256=aca691abfbfbbe00d482e0ed2249eec3091b65e96a0ce92947fb5b254d48b16d
ARG CARGO_CHEF_AARCH64_SHA256=cd59b90fce5fa84c4189648fec33404ec5491791eebdb812df5e7fca8336408d
ARG CARGO_AUDITABLE_VERSION=0.7.6
ARG CARGO_AUDITABLE_X86_64_SHA256=42b66c852fbb9074a9ca356279a92eb753f48dde16017b8c82f48dcd05d6c856
ARG CARGO_AUDITABLE_AARCH64_SHA256=57265fbd87e9277fbd850c74177d17e5a6f51f15e2803643db65c187b0d4feda
RUN set -eu; \
    arch=$(uname -m); \
    case "${arch}" in \
      x86_64) chef_sha=${CARGO_CHEF_X86_64_SHA256} auditable_sha=${CARGO_AUDITABLE_X86_64_SHA256} ;; \
      aarch64) chef_sha=${CARGO_CHEF_AARCH64_SHA256} auditable_sha=${CARGO_AUDITABLE_AARCH64_SHA256} ;; \
      *) echo "no pinned build tools for ${arch}" >&2; exit 1 ;; \
    esac; \
    install_tool() { \
      archive="$1-${arch}-unknown-linux-musl"; \
      curl -fsSLo "/tmp/${archive}.tar.xz" "$2/${archive}.tar.xz"; \
      echo "$3  /tmp/${archive}.tar.xz" | sha256sum -c -; \
      tar -xJf "/tmp/${archive}.tar.xz" -C /tmp; \
      mv "/tmp/${archive}/$1" "${CARGO_HOME}/bin/$1"; \
      rm -rf "/tmp/${archive}" "/tmp/${archive}.tar.xz"; \
    }; \
    install_tool cargo-chef "https://github.com/LukeMathWalker/cargo-chef/releases/download/v${CARGO_CHEF_VERSION}" "${chef_sha}"; \
    install_tool cargo-auditable "https://github.com/rust-secure-code/cargo-auditable/releases/download/v${CARGO_AUDITABLE_VERSION}" "${auditable_sha}"; \
    cargo chef --version

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
ENV CARGO_INCREMENTAL=0
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

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
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
