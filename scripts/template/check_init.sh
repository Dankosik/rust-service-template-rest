#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
repo=${ROOT_DIR}
mode=full
runtime_graphs=
graph_plan_args=(--list-graphs)
validation_work=
validation_receipt_dir=


plan_graphs() {
	python3 -B "${ROOT_DIR}/scripts/template/matrix.py" "${graph_plan_args[@]}"
}

prune_receipt_dir() {
	local dir=$1
	[[ -d ${dir} ]] || return 0
	python3 -B -c '
import pathlib, shutil, sys
root = pathlib.Path(sys.argv[1])
for is_logs in (False, True):
    items = sorted(
        (
            p for p in root.iterdir()
            if (p.name.startswith("attempt-logs") if is_logs else (p.name.startswith("attempt") and not p.name.startswith("attempt-logs")))
        ),
        key=lambda p: p.lstat().st_mtime_ns,
        reverse=True,
    )
    for stale in items[10:]:
        if stale.is_dir() and not stale.is_symlink():
            shutil.rmtree(stale, ignore_errors=True)
        else:
            stale.unlink(missing_ok=True)
' "${dir}"
}

cleanup_validation() {
	if [[ -n ${validation_work} ]]; then
		rm -rf -- "${validation_work}"
		validation_work=
	fi
	if [[ -n ${validation_receipt_dir} ]]; then
		prune_receipt_dir "${validation_receipt_dir}"
		validation_receipt_dir=
	fi
}

