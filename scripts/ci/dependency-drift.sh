#!/usr/bin/env bash
set -euo pipefail

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
findings=()

finding() {
	findings+=("$*")
}

fetch() {
	local auth=()
	[[ $1 != https://api.github.com/* || -z ${GH_TOKEN-} ]] || auth=(-H "Authorization: Bearer ${GH_TOKEN}")
	curl -fsS --retry 3 --max-time 30 -A "${user_agent}" "${auth[@]+"${auth[@]}"}" "$@"
}

# Prints "<version> <age in days>" of the newest stable release, optionally
# within a version line (a prefix such as "3.5.").
latest() {
	local registry=$1 package=$2 line=$3
	case "${registry}" in
	crates)
		fetch "https://crates.io/api/v1/crates/${package}" | jq -r --arg line "${line}" '
			[.versions[] | select((.yanked | not) and (.num | test("-") | not) and (.num | startswith($line)))][0]
			| "\(.num) \((now - (.created_at[:19] + "Z" | fromdateiso8601)) / 86400 | floor)"'
		;;
	npm)
		fetch "https://registry.npmjs.org/${package}" | jq -r '
			."dist-tags".latest as $v
			| "\($v) \((now - (.time[$v][:19] + "Z" | fromdateiso8601)) / 86400 | floor)"'
		;;
	pypi)
		fetch "https://pypi.org/pypi/${package}/json" | jq -r '
			.info.version as $v
			| "\($v) \((now - (.releases[$v][0].upload_time_iso_8601[:19] + "Z" | fromdateiso8601)) / 86400 | floor)"'
		;;
	go)
		fetch "https://proxy.golang.org/${package}/@latest" | jq -r '
			"\(.Version | ltrimstr("v")) \((now - (.Time[:19] + "Z" | fromdateiso8601)) / 86400 | floor)"'
		;;
	github)
		fetch "https://api.github.com/repos/${package}/releases?per_page=50" | jq -r --arg line "${line}" '
			[.[] | select((.draft or .prerelease) | not)
				| .version = (.tag_name | sub("^[^0-9]*"; ""))
				| select(.version | startswith($line))][0]
			| "\(.version) \((now - (.published_at[:19] + "Z" | fromdateiso8601)) / 86400 | floor)"'
		;;
	esac
}

vendor_metadata=$(env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B - <<'PY'
import pathlib
import tomllib

lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text(encoding="utf-8"))
by_name: dict[str, set[str]] = {}
for pkg in lock.get("package", []):
    pkg_name = pkg.get("name")
    if isinstance(pkg_name, str):
        src = "registry" if pkg.get("source") else "path"
        by_name.setdefault(pkg_name, set()).add(src)

unused = bool(lock.get("patch", {}).get("unused"))
print(f"PATCH_UNUSED\t{'1' if unused else '0'}")

for patches in sorted(pathlib.Path("vendor").glob("*/PATCHES.md")):
    manifest = tomllib.loads((patches.parent / "Cargo.toml").read_text(encoding="utf-8"))
    package = manifest.get("package", {})
    name = package.get("name", "")
    version = package.get("version", "")
    if not isinstance(name, str) or not name or not isinstance(version, str) or not version:
        raise SystemExit(f"missing package name or version in {patches.parent / 'Cargo.toml'}")
    sources = ",".join(sorted(by_name.get(name, set())))
    print(f"FORK\t{patches.as_posix()}\t{name}\t{version}\t{sources}")
PY
)

patch_unused=0
while IFS=$'\t' read -r kind patches name version sources; do
	if [[ ${kind} == PATCH_UNUSED ]]; then
		patch_unused=${patches}
		continue
	fi
	reviewed=$(sed -n 's/^Upstream reviewed: \([0-9][^ ]*\)$/\1/p' "${patches}" | head -n1)
	[[ -n ${reviewed} ]] || finding "${name}: ${patches} records no \`Upstream reviewed: <version>\` line"

	# A path package has no source line; a registry copy means the patch no
	# longer applies to some dependent.
	[[ ,${sources}, == *,path,* ]] || finding "${name}: Cargo.lock does not resolve the vendored copy"
	[[ ,${sources}, != *,registry,* ]] || finding "${name}: Cargo.lock also resolves a registry copy, so the patch is bypassed"

	advisories=$(fetch -X POST https://api.osv.dev/v1/query \
		-d "{\"package\":{\"name\":\"${name}\",\"ecosystem\":\"crates.io\"},\"version\":\"${version}\"}" |
		jq -r '[.vulns[]?.id] | join(", ")')
	[[ -z ${advisories} ]] || finding "${name} ${version}: fork base is affected by ${advisories}"

	if [[ ${upstream} == true ]]; then
		read -r newest age < <(latest crates "${name}" '')
		if [[ ${newest} != "${version}" && ${newest} != "${reviewed}" && ${age} -ge ${cooldown_days} ]]; then
			finding "${name}: upstream ${newest} (${age} days old) is unreviewed; fork base ${version}, reviewed ${reviewed:-none}"
		fi
	fi
done <<<"${vendor_metadata}"
[[ ${patch_unused} == 0 ]] || finding "Cargo.lock lists an unused [patch.crates-io] entry"

if [[ ${upstream} == true ]]; then
	# NAME|registry|package|version line held by a recorded decision
	while IFS='|' read -r pin registry package line; do
		value=$(sed -n "s/^${pin}=//p" tools/versions.env)
		[[ -n ${value} ]] || continue # a profile the initializer removed
		current=${value#*:}
		current=${current%%@*}
		current=${current#v}
		read -r newest age < <(latest "${registry}" "${package}" "${line}")
		if [[ ${newest} != "${current}" && ${age} -ge ${cooldown_days} ]]; then
			finding "tools/versions.env ${pin}: ${current} → ${newest} (${age} days old)"
		fi
	done <<'EOF'
CARGO_DENY_VERSION|crates|cargo-deny|
CARGO_NEXTEST_VERSION|crates|cargo-nextest|
CARGO_SHEAR_VERSION|crates|cargo-shear|
ZIZMOR_VERSION|crates|zizmor|
CARGO_MUTANTS_VERSION|crates|cargo-mutants|
SQLX_CLI_VERSION|crates|sqlx-cli|
CARGO_CHEF_VERSION|crates|cargo-chef|
CARGO_AUDITABLE_VERSION|crates|cargo-auditable|
REDOCLY_CLI_VERSION|npm|@redocly/cli|
JSCPD_VERSION|npm|jscpd|
SQUAWK_CLI_VERSION|npm|squawk-cli|
RUFF_VERSION|pypi|ruff|
OASDIFF_VERSION|go|github.com/oasdiff/oasdiff|
GITLEAKS_VERSION|go|github.com/zricethezav/gitleaks/v8|
ACTIONLINT_VERSION|go|github.com/rhysd/actionlint|
BUF_VERSION|go|github.com/bufbuild/buf|
SHELLCHECK_IMAGE|github|koalaman/shellcheck|
TRIVY_IMAGE|github|aquasecurity/trivy|
LYCHEE_IMAGE|github|lycheeverse/lychee|
KEYCLOAK_IMAGE|github|keycloak/keycloak|
COSIGN_VERSION|github|sigstore/cosign|
PROMTOOL_VERSION|github|prometheus/prometheus|3.5.
EOF
fi

if ((${#findings[@]})); then
	printf -- '- %s\n' "${findings[@]}"
	exit 1
fi
echo "dependency-drift: no finding"
