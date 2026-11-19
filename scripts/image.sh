#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"

# shellcheck source=tools/versions.env
. tools/versions.env

usage() {
	echo "usage: $0 dockerfile-check | build [image] | check <image> [expected-commit] | scan <image> | sbom <image> [output]" >&2
	exit 2
}

builder_args=()
[[ -z ${VALIDATION_BUILDER_NAME:-} ]] || builder_args=(--builder "${VALIDATION_BUILDER_NAME}")

resolve_service_meta() {
	python3 -B -c '
import tomllib
with open("crates/service/Cargo.toml", "rb") as f:
    pkg = tomllib.load(f).get("package", {})
name = pkg["name"]
ver = pkg["version"]
if isinstance(ver, dict) and ver.get("workspace") is True:
    with open("Cargo.toml", "rb") as f:
        ver = tomllib.load(f)["workspace"]["package"]["version"]
print(f"{name}\t{ver}")
'
}

cmd_dockerfile_check() {
	python3 -B -c '
import re, sys, tomllib
from pathlib import Path

toolchain = tomllib.loads(Path("rust-toolchain.toml").read_text())["toolchain"]["channel"]
versions = {}
for line in Path("tools/versions.env").read_text().splitlines():
    s = line.strip()
    if s and not s.startswith("#") and "=" in s:
        k, v = s.split("=", 1)
        versions[k] = v

dockerfile = Path("build/docker/Dockerfile").read_text()
m = re.search(r"^FROM\s+rust:([0-9]+\.[0-9]+\.[0-9]+)-", dockerfile, re.M)
found = m.group(1) if m else "<missing>"
if found != toolchain:
    sys.exit(f"Dockerfile FROM rust:{found} disagrees with rust-toolchain.toml ({toolchain})")

for name, default in re.findall(r"^ARG\s+([A-Z0-9_]+)=([^\s#]+)", dockerfile, re.M):
    if name in versions and versions[name] != default:
        sys.exit(f"Dockerfile ARG {name}={default} disagrees with tools/versions.env ({versions[name]})")
'
	docker buildx build ${builder_args[@]+"${builder_args[@]}"} --check -f build/docker/Dockerfile .
}

