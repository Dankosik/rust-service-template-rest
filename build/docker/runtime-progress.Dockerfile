# syntax=docker/dockerfile:1.27.1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

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
