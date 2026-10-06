#!/usr/bin/env bash
# Finite source-only rehearsal. The ignored Rust case owns actor fencing and
# reconciliation; this carrier owns historical rendering and native archives.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

refuse() { echo "consumer lifecycle: $*" >&2; exit 2; }

compose() {
	docker compose -p "$1" -f "${ROOT_DIR}/env/docker-compose.yml" \
		-f "${LIFECYCLE_EVIDENCE}/compose.yml" "${@:2}"
}

psql_in() { compose "$1" exec -T postgres psql -X -U app -d app -v ON_ERROR_STOP=1 -At "${@:2}"; }

archive_manifest() {
	python3 - "$1" "${LIFECYCLE_EVIDENCE}" <<'PY'
import hashlib, json, pathlib, sys
action, root = sys.argv[1], pathlib.Path(sys.argv[2])
archives = root / "archives"
observed = {str(p.relative_to(archives)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(archives.rglob("*")) if p.is_file()}
path = root / "archives.sha256.json"
if action == "write":
    path.write_text(json.dumps(observed, indent=2) + "\n")
else:
    if observed != json.loads(path.read_text()):
        raise SystemExit("native archive set or bytes changed; restore refused")
PY
}

native_archive() {
	[[ -f ${LIFECYCLE_EVIDENCE}/fenced.json ]] || refuse "missing joined-owner fence"
	[[ $(psql_in "${LIFECYCLE_SOURCE_PROJECT}" -c "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND backend_type = 'client backend' AND pid <> pg_backend_pid()") == 0 ]] || refuse "source database still has an admitted connection"
	mkdir "${LIFECYCLE_EVIDENCE}/archives"
	date -u +%Y-%m-%dT%H:%M:%SZ >"${LIFECYCLE_EVIDENCE}/archive-started.txt"
	compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T postgres pg_dump -U app -d app --format=custom >"${LIFECYCLE_EVIDENCE}/archives/database.dump"
	compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T postgres pg_restore --list <"${LIFECYCLE_EVIDENCE}/archives/database.dump" >"${LIFECYCLE_EVIDENCE}/database.toc"
	compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T postgres pg_dumpall -U app --roles-only --no-role-passwords >"${LIFECYCLE_EVIDENCE}/roles.sql"
	psql_in "${LIFECYCLE_SOURCE_PROJECT}" -c "SELECT json_build_object('roles',(SELECT json_agg(row_to_json(r) ORDER BY rolname) FROM (SELECT rolname,rolsuper,rolinherit,rolcreaterole,rolcreatedb,rolcanlogin,rolreplication,rolbypassrls,rolconfig FROM pg_roles) r),'extensions',(SELECT json_agg(row_to_json(e) ORDER BY extname) FROM (SELECT extname,extversion,extnamespace::regnamespace::text FROM pg_extension) e),'encoding',current_setting('server_encoding'),'timezone',current_setting('TimeZone'),'isolation',current_setting('default_transaction_isolation'))" >"${LIFECYCLE_EVIDENCE}/database-settings.json"
	for stream in LIFECYCLE_SOURCE LIFECYCLE_DLQ; do
		LLMFORMAT=1 nats --no-context --server "${NATS_URL}" --timeout 30s backup stream "${stream}" "${LIFECYCLE_EVIDENCE}/archives/${stream}" --consumers --no-progress
		LLMFORMAT=1 nats --no-context backup validate "${LIFECYCLE_EVIDENCE}/archives/${stream}" --no-progress
	done
	archive_manifest write
	chmod -R a-w "${LIFECYCLE_EVIDENCE}/archives" "${LIFECYCLE_EVIDENCE}/archives.sha256.json"
	date -u +%Y-%m-%dT%H:%M:%SZ >"${LIFECYCLE_EVIDENCE}/archive-completed.txt"
}

native_restore() {
	archive_manifest verify
	[[ $(psql_in "${LIFECYCLE_RESTORE_PROJECT}" -c "SELECT count(*) FROM pg_class WHERE relnamespace = 'public'::regnamespace AND relkind IN ('r','S','v','m')") == 0 ]] || refuse "restore database is not empty"
	[[ $(psql_in "${LIFECYCLE_RESTORE_PROJECT}" -c "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND backend_type = 'client backend' AND pid <> pg_backend_pid()") == 0 ]] || refuse "restore database has an admitted connection"
	# The existing Compose initializer supplies the same app role; grant/schema
	# DDL is retained in the whole database archive. Never load role passwords.
	psql_in "${LIFECYCLE_RESTORE_PROJECT}" -c "SELECT json_build_object('roles',(SELECT json_agg(row_to_json(r) ORDER BY rolname) FROM (SELECT rolname,rolsuper,rolinherit,rolcreaterole,rolcreatedb,rolcanlogin,rolreplication,rolbypassrls,rolconfig FROM pg_roles) r),'extensions',(SELECT json_agg(row_to_json(e) ORDER BY extname) FROM (SELECT extname,extversion,extnamespace::regnamespace::text FROM pg_extension) e),'encoding',current_setting('server_encoding'),'timezone',current_setting('TimeZone'),'isolation',current_setting('default_transaction_isolation'))" >"${LIFECYCLE_EVIDENCE}/restore-settings.json"
	python3 - "${LIFECYCLE_EVIDENCE}" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
if json.loads((p / "database-settings.json").read_text()) != json.loads((p / "restore-settings.json").read_text()):
    raise SystemExit("restore role, extension or session settings differ")
PY
	compose "${LIFECYCLE_RESTORE_PROJECT}" exec -T postgres pg_restore -U app -d app --single-transaction --exit-on-error <"${LIFECYCLE_EVIDENCE}/archives/database.dump"
	# Native restore refuses existing stream names. The destination broker was
	# started empty and has never been admitted to any actor.
	for stream in LIFECYCLE_SOURCE LIFECYCLE_DLQ; do
		LLMFORMAT=1 nats --no-context backup validate "${LIFECYCLE_EVIDENCE}/archives/${stream}" --no-progress
		LLMFORMAT=1 nats --no-context --server "${LIFECYCLE_RESTORE_NATS_URL}" --timeout 30s backup restore stream "${LIFECYCLE_EVIDENCE}/archives/${stream}" --no-progress
	done
	archive_manifest verify
}

prepare_actor() {
	local label=$1 revision=$2 checkout pristine lock_before cargo_target_dir
	local -a actor_build=(cargo build --locked -p integration-tests --features integration --example consumer_lifecycle_actor)
	checkout="${LIFECYCLE_EVIDENCE}/consumers/${label}"
	# Historical source graphs keep separate Cargo artifacts, including initialization.
	cargo_target_dir="${LIFECYCLE_EVIDENCE}/build/${label}"
	git clone --quiet --no-hardlinks --no-checkout "${ROOT_DIR}" "${checkout}"
	git -C "${checkout}" checkout --quiet --detach "${revision}"
	# Full public initialization runs in the selected source with its actual
	# helper, lockfile and rust-toolchain.toml. No current renderer is substituted.
	(
		cd "${checkout}"
		CARGO_TARGET_DIR="${cargo_target_dir}" bash scripts/init-module.sh \
			--repo "${checkout}" --service-name lifecycle-demo \
			--repository https://github.com/Dankosik/rust-consumer-lifecycle-demo \
			--description 'Synthetic consumer lifecycle rehearsal.' --codeowner @Dankosik \
			--database postgres --authn none --outbound-http none --outbound-auth none \
			--grpc none --http-idempotency none --jobs postgres --messaging nats-jetstream \
			--outbox postgres --webhooks none --inbound-webhooks none --cache none \
			--object-storage none --agent-harness core
		git add -A
		git -c user.name=consumer-lifecycle -c user.email=consumer-lifecycle@example.invalid -c commit.gpgsign=false commit --quiet -m 'Initialize synthetic historical consumer'
	)
	pristine=$(git -C "${checkout}" rev-parse HEAD)
	lock_before=$(shasum -a 256 "${checkout}/Cargo.lock" | awk '{print $1}')
	mkdir -p "${checkout}/test/examples"
	cp "${LIFECYCLE_EVIDENCE}/overlay/consumer_lifecycle_actor.rs" "${checkout}/test/examples/consumer_lifecycle_actor.rs"
	(
		cd "${checkout}"
		CARGO_TARGET_DIR="${cargo_target_dir}" "${actor_build[@]}"
		git diff --exit-code
		[[ $(git ls-files --others --exclude-standard) == test/examples/consumer_lifecycle_actor.rs ]] || refuse "historical build changed more than the actor overlay"
	)
	[[ $(shasum -a 256 "${checkout}/Cargo.lock" | awk '{print $1}') == "${lock_before}" ]] || refuse "historical lock changed during build"
	cp "${cargo_target_dir}/debug/examples/consumer_lifecycle_actor" "${LIFECYCLE_EVIDENCE}/bin/${label}"
	python3 - "${LIFECYCLE_EVIDENCE}" "${label}" "${revision}" "${pristine}" "${cargo_target_dir}" "${actor_build[@]}" <<'PY'
import hashlib, json, pathlib, subprocess, sys
p, label, revision, pristine, cargo_target_dir = pathlib.Path(sys.argv[1]), *sys.argv[2:6]
root = p / "consumers" / label
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
record = {"upstream": revision, "pristine_commit": pristine,
          "pristine_tree": subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD^{tree}"], text=True).strip(),
          "lock_sha256": sha(root / "Cargo.lock"), "toolchain": (root / "rust-toolchain.toml").read_text(),
          "cargo_target_dir": cargo_target_dir, "build_command": sys.argv[6:],
          "overlay_sha256": sha(p / "overlay/consumer_lifecycle_actor.rs"),
          "executable_sha256": sha(p / "bin" / label)}
path = p / "actors.json"
records = json.loads(path.read_text()) if path.exists() else {}
records[label] = record
path.write_text(json.dumps(records, indent=2) + "\n")
PY
}

run_rehearsal() {
	[[ ${ALLOW_HEAVY:-} == 1 || ${CI:-} == true ]] || refuse "set ALLOW_HEAVY=1 for the explicit native rehearsal"
	[[ -f make/source.mk ]] || refuse "this target exists only in the source template"
	require_docker
	command -v nats >/dev/null || refuse "existing native NATS CLI is required"
	for args in 'backup stream' 'backup validate' 'backup restore stream'; do
		# The interface is checked, not installed or upgraded by the rehearsal.
		# shellcheck disable=SC2086
		LLMFORMAT=1 nats --no-context ${args} --help >/dev/null
	done
	local output=${1:-"${ROOT_DIR}/target/consumer-lifecycle-$(date +%Y%m%d-%H%M%S)-$$"}
	[[ ! -e ${output} ]] || refuse "output directory must be new; retained archives are never overwritten"
	mkdir -p "${output}"
	LIFECYCLE_EVIDENCE=$(cd "${output}" && pwd)
	export LIFECYCLE_EVIDENCE
	# Keep the two historical builds serial in separate target directories. Refuse a
	# visibly unsuitable volume before full initialization/build consumes it.
	local available
	available=$(df -Pk "${LIFECYCLE_EVIDENCE}" | awk 'NR == 2 { print $4 }')
	[[ ${available} -ge 8388608 ]] || refuse "rehearsal volume needs at least 8 GiB available for two historical builds and archives"
	mkdir -p "${LIFECYCLE_EVIDENCE}/consumers" "${LIFECYCLE_EVIDENCE}/bin" "${LIFECYCLE_EVIDENCE}/overlay"
	cp test/examples/consumer_lifecycle_actor.rs "${LIFECYCLE_EVIDENCE}/overlay/consumer_lifecycle_actor.rs"
	prepare_actor old 67be869acea112af271ec8ba621cbc50ae9d36b7
	prepare_actor new 2cb871895b9edd018205fc98223477e269fce2e9
	LIFECYCLE_OLD_ACTOR="${LIFECYCLE_EVIDENCE}/bin/old"
	LIFECYCLE_NEW_ACTOR="${LIFECYCLE_EVIDENCE}/bin/new"
	LIFECYCLE_CARRIER="${ROOT_DIR}/scripts/ci/consumer-lifecycle-check.sh"
	LIFECYCLE_SOURCE_PROJECT="consumer-lifecycle-source-$(date +%s)-$$"
	LIFECYCLE_RESTORE_PROJECT="consumer-lifecycle-restore-$(date +%s)-$$"
	export LIFECYCLE_OLD_ACTOR LIFECYCLE_NEW_ACTOR LIFECYCLE_CARRIER LIFECYCLE_SOURCE_PROJECT LIFECYCLE_RESTORE_PROJECT
	# Use the existing pinned harness with supported Compose overrides for
	# loopback-only endpoints and named per-project storage. No new runner.
	cat >"${LIFECYCLE_EVIDENCE}/compose.yml" <<'YAML'
services:
  postgres:
    ports: !override ["127.0.0.1::5432"]
    volumes:
      - lifecycle-postgres:/var/lib/postgresql
  nats:
    ports: !override ["127.0.0.1::4222"]
    volumes:
      - lifecycle-nats:/data/jetstream
volumes:
  lifecycle-postgres:
  lifecycle-nats:
YAML
	lifecycle_completed=false
	lifecycle_started=false
	cleanup() {
		local cleanup_status=$?
		if [[ ${lifecycle_started} == true ]]; then
			for project in "${LIFECYCLE_SOURCE_PROJECT}" "${LIFECYCLE_RESTORE_PROJECT}"; do
				if [[ ${lifecycle_completed} == true ]]; then
					compose "${project}" down -v --remove-orphans || cleanup_status=1
				else
					compose "${project}" stop --timeout 45 || cleanup_status=1
				fi
			done
		fi
		echo "consumer lifecycle evidence: ${LIFECYCLE_EVIDENCE} (completed=${lifecycle_completed})"
		trap - EXIT
		exit "${cleanup_status}"
	}
	trap cleanup EXIT
	trap 'exit 130' INT
	trap 'exit 143' TERM
	lifecycle_started=true
	for project in "${LIFECYCLE_SOURCE_PROJECT}" "${LIFECYCLE_RESTORE_PROJECT}"; do
		compose "${project}" up -d --wait postgres nats
		compose "${project}" ps --format json >"${LIFECYCLE_EVIDENCE}/${project}-containers.json"
		docker volume ls --filter "label=com.docker.compose.project=${project}" --format json >"${LIFECYCLE_EVIDENCE}/${project}-volumes.json"
	done
	local address port
	address=$(compose "${LIFECYCLE_SOURCE_PROJECT}" port postgres 5432); port=${address##*:}
	DATABASE_URL="postgres://app:app@127.0.0.1:${port}/app?sslmode=disable"
	address=$(compose "${LIFECYCLE_SOURCE_PROJECT}" port nats 4222); port=${address##*:}
	NATS_URL="nats://127.0.0.1:${port}"
	address=$(compose "${LIFECYCLE_RESTORE_PROJECT}" port postgres 5432); port=${address##*:}
	LIFECYCLE_RESTORE_DATABASE_URL="postgres://app:app@127.0.0.1:${port}/app?sslmode=disable"
	address=$(compose "${LIFECYCLE_RESTORE_PROJECT}" port nats 4222); port=${address##*:}
	LIFECYCLE_RESTORE_NATS_URL="nats://127.0.0.1:${port}"
	export DATABASE_URL NATS_URL LIFECYCLE_RESTORE_DATABASE_URL LIFECYCLE_RESTORE_NATS_URL
	{
		docker version --format '{{.Server.Version}}'
		docker compose version
		LLMFORMAT=1 nats --version
		compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T nats nats-server --version
		compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T postgres pg_dump --version
		compose "${LIFECYCLE_SOURCE_PROJECT}" exec -T postgres pg_restore --version
		psql_in "${LIFECYCLE_SOURCE_PROJECT}" -c 'SELECT version()'
		uname -sm
	} >"${LIFECYCLE_EVIDENCE}/versions.txt"
	python3 - "${LIFECYCLE_EVIDENCE}" "${LIFECYCLE_SOURCE_PROJECT}" "${LIFECYCLE_RESTORE_PROJECT}" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
(p / "resources.json").write_text(json.dumps({"source_project": sys.argv[2], "restore_project": sys.argv[3],
    "data": "synthetic only", "ports": "loopback ephemeral", "failure_cleanup": "stop, preserve volumes"}, indent=2) + "\n")
PY
	# This selects exactly one ignored test. Both the harness output and the
	# independently written terminal receipt are required; zero tests is failure.
	cargo test --locked -p integration-tests --features integration --test consumer_lifecycle \
		historical_actors_survive_native_restore -- --ignored --exact --nocapture 2>&1 | tee "${LIFECYCLE_EVIDENCE}/rehearsal.log"
	python3 - "${LIFECYCLE_EVIDENCE}" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1])
receipt = json.loads((p / "completed.json").read_text())
if receipt.get("completed") is not True or receipt.get("scenario") != "historical_actors_survive_native_restore":
    raise SystemExit("required native scenario did not complete")
if "1 passed; 0 failed; 0 ignored" not in (p / "rehearsal.log").read_text():
    raise SystemExit("required native scenario was skipped or not selected")
PY
	lifecycle_completed=true
}

case "${1:-run}" in
run) run_rehearsal "${2:-}" ;;
archive | restore)
	[[ -n ${LIFECYCLE_EVIDENCE:-} && -n ${LIFECYCLE_SOURCE_PROJECT:-} && -n ${LIFECYCLE_RESTORE_PROJECT:-} ]] || refuse "native callback requires the active finite harness"
	if [[ $1 == archive ]]; then native_archive; else native_restore; fi
	;;
*) refuse "usage: consumer-lifecycle-check.sh run [new-output-directory]" ;;
esac
