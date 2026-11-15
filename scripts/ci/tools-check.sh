#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
manifest=tools/versions.env
dockerfile=build/docker/Dockerfile
toolchain=rust-toolchain.toml

fail() {
	printf 'tools-check: %s\n' "$*" >&2
	exit 1
}

# Shape first, so sourcing the file below cannot run anything but assignments.
names=()
while IFS= read -r line; do
	[[ -z ${line} || ${line} == \#* ]] && continue
	[[ ${line} =~ ^([A-Z][A-Z0-9_]*)=([A-Za-z0-9._:@/+-]+)$ ]] || fail "not a NAME=value line in ${manifest}: ${line}"
	name=${BASH_REMATCH[1]}
	value=${BASH_REMATCH[2]}
	for seen in "${names[@]+"${names[@]}"}"; do
		[[ ${seen} != "${name}" ]] || fail "${name} is pinned twice in ${manifest}"
	done
	names+=("${name}")
	if [[ ${name} == *_SHA256 ]]; then
		[[ ${value} =~ ^[0-9a-f]{64}$ ]] || fail "${name} is not a SHA256 digest: ${value}"
	fi
	if [[ ${name} == *_IMAGE ]]; then
		[[ ${value} =~ @sha256:[0-9a-f]{64}$ ]] || fail "${name} is not pinned by digest: ${value}"
	fi
done <"${manifest}"
[[ ${#names[@]} -gt 0 ]] || fail "${manifest} pins nothing"

# shellcheck source=tools/versions.env
. "${manifest}"

# The binary make passed in (a path under the Git common directory locally,
# a PATH name in CI) must exist and report the pinned version.
check_cargo_tool() {
	local var=$1 crate=$2 version=$3 binary banner reported='<none>'
	binary=${!var:-}
	[[ -n ${binary} ]] || fail "${var} is unset; run through make tools-check"
	command -v "${binary}" >/dev/null 2>&1 || fail "${crate} ${version} is not installed at ${binary}"
	banner=$("${binary}" --version 2>/dev/null) || fail "${binary} --version failed"
	if [[ ${banner} =~ [0-9]+\.[0-9]+\.[0-9]+ ]]; then reported=${BASH_REMATCH[0]}; fi
	[[ ${reported} == "${version}" ]] || fail "${binary} reports ${crate} ${reported}, ${manifest} pins ${version}"
	echo "tools-check: ${crate} ${version} resolves at ${binary}"
}

check_cargo_tool CARGO_DENY cargo-deny "${CARGO_DENY_VERSION}"
check_cargo_tool CARGO_NEXTEST cargo-nextest "${CARGO_NEXTEST_VERSION}"
check_cargo_tool CARGO_SHEAR cargo-shear "${CARGO_SHEAR_VERSION}"
check_cargo_tool ZIZMOR zizmor "${ZIZMOR_VERSION}"

# Dockerfile ARG defaults for the tools the image builder downloads.
check_dockerfile_arg() {
	local name=$1 pinned=$2 default
	default=$(sed -n "s/^ARG ${name}=//p" "${dockerfile}")
	[[ -n ${default} ]] || fail "${dockerfile} declares no ARG ${name} default"
	[[ ${default} == "${pinned}" ]] || fail "${dockerfile} defaults ${name} to ${default}, ${manifest} pins ${pinned}"
}
for name in CARGO_CHEF_VERSION CARGO_CHEF_X86_64_SHA256 CARGO_CHEF_AARCH64_SHA256 \
	CARGO_AUDITABLE_VERSION CARGO_AUDITABLE_X86_64_SHA256 CARGO_AUDITABLE_AARCH64_SHA256; do
	check_dockerfile_arg "${name}" "${!name}"
done

# Every FROM that names an image (not a stage) is pinned by digest, and the
# builder's rust tag is the channel the workspace pins.
channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "${toolchain}")
[[ -n ${channel} ]] || fail "${toolchain} declares no channel"
stages=$(sed -n 's/^FROM .* AS \([A-Za-z0-9_-]*\)$/\1/p' "${dockerfile}")
builder_seen=false
while IFS= read -r image; do
	[[ -n ${image} ]] || continue
	grep -Fqx "${image}" <<<"${stages}" && continue
	[[ ${image} =~ @sha256:[0-9a-f]{64}$ ]] || fail "${dockerfile} FROM ${image} is not pinned by digest"
	if [[ ${image} == rust:* ]]; then
		builder_seen=true
		version=${image#rust:}
		version=${version%%-*}
		[[ ${version} == "${channel}" ]] || fail "${dockerfile} builds with rust ${version}, ${toolchain} pins ${channel}"
	fi
done < <(sed -n 's/^FROM \([^ ]*\).*/\1/p' "${dockerfile}")
[[ ${builder_seen} == true ]] || fail "${dockerfile} has no rust:<version> builder stage"
echo "tools-check: ${dockerfile} defaults and base images agree with ${manifest} and ${toolchain}"

echo "tools-check: ${manifest} passed (${#names[@]} pins)"
