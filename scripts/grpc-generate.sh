#!/usr/bin/env bash
# Generate committed Rust from the owned protobuf module. Buf builds the
# descriptor set without imports; stock tonic-prost-build emits the code.
# Run through `make grpc-generate`, which supplies BUF. The optional argument
# is the output directory.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
root_dir="$(cd -- "${script_dir}/.." && pwd)"
readonly root_dir

read -r -a buf <<<"${BUF:?BUF is unset; run make grpc-generate}"
readonly generated_dir="${1:-${root_dir}/crates/grpc-contracts/src/generated}"
staging="$(mktemp -d)"
readonly staging
trap 'rm -rf -- "${staging}"' EXIT

"${buf[@]}" build "${root_dir}/api/proto" \
  --exclude-imports \
  --as-file-descriptor-set \
  --output "${staging}/descriptors.binpb"
cargo run --locked --manifest-path "${root_dir}/tools/grpc-codegen/Cargo.toml" -- \
  "${staging}/descriptors.binpb" "${staging}/rust"

# Replace the directory whole so a removed package leaves no stale file.
rm -rf -- "${generated_dir}"
mkdir -p -- "$(dirname -- "${generated_dir}")"
cp -R -- "${staging}/rust" "${generated_dir}"
