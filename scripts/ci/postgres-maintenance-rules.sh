#!/usr/bin/env bash
# Validate shipped rules with the checksum-pinned official promtool, without
# installing a global binary or starting a Prometheus server.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
# shellcheck source=tools/versions.env
. tools/versions.env

fail() { printf 'postgres-maintenance-rules: %s\n' "$*" >&2; exit 1; }
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
command -v curl >/dev/null || fail 'curl is required'
command -v tar >/dev/null || fail 'tar is required'
if command -v sha256sum >/dev/null; then
    hash() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null; then
    hash() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    fail 'sha256sum or shasum is required'
fi

# Worktree/task-local, including in CI; never share an extracted PATH binary.
cache=${root}/target/postgres-maintenance-tools
mkdir -p "${cache}"
work=$(mktemp -d "${cache}/verify.XXXXXX")
trap 'rm -rf -- "${work}"' EXIT
name="prometheus-${PROMTOOL_VERSION}.${os}-${arch}"
archive="${cache}/${name}.tar.gz"
if [[ ! -f ${archive} ]]; then
    curl --fail --location --proto '=https' --tlsv1.2 --retry 2 \
        "https://github.com/prometheus/prometheus/releases/download/v${PROMTOOL_VERSION}/${name}.tar.gz" \
        --output "${work}/archive.tar.gz"
    [[ $(hash "${work}/archive.tar.gz") == "${expected}" ]] || fail 'downloaded archive checksum mismatch'
    mv "${work}/archive.tar.gz" "${archive}"
fi
# Verify on every use and extract afresh: version text alone is not provenance.
[[ $(hash "${archive}") == "${expected}" ]] || fail "cached archive checksum mismatch: ${archive}"
printf '%s  %s\n' "${expected}" "${name}.tar.gz" | tee "${work}/sha256.txt"
tar -xzf "${archive}" -C "${work}" "${name}/promtool"
promtool="${work}/${name}/promtool"
"${promtool}" --version
"${promtool}" check rules env/monitoring/postgres-maintenance.rules.yml
"${promtool}" test rules scripts/tests/postgres-maintenance-rules.yml
