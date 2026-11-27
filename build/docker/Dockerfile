# syntax=docker/dockerfile:1.27.1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
# check=error=true

# Production image for the service binary. Measurements and the reasons for
# each choice: docs/ci-cd-production-ready.md (Decisions Recorded Here).
#
# - The builder tag is the toolchain authority for the image; .dockerignore
#   keeps rust-toolchain.toml out of the context so rustup does not download
#   components in every stage. `just dockerfile-check` asserts the tag equals the
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
#   an ARG names; this Dockerfile is the single owner of build-stage static
#   archive pins. Railway sets RAILWAY_GIT_COMMIT_SHA, which wins over
#   VCS_REF. No ARG carries a credential: BuildKit prints a referenced ARG in
#   the build log, and Railway's builds have no secret mounts.
# - CARGO_INCREMENTAL=0, fixed /src, stripping and SOURCE_DATE_EPOCH bounded
#   the historical byte-equality experiment documented in delivery policy.
#   Locked resolution is not a future binary/image equality guarantee: APT
#   inputs and tool/package availability can change.
# - JEMALLOC_SYS_WITH_MALLOC_CONF bakes `thp:never` into the allocator: on a
#   host whose transparent huge pages are `always`, khugepaged refills the
#   pages jemalloc returned (docs/backend-library-selection.md).
# - PGO=train builds the service twice so the release build reads a profile
#   of the requests build/docker/pgo-train.sh sends; a profile differs between
#   builds, so the binary is not reproducible. The default builds once.

ARG SERVICE_PACKAGE=service
ARG IMAGE_TESTS=none

