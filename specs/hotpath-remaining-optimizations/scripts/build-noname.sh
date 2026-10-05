#!/usr/bin/env bash
# Attribution only; source is restored before the next runtime comparison.
set -euo pipefail
# rustup creates this file on the disposable benchmark host.
# shellcheck source=/dev/null
source /root/.cargo/env
export CARGO_BUILD_JOBS=6 SQLX_OFFLINE=true
cd /root/remaining/baseline
test -f /root/remaining/evidence/candidate-ready.txt
saved=/root/remaining/evidence/final-observe.rs
cp crates/infra-http/src/observe.rs "$saved"
restore_source() { cp "$saved" crates/infra-http/src/observe.rs; }
trap restore_source EXIT
python3 /root/profiling/scripts/noname-source.py
git hash-object crates/infra-http/src/observe.rs > /root/remaining/evidence/noname-source-blob.txt
git diff -- crates/infra-http/src/observe.rs > /root/remaining/evidence/noname-attribution-source.diff
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  > /root/remaining/evidence/build-release-noname.log 2>&1
cp target/release/service /root/profiling/bin/noname
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus \
  > /root/remaining/evidence/build-profile-noname.log 2>&1
cp target/release/service /root/profiling/bin/profile-noname
restore_source
git hash-object crates/infra-http/src/observe.rs > /root/remaining/evidence/restored-observe-blob.txt
sha256sum /root/profiling/bin/* > /root/remaining/evidence/final-binaries.sha256
date -u --iso-8601=seconds > /root/remaining/evidence/noname-ready.txt