while (($#)); do
	case "$1" in
	--self-test | --source-checks | --projections-only | --quality-projections | --list-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		mode=${1#--}
		shift
		;;
	--runtime-graphs | --artifact-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		mode=${1#--}
		runtime_graphs=${2-}
		graph_plan_args=("$1=${runtime_graphs}")
		shift
		if (($#)); then shift; fi
		;;
	--default-artifact-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		mode=artifact-graphs
		graph_plan_args=(--artifact-graphs)
		shift
		;;
	--repo)
		[[ -n ${2:-} ]] || { echo "--repo requires a path" >&2; exit 2; }
		repo=$(cd "${2}" && pwd)
		shift 2
		;;
	*)
		echo "usage: $0 [--repo ROOT] [--source-checks|--projections-only|--quality-projections|--runtime-graphs IDS|--artifact-graphs IDS|--default-artifact-graphs|--list-graphs|--self-test]" >&2
		exit 2
		;;
	esac
done

if [[ ${mode} == runtime-graphs && ${ALLOW_FULL:-} != 1 && ${CI:-} != true ]]; then
	plan_graphs >/dev/null
	echo "--runtime-graphs requires ALLOW_FULL=1 (CI sets CI=true)" >&2
	exit 2
fi

if [[ ${mode} == artifact-graphs && ${ALLOW_HEAVY:-} != 1 && ${CI:-} != true ]]; then
	plan_graphs >/dev/null
	echo "--artifact-graphs requires ALLOW_HEAVY=1 (CI sets CI=true)" >&2
	exit 2
fi

copy_path() {
	local relative destination source
	relative=$1
	destination=$2
	source=${repo}/${relative}
	[[ ${relative} != /* && ${relative} != *'..'* && ${relative} != *$'\n'* ]] || {
		echo "unsafe candidate path: ${relative}" >&2; return 2
	}
	if [[ -L ${source} ]]; then
		echo "candidate path is an unsupported symlink: ${relative}" >&2
		return 2
	fi
	[[ -f ${source} ]] || { echo "candidate path is not a regular file: ${relative}" >&2; return 2; }
	mkdir -p "${destination}/$(dirname "${relative}")" || return
	cp -p "${source}" "${destination}/${relative}"
}

snapshot_candidate() {
	local destination=$1 relative untracked
	# This function runs in command substitution: explicit propagation keeps a
	# failed inventory/copy/git operation from committing an incomplete source.
	mkdir -p "${destination}" || return
	git -C "${repo}" ls-files -z | while IFS= read -r -d '' relative; do
		# A tracked worktree deletion belongs to the candidate as a deletion.
		[[ -e ${repo}/${relative} || -L ${repo}/${relative} ]] || continue
		copy_path "${relative}" "${destination}" || return
	done || return
	# The candidate is the index as the worktree holds it. A new file joins it
	# through `git add`; naming the ones left out keeps a local pass from hiding
	# code the committed tree will contain.
	untracked=$(git -C "${repo}" ls-files --others --exclude-standard --directory) || return
	[[ -z ${untracked} ]] || printf 'untracked paths are outside the candidate; git add them to include them:\n%s\n' "${untracked}" >&2
	git -C "${destination}" init -q -b main || return
	git -C "${destination}" config user.email template-init-check@example.invalid || return
	git -C "${destination}" config user.name template-init-check || return
	git -C "${destination}" add -A || return
	git -C "${destination}" commit -qm 'fixed template candidate' || return
	git -C "${destination}" rev-parse HEAD || return
}

record_command() {
	local receipt=$1 log=$2 label=$3 started=${SECONDS} started_epoch
	started_epoch=$(date +%s)
	shift 3
	printf 'command=%q ' "$@" >>"${receipt}"
	printf '\nstatus=running label=%s started_epoch=%s\n' "${label}" "${started_epoch}" >>"${receipt}"
	if "$@" >"${log}" 2>&1; then
		printf 'status=passed label=%s log_sha256=%s output=%s duration_seconds=%s\n' "${label}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" "$((SECONDS - started))" >>"${receipt}"
		cat "${log}"
		return 0
	else
		local status=$?
		printf 'status=failed label=%s exit_code=%s log_sha256=%s output=%s duration_seconds=%s\n' "${label}" "${status}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" "$((SECONDS - started))" >>"${receipt}"
		printf 'template initializer gate failed: %s (exit %s), log: %s\n' "${label}" "${status}" "${log}" >&2
		cat "${log}" >&2
		return "${status}"
	fi
}

recorder_self_test() {
	local fixture receipt log later exit_code prune_dir index remaining_attempts remaining_logs
	fixture=$(mktemp -d)
	trap 'rm -rf -- "${fixture}"' RETURN
	receipt=${fixture}/receipt
	log=${fixture}/failed.log
	later=${fixture}/later
	: >"${receipt}"
	if (record_command "${receipt}" "${log}" "forced-failure" bash -c 'exit 7' &&
		record_command "${receipt}" "${fixture}/later.log" "must-not-run" touch "${later}"); then
		echo "recorder self-test accepted a failing command" >&2
		return 1
	else
		exit_code=$?
	fi
	[[ ${exit_code} == 7 ]] || { echo "recorder self-test changed exit ${exit_code}" >&2; return 1; }
	grep -q 'status=failed label=forced-failure exit_code=7 ' "${receipt}"
	[[ ! -e ${later} ]] || { echo "recorder self-test executed a later stage" >&2; return 1; }
	graph_selection_self_test "${fixture}"
	prune_dir=${fixture}/prune
	mkdir -p "${prune_dir}"
	for index in {1..13}; do
		: >"${prune_dir}/attempt.${index}"
		mkdir -p "${prune_dir}/attempt-logs.${index}"
	done
	prune_receipt_dir "${prune_dir}"
	remaining_attempts=$(find "${prune_dir}" -mindepth 1 -maxdepth 1 -name 'attempt.*' | wc -l | tr -d ' ')
	remaining_logs=$(find "${prune_dir}" -mindepth 1 -maxdepth 1 -name 'attempt-logs.*' | wc -l | tr -d ' ')
	[[ ${remaining_attempts} == 10 && ${remaining_logs} == 10 ]] || {
		echo "receipt pruning left attempts=${remaining_attempts} logs=${remaining_logs}" >&2
		return 1
	}
	snapshot_failure_self_test "${fixture}"
	artifact_recorder_self_test "${fixture}"
	printf 'template initializer graph selection self-test: pass\n'
	printf 'template initializer recorder self-test: pass\n'
}

graph_selection_self_test() {
	local fixture=$1 listing selection invalid failure status fake_source
	local planner="${ROOT_DIR}/scripts/template/matrix.py"
	listing=$(bash "$0" --list-graphs)
	awk '$1 != NR || NF != 15 || $0 !~ /^[0-9]+( [a-z0-9-]+)+$/ { exit 1 } END { if (NR != 68) exit 1 }' <<<"${listing}" ||
		{ echo "graph listing changed its 68-row CLI format" >&2; return 1; }
	selection=$(python3 -B "${planner}" --runtime-graphs=68,1,7)
	[[ $(cut -d ' ' -f 1 <<<"${selection}") == $'1\n7\n68' ]] ||
		{ echo "explicit runtime selection must follow catalog order" >&2; return 1; }
	selection=$(python3 -B "${planner}" --artifact-graphs=65,1)
	[[ $(cut -d ' ' -f 1 <<<"${selection}") == $'1\n65' ]] ||
		{ echo "explicit artifact selection must follow catalog order" >&2; return 1; }
	selection=$(python3 -B "${planner}" --artifact-graphs)
	[[ $(cut -d ' ' -f 1 <<<"${selection}") == $'1\n7\n47\n65' ]] ||
		{ echo "default artifact selection must include every release representative" >&2; return 1; }
	for invalid in '' 0 69 01 '1,1' '2,,3' '1,'; do
		status=0
		failure=$(CI=false ALLOW_FULL= bash "$0" --runtime-graphs "${invalid}" 2>&1) || status=$?
		[[ ${status} == 2 && ${failure} == "--runtime-graphs requires distinct comma-separated graph IDs 1..68" ]] ||
			{ echo "invalid runtime selection was not rejected: ${invalid}" >&2; return 1; }
	done
	for invalid in '' 2 48 '1,7,7' '1,65,64'; do
		status=0
		failure=$(CI=false ALLOW_HEAVY= bash "$0" --artifact-graphs "${invalid}" 2>&1) || status=$?
		[[ ${status} == 2 && ${failure} == "--"*' distinct '* ]] ||
			{ echo "invalid artifact selection was not rejected: ${invalid}" >&2; return 1; }
	done

	fake_source=${fixture}/failed-planner
	mkdir -p "${fake_source}/scripts/template" "${fake_source}/scripts/verify" "${fake_source}/bin"
	cp "${ROOT_DIR}/scripts/template/check_init.sh" "${fake_source}/scripts/template/"
	cp "${ROOT_DIR}/scripts/verify/lock.py" "${fake_source}/scripts/verify/"
	{
		printf 'print("%s")\n' "${listing%%$'\n'*}"
		printf 'raise SystemExit(7)\n'
	} >"${fake_source}/scripts/template/matrix.py"
	printf '#!/usr/bin/env bash\necho "validation must not start" >&2\nexit 9\n' >"${fake_source}/bin/git"
	chmod +x "${fake_source}/bin/git"
	status=0
	failure=$(PATH="${fake_source}/bin:${PATH}" VALIDATION_LOCK_HELD=1 VALIDATION_LOCK_TOKEN=self-test ALLOW_FULL=1 \
		bash "${fake_source}/scripts/template/check_init.sh" --runtime-graphs 1 2>&1) || status=$?
	[[ ${status} == 7 && -z ${failure} ]] ||
		{ echo "failed graph planner started validation or changed status: ${status} ${failure}" >&2; return 1; }
}

snapshot_failure_self_test() (
	local fixture=$1 fixture_repo destination result selected
	fixture_repo=${fixture}/snapshot-source
	destination=${fixture}/partial-snapshot
	mkdir -p "${fixture_repo}" "${fixture}/snapshot-bin"
	printf 'kept\n' >"${fixture_repo}/a"
	printf 'must not disappear\n' >"${fixture_repo}/b"
	git -C "${fixture_repo}" init -q
	git -C "${fixture_repo}" add a b
	git -C "${fixture_repo}" -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit -qm fixture
	cat >"${fixture}/snapshot-bin/cp" <<'SH'
#!/usr/bin/env bash
if [[ $2 == */b ]]; then echo 'forced snapshot copy failure' >&2; exit 28; fi
exec /bin/cp "$@"
SH
	chmod +x "${fixture}/snapshot-bin/cp"
	# A real first file makes the broken snapshot committable. The production
	# command substitution must propagate the next copy failure rather than
	# returning a valid-looking revision of that incomplete source.
	export -f copy_path snapshot_candidate
	# The child shell must own the command substitution and expand its $1.
	# shellcheck disable=SC2016
	if selected=$(env "PATH=${fixture}/snapshot-bin:${PATH}" "repo=${fixture_repo}" \
		bash -c 'set -euo pipefail; if candidate=$(snapshot_candidate "$1"); then printf "%s\n" "${candidate}"; else exit $?; fi' _ "${destination}"); then
		echo "snapshot accepted a failed copy and returned ${selected}" >&2; exit 1
	else result=$?; fi
	[[ ${result} == 28 && $(cat "${destination}/a") == kept && ! -e ${destination}/.git ]] || {
		echo "snapshot did not stop at the failed copy (exit ${result})" >&2; exit 1;
	}
	printf 'template initializer snapshot failure self-test: pass\n'
)

artifact_recorder_self_test() (
	local fixture=$1 expected_image result artifact_receipt artifact_logs
	expected_image="sha256:$(printf 'a%.0s' {1..64})"
	artifact_receipt=${fixture}/artifact-receipt
	artifact_logs=${fixture}/logs
	mkdir -p "${fixture}/bin" "${fixture}/logs"
	# Stand-ins control only external process outcomes. The real runner owns
	# ordering, image identity forwarding, fail-fast behavior and its receipt.
	cat >"${fixture}/bin/docker" <<'SH'
#!/usr/bin/env bash
printf 'docker %s\n' "$*" >>"${ARTIFACT_TEST_CALLS}"
if [[ $1 == image && $2 == inspect ]]; then printf '%s\n' "${ARTIFACT_TEST_IMAGE}"; fi
SH
	cat >"${fixture}/bin/make" <<'SH'
#!/usr/bin/env bash
printf 'make %s\n' "$*" >>"${ARTIFACT_TEST_CALLS}"
if [[ $3 == "${ARTIFACT_TEST_FAIL:-}" ]]; then exit 19; fi
if [[ $3 == container-sbom ]]; then printf '{"fixture":"sbom"}\n' >"${5#SBOM_OUTPUT=}"; fi
SH
	chmod +x "${fixture}/bin/"{docker,make}
	export PATH="${fixture}/bin:${PATH}" ARTIFACT_TEST_CALLS="${fixture}/calls" ARTIFACT_TEST_IMAGE="${expected_image}"
	local -a artifact_environment=(env candidate=fixture-candidate "receipt=${artifact_receipt}" "log_dir=${artifact_logs}")
	export -f run_artifact record_command
	"${artifact_environment[@]}" bash -c 'set -euo pipefail; run_artifact 47 /project initialized-revision renamed-service'
	# A moved tag would change the artifact if any post-build gate consumed it.
	grep -q "runtime-image-check RUNTIME_IMAGE=${expected_image} RUNTIME_EXPECTED_COMMIT=initialized-revision$" "${ARTIFACT_TEST_CALLS}"
	grep -q "container-security CONTAINER_IMAGE=${expected_image}$" "${ARTIFACT_TEST_CALLS}"
	grep -q "container-sbom CONTAINER_IMAGE=${expected_image} SBOM_OUTPUT=" "${ARTIFACT_TEST_CALLS}"
	grep -q "artifact_graph=47 sbom_sha256=$(shasum -a 256 "${artifact_logs}/artifact-47.cdx.json" | awk '{print $1}')" "${artifact_receipt}"
	[[ $(grep -c 'runtime-image-build' "${ARTIFACT_TEST_CALLS}") == 1 ]] || { echo "template-init self-test assertion failed at build count" >&2; exit 1; }
	[[ $(wc -l <"${ARTIFACT_TEST_CALLS}" | tr -d ' ') == 6 ]] || { echo "template-init self-test assertion failed at call count" >&2; exit 1; }
	: >"${ARTIFACT_TEST_CALLS}"
	: >"${artifact_receipt}"
	export ARTIFACT_TEST_FAIL=container-security
	if "${artifact_environment[@]}" bash -c 'set -euo pipefail; run_artifact 47 /project initialized-revision renamed-service; run_artifact 65 /project later-revision later-service'; then
		echo "artifact runner accepted a failed security gate" >&2; exit 1
	else result=$?; fi
	[[ ${result} == 19 ]] || { echo "template-init self-test assertion failed at exit code" >&2; exit 1; }
	grep -q 'status=failed label=artifact-47-security exit_code=19 ' "${artifact_receipt}"
	if grep -Eq 'container-sbom|later-service|image rm' "${ARTIFACT_TEST_CALLS}"; then
		echo "artifact runner continued after a failed security gate" >&2; exit 1
	fi
)

record_source_suites() {
	record_command "${receipt}" "${log_dir}/source-purity.log" "source-purity" \
		"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-owned-purity.py" --repo "${source}"
	record_command "${receipt}" "${log_dir}/source-init-safety.log" "source-init-safety" \
		"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-init-safety.py" --source "${source}"
	record_command "${receipt}" "${log_dir}/source-sync-canary.log" "source-sync-canary" \
		"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-sync-canary.py" --source "${source}"
}

run_artifact() {
	local graph=$1 target=$2 revision=$3 identity=$4 image image_id sbom_sha256
	image="template-artifact:${candidate:0:12}-${graph}"
	# A renamed cooked graph is no other build's cache: exporting it would only
	# multiply remote cache storage, so a derived build never exports one.
	record_command "${receipt}" "${log_dir}/artifact-${graph}-build.log" "artifact-${graph}-build" \
		env "VCS_REF=${revision}" "SERVICE_PACKAGE=${identity}" "SERVICE_BIN=${identity}" RUNTIME_IMAGE_CACHE_TO= \
		make -C "${target}" runtime-image-build "RUNTIME_IMAGE=${image}"
	record_command "${receipt}" "${log_dir}/artifact-${graph}-identity.log" "artifact-${graph}-identity" \
		docker image inspect --format '{{.Id}}' "${image}"
	image_id=$(cat "${log_dir}/artifact-${graph}-identity.log")
	[[ ${image_id} =~ ^sha256:[a-f0-9]{64}$ ]] || { echo "artifact-${graph}-identity: invalid image ID" >&2; return 1; }
	printf 'artifact_graph=%s candidate=%s output_revision=%s image_id=%s\n' \
		"${graph}" "${candidate}" "${revision}" "${image_id}" >>"${receipt}"
	# The lifecycle target also checks every retained/pruned image entrypoint.
	# All gates consume the resolved ID; a moving tag cannot change the artifact.
	record_command "${receipt}" "${log_dir}/artifact-${graph}-lifecycle.log" "artifact-${graph}-lifecycle" \
		env -u RUNTIME_IMAGE_POSTGRES_DSN -u RUNTIME_IMAGE_NETWORK \
		make -C "${target}" runtime-image-check "RUNTIME_IMAGE=${image_id}" "RUNTIME_EXPECTED_COMMIT=${revision}"
	record_command "${receipt}" "${log_dir}/artifact-${graph}-security.log" "artifact-${graph}-security" \
		make -C "${target}" container-security "CONTAINER_IMAGE=${image_id}"
	record_command "${receipt}" "${log_dir}/artifact-${graph}-sbom.log" "artifact-${graph}-sbom" \
		make -C "${target}" container-sbom "CONTAINER_IMAGE=${image_id}" "SBOM_OUTPUT=${log_dir}/artifact-${graph}.cdx.json"
	sbom_sha256=$(shasum -a 256 "${log_dir}/artifact-${graph}.cdx.json" | awk '{print $1}')
	printf 'artifact_graph=%s sbom_sha256=%s\n' "${graph}" "${sbom_sha256}" >>"${receipt}"
	# Keep BuildKit cache and receipt/SBOM, release the loaded image before the
	# next serial graph so disk usage is bounded by one derived image at a time.
	record_command "${receipt}" "${log_dir}/artifact-${graph}-cleanup.log" "artifact-${graph}-cleanup" \
		docker image rm "${image}"
}

run_graph() {
	local graph=$1 database=$2 authn=$3 outbound_http=$4 outbound_auth=$5 http_idempotency=$6 jobs=$7 messaging=$8 outbox=$9 webhooks=${10} inbound_webhooks=${11} grpc=${12:-none} cache=${13:-none} object_storage=${14:-none} service_auth=${15:-none}
	local target output_revision openapi_sha256 cargo_lock_sha256 identity description full_graph source_common source_tools_root
	local -a db_tests=() runtime_environment=("${scrubbed_identity[@]}" "CARGO_TARGET_DIR=${target_cache}")
	if [[ ${outbox} == postgres ]]; then
		identity="matrix-outbox-${graph}"
		description="Transactional outbox matrix ${database} ${authn} ${outbound_http} ${outbound_auth} ${http_idempotency} ${webhooks} ${inbound_webhooks}"
	elif [[ ${messaging} == nats-jetstream ]]; then
		identity="matrix-messaging-${graph}"
		description="Messaging matrix ${database} ${authn} ${outbound_http} ${outbound_auth} ${http_idempotency} ${jobs}"
	elif [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		identity="matrix-w${graph}"
		description="Webhook matrix ${database} ${authn} ${outbound_http} ${outbound_auth} ${http_idempotency} ${webhooks} ${inbound_webhooks}"
	elif [[ ${jobs} == none ]]; then
		identity="matrix-${graph}-core"
		description="Matrix ${database} ${authn} ${outbound_http} ${outbound_auth} ${http_idempotency} core"
	else
		identity="matrix-${graph}-jobs-core"
		description="Matrix ${database} ${authn} ${outbound_http} ${outbound_auth} ${http_idempotency} jobs core"
	fi
	if [[ ${http_idempotency} == postgres ]]; then
		db_tests+=(--test http_idempotency)
	fi
	if [[ ${jobs} == postgres ]]; then
		db_tests+=(--test jobs)
	fi
	if [[ ${outbox} == postgres ]]; then
		db_tests+=(--test messaging_outbox)
	fi
	if ((graph <= 26)); then
		full_graph=true
	else
		case "${graph}" in
		27 | 29 | 30 | 47 | 48 | 49 | 50 | 53 | 54 | 55 | 66 | 67) full_graph=true ;;
		*) full_graph=false ;;
		esac
	fi
	if [[ ${full_graph} == true ]] && [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		db_tests+=(--test webhooks)
	fi
	target=${work}/runtime-${graph}-${database}-${authn}-${outbound_http}-${outbound_auth}-${http_idempotency}-${jobs}-${messaging}-${outbox}-${webhooks}-${inbound_webhooks}-${grpc}-${cache}-${object_storage}-${service_auth}
	git clone --quiet --no-local "${source}" "${target}"
	if [[ -d ${target}/vendor ]]; then
		find "${target}/vendor" -exec touch -h -r "${vendor_stamp}" {} +
	fi
	# Pinned tools (cargo-shear for every graph, protoc for gRPC) are built
	# once per attempt rather than once per initialized clone.
	source_common=$(git -C "${source}" rev-parse --path-format=absolute --git-common-dir)
	source_tools_root=${TOOLS_ROOT:-${source_common}/tools}
	runtime_environment+=("TOOLS_ROOT=${source_tools_root}")
	printf 'runtime_graph=%s database=%s authn=%s outbound_http=%s outbound_auth=%s grpc=%s http_idempotency=%s jobs=%s messaging=%s outbox=%s webhooks=%s inbound_webhooks=%s cache=%s object_storage=%s service_auth=%s harness=core candidate=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${outbound_auth}" "${grpc}" "${http_idempotency}" "${jobs}" "${messaging}" "${outbox}" "${webhooks}" "${inbound_webhooks}" "${cache}" "${object_storage}" "${service_auth}" "${candidate}" >>"${receipt}"
	record_command "${receipt}" "${log_dir}/runtime-${graph}-init.log" "runtime-${graph}-init" \
		"${runtime_environment[@]}" env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 \
		python3 -B -E "${source}/scripts/template/init.py" --repo "${target}" \
		--service-name "${identity}" \
		--repository "https://github.com/example/${identity}" \
		--description "${description}" \
		--codeowner @example/platform --database "${database}" --authn "${authn}" \
		--outbound-http "${outbound_http}" --outbound-auth "${outbound_auth}" --grpc "${grpc}" --http-idempotency "${http_idempotency}" --jobs "${jobs}" --messaging "${messaging}" --outbox "${outbox}" \
		--webhooks "${webhooks}" --inbound-webhooks "${inbound_webhooks}" --cache "${cache}" --object-storage "${object_storage}" \
		--service-auth "${service_auth}" --agent-harness core
	git -C "${target}" config user.email template-init-check@example.invalid
	git -C "${target}" config user.name template-init-check
	git -C "${target}" add -A
	git -C "${target}" commit -qm "initialized ${database}/${authn}/${outbound_http}/${outbound_auth}/${grpc}/${http_idempotency}/${jobs}/${messaging}/${outbox}/core"
	output_revision=$(git -C "${target}" rev-parse HEAD)
	openapi_sha256=$(shasum -a 256 "${target}/api/openapi/service.yaml" | awk '{print $1}')
	cargo_lock_sha256=$(shasum -a 256 "${target}/Cargo.lock" | awk '{print $1}')
	printf 'status=passed label=runtime-%s-initialized output_revision=%s openapi_sha256=%s cargo_lock_sha256=%s\n' \
		"${graph}" "${output_revision}" "${openapi_sha256}" "${cargo_lock_sha256}" >>"${receipt}"
	printf 'template initializer runtime_graph=%s database=%s authn=%s outbound_http=%s outbound_auth=%s grpc=%s http_idempotency=%s jobs=%s messaging=%s outbox=%s webhooks=%s inbound_webhooks=%s cache=%s object_storage=%s service_auth=%s candidate=%s revision=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${outbound_auth}" "${grpc}" "${http_idempotency}" "${jobs}" "${messaging}" "${outbox}" "${webhooks}" "${inbound_webhooks}" "${cache}" "${object_storage}" "${service_auth}" "${candidate}" "${output_revision}"
	if [[ ${mode} == artifact-graphs ]]; then
		local expected_inventory=/service
		[[ ${database} == none ]] || expected_inventory+=,/migrate
		[[ ${jobs} == none && ${messaging} == none ]] || expected_inventory+=,/jobs-worker
		printf 'artifact_graph=%s expected_inventory=%s output_tree=%s\n' "${graph}" "${expected_inventory}" \
			"$(git -C "${target}" rev-parse 'HEAD^{tree}')" >>"${receipt}"
		run_artifact "${graph}" "${target}" "${output_revision}" "${identity}"
		return
	fi
	if ((graph > 26)); then
		# The child shell expands its manifest argument; the caller must preserve $1.
		# shellcheck disable=SC2016
		record_command "${receipt}" "${log_dir}/runtime-${graph}-metadata.log" "runtime-${graph}-metadata" \
			"${runtime_environment[@]}" bash -c \
			'exec cargo metadata --locked --offline --format-version 1 --manifest-path "$1" >/dev/null' _ "${target}/Cargo.toml"
	fi
	# A profile that removes the last user of a dependency must remove its
	# declaration too; the derived service's own gate rejects the rest.
	record_command "${receipt}" "${log_dir}/runtime-${graph}-unused-deps.log" "runtime-${graph}-unused-deps" \
		"${runtime_environment[@]}" make -C "${target}" unused-deps
	if [[ ${full_graph} == true ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-build.log" "runtime-${graph}-build" \
			"${runtime_environment[@]}" make -C "${target}" build
		# A derived service lints its own profile subset; the full template hides
		# code that only a removed profile used.
		record_command "${receipt}" "${log_dir}/runtime-${graph}-lint.log" "runtime-${graph}-lint" \
			"${runtime_environment[@]}" make -C "${target}" lint
		record_command "${receipt}" "${log_dir}/runtime-${graph}-test.log" "runtime-${graph}-test" \
			"${runtime_environment[@]}" make -C "${target}" test
		if ((${#db_tests[@]} > 0)); then
			record_command "${receipt}" "${log_dir}/runtime-${graph}-db.log" "runtime-${graph}-db" \
				"${runtime_environment[@]}" REQUIRE_DOCKER=1 bash "${target}/scripts/profiles/integration.sh" db "${db_tests[@]}"
		fi
		return
	fi
	local -a check_features=()
	if [[ ${database} == postgres ]]; then
		check_features=(--features integration-tests/integration)
	fi
	# Clippy compiles what check compiled and holds the derived service's lint
	# policy, which the full template alone cannot prove for this profile set.
	record_command "${receipt}" "${log_dir}/runtime-${graph}-check.log" "runtime-${graph}-check" \
		"${runtime_environment[@]}" cargo clippy --workspace --all-targets \
			${check_features[@]+"${check_features[@]}"} --locked --offline --manifest-path "${target}/Cargo.toml" -- -D warnings
	case "${graph}" in
	57 | 58 | 59 | 68)
		record_command "${receipt}" "${log_dir}/runtime-${graph}-grpc-transport.log" "runtime-${graph}-grpc-transport" \
			"${runtime_environment[@]}" cargo test -p infra-grpc --test transport --locked --offline --manifest-path "${target}/Cargo.toml"
		;;
	esac
	case "${graph}" in
	58 | 59 | 68)
		record_command "${receipt}" "${log_dir}/runtime-${graph}-grpc-verifier.log" "runtime-${graph}-grpc-verifier" \
			"${runtime_environment[@]}" cargo test -p "${identity}" --test lifecycle \
				missing_grpc_verifier_lists_only_the_supported_modes --locked --offline --manifest-path "${target}/Cargo.toml"
		;;
	esac
	if [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-provider.log" "runtime-${graph}-provider" \
			"${runtime_environment[@]}" make -C "${target}" test-package PKG=infra-webhooks
	fi
	if [[ ${inbound_webhooks} == standard-webhooks ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-contract.log" "runtime-${graph}-contract" \
			"${runtime_environment[@]}" cargo test -p "${identity}" --test openapi --locked --offline --manifest-path "${target}/Cargo.toml"
		record_command "${receipt}" "${log_dir}/runtime-${graph}-lifecycle.log" "runtime-${graph}-lifecycle" \
			"${runtime_environment[@]}" cargo test -p "${identity}" --test lifecycle \
				inert_inbound_webhook_route_rejects_unknown_endpoint_without_signature_work --locked --offline --manifest-path "${target}/Cargo.toml"
	fi
}

run_validation() {
	local work source candidate common receipt_dir receipt log_dir target_cache vendor_stamp graph_rows graph
	local started=${SECONDS}
	local -a row
	local -a scrubbed_identity=(env -u SERVICE_NAME -u REPOSITORY -u DESCRIPTION -u CODEOWNER -u DATABASE -u AUTHN -u OUTBOUND_HTTP -u OUTBOUND_AUTH -u GRPC -u HTTP_IDEMPOTENCY -u JOBS -u MESSAGING -u OUTBOX -u WEBHOOKS -u INBOUND_WEBHOOKS -u CACHE -u OBJECT_STORAGE -u SERVICE_AUTH -u AGENT_HARNESS)
	if [[ ${mode} == full || ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then
		graph_rows=$(plan_graphs) || return $?
	fi
	if [[ ${mode} == full || ${mode} == runtime-graphs ]]; then
		while IFS=' ' read -r graph _; do
			case "${graph}" in
			1[3-9] | 2[0-6] | 27 | 29 | 30 | 48 | 49 | 53 | 55)
				if ! docker info >/dev/null 2>&1; then
					echo "graphs 13-26,27,29,30,48,49,53,55 need a usable Docker daemon for their retained database suites; select an eligible focused graph when Docker is unavailable" >&2
					return 2
				fi
				break
				;;
			esac
		done <<<"${graph_rows}"
	fi
	if [[ ${mode} == artifact-graphs && -z ${runtime_graphs} ]]; then
		while IFS=' ' read -r graph _; do
			runtime_graphs+="${runtime_graphs:+,}${graph}"
		done <<<"${graph_rows}"
	fi
	common=$(git -C "${repo}" rev-parse --path-format=absolute --git-common-dir)
	receipt_dir=${common}/codex/template-init
	mkdir -p "${receipt_dir}"
	work=$(mktemp -d)
	validation_work=${work}
	validation_receipt_dir=${receipt_dir}
	trap cleanup_validation EXIT
	receipt=$(mktemp "${receipt_dir}/attempt.XXXXXX")
	log_dir=$(mktemp -d "${receipt_dir}/attempt-logs.XXXXXX")
	prune_receipt_dir "${receipt_dir}"
	# Explicit caller caches survive this attempt; only private work is removed.
	# Every initialization and representative shares this one absolute cache,
	# so the locked dependency graph compiles once per attempt, not per init.
	target_cache=${CARGO_TARGET_DIR:-${work}/cargo-target}
	[[ ${target_cache} == /* ]] || target_cache=${PWD}/${target_cache}
	export CARGO_TARGET_DIR=${target_cache}
	# The vendored [patch] crates are path dependencies, fresh by timestamp,
	# and byte-identical in every clone of this attempt: no profile renders
	# them, and a removed profile deletes a whole directory. Each clone dates
	# them to this moment, before anything compiles, so a graph reuses what an
	# earlier graph built instead of rebuilding them and every dependent in
	# each lint, test and database step. An older artifact still rebuilds.
	vendor_stamp=${work}/vendor-stamp
	touch "${vendor_stamp}"
	source=${work}/source
	candidate=$(snapshot_candidate "${source}")
	printf 'candidate=%s\nsource_revision=%s\nmode=%s\nstate=running\n' "${candidate}" "$(git -C "${repo}" rev-parse HEAD)" "${mode}" >"${receipt}"
	printf 'source_tree=%s\ncandidate_tree=%s\nrun_id=%s\nproducing_attempt=%s\njob=%s\nstarted_epoch=%s\n' \
		"$(git -C "${repo}" rev-parse 'HEAD^{tree}')" "$(git -C "${source}" rev-parse 'HEAD^{tree}')" \
		"${GITHUB_RUN_ID:-local}" "${GITHUB_RUN_ATTEMPT:-local}" "${GITHUB_JOB:-local}" "$(date +%s)" >>"${receipt}"
	if [[ ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then printf 'requested_runtime_graphs=%s\n' "${runtime_graphs}" >>"${receipt}"; fi
	printf 'template initializer fixed candidate: %s\n' "${candidate}"
	printf 'template initializer receipt: %s\n' "${receipt}"

	if [[ ${mode} == full || ${mode} == source-checks ]]; then record_source_suites; fi
	if [[ ${mode} == full || ${mode} == projections-only ]]; then
		record_command "${receipt}" "${log_dir}/projection-self-test.log" "projection-self-test" \
			"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --self-test
		record_command "${receipt}" "${log_dir}/projections.log" "canonical-projections" \
			"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-profile-projections.py" --source "${source}"
	fi
	if [[ ${mode} == quality-projections ]]; then
		record_command "${receipt}" "${log_dir}/quality-projections.log" "quality-projections" \
			"${scrubbed_identity[@]}" python3 -B "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --quality-only
	fi
	if [[ ${mode} == full || ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then
		while IFS=' ' read -r -a row; do
			run_graph "${row[@]}"
		done <<<"${graph_rows}"
	fi
	printf 'state=passed\nduration_seconds=%s\n' "$((SECONDS - started))" >>"${receipt}"
	trap - EXIT
	cleanup_validation
}

# The outer call owns the validation lock once. Representatives inherit it
# and run sequentially; focused receipts retain their narrower mode.
if [[ ${mode} == self-test ]]; then
	recorder_self_test
elif [[ ${mode} == list-graphs ]]; then
	graph_rows=$(plan_graphs) || exit $?
	printf '%s\n' "${graph_rows}"
elif [[ ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	python3 -B "${repo}/scripts/verify/lock.py" --assert-held
	run_validation
else
	arguments=(--repo "${repo}")
	if [[ ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then
		if [[ ${graph_plan_args[0]} == --artifact-graphs ]]; then
			arguments+=(--default-artifact-graphs)
		else
			arguments+=("--${mode}" "${runtime_graphs}")
		fi
	elif [[ ${mode} != full ]]; then
		arguments+=("--${mode}")
	fi
	python3 -B "${repo}/scripts/verify/lock.py" -- bash "$0" "${arguments[@]}"
fi
