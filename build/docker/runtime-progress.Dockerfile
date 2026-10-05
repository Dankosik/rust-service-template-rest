# syntax=docker/dockerfile:1

# The runner reads the digest-pinned Rust/Debian base from the production
# Dockerfile. The Linux release binary is built once before this packaging step.
ARG RUNTIME_PROGRESS_BASE_IMAGE
FROM ${RUNTIME_PROGRESS_BASE_IMAGE}
WORKDIR /proof
COPY runtime_progress /proof/runtime_progress
ARG RUNTIME_PROGRESS_SOURCE
ARG RUNTIME_PROGRESS_RUN
LABEL org.opencontainers.image.revision="${RUNTIME_PROGRESS_SOURCE}" \
      io.runtime-progress.run="${RUNTIME_PROGRESS_RUN}"
STOPSIGNAL SIGTERM
ENTRYPOINT ["/proof/runtime_progress"]
