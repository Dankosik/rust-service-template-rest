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
# - cargo-auditable embeds the dependency list, so the image scan sees the
#   Rust packages and not only the Debian ones.
# - Every ARG has a default because Railway passes no build arguments;
#   `make tools-check` asserts the tool defaults equal tools/versions.env.
#   Railway does set RAILWAY_GIT_COMMIT_SHA, which wins over VCS_REF.
# - The binary is reproducible: CARGO_INCREMENTAL=0, a fixed /src path,
#   `strip = true` in the release profile, and SOURCE_DATE_EPOCH for its mtime.

# Declared once for the stages that cook and build the service package.
ARG SERVICE_PACKAGE=service

FROM rust:1.99.0-slim-trixie@sha256:01dd4f9c24801cfc8ba9cf8a5dd6dcca451cd17d1ae73574edc22591de6e6816 AS chef
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
COPY --from=planner /src/vendor/async-nats /src/vendor/async-nats
# template:end messaging:docker-messaging-vendor
# template:begin object-storage:docker-object-storage-vendor
COPY --from=planner /src/vendor/aws-smithy-http-client /src/vendor/aws-smithy-http-client
# template:end object-storage:docker-object-storage-vendor
ARG SERVICE_PACKAGE
RUN cargo chef cook --release --locked --recipe-path recipe.json -p "${SERVICE_PACKAGE}"
# template:begin postgres:docker-migrator-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p migrate
# template:end postgres:docker-migrator-build
# template:begin worker:docker-jobs-worker-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p jobs-worker
# template:end worker:docker-jobs-worker-build

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

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97
COPY --from=runtime-openssl /patched/ /
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
