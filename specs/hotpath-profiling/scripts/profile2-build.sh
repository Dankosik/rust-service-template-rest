#!/usr/bin/env bash
# Run after every measurement process has stopped; preserves ordinary release.
set -euo pipefail
source /root/.cargo/env
cd /root/profiling/source
export CARGO_BUILD_JOBS=6
cargo update --workspace > /root/profiling/evidence/lock-update2.log 2>&1
cargo fmt --all
CARGO_PROFILE_RELEASE_STRIP=none /usr/bin/time -v \
  cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu \
  > /root/profiling/evidence/build-profile2.log 2>&1
cp target/release/service /root/profiling/bin/profile2
sha256sum /root/profiling/bin/* > /root/profiling/evidence/binaries.sha256