FROM rust:1.100.0-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a AS chef
WORKDIR /src
# jemalloc (the service allocator) builds with configure and make; slim has
# gcc only. curl and xz fetch and unpack the build tools below.
RUN apt-get update && apt-get install -y --no-install-recommends make curl xz-utils \
 && rm -rf /var/lib/apt/lists/*
# The upstream static release binaries, checked against their release digests:
# compiling both tools took 67 s of a 728 s cold build, and CI image builds
# start without a layer cache.
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_CHEF_X86_64_SHA256=aca691abfbfbbe00d482e0ed2249eec3091b65e96a0ce92947fb5b254d48b16d
ARG CARGO_CHEF_AARCH64_SHA256=cd59b90fce5fa84c4189648fec33404ec5491791eebdb812df5e7fca8336408d
ARG CARGO_AUDITABLE_VERSION=0.7.7
ARG CARGO_AUDITABLE_X86_64_SHA256=7676443367c5e33d9fb22acef63cccb5e9202de12dae1b0579995eb3e2bc7a18
ARG CARGO_AUDITABLE_AARCH64_SHA256=c324af56990601354bbf40080f582ba46f8ec8b807a1388e25bb7e429d285c18
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

FROM chef AS rust-notices-tool
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
 && rm -rf /var/lib/apt/lists/*
ARG CARGO_ABOUT_VERSION=0.9.3
ARG CARGO_ABOUT_X86_64_SHA256=3ad1e94ea1e63d69d1961f4fb15ccbbe9575d906111f96d2cb4dce6df48f71b4
ARG CARGO_ABOUT_AARCH64_SHA256=eb4d9ff6ebd2518152a8308729431a04b3836e71d2bd5ead7aea69f91a71162e
RUN set -eu; \
    arch=$(uname -m); \
    case "${arch}" in \
      x86_64) checksum=${CARGO_ABOUT_X86_64_SHA256} ;; \
      aarch64) checksum=${CARGO_ABOUT_AARCH64_SHA256} ;; \
      *) echo "no pinned cargo-about for ${arch}" >&2; exit 1 ;; \
    esac; \
    package="cargo-about-${CARGO_ABOUT_VERSION}-${arch}-unknown-linux-musl"; \
    curl -fsSLo /tmp/cargo-about.tar.gz "https://github.com/EmbarkStudios/cargo-about/releases/download/${CARGO_ABOUT_VERSION}/${package}.tar.gz"; \
    echo "${checksum}  /tmp/cargo-about.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/cargo-about.tar.gz -C "${CARGO_HOME}/bin" --strip-components=1 "${package}/cargo-about"; \
    rm /tmp/cargo-about.tar.gz

FROM rust-notices-tool AS rust-notices
COPY . .
RUN mkdir /out \
 && test -f LICENSE && test ! -L LICENSE \
 && python3 build/docker/rust-notices.py "$(rustc --print host-tuple)" /out/THIRD-PARTY-RUST.txt \
 && cp LICENSE /out/LICENSE

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The binaries build in parallel stages over one cooked layer: each is
# one fat-LTO link on one codegen unit, so serial builds left cores idle.
# Separate cargo invocations keep each binary's own feature resolution,
# as the separate cooks do.
FROM chef AS cooked
ENV CARGO_INCREMENTAL=0
ENV JEMALLOC_SYS_WITH_MALLOC_CONF=thp:never
COPY --from=planner /src/recipe.json recipe.json
# Optional profiling dependency must be visible even during a default cook.
COPY --from=planner /src/vendor/hotpath /src/vendor/hotpath
# template:begin postgres:docker-postgres-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/sqlx-core /src/vendor/sqlx-core
COPY --from=planner /src/vendor/sqlx-postgres /src/vendor/sqlx-postgres
# template:end postgres:docker-postgres-vendor
# template:begin messaging:docker-messaging-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/async-nats /src/vendor/async-nats
# template:end messaging:docker-messaging-vendor
# template:begin object-storage:docker-object-storage-vendor
# Excluded from the chef skeleton: cook the actual patched dependency.
COPY --from=planner /src/vendor/aws-runtime /src/vendor/aws-runtime
COPY --from=planner /src/vendor/aws-smithy-http-client /src/vendor/aws-smithy-http-client
# template:end object-storage:docker-object-storage-vendor
# Shared HTTP clients, including telemetry, use the same native connector.
COPY --from=planner /src/vendor/hyper-util /src/vendor/hyper-util
# template:begin tonic:docker-tonic-vendor
COPY --from=planner /src/vendor/tonic /src/vendor/tonic
# template:end tonic:docker-tonic-vendor
# template:begin clickhouse:docker-clickhouse-vendor
COPY --from=planner /src/vendor/clickhouse /src/vendor/clickhouse
# template:end clickhouse:docker-clickhouse-vendor
ARG SERVICE_PACKAGE
RUN cargo chef cook --release --locked --recipe-path recipe.json -p "${SERVICE_PACKAGE}"
# template:begin postgres:docker-migrator-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p migrate
# template:end postgres:docker-migrator-build
# template:begin worker:docker-jobs-worker-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p jobs-worker
# template:end worker:docker-jobs-worker-build
# template:begin temporal:docker-temporal-worker-build
RUN cargo chef cook --release --locked --recipe-path recipe.json -p temporal-worker
# template:end temporal:docker-temporal-worker-build

# IMAGE_TESTS=unit runs the workspace's unit tests, which need no database
# or network beyond loopback, in their own debug build. Dynamic stage
# selection leaves unit-tests-unit-run unreachable when IMAGE_TESTS=none, so
# default builds evaluate zero unit-test COPY or RUN steps; when IMAGE_TESTS=unit,
# copying the empty marker makes a failing test fail the image build.
FROM scratch AS unit-tests-none

FROM chef AS unit-tests-unit-run
ENV CARGO_INCREMENTAL=0
COPY --from=planner /src/recipe.json recipe.json
COPY --from=planner /src/vendor /src/vendor
# procps supplies the kill(1) the signal tests send to their own process.
RUN apt-get update && apt-get install -y --no-install-recommends procps \
 && rm -rf /var/lib/apt/lists/* \
 && cargo chef cook --tests --workspace --locked --recipe-path recipe.json
COPY . .
# BuildKit points OTEL_* and TRACEPARENT at its own trace socket inside RUN,
# and the tests read the process environment as the service does.
RUN for name in $(env | sed -n 's/^\(OTEL_[A-Za-z0-9_]*\)=.*/\1/p') TRACEPARENT; do unset "${name}"; done; \
    cargo test --workspace --lib --bins --locked --no-fail-fast \
 && mkdir /unit-tests

