#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"
# shellcheck source=tools/versions.env
. tools/versions.env

fail() { printf 'monitoring-rules: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
Darwin) os=darwin; pin_os=DARWIN ;;
Linux) os=linux; pin_os=LINUX ;;
*) fail 'supported hosts are Darwin and Linux' ;;
esac
case "$(uname -m)" in
x86_64 | amd64) arch=amd64; pin_arch=AMD64 ;;
aarch64 | arm64) arch=arm64; pin_arch=ARM64 ;;
*) fail 'supported architectures are amd64 and arm64' ;;
esac

pin="PROMTOOL_${pin_os}_${pin_arch}_SHA256"
expected=${!pin:-}
[[ ${expected} =~ ^[0-9a-f]{64}$ ]] || fail "missing archive digest ${pin}"

if command -v sha256sum >/dev/null; then
	sha256_of() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null; then
	sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
else
	fail 'sha256sum or shasum is required'
fi

if git_common_dir=$(git -C "${ROOT_DIR}" rev-parse --git-common-dir 2>/dev/null) && [[ -n ${git_common_dir} ]]; then
	cache="$(cd "${ROOT_DIR}" && cd "${git_common_dir}" && pwd)/tools/monitoring"
else
	cache="${ROOT_DIR}/target/monitoring-tools"
fi
mkdir -p "${cache}"
work=$(mktemp -d "${cache}/verify.XXXXXX")
trap 'rm -rf -- "${work}"' EXIT

name="prometheus-${PROMTOOL_VERSION}.${os}-${arch}"
archive="${cache}/${name}.tar.gz"
if [[ ! -f ${archive} ]]; then
	curl --fail --location --proto '=https' --tlsv1.2 --retry 2 \
		"https://github.com/prometheus/prometheus/releases/download/v${PROMTOOL_VERSION}/${name}.tar.gz" \
		--output "${work}/archive.tar.gz"
	[[ $(sha256_of "${work}/archive.tar.gz") == "${expected}" ]] || fail 'downloaded archive checksum mismatch'
	mv -f "${work}/archive.tar.gz" "${archive}"
fi
[[ $(sha256_of "${archive}") == "${expected}" ]] || fail "cached archive checksum mismatch: ${archive}"

tar -xzf "${archive}" -C "${work}" "${name}/promtool"
promtool="${work}/${name}/promtool"

rules=(env/monitoring/*.rules.yml)
[[ -f ${rules[0]} ]] || fail 'no rule files under env/monitoring'
tests=()
for rule in "${rules[@]}"; do
	base=$(basename "${rule}" .rules.yml)
	test_file="env/monitoring/tests/${base}-rules.yml"
	[[ -f ${test_file} ]] || fail "missing fixture ${test_file} for ${rule}"
	tests+=("${test_file}")
done
"${promtool}" check rules "${rules[@]}"
"${promtool}" test rules "${tests[@]}"
