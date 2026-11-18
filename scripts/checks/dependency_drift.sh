#!/usr/bin/env bash
set -euo pipefail

fail() {
	printf 'dependency-drift: %s\n' "$*" >&2
	exit 2
}

trap 'fail "check failed at line ${LINENO}"' ERR

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"

upstream=false
case "${1-}" in
--upstream) upstream=true ;;
'') ;;
*)
	printf 'usage: %s [--upstream]\n' "$0" >&2
	exit 2
	;;
esac

# Matches cooldown.default-days in .github/dependabot.yml.
cooldown_days=7
user_agent='rust-service-template dependency-drift'
single_json='if length == 1 then .[0] else error("expected one JSON document") end'
version_pattern='\A[0-9]+\.[0-9]+\.[0-9]+([.+_-][0-9A-Za-z.+_-]+)?\z'
findings=()

finding() {
	findings+=("$*")
}

fetch() {
	local url=$1
	shift
	local auth=()
	[[ ${url} != https://api.github.com/* || -z ${GH_TOKEN-} ]] || auth=(-H "Authorization: Bearer ${GH_TOKEN}")
	curl -fsS --compressed --retry 3 --max-time 30 -A "${user_agent}" "${auth[@]+"${auth[@]}"}" "$@" "${url}" ||
		fail "request failed for ${url}"
}

crate_versions() {
	local name=$1
	fetch "https://crates.io/api/v1/crates/${name}" |
		jq -ces --arg name "${name}" --arg version_pattern "${version_pattern}" "${single_json}"' |
		if .crate.name == $name and (.versions | type == "array" and length > 0)
			and all(.versions[]; (.num | type == "string" and test($version_pattern)) and (.yanked | type == "boolean"))
		then .versions
		else error("invalid crate version list")
		end'
}

release_metadata() {
	local registry=$1 selector=${2-}
	jq -ers --arg registry "${registry}" --arg selector "${selector}" --arg version_pattern "${version_pattern}" '
		def release:
			if $registry == "crates" then
				first(.[] | select(.yanked == false and (.num | test("-") | not) and (.num | startswith($selector))))
				| [.num, .created_at]
			elif $registry == "npm" then
				."dist-tags".latest as $version | [$version, .time[$version]]
			elif $registry == "pypi" then
				[.info.version, .urls[0].upload_time_iso_8601]
			elif $registry == "go" then
				[(.Version | ltrimstr("v")), .Time]
			elif $registry == "go-toolchain" then
				select(.Version == $selector)
				| [(.Version | capture("-go(?<v>[0-9]+\\.[0-9]+\\.[0-9]+)\\.").v), .Time]
			elif $registry == "github" then
				if $selector == "" then
					if type == "object" then [.] else error("invalid latest GitHub release") end
				else . end
				| if type != "array" or any(.[]; (.draft | type != "boolean")
					or (.prerelease | type != "boolean") or (.tag_name | type != "string"))
				then error("invalid GitHub release list") else . end
				| first(.[] | select(.draft == false and .prerelease == false)
					| (.tag_name | sub("^[^0-9]*"; "")) as $version
					| select(($version | length > 0) and ($version | startswith($selector)))
					| [$version, .published_at])
			else error("unknown release registry")
			end;
		'"${single_json}"' | release as [$version, $published]
		| if ($version | type == "string" and test($version_pattern))
			and ($published | type == "string" and test("\\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?Z\\z"))
		then ($published[:19] + "Z") as $utc
			| ($utc | fromdateiso8601) as $time
			| if ($time | todateiso8601) == $utc then "\($version) \((now - $time) / 86400 | floor)"
			  else error("invalid release publish time") end
		else error("missing or invalid release version or publish time")
		end'
}

# Prints "<version> <age in days>" of the newest stable release, optionally
# within a version line (a prefix such as "3.5.").
latest() {
	local registry=$1 package=$2 line=$3 url mod_ver
	case "${registry}" in
	crates)
		crate_versions "${package}" | release_metadata "${registry}" "${line}"
		return
		;;
	npm) url="https://registry.npmjs.org/${package}" ;;
	pypi) url="https://pypi.org/pypi/${package}/json" ;;
	go) url="https://proxy.golang.org/${package}/@latest" ;;
	go-toolchain)
		mod_ver=$(fetch "https://proxy.golang.org/${package}/@v/list" |
			awk -v prefix="v0.0.1-go${line}" 'index($0, prefix) == 1 && /^v0\.0\.1-go[0-9]+\.[0-9]+\.[0-9]+\.linux-amd64$/' |
			sort -V | tail -n1) || fail "could not list Go toolchain releases for ${package}"
		[[ -n ${mod_ver} ]] || fail "no stable Go toolchain release for ${package}"
		url="https://proxy.golang.org/${package}/@v/${mod_ver}.info"
		line=${mod_ver}
		;;
	github)
		url="https://api.github.com/repos/${package}/releases"
		if [[ -z ${line} ]]; then
			url+=/latest
		else
			url+='?per_page=50'
		fi
		;;
	*) fail "unknown release registry ${registry}" ;;
	esac
	fetch "${url}" | release_metadata "${registry}" "${line}"
}

