#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
if ! python3 "${ROOT_DIR}/scripts/verify/lock.py" --assert-held; then
	exec python3 "${ROOT_DIR}/scripts/verify/lock.py" -- bash "${ROOT_DIR}/scripts/image/scan.sh" "$@"
fi
mode=${1:?security, sbom or filesystem mode is required}
image=${2:?runtime image is required}
output=${3:-sbom.cdx.json}
case "${mode}" in security | sbom | filesystem) ;; *)
	echo "unsupported scan mode: ${mode}" >&2
	exit 2
	;;
esac
# shellcheck source=tools/versions.env
. tools/versions.env
trivy_cache=${TRIVY_CACHE_VOLUME:-trivy-cache}

# Tags may move between invocations, never between this invocation's checks.
image_details=$(docker image inspect --format \
	'{{.Id}} {{if index . "Descriptor"}}descriptor{{else}}config{{end}} {{.Os}}/{{.Architecture}}{{with index . "Variant"}}/{{.}}{{end}}' "${image}")
read -r image_id id_kind image_platform <<<"${image_details}"
[[ ${image_id} =~ ^sha256:[0-9a-f]{64}$ && ( ${id_kind} == config || ${id_kind} == descriptor ) ]] || {
	echo "could not resolve a fixed image ID" >&2
	exit 1
}
platform_args=()
if [[ ${mode} != filesystem && ${id_kind} == descriptor ]]; then
	platform_args=(--platform "${DOCKER_DEFAULT_PLATFORM:-${image_platform}}")
fi
umask 077
temporary=$(mktemp -d "${TMPDIR:-/tmp}/runtime-image-scan.XXXXXXXX")
container=
lock_token=${VALIDATION_LOCK_TOKEN:-nolock}
container_name="runtime-image-inventory-${lock_token:0:12}-$(date +%s)-$$"
# shellcheck disable=SC2329
cleanup() {
	local status=$?
	trap - EXIT INT TERM HUP
	if [[ -n ${container} ]]; then
		docker rm -f -v "${container}" >/dev/null 2>&1 || true
	fi
	rm -rf "${temporary}"
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

python3 "${ROOT_DIR}/scripts/image/inventory.py" --expectations >"${temporary}/expected"
container=$(docker create --pull never ${platform_args[@]+"${platform_args[@]}"} \
	--name "${container_name}" --entrypoint /service "${image_id}")
pruned=()
while IFS=$'\t' read -r selection path; do
	case "${selection}" in
	retained)
		if ! docker cp "${container}:${path}" "${temporary}/binary" >/dev/null; then
			echo "${path}: could not copy retained binary from ${image_id}" >&2
			exit 1
		fi
		[[ -f ${temporary}/binary && -s ${temporary}/binary && -x ${temporary}/binary ]] || {
			echo "${path}: retained entrypoint is not a nonempty executable file" >&2
			exit 1
		}
		rm -rf "${temporary}/binary"
		;;
	pruned) pruned+=("${path}") ;;
	*)
		echo "invalid binary expectation: ${selection} ${path}" >&2
		exit 1
		;;
	esac
done <"${temporary}/expected"
if [[ -n ${pruned[0]:-} ]]; then
	docker container inspect --format '{{json .Mounts}}' "${container}" >"${temporary}/mounts.json"
	docker export "${container}" | python3 "${ROOT_DIR}/scripts/image/inventory.py" \
		--rootfs-mounts "${temporary}/mounts.json" --pruned "${pruned[@]}"
fi
docker rm -f -v "${container}" >/dev/null 2>&1 || true
container=
[[ ${mode} != filesystem ]] || exit 0

docker image save ${platform_args[@]+"${platform_args[@]}"} "${image_id}" >"${temporary}/image.tar"
config_id=$(python3 "${ROOT_DIR}/scripts/image/inventory.py" --archive "${temporary}/image.tar")
if [[ ${id_kind} == config && ${config_id} != "${image_id}" ]]; then
	echo "Docker archive configuration does not identify the fixed image ID ${image_id}" >&2
	exit 1
fi
chmod 0444 "${temporary}/image.tar"
trivy_run=(docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges --tmpfs '/tmp:rw,noexec,nosuid,nodev')
"${trivy_run[@]}" \
	-v "${trivy_cache}:/root/.cache/trivy" \
	-e TRIVY_DB_REPOSITORY \
	"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
	--skip-version-check --disable-telemetry --scanners vuln --download-db-only
"${trivy_run[@]}" --network none \
	-v "${trivy_cache}:/root/.cache/trivy:ro" \
	-v "${temporary}/image.tar:/input/image.tar:ro" \
	"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
	--skip-version-check --disable-telemetry --cache-backend memory --skip-db-update --skip-java-db-update \
	--offline-scan --scanners vuln --list-all-pkgs=true --format json --input /input/image.tar >"${temporary}/native.json"

admission=(python3 "${ROOT_DIR}/scripts/image/inventory.py" --report "${temporary}/native.json" --image-id "${config_id}")
if [[ ${mode} == security ]]; then
	fixed_high_critical=$("${admission[@]}" --count-fixed-high-critical)
	"${trivy_run[@]}" --network none -i "${TRIVY_IMAGE}" convert \
		--quiet --scanners vuln --severity HIGH,CRITICAL --format table /dev/stdin <"${temporary}/native.json"
	if [[ ${fixed_high_critical} != 0 ]]; then
		echo "Security scan found ${fixed_high_critical} fixed HIGH/CRITICAL vulnerabilities in ${image_id}" >&2
		exit 1
	fi
else
	"${admission[@]}"
	"${trivy_run[@]}" --network none -i "${TRIVY_IMAGE}" convert \
		--quiet --format cyclonedx /dev/stdin <"${temporary}/native.json" >"${temporary}/sbom.cdx.json"
	# Do not truncate an existing report when scan, admission or conversion fails.
	mv "${temporary}/sbom.cdx.json" "${output}"
fi
