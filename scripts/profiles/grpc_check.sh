#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
base=${GRPC_BASE_REF:-${BASE_REF:-origin/main}}
base_commit=$(git rev-parse --verify "${base}^{commit}") || {
	echo "grpc-check: comparison base is unavailable: ${base}" >&2
	exit 2
}

read -r -a buf <<<"${BUF:?BUF is unset; run make grpc-check}"
"${buf[@]}" format --exit-code --diff
"${buf[@]}" lint

if [[ -f "${root}/tools/grpc-codegen/Cargo.toml" ]]; then
	CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/target/grpc-codegen}" cargo clippy --locked --manifest-path "${root}/tools/grpc-codegen/Cargo.toml" --all-targets -- -D warnings
	CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/target/grpc-codegen}" RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" cargo doc --locked --manifest-path "${root}/tools/grpc-codegen/Cargo.toml" --no-deps --document-private-items
fi

temporary=$(mktemp -d)
trap 'rm -rf -- "${temporary}"' EXIT
bash scripts/profiles/grpc_generate.sh "${temporary}/generated"
diff -r crates/grpc-contracts/src/generated "${temporary}/generated"

# A valid base without this capability is an explicit first addition.
base_proto=$(git ls-tree --name-only "${base_commit}" -- api/proto)
if [[ -n ${base_proto} ]]; then
	# Buf reads the base straight from Git and needs the full commit id. Imports
	# are not the owned wire contract.
	"${buf[@]}" breaking --against ".git#ref=${base_commit}" --exclude-imports
else
	echo "grpc-check: ${base} has no protobuf contract; first addition, compatibility baseline absent"
fi
echo 'grpc-check: format, lint, generation drift and applicable compatibility passed'