vendor_metadata=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B - <<'PY'
import pathlib
import re
import tomllib

workspace = tomllib.loads(pathlib.Path("Cargo.toml").read_text(encoding="utf-8"))
lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text(encoding="utf-8"))
by_name: dict[str, list[dict]] = {}
for package in lock.get("package", []):
    by_name.setdefault(package["name"], []).append(package)

if lock.get("patch", {}).get("unused"):
    print("FINDING\tCargo.lock lists an unused [patch.crates-io] entry")

codegen_manifest = pathlib.Path("tools/grpc-codegen/Cargo.toml")
if codegen_manifest.is_file():
    ws_deps = workspace.get("workspace", {}).get("dependencies", {})
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
            print(f"FINDING\t{codegen_manifest} pins {gen_name} to {gen_ver}, Cargo.toml pins {ws_name} to {ws_ver}")

for key, patch in sorted(workspace.get("patch", {}).get("crates-io", {}).items()):
    path = patch.get("path") if isinstance(patch, dict) else None
    if not isinstance(path, str) or not path or any(char in path for char in "\t\r\n"):
        raise SystemExit(f"[patch.crates-io] {key} must specify a local path")
    directory = pathlib.Path(path).resolve()
    if not directory.is_relative_to(pathlib.Path.cwd()):
        raise SystemExit(f"[patch.crates-io] {key} points outside the repository: {path}")
    manifest = pathlib.Path(path) / "Cargo.toml"
    patches = manifest.with_name("PATCHES.md")
    for file in (manifest, patches):
        if not file.is_file():
            raise SystemExit(f"[patch.crates-io] {key} requires {file}")
    package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
    name = package.get("name")
    version = package.get("version")
    if name != patch.get("package", key):
        raise SystemExit(f"[patch.crates-io] {key} does not match the package name in {manifest}")
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name):
        raise SystemExit(f"invalid package name in {manifest}")
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.+-]+)?", version):
        raise SystemExit(f"invalid package version in {manifest}")

    locked = by_name.get(name, [])
    if not any(pkg["version"] == version and "source" not in pkg for pkg in locked):
        print(f"FINDING\t{name}: Cargo.lock does not resolve the vendored version {version}")
    if any("source" in pkg for pkg in locked):
        print(f"FINDING\t{name}: Cargo.lock also resolves another source, so the patch is bypassed")
    print(f"FORK\t{patches}\t{name}\t{version}")
PY
) || fail "could not validate local patch metadata"

