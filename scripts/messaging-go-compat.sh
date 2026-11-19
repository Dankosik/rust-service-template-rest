#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
readonly bridge_source="${ROOT_DIR}/crates/infra-messaging/tests/fixtures/go-wire/go_wire_compat_bridge_test.go"
readonly compat_source_url="https://github.com/Dankosik/go-service-template-rest"
readonly source_url="${GO_WIRE_SOURCE_URL:-${compat_source_url}.git}"
readonly source_revision="a50f43cf869b86accb3fdba24d758cc2d15a02df"
readonly wire_hash="c98d7c1db43f515e072e91416cf1baa6d0da5c898337d62be9fef3173ed9fe88"
readonly dead_letter_hash="4063580a0322974331c13f3de19e3296b0c40d1785cbbf98d695dc74ef5b953c"
readonly event_hash="add1371aa3ef73b26a9f1eb08d9b992f48969d4a8f6dbd256d81d6d1a058e556"

export COMPAT_SOURCE_URL="${compat_source_url}"
export COMPAT_SOURCE_REVISION="${source_revision}"
export COMPAT_WIRE_HASH="${wire_hash}"
export COMPAT_DEAD_LETTER_HASH="${dead_letter_hash}"
export COMPAT_EVENT_HASH="${event_hash}"

usage() {
	printf '%s\n' "usage: $0 generate|check|verify-rust [fixture-file] [rust-export-file]" >&2
	exit 64
}

mode="${1:-}"
fixture_file="${2:-crates/infra-messaging/tests/fixtures/go-wire/records.json}"
rust_export_file="${3:-}"
case "${mode}" in
generate | check) ;;
verify-rust) [[ -n ${rust_export_file} ]] || usage ;;
*) usage ;;
esac

[[ -f ${bridge_source} ]] || { printf 'missing Go bridge fixture: %s\n' "${bridge_source}" >&2; exit 1; }

fixture_dir="$(dirname "${fixture_file}")"
mkdir -p "${fixture_dir}"
fixture_file="$(cd "${fixture_dir}" && pwd)/$(basename "${fixture_file}")"
if [[ ${mode} == verify-rust ]]; then
	[[ -f ${rust_export_file} ]] || { printf 'Rust export file does not exist: %s\n' "${rust_export_file}" >&2; exit 1; }
	rust_export_file="$(cd "$(dirname "${rust_export_file}")" && pwd)/$(basename "${rust_export_file}")"
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/go-wire.XXXXXX")"
trap 'rm -rf "${scratch}"' EXIT

git clone --quiet --filter=blob:none --no-checkout "${source_url}" "${scratch}/source"
git -C "${scratch}/source" checkout --quiet --detach "${source_revision}"
[[ "$(git -C "${scratch}/source" rev-parse HEAD)" == "${source_revision}" ]] || {
	printf '%s\n' 'pinned Go source revision did not resolve' >&2
	exit 1
}

verify_hash() {
	local expected="$1" path="$2" actual
	if command -v shasum >/dev/null 2>&1; then
		actual="$(shasum -a 256 "${scratch}/source/${path}" | awk '{print $1}')"
	else
		actual="$(sha256sum "${scratch}/source/${path}" | awk '{print $1}')"
	fi
	[[ ${actual} == "${expected}" ]] || {
		printf 'Go source hash mismatch for %s: got %s, want %s\n' "${path}" "${actual}" "${expected}" >&2
		exit 1
	}
}
verify_hash "${wire_hash}" internal/infra/natsjs/message_wire.go
verify_hash "${dead_letter_hash}" internal/infra/natsjs/message_deadletter.go
verify_hash "${event_hash}" internal/domainevent/event.go

cp -- "${bridge_source}" "${scratch}/source/internal/infra/natsjs/go_wire_compat_bridge_test.go"

GO_WIRE_FIXTURES="${fixture_file}" GO_WIRE_MODE="${mode}" GO_WIRE_RUST_EXPORT="${rust_export_file}" \
	go -C "${scratch}/source" test ./internal/infra/natsjs -run '^TestGoWireCompatibilityBridge$' -count=1
