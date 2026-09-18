# syntax=docker/dockerfile:1

# Production image for the service binary. Measurements and the reasons for
# each choice: specs/validation-delivery/research/synthesis.md (Runtime image).
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
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json -p service
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
ARG SOURCE_DATE_EPOCH=0
# vergen emits VERGEN_GIT_SHA verbatim when the build has no .git (see
# crates/service/build.rs); this is what the binary logs as app.commit.
ENV VERGEN_GIT_SHA=${RAILWAY_GIT_COMMIT_SHA:-${VCS_REF}}
COPY . .
RUN cargo auditable build --release --locked -p service --bin service \
 && mkdir -p /out && cp target/release/service /out/service \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/service

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
COPY --from=builder /out/service /service
EXPOSE 8080
STOPSIGNAL SIGTERM
USER nonroot:nonroot
ENTRYPOINT ["/service"]
