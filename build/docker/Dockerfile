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

FROM rust:1.98.1-slim-trixie@sha256:3999a7ff854f315cf5f2b9a58071cb71196fdfc2ccd32fa20eedce8e754fd62a AS chef
WORKDIR /src
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_AUDITABLE_VERSION=0.7.6
RUN cargo install --locked cargo-chef --version "${CARGO_CHEF_VERSION}" \
 && cargo install --locked cargo-auditable --version "${CARGO_AUDITABLE_VERSION}"

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ENV CARGO_INCREMENTAL=0
# template:begin grpc:docker-protoc-build-deps
ENV TOOLS_ROOT=/opt/service-tools
COPY .cargo/config.toml .cargo/config.toml
COPY scripts/grpc-protoc.py scripts/grpc-protoc.py
COPY tools/versions.env tools/versions.env
RUN apt-get update \
 && apt-get install --no-install-recommends --yes python3 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && python3 scripts/grpc-protoc.py --provision
# template:end grpc:docker-protoc-build-deps
COPY --from=planner /src/recipe.json recipe.json
ARG SERVICE_PACKAGE=service
ARG SERVICE_BIN=service
RUN cargo chef cook --release --locked --recipe-path recipe.json -p "${SERVICE_PACKAGE}"
# template:begin postgres:docker-migrator-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p migrate
# template:end postgres:docker-migrator-build
# template:begin worker:docker-jobs-worker-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p jobs-worker
# template:end worker:docker-jobs-worker-build
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
ARG SOURCE_DATE_EPOCH=0
# vergen emits VERGEN_GIT_SHA verbatim when the build has no .git (see
# crates/config/build.rs); this is what the binary logs as app.commit.
ENV VERGEN_GIT_SHA=${RAILWAY_GIT_COMMIT_SHA:-${VCS_REF}}
COPY . .
RUN cargo auditable build --release --locked -p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}" \
 && mkdir -p /out && cp "target/release/${SERVICE_BIN}" "/out/${SERVICE_BIN}" \
 && touch -d "@${SOURCE_DATE_EPOCH}" "/out/${SERVICE_BIN}"
# template:begin postgres:docker-migrator-output
RUN cargo auditable build --release --locked -p migrate --bin migrate \
 && cp target/release/migrate /out/migrate \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/migrate
# template:end postgres:docker-migrator-output
# template:begin worker:docker-jobs-worker-output
RUN cargo auditable build --release --locked -p jobs-worker --bin jobs-worker \
 && cp target/release/jobs-worker /out/jobs-worker \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/jobs-worker
# template:end worker:docker-jobs-worker-output

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:54df941ed0d06a1bd95ef5e0ce391fd8d9f94b64782dc9a60062727849ee3f97
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
COPY --from=builder "/out/${SERVICE_BIN}" /service
# The migration job runs the same image with `--entrypoint /migrate`
# (docs/architecture/persistence.md); it needs no migration directory.
# template:begin postgres:docker-migrator-runtime
COPY --from=builder /out/migrate /migrate
# template:end postgres:docker-migrator-runtime
# template:begin worker:docker-jobs-worker-runtime
# The jobs worker runs the same image with --entrypoint /jobs-worker (docs/background-jobs.md).
COPY --from=builder /out/jobs-worker /jobs-worker
# template:end worker:docker-jobs-worker-runtime
EXPOSE 8080
STOPSIGNAL SIGTERM
USER nonroot:nonroot
ENTRYPOINT ["/service"]
