#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

read -r -a buf <<<"${BUF:?BUF is unset; run through make grpc-generate or make grpc-check}"

generate() {
	local output_dir="${1:-${ROOT_DIR}/crates/grpc-contracts/src/generated}"
	local staging
	staging=$(mktemp -d)
	trap 'rm -rf -- "${staging}"' RETURN

	"${buf[@]}" build "${ROOT_DIR}/api/proto" --as-file-descriptor-set --output "${staging}/descriptors.binpb"
	CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/grpc-codegen}" \
		cargo run --locked --manifest-path "${ROOT_DIR}/tools/grpc-codegen/Cargo.toml" -- \
		"${staging}/descriptors.binpb" "${staging}/rust"
	cp -- "${staging}/descriptors.binpb" "${staging}/rust/file_descriptor_set.binpb"

	rm -rf -- "${output_dir}"
	mkdir -p -- "$(dirname -- "${output_dir}")"
	cp -R -- "${staging}/rust" "${output_dir}"
}

check() {
	local base="${GRPC_BASE_REF:-${BASE_REF:-origin/main}}"
	local base_commit staging base_proto
	base_commit=$(git rev-parse --verify "${base}^{commit}") || {
		echo "grpc-check: comparison base is unavailable: ${base}" >&2
		exit 2
	}

	"${buf[@]}" format --exit-code --diff
	"${buf[@]}" lint

	if [[ -f "${ROOT_DIR}/tools/grpc-codegen/Cargo.toml" ]]; then
		CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/grpc-codegen}" \
			cargo clippy --locked --manifest-path "${ROOT_DIR}/tools/grpc-codegen/Cargo.toml" --all-targets -- -D warnings
		CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/grpc-codegen}" \
			RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
			cargo doc --locked --manifest-path "${ROOT_DIR}/tools/grpc-codegen/Cargo.toml" --no-deps --document-private-items
	fi

	staging=$(mktemp -d)
	trap 'rm -rf -- "${staging}"' RETURN
	generate "${staging}/generated"
	diff -r crates/grpc-contracts/src/generated "${staging}/generated"

	base_proto=$(git ls-tree --name-only "${base_commit}" -- api/proto)
	if [[ -n ${base_proto} ]]; then
		"${buf[@]}" breaking --against ".git#ref=${base_commit}" --exclude-imports
	else
		echo "grpc-check: ${base} has no protobuf contract; compatibility baseline absent"
	fi
	echo "grpc-check: passed"
}

case "${1:-}" in
generate) generate "${2:-}" ;;
check) check ;;
*)
	echo "usage: $0 generate [output-dir] | check" >&2
	exit 2
	;;
esac