cmd_build() {
	local image=${1:-service:ci}
	local vcs_ref source_date_epoch meta service_package app_version
	vcs_ref=${VCS_REF:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}
	source_date_epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}
	meta=$(resolve_service_meta)
	service_package=${SERVICE_PACKAGE:-${meta%%$'\t'*}}
	app_version=${APP_VERSION:-${meta#*$'\t'}}

	local args=(
		--build-arg "CARGO_CHEF_VERSION=${CARGO_CHEF_VERSION}"
		--build-arg "CARGO_CHEF_X86_64_SHA256=${CARGO_CHEF_X86_64_SHA256}"
		--build-arg "CARGO_CHEF_AARCH64_SHA256=${CARGO_CHEF_AARCH64_SHA256}"
		--build-arg "CARGO_AUDITABLE_VERSION=${CARGO_AUDITABLE_VERSION}"
		--build-arg "CARGO_AUDITABLE_X86_64_SHA256=${CARGO_AUDITABLE_X86_64_SHA256}"
		--build-arg "CARGO_AUDITABLE_AARCH64_SHA256=${CARGO_AUDITABLE_AARCH64_SHA256}"
		--build-arg "SERVICE_PACKAGE=${service_package}"
		--build-arg "SERVICE_BIN=${SERVICE_BIN:-${service_package}}"
		--build-arg "APP_VERSION=${app_version}"
		--build-arg "VCS_REF=${vcs_ref}"
		--build-arg "SOURCE_URL=${SOURCE_URL:-}"
		--build-arg "SOURCE_DATE_EPOCH=${source_date_epoch}"
		--build-arg "IMAGE_TESTS=${IMAGE_TESTS:-none}"
		--build-arg "PGO=${PGO:-none}"
		-f build/docker/Dockerfile
	)
	[[ -z ${RUNTIME_IMAGE_CACHE_FROM:-} ]] || args+=(--cache-from "${RUNTIME_IMAGE_CACHE_FROM}")
	if [[ -n ${RUNTIME_IMAGE_CACHE_TO:-} ]]; then
		docker buildx build ${builder_args[@]+"${builder_args[@]}"} "${args[@]}" --target cooked --cache-to "${RUNTIME_IMAGE_CACHE_TO}" .
	fi
	[[ -z ${RUNTIME_IMAGE_METADATA_FILE:-} ]] || args+=(--metadata-file "${RUNTIME_IMAGE_METADATA_FILE}")
	docker buildx build ${builder_args[@]+"${builder_args[@]}"} --load "${args[@]}" -t "${image}" .
}

assert_allocator_options() {
	local container=$1 binary=$2 expected='background_thread:true,thp:never'
	[[ $(docker cp "${container}:${binary}" - | tar -xO | grep -aFc "${expected}") -ge 1 ]] || {
		echo "${binary} does not carry jemalloc options ${expected}" >&2
		exit 1
	}
}

check_expected_binaries() {
	local image_id=$1 tmp container
	tmp=$(mktemp -d "${TMPDIR:-/tmp}/image-bins.XXXXXXXX")
	container="image-bins-check-$$-$(date +%s)"
	trap 'docker rm -f -v "${container}" >/dev/null 2>&1 || true; rm -rf -- "${tmp}"' RETURN
	docker create --pull never --name "${container}" --entrypoint /service "${image_id}" >/dev/null
	local retained=(/service) pruned=()
	if [[ -d crates/migrate ]]; then retained+=(/migrate); else pruned+=(/migrate); fi
	if [[ -d crates/jobs-worker ]]; then retained+=(/jobs-worker); else pruned+=(/jobs-worker); fi

	local bin
	for bin in "${retained[@]}"; do
		docker cp "${container}:${bin}" "${tmp}/bin" >/dev/null || { echo "missing retained binary ${bin}" >&2; exit 1; }
		[[ -f ${tmp}/bin && -s ${tmp}/bin && -x ${tmp}/bin ]] || { echo "${bin} is not an executable file" >&2; exit 1; }
		rm -f "${tmp}/bin"
	done
	for bin in "${pruned[@]}"; do
		if docker cp "${container}:${bin}" "${tmp}/bin" >/dev/null 2>&1; then
			echo "pruned binary unexpectedly present in image: ${bin}" >&2
			exit 1
		fi
	done
}

cmd_check() {
	local image=${1:?runtime image is required}
	local expected_commit=${2:-}
	command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 2; }
	command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

	local image_id
	image_id=$(docker image inspect --format '{{.Id}}' "${image}")
	check_expected_binaries "${image_id}"

	CHECK_CONTAINER="service-runtime-check-$$-$(date +%s)"
	CHECK_WORKER=""
	# shellcheck disable=SC2329
	cleanup_check() {
		local status=$?
		trap - EXIT INT TERM HUP
		[[ -z ${CHECK_WORKER:-} ]] || docker rm -f -v "${CHECK_WORKER}" >/dev/null 2>&1 || true
		[[ -z ${CHECK_CONTAINER:-} ]] || docker rm -f -v "${CHECK_CONTAINER}" >/dev/null 2>&1 || true
		exit "${status}"
	}
	trap cleanup_check EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	trap 'exit 129' HUP

	local container=${CHECK_CONTAINER}
	local docker_args=(--label "runtime-check=${container}")
	[[ -z ${RUNTIME_IMAGE_NETWORK:-} ]] || docker_args+=(--network "${RUNTIME_IMAGE_NETWORK}")
	if [[ -n ${RUNTIME_IMAGE_POSTGRES_DSN:-} ]]; then
		docker_args+=(
			-e APP__POSTGRES__ENABLED=true
			-e "APP__APP__ALLOW_INSECURE_DEVELOPMENT=${RUNTIME_IMAGE_ALLOW_INSECURE_DEVELOPMENT:-false}"
			-e "APP__POSTGRES__DSN=${RUNTIME_IMAGE_POSTGRES_DSN}"
		)
		[[ -z ${RUNTIME_IMAGE_APP_ENV:-} ]] || docker_args+=(-e "APP__APP__ENV=${RUNTIME_IMAGE_APP_ENV}")
	fi

	docker run -d --name "${container}" \
		-p 127.0.0.1::8080 \
		--read-only --cap-drop=ALL --security-opt=no-new-privileges \
		"${docker_args[@]}" "${image_id}" >/dev/null

	docker cp "${container}:/usr/share/licenses/service/LICENSE" - | tar -xO | grep -q '[^[:space:]]' || {
		echo "runtime image missing LICENSE" >&2; exit 1
	}
	docker cp "${container}:/usr/share/licenses/service/THIRD-PARTY-RUST.txt" - | tar -xO | grep -qFx 'Copyright (c) Tokio Contributors' || {
		echo "runtime image missing THIRD-PARTY-RUST.txt notices" >&2; exit 1
	}

	local address port ready=false
	address=$(docker port "${container}" 8080/tcp 2>/dev/null | head -n 1 || true)
	port=${address##*:}
	[[ -n ${port} ]] || { docker logs "${container}" >&2; echo "failed to resolve service port" >&2; exit 1; }

	for _ in {1..45}; do
		if curl -fs --max-time 2 "http://127.0.0.1:${port}/health/ready" >/dev/null; then
			ready=true
			break
		fi
		[[ $(docker inspect -f '{{.State.Running}}' "${container}") == true ]] || break
		sleep 1
	done
	[[ ${ready} == true ]] || { docker logs "${container}" >&2; echo "runtime image did not become ready" >&2; exit 1; }

	local logs starting
	logs=$(docker logs "${container}" 2>&1)
	starting=$(printf '%s\n' "${logs}" | jq -c 'fromjson? | select(type == "object" and .message == "service_starting")' | head -n 1)
	[[ -n ${starting} ]] || { printf '%s\n' "${logs}" >&2; echo "missing service_starting log" >&2; exit 1; }
	if [[ -n ${expected_commit} ]]; then
		[[ $(jq -r '.["app.commit"] // ""' <<<"${starting}") == "${expected_commit}" ]] || {
			echo "app.commit mismatch: expected ${expected_commit}, got ${starting}" >&2; exit 1
		}
	fi
	if [[ -n ${RUNTIME_IMAGE_POSTGRES_DSN:-} ]]; then
		printf '%s\n' "${logs}" | jq -e 'fromjson? | select(type == "object" and .message == "postgres_pool_opened")' >/dev/null || {
			printf '%s\n' "${logs}" >&2; echo "runtime image did not open postgres pool" >&2; exit 1
		}
	fi
	echo "runtime image ready: $(jq -c '{version: .["app.version"], commit: .["app.commit"]}' <<<"${starting}")"

	assert_allocator_options "${container}" /service
	docker run --rm --read-only --network none --cap-drop=ALL --security-opt=no-new-privileges \
		--user 65532:65532 --pid="container:${container}" --entrypoint /bin/sh "${BUILDKIT_IMAGE}" \
		-c 'grep -Fxq jemalloc_bg_thd /proc/[0-9]*/task/*/comm' || {
		echo "could not verify jemalloc background thread" >&2; exit 1
	}

	local started elapsed exit_code
	started=$(date +%s)
	docker stop --time 45 "${container}" >/dev/null
	elapsed=$(($(date +%s) - started))
	exit_code=$(docker inspect -f '{{.State.ExitCode}}' "${container}")
	[[ ${exit_code} == 0 ]] || { docker logs "${container}" >&2; echo "non-zero SIGTERM exit (${exit_code})" >&2; exit 1; }
	echo "runtime image stopped cleanly in ${elapsed}s"

	if [[ -d crates/jobs-worker ]]; then
		CHECK_WORKER="${container}-jobs-worker"
		local worker=${CHECK_WORKER}
		docker create --name "${worker}" --read-only --cap-drop=ALL --security-opt=no-new-privileges \
			--network none --entrypoint /jobs-worker "${image_id}" >/dev/null
		assert_allocator_options "${worker}" /jobs-worker
		local worker_out worker_exit expected_msg
		worker_out=$(docker start --attach "${worker}" 2>&1 || true)
		worker_exit=$(docker inspect -f '{{.State.ExitCode}}' "${worker}")
		expected_msg="is registered: register this service's retained capabilities"
		if [[ -d crates/infra-webhooks || -f crates/infra-messaging/src/outbox.rs ]]; then
			expected_msg='postgres.enabled must be true to run the jobs worker'
		fi
		if [[ ${worker_exit} != 1 ]] || ! grep -Fq "${expected_msg}" <<<"${worker_out}"; then
			printf '%s\n' "${worker_out}" >&2
			echo "jobs-worker did not give expected startup refusal (exit ${worker_exit})" >&2
			exit 1
		fi
		echo "jobs-worker refused before dependency I/O"
	fi
}

cmd_trivy() {
	local mode=$1 image=${2:?runtime image is required} output=${3:-sbom.cdx.json}
	command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }
	local image_id trivy_cache=${TRIVY_CACHE_VOLUME:-trivy-cache}
	image_id=$(docker image inspect --format '{{.Id}}' "${image}")
	check_expected_binaries "${image_id}"

	umask 077
	local tmp
	tmp=$(mktemp -d "${TMPDIR:-/tmp}/image-scan.XXXXXXXX")
	trap 'rm -rf -- "${tmp}"' RETURN

	docker image save "${image_id}" >"${tmp}/image.tar"
	chmod 0444 "${tmp}/image.tar"

	local trivy_run=(docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges --tmpfs '/tmp:rw,noexec,nosuid,nodev')
	"${trivy_run[@]}" -v "${trivy_cache}:/root/.cache/trivy" -e TRIVY_DB_REPOSITORY \
		"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
		--skip-version-check --disable-telemetry --scanners vuln --download-db-only
	"${trivy_run[@]}" --network none \
		-v "${trivy_cache}:/root/.cache/trivy:ro" \
		-v "${tmp}/image.tar:/input/image.tar:ro" \
		"${TRIVY_IMAGE}" image --cache-dir /root/.cache/trivy --quiet \
		--skip-version-check --disable-telemetry --cache-backend memory --skip-db-update --skip-java-db-update \
		--offline-scan --scanners vuln --list-all-pkgs=true --format json --input /input/image.tar >"${tmp}/native.json"

	local expected_bins=(service)
	[[ ! -d crates/migrate ]] || expected_bins+=(migrate)
	[[ ! -d crates/jobs-worker ]] || expected_bins+=(jobs-worker)
	local bin
	for bin in "${expected_bins[@]}"; do
		jq -e --arg bin "${bin}" \
			'[.Results[]? | select(.Type == "rustbinary" and ((.Target // "") | ltrimstr("/")) == $bin and ((.Packages // []) | length >= 1))] | length >= 1' \
			"${tmp}/native.json" >/dev/null || {
			echo "Trivy report missing cargo-auditable rustbinary inventory for /${bin}" >&2
			exit 1
		}
	done

	if [[ ${mode} == scan ]]; then
		"${trivy_run[@]}" --network none -i "${TRIVY_IMAGE}" convert \
			--quiet --scanners vuln --severity HIGH,CRITICAL --format table /dev/stdin <"${tmp}/native.json"
		local fixed_count
		fixed_count=$(jq '[.Results[]?.Vulnerabilities[]? | select((.Severity == "HIGH" or .Severity == "CRITICAL") and ((.FixedVersion // "") != ""))] | length' "${tmp}/native.json")
		if [[ ${fixed_count} != 0 ]]; then
			echo "Security scan found ${fixed_count} fixable HIGH/CRITICAL vulnerabilities in ${image_id}" >&2
			exit 1
		fi
	else
		"${trivy_run[@]}" --network none -i "${TRIVY_IMAGE}" convert \
			--quiet --format cyclonedx /dev/stdin <"${tmp}/native.json" >"${tmp}/sbom.cdx.json"
		mv "${tmp}/sbom.cdx.json" "${output}"
	fi
}

case "${1:-}" in
dockerfile-check) cmd_dockerfile_check ;;
build) shift; cmd_build "$@" ;;
check) shift; cmd_check "$@" ;;
scan) shift; cmd_trivy scan "$@" ;;
sbom) shift; cmd_trivy sbom "$@" ;;
*) usage ;;
esac
