#!/usr/bin/env bash
# Verify the owned wire contract and the committed generation. Run through
# `make grpc-check`, which supplies BUF.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
base=${GRPC_BASE_REF:-${BASE_REF:-origin/main}}
git rev-parse --verify "${base}^{commit}" >/dev/null || {
  echo "grpc-check: comparison base is unavailable: ${base}" >&2
  exit 2
}

read -r -a buf <<<"${BUF:?BUF is unset; run make grpc-check}"
"${buf[@]}" format --exit-code --diff
"${buf[@]}" lint

temporary=$(mktemp -d)
trap 'rm -rf -- "${temporary}"' EXIT
bash scripts/grpc-generate.sh "${temporary}/generated"
diff -r crates/grpc-contracts/src/generated "${temporary}/generated"

# A valid base without this capability is an explicit first addition.
if [[ -n "$(git ls-tree --name-only "${base}" -- api/proto)" ]]; then
  # Buf reads the base straight from Git and needs the full commit id. Imports
  # are not the owned wire contract.
  "${buf[@]}" breaking --against ".git#ref=$(git rev-parse "${base}^{commit}")" --exclude-imports
else
  echo "grpc-check: ${base} has no protobuf contract; first addition, compatibility baseline absent"
fi
echo 'grpc-check: format, lint, generation drift and applicable compatibility passed'
