#!/usr/bin/env bash
# Anonymous and authenticated proof run sequentially on one disposable broker.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

# Cache warming has no Docker, endpoint, or lifecycle admission side effects.
for argument in "$@"; do
	if [[ ${argument} == --no-run ]]; then
		exec cargo test --locked -p infra-messaging --features integration \
			--test jetstream --test idle_pull --test credential_rotation "$@"
	fi
done
if [[ ${NATS_URL:-} == compile-only ]]; then
	echo "NATS_URL=compile-only requires --no-run" >&2
	exit 2
fi

compose_project=${INTEGRATION_COMPOSE_PROJECT:-}
owned=false
switched=false
inputs=

require_docker() {
	if docker info >/dev/null 2>&1; then
		return 0
	fi
	if [[ ${REQUIRE_DOCKER:-} == 1 ]]; then
		echo "docker is required (REQUIRE_DOCKER=1) but not available" >&2
		exit 1
	fi
	echo "refusing: docker is not available" >&2
	exit 2
}

normal_compose() {
	docker compose -p "${compose_project}" -f "${inputs}/normal.json" "$@"
}

auth_compose() {
	docker compose -p "${compose_project}" -f "${inputs}/normal.json" -f "${inputs}/auth.json" "$@"
}

cleanup() {
	local result=$? cleanup_result=0
	trap - EXIT INT TERM
	set +e
	if [[ ${owned} == true ]]; then
		normal_compose down -v --remove-orphans
		cleanup_result=$?
	elif [[ ${switched} == true ]]; then
		# Restore through the original input path as well as its original port,
		# so Compose retains usable config-file identity for the next caller.
		docker compose -p "${compose_project}" -f "${ROOT_DIR}/env/docker-compose.yml" \
			up -d --wait --force-recreate --no-deps nats
		cleanup_result=$?
	fi
	if ((cleanup_result != 0)); then
		echo "NATS cleanup failed for project ${compose_project}; original inputs retained at ${inputs}" >&2
		# Preserve an authenticated-segment failure even when restoration also fails.
		((result != 0)) || result=${cleanup_result}
	elif [[ -n ${inputs} ]]; then
		rm -rf -- "${inputs}"
	fi
	exit "${result}"
}

published_url() {
	local address port
	address=$(normal_compose port nats 4222)
	port=${address##*:}
	[[ ${port} =~ ^[0-9]+$ ]] || {
		echo "failed to resolve the published NATS port for ${compose_project}" >&2
		return 1
	}
	printf 'nats://127.0.0.1:%s\n' "${port}"
}

if [[ -z ${NATS_URL:-} ]]; then
	require_docker
	compose_project="service-messaging-$(date +%s)-$$"
	export NATS_PORT=0
elif [[ ${INTEGRATION_COMPOSE_MANAGED:-} == 1 ]]; then
	# This flag delegates exclusive custody: all other broker consumers must
	# have joined before invocation. Identity is independently checked below.
	[[ -n ${compose_project} ]] || {
		echo "managed NATS requires INTEGRATION_COMPOSE_PROJECT and exclusive service custody" >&2
		exit 2
	}
	require_docker
	container_id=$(docker compose -p "${compose_project}" -f env/docker-compose.yml ps --quiet nats)
	[[ -n ${container_id} ]] || { echo "managed project has no running NATS service" >&2; exit 2; }
	published=$(docker compose -p "${compose_project}" -f env/docker-compose.yml port nats 4222)
	# Check project/service/config identity, normal mount, and the actual local
	# published endpoint before granting any reconfiguration authority.
	docker inspect "${container_id}" | python3 -c '
import ipaddress, json, socket, sys, urllib.parse
container = json.load(sys.stdin)[0]
project, root, url, published = sys.argv[1:]
labels = container["Config"]["Labels"]
assert labels.get("com.docker.compose.project") == project, "project identity mismatch"
assert labels.get("com.docker.compose.service") == "nats", "service identity mismatch"
assert labels.get("com.docker.compose.project.config_files") == root + "/env/docker-compose.yml", "original Compose inputs do not match"
assert any(m["Destination"] == "/etc/nats/nats-server.conf" and m["Source"] == root + "/env/nats/nats-server.conf" for m in container["Mounts"]), "normal configuration mount does not match"
endpoint = urllib.parse.urlsplit(url)
assert endpoint.scheme == "nats" and endpoint.hostname and not endpoint.username and not endpoint.password and endpoint.path in ("", "/") and not endpoint.query and not endpoint.fragment, "managed URL must be one plain NATS endpoint"
binding, port = published.rsplit(":", 1)
address = ipaddress.ip_address(binding.strip("[]"))
assert address.is_loopback or address.is_unspecified, "published binding is not local loopback or wildcard"
assert endpoint.port == int(port), "published port mismatch"
addresses = socket.getaddrinfo(endpoint.hostname, endpoint.port, type=socket.SOCK_STREAM)
assert addresses and all(ipaddress.ip_address(a[4][0]).is_loopback for a in addresses), "managed endpoint must resolve only to loopback"
' "${compose_project}" "${ROOT_DIR}" "${NATS_URL}" "${published}"
	export NATS_PORT=${published##*:}
fi

if [[ -z ${NATS_URL:-} || ${INTEGRATION_COMPOSE_MANAGED:-} == 1 ]]; then
	inputs=$(mktemp -d "${TMPDIR:-/tmp}/messaging-compose.XXXXXX")
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	# Render once to retain absolute mounts and exact interpolated original
	# inputs through restoration, even after an ephemeral port is reassigned.
	docker compose -p "${compose_project}" -f env/docker-compose.yml config --format json >"${inputs}/normal.json"
	python3 - "${ROOT_DIR}" "${inputs}/auth.json" <<'PY_OVERRIDE'
import json, sys
with open(sys.argv[2], "w") as output:
    json.dump({"services": {"nats": {"volumes": [{"type": "bind", "source": sys.argv[1] + "/env/nats/credential-rotation.conf", "target": "/etc/nats/nats-server.conf", "read_only": True}]}}}, output)
PY_OVERRIDE
	if [[ -z ${NATS_URL:-} ]]; then
		owned=true
		normal_compose up -d --wait nats
		NATS_URL=$(published_url)
	fi
fi

export NATS_URL
cargo test --locked -p infra-messaging --features integration --test jetstream --test idle_pull "$@"

if [[ -n ${inputs} ]]; then
	switched=true
	auth_compose up -d --wait --force-recreate --no-deps nats
	NATS_AUTH_URL=$(published_url)
fi
if [[ -z ${NATS_AUTH_URL:-} ]]; then
	echo "anonymous coverage finished; full messaging proof requires NATS_AUTH_URL or explicit managed project custody" >&2
	exit 2
fi
export NATS_AUTH_URL
# Anonymous-suite filters never silently filter away the required auth proof.
cargo test --locked -p infra-messaging --features integration --test credential_rotation
