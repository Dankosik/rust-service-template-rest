#!/usr/bin/env bash
set -euo pipefail

readonly script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly root_dir="$(cd -- "${script_dir}/.." && pwd)"

# shellcheck source=tools/versions.env
source "${root_dir}/tools/versions.env"

run_buf() {
  go run "github.com/bufbuild/buf/cmd/buf@v${BUF_VERSION}" "$@"
}

if [[ "${1:-}" == "buf" ]]; then
  shift
  run_buf "$@"
  exit 0
fi

readonly output_dir="${1:-${root_dir}/crates/grpc-contracts/src}"
readonly descriptor_path="${output_dir}/descriptor.bin"
readonly generated_dir="${output_dir}/generated"

mkdir -p "${generated_dir}"
run_buf build "${root_dir}/api/proto" \
  --output "${descriptor_path}" \
  --as-file-descriptor-set
cargo run --locked --manifest-path "${root_dir}/tools/grpc-codegen/Cargo.toml" -- \
  "${descriptor_path}" "${generated_dir}"