while IFS=$'\t' read -r kind patches name version; do
	if [[ ${kind} == FINDING ]]; then
		finding "${patches}"
		continue
	fi
	[[ ${kind} == FORK ]] || continue
	reviewed=$(sed -n 's/^Upstream reviewed: \([0-9][^ ]*\)$/\1/p' "${patches}" | head -n1)
	[[ -n ${reviewed} ]] || finding "${name}: ${patches} records no \`Upstream reviewed: <version>\` line"

	advisories=$(fetch https://api.osv.dev/v1/query -H 'Content-Type: application/json' \
		-d "{\"package\":{\"name\":\"${name}\",\"ecosystem\":\"crates.io\"},\"version\":\"${version}\"}" |
		jq -ers "${single_json}"' |
			if . == {} then ""
			elif type == "object" and has("next_page_token") then error("incomplete OSV response: more pages")
			elif type == "object" and keys == ["vulns"] and (.vulns | type == "array")
				and all(.vulns[]; .id | type == "string" and test("\\A[A-Za-z0-9][A-Za-z0-9._-]*\\z"))
			then [.vulns[].id] | join(", ")
			else error("invalid OSV response")
			end') || fail "could not check OSV for ${name} ${version}"
	[[ -z ${advisories} ]] || finding "${name} ${version}: fork base is affected by ${advisories}"

	versions=$(crate_versions "${name}") || fail "could not check crates.io for ${name} ${version}"
	yanked=$(jq -er --arg ver "${version}" '
		[.[] | select(.num == $ver)] as $matches
		| if ($matches | length) == 1 then $matches[0].yanked | tostring
		elif ($matches | length) == 0 then "missing"
		else error("duplicate version records")
		end' <<<"${versions}") || fail "invalid crates.io result for ${name} ${version}"
	case "${yanked}" in
	false) ;;
	true) finding "${name} ${version}: fork base is yanked on crates.io" ;;
	*) finding "${name} ${version}: fork base version is absent on crates.io" ;;
	esac

	if [[ ${upstream} == true ]]; then
		release=$(release_metadata crates <<<"${versions}") || fail "could not determine latest crates.io release for ${name}"
		read -r newest age <<<"${release}"
		if [[ ${newest} != "${version}" && ${newest} != "${reviewed}" && ${age} -ge ${cooldown_days} ]]; then
			finding "${name}: upstream ${newest} (${age} days old) is unreviewed; fork base ${version}, reviewed ${reviewed:-none}"
		fi
	fi
done <<<"${vendor_metadata}"

if [[ ${upstream} == true ]]; then
	# NAME|registry|package|version line held by a recorded decision
	while IFS='|' read -r pin registry package line; do
		value=$(sed -n "s/^${pin}=//p" tools/versions.env)
		[[ -n ${value} ]] || continue # a profile the initializer removed
		current=${value#*:}
		current=${current%%@*}
		current=${current#v}
		release=$(latest "${registry}" "${package}" "${line}") ||
			fail "could not determine latest release for ${pin} (${registry}: ${package})"
		read -r newest age <<<"${release}"
		if [[ ${newest} != "${current}" && ${age} -ge ${cooldown_days} ]]; then
			finding "tools/versions.env ${pin}: ${current} → ${newest} (${age} days old)"
		fi
	done <<'EOF'
CARGO_DENY_VERSION|crates|cargo-deny|
CARGO_NEXTEST_VERSION|crates|cargo-nextest|
CARGO_SHEAR_VERSION|crates|cargo-shear|
TYPOS_VERSION|crates|typos-cli|
ZIZMOR_VERSION|crates|zizmor|
CARGO_MUTANTS_VERSION|crates|cargo-mutants|
SQLX_CLI_VERSION|crates|sqlx-cli|
CARGO_CHEF_VERSION|crates|cargo-chef|
CARGO_AUDITABLE_VERSION|crates|cargo-auditable|
CARGO_ABOUT_VERSION|crates|cargo-about|
REDOCLY_CLI_VERSION|npm|@redocly/cli|
JSCPD_VERSION|npm|jscpd|
SQUAWK_CLI_VERSION|npm|squawk-cli|
RUFF_VERSION|pypi|ruff|
GO_VERSION|go-toolchain|golang.org/toolchain|
OASDIFF_VERSION|go|github.com/oasdiff/oasdiff|
GITLEAKS_VERSION|go|github.com/zricethezav/gitleaks/v8|
ACTIONLINT_VERSION|go|github.com/rhysd/actionlint|
BUF_VERSION|go|github.com/bufbuild/buf|
SHELLCHECK_IMAGE|github|koalaman/shellcheck|
TRIVY_IMAGE|github|aquasecurity/trivy|
LYCHEE_IMAGE|github|lycheeverse/lychee|
KEYCLOAK_IMAGE|github|keycloak/keycloak|
COSIGN_VERSION|github|sigstore/cosign|
BUILDKIT_IMAGE|github|moby/buildkit|
BUILDX_VERSION|github|docker/buildx|
PROMTOOL_VERSION|github|prometheus/prometheus|3.5.
EOF
fi

if ((${#findings[@]})); then
	printf -- '- %s\n' "${findings[@]}"
	exit 1
fi
echo "dependency-drift: no finding"