FROM scratch AS unit-tests-unit
COPY --from=unit-tests-unit-run /unit-tests/ /

FROM unit-tests-${IMAGE_TESTS} AS unit-tests

FROM cooked AS source
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
# crates/config/build.rs stamps VERGEN_GIT_SHA when the build has no .git;
# this is what the binary logs as app.commit.
ENV VERGEN_GIT_SHA=${RAILWAY_GIT_COMMIT_SHA:-${VCS_REF}}
COPY . .

# PGO=train compiles the service with counters, runs it against
# build/docker/pgo-train.sh, merges what it counted and builds again with that
# profile. One `build` serves both compilations: cargo-auditable's rustc
# wrapper changes the workspace crates' symbol names, and a profile recorded
# under other names is ignored without a warning. The counted build has its
# own target directory, and the build scripts' own counts are dropped before
# the service runs.
FROM source AS service-build
ARG PGO=none
ARG SERVICE_PACKAGE
ARG SERVICE_BIN=service
ARG SOURCE_DATE_EPOCH=0
RUN build() { cargo auditable build --release --locked -p "${SERVICE_PACKAGE}" --bin "${SERVICE_BIN}" "$@"; } \
 && case "${PGO}" in \
      none) ;; \
      train) rustup component add llvm-tools-preview \
        && export RUSTFLAGS="-Cprofile-generate=/pgo/raw" \
        && build --target-dir /pgo/target \
        && rm -rf /pgo/raw \
        && sh build/docker/pgo-train.sh "/pgo/target/release/${SERVICE_BIN}" \
        && "$(rustc --print sysroot)/lib/rustlib/$(rustc --print host-tuple)/bin/llvm-profdata" \
             merge --sparse -o /pgo/service.profdata /pgo/raw \
        && rm -rf /pgo/target /pgo/raw \
        && export RUSTFLAGS="-Cprofile-use=/pgo/service.profdata" ;; \
      *) echo "PGO is none or train" >&2; exit 1 ;; \
    esac \
 && build \
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
# template:begin temporal:docker-temporal-worker-output

FROM source AS temporal-worker-build
ARG SOURCE_DATE_EPOCH=0
RUN cargo auditable build --release --locked -p temporal-worker --bin temporal-worker \
 && mkdir -p /out && cp target/release/temporal-worker /out/temporal-worker \
 && touch -d "@${SOURCE_DATE_EPOCH}" /out/temporal-worker
# template:end temporal:docker-temporal-worker-output

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:29ee70762214fe8190cea04ef1c0a84a8365879e047ce8f911ae5a5ab2355fa2
# Empty: the dependency alone makes the selected unit-test gate part of the build.
COPY --from=unit-tests / /
# The version is the Cargo package version; scripts/image.py
# passes it from the workspace manifest.
ARG APP_VERSION=unknown
ARG VCS_REF=unknown
ARG RAILWAY_GIT_COMMIT_SHA
ARG SOURCE_URL=""
ARG PGO=none
LABEL org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${RAILWAY_GIT_COMMIT_SHA:-$VCS_REF}" \
      org.opencontainers.image.source="${SOURCE_URL}" \
      service.build.pgo="${PGO}"
ARG SERVICE_BIN=service
COPY --from=service-build "/out/${SERVICE_BIN}" /service
COPY --from=rust-notices --chmod=0444 /out/LICENSE /out/THIRD-PARTY-RUST.txt /usr/share/licenses/service/
# template:begin postgres:docker-migrator-runtime
COPY --from=migrate-build /out/migrate /migrate
# template:end postgres:docker-migrator-runtime
# template:begin worker:docker-jobs-worker-runtime
COPY --from=jobs-worker-build /out/jobs-worker /jobs-worker
# template:end worker:docker-jobs-worker-runtime
# template:begin temporal:docker-temporal-worker-runtime
COPY --from=temporal-worker-build /out/temporal-worker /temporal-worker
# template:end temporal:docker-temporal-worker-runtime
EXPOSE 8080
STOPSIGNAL SIGTERM
USER nonroot:nonroot
ENTRYPOINT ["/service"]
