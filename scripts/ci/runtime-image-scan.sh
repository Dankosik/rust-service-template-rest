#!/usr/bin/env bash
# Admit one fixed image's binary inventory before native Trivy reporting.
# runtime-image-scan.sh security|sbom|filesystem IMAGE [SBOM_OUTPUT]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
if ! bash "${root}/scripts/ci/validation-lock.sh" --assert-held; then
	exec bash "${root}/scripts/ci/validation-lock.sh" -- bash "${root}/scripts/ci/runtime-image-scan.sh" "$@"
fi
mode=${1:?security, sbom or filesystem mode is required}
image=${2:?runtime image is required}
output=${3:-sbom.cdx.json}
case "${mode}" in security | sbom | filesystem) ;; *) echo "unsupported scan mode: ${mode}" >&2; exit 2 ;; esac
# shellcheck source=tools/versions.env
. tools/versions.env
trivy_cache=${TRIVY_CACHE_VOLUME:-trivy-cache}

# Tags may move between invocations, never between this invocation's checks.
image_id=$(docker image inspect --format '{{.Id}}' "${image}")
[[ ${image_id} == sha256:* ]] || { echo "could not resolve a fixed image ID" >&2; exit 1; }
umask 077
temporary=$(mktemp -d "${TMPDIR:-/tmp}/runtime-image-scan.XXXXXXXX")
container=
resource=
container_name="runtime-image-inventory-${VALIDATION_LOCK_TOKEN:0:12}-$(date +%s)-$$"
# shellcheck disable=SC2329
cleanup() {
	local status=$?
	trap - EXIT INT TERM
	if [[ -n ${resource} ]]; then
		if ! bash "${root}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}"; then
			echo "validation image inventory cleanup incomplete: ${resource}" >&2
			if [[ ${status} == 0 ]]; then status=1; fi
		fi
	fi
	rm -rf "${temporary}"
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

python3 scripts/ci/runtime-image-inventory.py --expectations > "${temporary}/expected"
resource=$(bash "${root}/scripts/ci/validation-lock.sh" --resource-register container "${container_name}")
container=$(bash "${root}/scripts/ci/validation-lock.sh" --resource-run "${resource}" -- \
	docker create --name "${container_name}" \
	--label "dev.rust-service.validation-owner=${VALIDATION_LOCK_TOKEN}" --entrypoint /service "${image_id}")
bash "${root}/scripts/ci/validation-lock.sh" --resource-bind "${resource}" "${container}"
while IFS=$'\t' read -r selection path; do
	copied=false
	if docker cp "${container}:${path}" "${temporary}/binary" >/dev/null 2>&1; then copied=true; fi
	case "${selection}:${copied}" in
	retained:true)
		[[ -f ${temporary}/binary && -s ${temporary}/binary && -x ${temporary}/binary ]] || {
			echo "${path}: retained entrypoint is not a nonempty executable file" >&2; exit 1;
		}
		;;
	retained:false) echo "${path}: retained binary is absent from ${image_id}" >&2; exit 1 ;;
	pruned:true) echo "${path}: pruned binary is present in ${image_id}" >&2; exit 1 ;;
	pruned:false) ;;
	*) echo "invalid binary expectation: ${selection} ${path}" >&2; exit 1 ;;
	esac
	rm -rf "${temporary}/binary"
done < "${temporary}/expected"
bash "${root}/scripts/ci/validation-lock.sh" --resource-cleanup "${resource}"
resource=
container=
[[ ${mode} != filesystem ]] || exit 0

# All packages and severities survive into admission. Trivy 0.74 supports
# --ignore-unfixed only on image, where it filters vulnerabilities, not Packages.
# Security mode retains the existing fixable-only policy; SBOM keeps all statuses.
scan_policy=(--list-all-pkgs=true)
if [[ ${mode} == security ]]; then scan_policy+=(--ignore-unfixed); fi
bash "${root}/scripts/ci/validation-lock.sh" --container-run -- docker run --rm \
	-v /var/run/docker.sock:/var/run/docker.sock \
	-v "${trivy_cache}:/root/.cache/trivy" \
	-v "${temporary}:/work" \
	-e DOCKER_HOST=unix:///var/run/docker.sock \
	-e TRIVY_DB_REPOSITORY \
	"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
	--scanners vuln "${scan_policy[@]}" --format json --output /work/native.json "${image_id}"
python3 scripts/ci/runtime-image-inventory.py --report "${temporary}/native.json" --image-id "${image_id}"

if [[ ${mode} == security ]]; then
	bash "${root}/scripts/ci/validation-lock.sh" --container-run -- docker run --rm --network none -v "${temporary}:/work" "${TRIVY_IMAGE}" convert \
		--quiet --scanners vuln --severity HIGH,CRITICAL --exit-code 1 \
		--format table /work/native.json
else
	bash "${root}/scripts/ci/validation-lock.sh" --container-run -- docker run --rm --network none -v "${temporary}:/work" "${TRIVY_IMAGE}" convert \
		--quiet --format cyclonedx --output /work/sbom.cdx.json /work/native.json
	# Do not truncate an existing report when scan, admission or conversion fails.
	mv "${temporary}/sbom.cdx.json" "${output}"
fi
