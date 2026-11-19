#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../.."
# shellcheck source=tools/versions.env
. tools/versions.env

repository=$(printf '%s' "${GITHUB_REPOSITORY:?repository is required}" | tr '[:upper:]' '[:lower:]')
owner_type=$(gh api "repos/${repository}" --jq '.owner.type')
case "${owner_type}" in
User) namespace=users ;;
Organization) namespace=orgs ;;
*) printf 'Unsupported repository owner type: %s\n' "${owner_type}" >&2; exit 2 ;;
esac

endpoint="${namespace}/${repository%%/*}/packages/container/${repository#*/}/versions?per_page=100"
images=$(gh api --paginate --slurp "${endpoint}" | jq -er '
	add
	| map({
		digest: .name,
		tags: [.metadata.container.tags[] | select(test("^(main|latest|v[A-Za-z0-9_.-]{0,127}|sha-[0-9a-f]{12})\\z"))]
	})
	| map(select(.tags | length > 0))
	| group_by(.digest)
	| map([.[0].digest, (map(.tags[]) | unique | join(", "))] | @tsv)
	| join("\n")
') || { printf 'Could not inventory ghcr.io/%s.\n' "${repository}" >&2; exit 2; }

summary=${GITHUB_STEP_SUMMARY:?GITHUB_STEP_SUMMARY is required}
printf '### Published image scans for %s\n\n' "\`ghcr.io/${repository}\`" >>"${summary}"
if [[ -z ${images} ]]; then
	printf 'No published digest carries a deployable tag.\n' | tee -a "${summary}"
	exit 0
fi

umask 077
temporary=$(mktemp -d)
trap 'rm -rf "${temporary}"' EXIT
mkdir "${temporary}/cache" "${temporary}/registry"
trivy_run=(docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges
	--user "$(id -u):$(id -g)" --tmpfs '/tmp:rw,noexec,nosuid,nodev' -e HOME=/tmp)
trivy_args=(image --cache-dir /cache --disable-telemetry --skip-version-check --scanners vuln)

printf '%s\n' "${GH_TOKEN:?GH_TOKEN is required}" |
	"${trivy_run[@]}" -i -e DOCKER_CONFIG=/registry -v "${temporary}/registry:/registry" \
		"${TRIVY_IMAGE}" registry login --username "${GITHUB_ACTOR:?GITHUB_ACTOR is required}" --password-stdin ghcr.io
"${trivy_run[@]}" -v "${temporary}/cache:/cache" "${TRIVY_IMAGE}" "${trivy_args[@]}" --download-db-only

finding_count=0
error_count=0
printf '| Digest | Deployable tags | Result |\n| --- | --- | --- |\n' >>"${summary}"
while IFS=$'\t' read -r digest tags; do
	printf '\nScanning ghcr.io/%s@%s (%s)\n' "${repository}" "${digest}" "${tags}"
	status=0
	token=$(openssl rand -hex 16)
	printf '::stop-commands::%s\n' "${token}"
	"${trivy_run[@]}" -e DOCKER_CONFIG=/registry -v "${temporary}/registry:/registry:ro" \
		-v "${temporary}/cache:/cache:ro" "${TRIVY_IMAGE}" "${trivy_args[@]}" \
		--cache-backend memory --skip-db-update --skip-java-db-update --offline-scan \
		--image-src remote --platform linux/amd64 --ignorefile '' --severity HIGH,CRITICAL --exit-code 42 \
		"ghcr.io/${repository}@${digest}" || status=$?
	printf '::%s::\n' "${token}"
	case "${status}" in
	0) result='No HIGH or CRITICAL findings' ;;
	42) result='HIGH or CRITICAL findings require attention'; finding_count=$((finding_count + 1)) ;;
	*) result="Scan failed (exit ${status})"; error_count=$((error_count + 1)) ;;
	esac
	printf '| %s | %s | %s |\n' "\`${digest}\`" "${tags}" "${result}" >>"${summary}"
done <<<"${images}"

printf '%s digests with HIGH or CRITICAL findings. %s failed scans.\n' "${finding_count}" "${error_count}"
((error_count == 0)) || exit 2
((finding_count == 0)) || exit 1
