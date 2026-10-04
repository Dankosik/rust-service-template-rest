#!/usr/bin/env bash
# Remote-only precision repair. Finish all existing loads before building.
set -euo pipefail
source /root/.cargo/env
export CARGO_BUILD_JOBS=6 SQLX_OFFLINE=true
cd /root/optimization/baseline
evidence=/root/optimization/evidence
test -f /root/profiling/evidence/comparison-completed.txt
files=(crates/infra-webhooks/src/inbound.rs crates/infra-http/src/webhooks.rs test/tests/webhooks/inbound.rs)
restore_candidate() {
  for file in "${files[@]}"; do cp "/root/optimization/candidate-delta/$file" "$file"; done
}
trap restore_candidate EXIT
sha256sum Cargo.lock > "$evidence/raw-lock-before.sha256"
for file in "${files[@]}"; do git show "HEAD:$file" > "$file"; done
git hash-object "${files[@]}" > "$evidence/raw-baseline-source-blobs.txt"
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus \
  > "$evidence/build-raw-baseline.log" 2>&1
cp target/release/service /root/profiling/bin/profile-raw-baseline
restore_candidate
git hash-object "${files[@]}" > "$evidence/raw-candidate-source-blobs.txt"
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus \
  > "$evidence/build-raw-candidate.log" 2>&1
cp target/release/service /root/profiling/bin/profile-raw-candidate
sha256sum Cargo.lock > "$evidence/raw-lock-after.sha256"
cmp "$evidence/raw-lock-before.sha256" "$evidence/raw-lock-after.sha256"
cargo tree --locked -p service --features hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus \
  -e features > "$evidence/raw-feature-tree.txt"
sha256sum /root/profiling/bin/* > "$evidence/final-binaries.sha256"
date -u --iso-8601=seconds > "$evidence/raw-ready.txt"
