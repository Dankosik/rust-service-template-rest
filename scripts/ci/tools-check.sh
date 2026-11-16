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
[[ ${GO_VERSION:-} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "GO_VERSION in ${manifest} is not X.Y.Z: ${GO_VERSION:-<unset>}"

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
channel=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B - "${toolchain}" <<'PY'
import sys
import tomllib
from pathlib import Path

data = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
channel = data.get("toolchain", {}).get("channel")
if isinstance(channel, str):
    print(channel)
PY
)
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

# 7-day publish-age cooldown agreement across Cargo, npx, dependency-drift, and Dependabot.
[[ -f .cargo/config.toml ]] || fail "missing .cargo/config.toml"
[[ -f scripts/ci/dependency-drift.sh ]] || fail "missing scripts/ci/dependency-drift.sh"
if ! cooldown_err=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B - <<'PY' 2>&1
import re
import sys
import tomllib
from pathlib import Path

cargo_cfg = tomllib.loads(Path(".cargo/config.toml").read_text(encoding="utf-8"))
cargo_age = cargo_cfg.get("registry", {}).get("global-min-publish-age")
if cargo_age != "7 days":
    print(f".cargo/config.toml registry.global-min-publish-age is {cargo_age!r}, expected '7 days'", file=sys.stderr)
    sys.exit(1)

drift_text = Path("scripts/ci/dependency-drift.sh").read_text(encoding="utf-8")
drift_matches = re.findall(r"(?m)^cooldown_days=([0-9]+)$", drift_text)
if drift_matches != ["7"]:
    print(f"scripts/ci/dependency-drift.sh cooldown_days is {drift_matches!r}, expected ['7']", file=sys.stderr)
    sys.exit(1)

for owner, pattern in (
    ("make/template.mk", r"(?m)^NPX_COOLDOWN_DAYS\s*:=\s*([0-9]+)$"),
    ("scripts/ci/migration-history-check.sh", r"(?m)^\s*npx_cooldown_days=([0-9]+)$"),
):
    candidate = Path(owner)
    if candidate.is_file():
        matches = re.findall(pattern, candidate.read_text(encoding="utf-8"))
        if matches != ["7"]:
            print(f"{owner} npx cooldown days is {matches!r}, expected ['7']", file=sys.stderr)
            sys.exit(1)

if Path(".railway/package.json").is_file():
    railway_npmrc = Path(".railway/.npmrc")
    if not railway_npmrc.is_file():
        print("missing .railway/.npmrc alongside .railway/package.json", file=sys.stderr)
        sys.exit(1)
    npmrc_kv: dict[str, list[str]] = {}
    for raw_line in railway_npmrc.read_text(encoding="utf-8").splitlines():
        stripped = raw_line.strip()
        if not stripped or stripped.startswith(("#", ";")):
            continue
        if "=" not in stripped:
            print(f".railway/.npmrc has non-assignment line: {stripped!r}", file=sys.stderr)
            sys.exit(1)
        key, value = (part.strip() for part in stripped.split("=", 1))
        npmrc_kv.setdefault(key, []).append(value)
    if npmrc_kv.get("ignore-scripts") != ["true"]:
        print(
            f".railway/.npmrc ignore-scripts is {npmrc_kv.get('ignore-scripts')!r}, expected ['true']",
            file=sys.stderr,
        )
        sys.exit(1)
    if npmrc_kv.get("min-release-age") != ["7"]:
        print(
            f".railway/.npmrc min-release-age is {npmrc_kv.get('min-release-age')!r}, expected ['7']",
            file=sys.stderr,
        )
        sys.exit(1)

codegen_manifest = Path("tools/grpc-codegen/Cargo.toml")
root_manifest = Path("Cargo.toml")
if codegen_manifest.is_file() and root_manifest.is_file():
    ws_deps = tomllib.loads(root_manifest.read_text(encoding="utf-8")).get("workspace", {}).get("dependencies", {})
    gen_deps = tomllib.loads(codegen_manifest.read_text(encoding="utf-8")).get("dependencies", {})
    for gen_name, ws_name in (
        ("prost", "prost"),
        ("prost-types", "prost-types"),
        ("tonic-prost-build", "tonic-prost"),
    ):
        gen_entry = gen_deps.get(gen_name)
        ws_entry = ws_deps.get(ws_name)
        gen_ver = gen_entry.get("version") if isinstance(gen_entry, dict) else gen_entry
        ws_ver = ws_entry.get("version") if isinstance(ws_entry, dict) else ws_entry
        if ws_ver is not None and gen_ver != ws_ver:
            print(
                f"tools/grpc-codegen/Cargo.toml pins {gen_name} to {gen_ver!r}, Cargo.toml pins {ws_name} to {ws_ver!r}",
                file=sys.stderr,
            )
            sys.exit(1)

dependabot = Path(".github/dependabot.yml")
if dependabot.is_file():
    ecosystems = 0
    current_cooldowns: list[str] = []
    in_updates = False
    in_cooldown = False
    for raw_line in dependabot.read_text(encoding="utf-8").splitlines():
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not raw_line.startswith(" "):
            in_updates = stripped == "updates:"
            in_cooldown = False
            continue
        if not in_updates:
            continue
        if re.match(r"^  -\s+package-ecosystem:", raw_line):
            if ecosystems > 0 and current_cooldowns != ["7"]:
                print(
                    f".github/dependabot.yml update entry #{ecosystems} has cooldown.default-days {current_cooldowns!r}, expected ['7']",
                    file=sys.stderr,
                )
                sys.exit(1)
            ecosystems += 1
            current_cooldowns = []
            in_cooldown = False
        elif re.match(r"^    [A-Za-z0-9_-]+:", raw_line):
            in_cooldown = stripped == "cooldown:"
        elif in_cooldown:
            match = re.match(r"^      default-days:\s*(\S+)\s*$", raw_line)
            if match:
                current_cooldowns.append(match.group(1).strip("\"'"))
    if ecosystems == 0:
        print(".github/dependabot.yml declares no package-ecosystem entries", file=sys.stderr)
        sys.exit(1)
    if current_cooldowns != ["7"]:
        print(
            f".github/dependabot.yml update entry #{ecosystems} has cooldown.default-days {current_cooldowns!r}, expected ['7']",
            file=sys.stderr,
        )
        sys.exit(1)
PY
); then
	fail "${cooldown_err}"
fi
echo "tools-check: 7-day publish-age cooldown and generator pins agree across Cargo, npm, dependency-drift, and Dependabot"

echo "tools-check: ${manifest} passed (${#names[@]} pins)"
