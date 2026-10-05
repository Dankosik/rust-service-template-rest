#!/usr/bin/env bash
# Execute ONLY on the approved disposable DigitalOcean host.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y build-essential pkg-config clang llvm libssl-dev cmake \
  git rsync curl jq python3 postgresql postgresql-contrib wrk sysstat \
  linux-tools-generic shellcheck
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain 1.98.1
# rustup creates this file on the disposable benchmark host.
# shellcheck source=/dev/null
source /root/.cargo/env
cargo install hotpath --version '^0.28' --locked --features tui
if ! hotpath --version | grep -Eq '(^| )0\.28\.4($| )'; then
  cargo install hotpath --version '=0.28.4' --locked --features tui --force
fi
hotpath --version
rustc --version
mkdir -p /root/profiling/evidence /root/profiling/bin
uname -a > /root/profiling/evidence/uname.txt
lscpu > /root/profiling/evidence/lscpu.txt
free -b > /root/profiling/evidence/memory.txt
df -B1 > /root/profiling/evidence/disk.txt
date -u --iso-8601=seconds > /root/profiling/evidence/bootstrap-completed.txt
