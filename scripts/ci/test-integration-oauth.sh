#!/usr/bin/env bash
# OAuth adapter proof against a throwaway Keycloak container. A caller can
# supply an already-running one through OAUTH_TEST_KEYCLOAK_URL; its bootstrap
# administrator must be admin/admin, as started here.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

# Bumped by hand: the proof runs outside Compose, so Dependabot does not see
# this pin. The adapter guide records the version its compatibility row names.
KEYCLOAK_IMAGE=quay.io/keycloak/keycloak:26.8.0@sha256:b0f60d489d51c5d113390bdf5461d4c06e6051be026c05549f2e1e10ec352bcc
READY_TIMEOUT_SECONDS=180

container=

require_docker() {
	if docker info >/dev/null 2>&1; then
		return 0
	fi
	if [[ ${REQUIRE_DOCKER:-} == 1 ]]; then
		echo "docker is required (REQUIRE_DOCKER=1) but not available" >&2
		exit 1
	fi
	echo "refusing: docker is not available; set REQUIRE_DOCKER=1 to fail instead of refusing" >&2
	exit 2
}

cleanup() {
	if [[ -n ${container} ]]; then
		docker rm -f "${container}" >/dev/null 2>&1 || true
	fi
}

if [[ -z ${OAUTH_TEST_KEYCLOAK_URL:-} ]]; then
	require_docker
	trap cleanup EXIT INT TERM
	# Development mode: plain HTTP on loopback and an in-memory database, which
	# is all a throwaway realm needs.
	container=$(docker run -d -p 127.0.0.1::8080 \
		-e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
		"${KEYCLOAK_IMAGE}" start-dev)
	address=$(docker port "${container}" 8080/tcp)
	port=${address##*:}
	if [[ -z ${port} ]]; then
		echo "failed to resolve the Keycloak port" >&2
		exit 1
	fi
	OAUTH_TEST_KEYCLOAK_URL="http://127.0.0.1:${port}"
	deadline=$((SECONDS + READY_TIMEOUT_SECONDS))
	until curl -fsS -o /dev/null "${OAUTH_TEST_KEYCLOAK_URL}/realms/master" 2>/dev/null; do
		if ((SECONDS >= deadline)) || [[ $(docker inspect --format '{{.State.Running}}' "${container}") != true ]]; then
			echo "Keycloak did not become ready within ${READY_TIMEOUT_SECONDS}s" >&2
			docker logs --tail 100 "${container}" >&2 || true
			exit 1
		fi
		sleep 2
	done
fi
export OAUTH_TEST_KEYCLOAK_URL
cargo test --locked -p infra-oauth2-client-credentials --features integration tests::keycloak "$@"
