#!/usr/bin/env bash
# Remote-only final proof; do not overlap with load comparisons.
set -euo pipefail
source /root/.cargo/env
export CARGO_BUILD_JOBS=6 SQLX_OFFLINE=true
cd /root/optimization/baseline
evidence=/root/optimization/evidence
make fmt-check > "$evidence/fmt-check.log" 2>&1
make build > "$evidence/build-candidate.log" 2>&1
make test > "$evidence/test-candidate.log" 2>&1
ALLOW_HEAVY=1 REQUIRE_DOCKER=1 make test-integration-db > "$evidence/test-integration-db.log" 2>&1
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  > "$evidence/build-release-candidate.log" 2>&1
cp target/release/service /root/profiling/bin/candidate
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp \
  > "$evidence/build-profile-candidate.log" 2>&1
cp target/release/service /root/profiling/bin/profile-candidate
git diff --binary > "$evidence/optimization.patch"
git hash-object crates/infra-webhooks/src/inbound.rs crates/infra-http/src/webhooks.rs \
  test/tests/webhooks/inbound.rs > "$evidence/candidate-source-blobs.txt"
sha256sum /root/profiling/bin/* > "$evidence/final-binaries.sha256"
date -u --iso-8601=seconds > "$evidence/candidate-ready.txt"
