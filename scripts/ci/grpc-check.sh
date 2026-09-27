#!/usr/bin/env bash
# Verify the owned wire contract and reproducible committed generation.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
base=${GRPC_BASE_REF:-${BASE_REF:-origin/main}}
git rev-parse --verify "${base}^{commit}" >/dev/null || {
  echo "grpc-check: comparison base is unavailable: ${base}" >&2
  exit 2
}

run_buf() { bash scripts/grpc-generate.sh buf "$@"; }
run_buf format --exit-code --diff
run_buf lint

temporary=$(mktemp -d)
trap 'rm -rf -- "${temporary}"' EXIT

bash scripts/grpc-generate.sh "${temporary}/first"
bash scripts/grpc-generate.sh "${temporary}/second"
diff -r "${temporary}/first" "${temporary}/second"
diff -r crates/grpc-contracts/src/generated "${temporary}/first"

# A valid base without this capability is an explicit first addition. A bad
# reference or unreadable existing contract never falls back to the head.
if [[ -n "$(git ls-tree --name-only "${base}" -- api/proto)" ]]; then
  mkdir -p "${temporary}/base"
  archive_paths=(api/proto buf.yaml)
  if [[ -n "$(git ls-tree --name-only "${base}" -- buf.lock)" ]]; then
    archive_paths+=(buf.lock)
  fi
  git archive "${base}" -- "${archive_paths[@]}" | tar -x -C "${temporary}/base"
  [[ -s ${temporary}/base/buf.yaml ]] || {
    echo 'grpc-check: existing base contract has no readable Buf configuration' >&2
    exit 2
  }
  # Compatibility covers the owned module; removed or changed imports are not wire contract.
  run_buf breaking --against "${temporary}/base" --exclude-imports
else
  echo "grpc-check: ${base} has no protobuf contract; first addition, compatibility baseline absent"
fi
echo 'grpc-check: format, lint, repeat generation, drift and applicable compatibility passed'
