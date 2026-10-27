#!/usr/bin/env bash
# Dependency drift that Dependabot and cargo-deny cannot see. Each vendor/<crate>
# is a [patch.crates-io] path dependency: Dependabot does not update it and
# cargo-deny matches advisories only against registry sources, so a stale or
# vulnerable fork base is invisible to both. tools/versions.env has no
# Dependabot ecosystem either.
#
# Every run, per fork: the lock resolves the vendored copy and no registry copy
# (a dependent that outgrows the fork's version silently drops the patch), OSV
# lists no advisory for the fork's base version, and PATCHES.md records the
# last upstream release reviewed against it ("Upstream reviewed: <version>").
#
# --upstream adds the moving targets: a fork whose upstream released a version
# that is neither its base nor its reviewed version, and a tools/versions.env
# pin behind its upstream release. A release younger than the Dependabot
# cooldown waits for the next run. Prints a Markdown report; exits 1 on any
# finding. The weekly dependency-drift workflow files the report as an issue.
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

for patches in vendor/*/PATCHES.md; do
	dir=${patches%/PATCHES.md}
	name=$(sed -n 's/^name = "\(.*\)"$/\1/p' "${dir}/Cargo.toml" | head -n1)
	version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "${dir}/Cargo.toml" | head -n1)
	reviewed=$(sed -n 's/^Upstream reviewed: \([0-9][^ ]*\)$/\1/p' "${patches}" | head -n1)
	[[ -n ${reviewed} ]] || finding "${name}: ${patches} records no \`Upstream reviewed: <version>\` line"

	# A path package has no source line; a registry copy means the patch no
	# longer applies to some dependent.
	sources=$(awk -v name="${name}" '
		/^\[/ { if (hit) print source; hit = 0; package = $0 == "[[package]]"; source = "path" }
		package && $0 == "name = \"" name "\"" { hit = 1 }
		/^source = / { source = $3 }
		END { if (hit) print source }' Cargo.lock)
	grep -qx path <<<"${sources}" || finding "${name}: Cargo.lock does not resolve the vendored copy"
	! grep -qv '^path$' <<<"${sources}" || finding "${name}: Cargo.lock also resolves a registry copy, so the patch is bypassed"

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
done
! grep -q '^\[\[patch.unused\]\]$' Cargo.lock || finding "Cargo.lock lists an unused [patch.crates-io] entry"

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
CARGO_SHEAR_VERSION|crates|cargo-shear|
ZIZMOR_VERSION|crates|zizmor|
SCCACHE_VERSION|crates|sccache|
SQLX_CLI_VERSION|crates|sqlx-cli|
CARGO_CHEF_VERSION|crates|cargo-chef|
CARGO_AUDITABLE_VERSION|crates|cargo-auditable|
REDOCLY_CLI_VERSION|npm|@redocly/cli|
JSCPD_VERSION|npm|jscpd|
SQUAWK_CLI_VERSION|npm|squawk-cli|
OASDIFF_VERSION|go|github.com/oasdiff/oasdiff|
GITLEAKS_VERSION|go|github.com/zricethezav/gitleaks/v8|
ACTIONLINT_VERSION|go|github.com/rhysd/actionlint|
NATS_CLI_VERSION|go|github.com/nats-io/natscli|
BUF_VERSION|go|github.com/bufbuild/buf|
SHELLCHECK_IMAGE|github|koalaman/shellcheck|
TRIVY_IMAGE|github|aquasecurity/trivy|
LYCHEE_IMAGE|github|lycheeverse/lychee|
COSIGN_VERSION|github|sigstore/cosign|
PROMTOOL_VERSION|github|prometheus/prometheus|3.5.
EOF
fi

if ((${#findings[@]})); then
	printf -- '- %s\n' "${findings[@]}"
	exit 1
fi
echo "dependency-drift: no finding"
