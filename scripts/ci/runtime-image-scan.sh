#!/usr/bin/env bash
# Admit one fixed image's binary inventory before native Trivy reporting.
# runtime-image-scan.sh security|sbom|filesystem IMAGE [SBOM_OUTPUT]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
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
cleanup() {
	[[ -z ${container} ]] || docker rm -f "${container}" >/dev/null 2>&1 || true
	rm -rf "${temporary}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

python3 scripts/ci/runtime-image-inventory.py --expectations > "${temporary}/expected"
container=$(docker create --entrypoint /service "${image_id}")
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
docker rm "${container}" >/dev/null
container=
[[ ${mode} != filesystem ]] || exit 0

# All packages and severities survive into admission. Native convert applies the
# existing fixable HIGH/CRITICAL verdict to precisely this admitted scan.
docker run --rm \
	-v /var/run/docker.sock:/var/run/docker.sock \
	-v "${trivy_cache}:/root/.cache/trivy" \
	-v "${temporary}:/work" \
	-e DOCKER_HOST=unix:///var/run/docker.sock \
	-e TRIVY_DB_REPOSITORY \
	"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
	--scanners vuln --list-all-pkgs=true --format json --output /work/native.json "${image_id}"
python3 scripts/ci/runtime-image-inventory.py --report "${temporary}/native.json" --image-id "${image_id}"

if [[ ${mode} == security ]]; then
	docker run --rm --network none -v "${temporary}:/work" "${TRIVY_IMAGE}" convert \
		--quiet --scanners vuln --severity HIGH,CRITICAL --ignore-unfixed --exit-code 1 \
		--format table /work/native.json
else
	docker run --rm --network none -v "${temporary}:/work" "${TRIVY_IMAGE}" convert \
		--quiet --format cyclonedx --output /work/sbom.cdx.json /work/native.json
	# Do not truncate an existing report when scan, admission or conversion fails.
	mv "${temporary}/sbom.cdx.json" "${output}"
fi
