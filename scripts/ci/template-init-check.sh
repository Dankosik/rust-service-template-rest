#!/usr/bin/env bash
# Validate all canonical projections and sixty-five distinct runtime graphs
# from one private, fixed source candidate. The shared checkout is never
# staged or committed.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
repo=${ROOT_DIR}
mode=full
runtime_graphs=all


validate_runtime_graphs() {
	local selection=$1 number seen=,
	[[ ${selection} =~ ^([1-9]|[1-5][0-9]|6[0-5])(,([1-9]|[1-5][0-9]|6[0-5]))*$ ]] || return 1
	local -a numbers
	IFS=, read -r -a numbers <<<"${selection}"
	for number in "${numbers[@]}"; do
		[[ ${seen} != *",${number},"* ]] || return 1
		seen+="${number},"
	done
}

validate_artifact_graphs() {
	validate_runtime_graphs "$1" || return
	local number
	local -a numbers
	IFS=, read -r -a numbers <<<"$1"
	for number in "${numbers[@]}"; do
		case "${number}" in 1 | 7 | 47 | 65) ;; *) return 1 ;; esac
	done
}

runtime_graph_selected() {
	[[ ( ${mode} != runtime-graphs && ${mode} != artifact-graphs ) || ,${runtime_graphs}, == *",${1},"* ]]
}

while (($#)); do
	case "$1" in
	--self-test | --source-checks | --projections-only | --quality-projections | --image-context | --list-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		mode=${1#--}
		shift
		;;
	--runtime-graphs | --artifact-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		validate_runtime_graphs "${2:-}" || { echo "--runtime-graphs requires distinct comma-separated graph IDs 1..65" >&2; exit 2; }
		mode=${1#--}
		if [[ ${mode} == artifact-graphs ]]; then
			validate_artifact_graphs "${2:-}" || { echo "--artifact-graphs accepts only distinct IDs 1,7,47,65" >&2; exit 2; }
		fi
		runtime_graphs=$2
		shift 2
		;;
	--repo)
		[[ -n ${2:-} ]] || { echo "--repo requires a path" >&2; exit 2; }
		repo=$(cd "${2}" && pwd)
		shift 2
		;;
	*)
		echo "usage: $0 [--repo ROOT] [--source-checks|--projections-only|--quality-projections|--image-context|--runtime-graphs IDS|--artifact-graphs IDS|--list-graphs|--self-test]" >&2
		exit 2
		;;
	esac
done

if [[ ${mode} == runtime-graphs && ${ALLOW_FULL:-} != 1 && ${CI:-} != true ]]; then
	echo "--runtime-graphs requires ALLOW_FULL=1 (CI sets CI=true)" >&2
	exit 2
fi

if [[ ${mode} == artifact-graphs && ${ALLOW_HEAVY:-} != 1 && ${CI:-} != true ]]; then
	echo "--artifact-graphs requires ALLOW_HEAVY=1 (CI sets CI=true)" >&2
	exit 2
fi

if [[ ${mode} == full || ${mode} == runtime-graphs ]]; then
	needs_docker=false
	for number in {13..26} 27 29 30 48 49 53 55; do
		runtime_graph_selected "${number}" && needs_docker=true
	done
	if [[ ${needs_docker} == true ]] && ! docker info >/dev/null 2>&1; then
		echo "graphs 13-26,27,29,30,48,49,53,55 need a usable Docker daemon for their retained database suites; select an eligible focused graph when Docker is unavailable" >&2
		exit 2
	fi
fi

candidate_paths=${TEMPLATE_INIT_CANDIDATE_PATHS:-${repo}/scripts/tests/template-candidate-paths.txt}
[[ -f ${candidate_paths} ]] || { echo "candidate path allowlist is missing: ${candidate_paths}" >&2; exit 2; }

copy_path() {
	local relative destination source
	relative=$1
	destination=$2
	source=${repo}/${relative}
	[[ ${relative} != /* && ${relative} != *'..'* && ${relative} != *$'\n'* ]] || {
		echo "unsafe candidate path: ${relative}" >&2; return 2
	}
	if [[ -L ${source} ]]; then
		case "${relative}" in
		.claude/skills/* | .qwen/skills/*)
			local link_target
			link_target=$(readlink "${source}")
			[[ ${link_target} == ../../.agents/skills/* && -d "${repo}/.agents/skills/${link_target##*/}" ]] || {
				echo "candidate canonical skill link is unsafe: ${relative}" >&2; return 2
			}
			mkdir -p "${destination}/$(dirname "${relative}")" || return
			ln -s "${link_target}" "${destination}/${relative}" || return
			return
			;;
		esac
		echo "candidate path is an unsupported symlink: ${relative}" >&2
		return 2
	fi
	[[ -f ${source} ]] || { echo "candidate path is not a regular file: ${relative}" >&2; return 2; }
	mkdir -p "${destination}/$(dirname "${relative}")" || return
	cp -p "${source}" "${destination}/${relative}"
}

snapshot_candidate() {
	local destination=$1 relative
	# This function runs in command substitution: explicit propagation keeps a
	# failed inventory/copy/git operation from committing an incomplete source.
	mkdir -p "${destination}" || return
	git -C "${repo}" ls-files -z | while IFS= read -r -d '' relative; do
		# A tracked worktree deletion belongs to the candidate as a deletion.
		[[ -e ${repo}/${relative} || -L ${repo}/${relative} ]] || continue
		copy_path "${relative}" "${destination}" || return
	done || return
	while IFS= read -r relative || [[ -n ${relative} ]]; do
		[[ -z ${relative} || ${relative} == \#* ]] && continue
		if [[ ${relative} == */ ]]; then
			[[ ${relative} == evals/template-initializer/ || ${relative} == specs/template-initializer/ || ${relative} == crates/infra-bearerauthn/ || ${relative} == crates/infra-outbound-http/ || ${relative} == crates/infra-oauth2-client-credentials/ || ${relative} == crates/infra-webhooks/ || ${relative} == crates/infra-idempotency-store/ || ${relative} == crates/infra-http/src/idempotency/ || ${relative} == crates/infra-jobs/ || ${relative} == crates/domain-events/ || ${relative} == crates/infra-messaging/ || ${relative} == crates/infra-cache/ || ${relative} == test/tests/http_idempotency/ || ${relative} == crates/jobs-worker/ || ${relative} == test/tests/jobs/ || ${relative} == test/tests/webhooks/ || ${relative} == test/src/bin/ || ${relative} == env/nats/ || ${relative} == env/nats/auth-rotation/ || ${relative} == vendor/async-nats/ || ${relative} == vendor/aws-smithy-http-client/ || ${relative} == vendor/hyper-util/ || ${relative} == docs/universal-disciplines/ || ${relative} == evals/rust-reliability/ ]] || {
				echo "candidate directory is not authorized: ${relative}" >&2; return 2
			}
			find -P "${repo}/${relative}" \( -type f -o -type l \) -print0 | while IFS= read -r -d '' nested; do
				nested=${nested#"${repo}"/}
				git -C "${repo}" check-ignore -q -- "${nested}" && continue
				copy_path "${nested}" "${destination}" || return
			done || return
			continue
		fi
		git -C "${repo}" check-ignore -q -- "${relative}" && {
			echo "authorized candidate path is ignored: ${relative}" >&2; return 2
		}
		copy_path "${relative}" "${destination}" || return
	done <"${candidate_paths}" || return
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
	local fixture receipt log later exit_code
	fixture=$(mktemp -d)
	trap 'rm -rf -- "${fixture}"' RETURN
	receipt=${fixture}/receipt
	log=${fixture}/failed.log
	later=${fixture}/later
	: >"${receipt}"
	sequence() {
		record_command "${receipt}" "${log}" "forced-failure" bash -c 'exit 7' && \
			record_command "${receipt}" "${fixture}/later.log" "must-not-run" touch "${later}"
	}
	if sequence; then
		echo "recorder self-test accepted a failing command" >&2
		return 1
	else
		exit_code=$?
	fi
	[[ ${exit_code} == 7 ]] || { echo "recorder self-test changed exit ${exit_code}" >&2; return 1; }
	grep -q 'status=failed label=forced-failure exit_code=7 ' "${receipt}"
	[[ ! -e ${later} ]] || { echo "recorder self-test executed a later stage" >&2; return 1; }
	local mode=full runtime_graphs=all number invalid
	for number in {1..65}; do
		runtime_graph_selected "${number}" || { echo "default full mode skipped graph ${number}" >&2; return 1; }
	done
	mode=runtime-graphs
	runtime_graphs="3,4,5,6,$(seq -s, 9 65)"
	runtime_graphs=${runtime_graphs%,}
	validate_runtime_graphs "${runtime_graphs}"
	for number in {1..65}; do
		case "${number}" in
		1 | 2 | 7 | 8) if runtime_graph_selected "${number}"; then echo "subset selected graph ${number}" >&2; return 1; fi ;;
		*) runtime_graph_selected "${number}" || { echo "subset skipped graph ${number}" >&2; return 1; } ;;
		esac
	done
	for invalid in '' 0 66 01 '1,1' '2,,3' '1,'; do
		if validate_runtime_graphs "${invalid}"; then echo "invalid graph selection accepted" >&2; return 1; fi
	done
	mode=artifact-graphs
	runtime_graphs=1,7,47,65
	validate_artifact_graphs "${runtime_graphs}"
	for number in {1..65}; do
		case "${number}" in
		1 | 7 | 47 | 65) runtime_graph_selected "${number}" || return 1 ;;
		*) if runtime_graph_selected "${number}"; then return 1; fi ;;
		esac
	done
	for invalid in 2 48 '1,7,7' '1,65,64'; do
		if validate_artifact_graphs "${invalid}"; then echo "noncanonical artifact selection accepted" >&2; return 1; fi
	done
	bash "${ROOT_DIR}/scripts/ci/measure.sh" --self-test
	python3 "${ROOT_DIR}/scripts/ci/image-results.py" --self-test
	snapshot_failure_self_test "${fixture}"
	artifact_recorder_self_test "${fixture}"
	printf 'template initializer graph selection self-test: pass\n'
	printf 'template initializer recorder self-test: pass\n'
}

snapshot_failure_self_test() (
	local fixture=$1 fixture_repo fixture_paths destination result selected
	fixture_repo=${fixture}/snapshot-source
	fixture_paths=${fixture}/snapshot-paths
	destination=${fixture}/partial-snapshot
	mkdir -p "${fixture_repo}" "${fixture}/snapshot-bin"
	printf 'kept\n' >"${fixture_repo}/a"
	printf 'must not disappear\n' >"${fixture_repo}/b"
	: >"${fixture_paths}"
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
	if selected=$(env "PATH=${fixture}/snapshot-bin:${PATH}" "repo=${fixture_repo}" "candidate_paths=${fixture_paths}" \
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
	[[ $(grep -c 'runtime-image-build' "${ARTIFACT_TEST_CALLS}") == 1 ]]
	[[ $(wc -l <"${ARTIFACT_TEST_CALLS}" | tr -d ' ') == 6 ]]
	: >"${ARTIFACT_TEST_CALLS}"
	: >"${artifact_receipt}"
	export ARTIFACT_TEST_FAIL=container-security
	if "${artifact_environment[@]}" bash -c 'set -euo pipefail; run_artifact 47 /project initialized-revision renamed-service; run_artifact 65 /project later-revision later-service'; then
		echo "artifact runner accepted a failed security gate" >&2; exit 1
	else result=$?; fi
	[[ ${result} == 19 ]]
	grep -q 'status=failed label=artifact-47-security exit_code=19 ' "${artifact_receipt}"
	if grep -Eq 'container-sbom|later-service|image rm' "${ARTIFACT_TEST_CALLS}"; then
		echo "artifact runner continued after a failed security gate" >&2; exit 1
	fi
)

record_source_suites() {
	record_command "${receipt}" "${log_dir}/source-purity.log" "source-purity" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-owned-purity.py" --repo "${source}"
	record_command "${receipt}" "${log_dir}/source-init-safety.log" "source-init-safety" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-init-safety.py" --source "${source}"
	record_command "${receipt}" "${log_dir}/source-sync-canary.log" "source-sync-canary" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-sync-canary.py" --source "${source}"
	record_command "${receipt}" "${log_dir}/source-upgrade.log" "source-upgrade" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-upgrade.py" --source "${source}"
}

run_artifact() {
	local graph=$1 target=$2 revision=$3 identity=$4 image image_id sbom_sha256
	image="template-artifact:${candidate:0:12}-${graph}"
	# Each graph keeps its local BuildKit layers. Exporting four renamed cooked
	# graphs over the source cache would evict its useful entry and multiply
	# remote cache storage, so derived builds only import the existing cache.
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
	local graph=$1 database=$2 authn=$3 outbound_http=$4 outbound_auth=$5 http_idempotency=$6 jobs=$7 messaging=$8 outbox=$9 webhooks=${10} inbound_webhooks=${11} grpc=${12:-none} cache=${13:-none} object_storage=${14:-none}
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
		27 | 29 | 30 | 47 | 48 | 49 | 50 | 53 | 54 | 55) full_graph=true ;;
		*) full_graph=false ;;
		esac
	fi
	if [[ ${full_graph} == true ]] && [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		db_tests+=(--test webhooks)
	fi
	target=${work}/runtime-${graph}-${database}-${authn}-${outbound_http}-${outbound_auth}-${http_idempotency}-${jobs}-${messaging}-${outbox}-${webhooks}-${inbound_webhooks}-${grpc}-${cache}-${object_storage}
	git clone --quiet --no-local "${source}" "${target}"
	if [[ ${grpc} == enabled ]]; then
		source_common=$(git -C "${source}" rev-parse --git-common-dir)
		[[ ${source_common} == /* ]] || source_common=${source}/${source_common}
		source_tools_root=${TOOLS_ROOT:-$(cd "${source_common}" && pwd)/tools}
		runtime_environment+=("TOOLS_ROOT=${source_tools_root}")
	fi
	printf 'runtime_graph=%s database=%s authn=%s outbound_http=%s outbound_auth=%s grpc=%s http_idempotency=%s jobs=%s messaging=%s outbox=%s webhooks=%s inbound_webhooks=%s cache=%s object_storage=%s harness=core candidate=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${outbound_auth}" "${grpc}" "${http_idempotency}" "${jobs}" "${messaging}" "${outbox}" "${webhooks}" "${inbound_webhooks}" "${cache}" "${object_storage}" "${candidate}" >>"${receipt}"
	record_command "${receipt}" "${log_dir}/runtime-${graph}-init.log" "runtime-${graph}-init" \
		"${runtime_environment[@]}" bash "${source}/scripts/init-module.sh" --repo "${target}" \
		--service-name "${identity}" \
		--repository "https://github.com/example/${identity}" \
		--description "${description}" \
		--codeowner @example/platform --database "${database}" --authn "${authn}" \
		--outbound-http "${outbound_http}" --outbound-auth "${outbound_auth}" --grpc "${grpc}" --http-idempotency "${http_idempotency}" --jobs "${jobs}" --messaging "${messaging}" --outbox "${outbox}" \
		--webhooks "${webhooks}" --inbound-webhooks "${inbound_webhooks}" --cache "${cache}" --object-storage "${object_storage}" --agent-harness core
	git -C "${target}" config user.email template-init-check@example.invalid
	git -C "${target}" config user.name template-init-check
	git -C "${target}" add -A
	git -C "${target}" commit -qm "initialized ${database}/${authn}/${outbound_http}/${outbound_auth}/${grpc}/${http_idempotency}/${jobs}/${messaging}/${outbox}/core"
	output_revision=$(git -C "${target}" rev-parse HEAD)
	openapi_sha256=$(shasum -a 256 "${target}/api/openapi/service.yaml" | awk '{print $1}')
	cargo_lock_sha256=$(shasum -a 256 "${target}/Cargo.lock" | awk '{print $1}')
	printf 'status=passed label=runtime-%s-initialized output_revision=%s openapi_sha256=%s cargo_lock_sha256=%s\n' \
		"${graph}" "${output_revision}" "${openapi_sha256}" "${cargo_lock_sha256}" >>"${receipt}"
	printf 'template initializer runtime_graph=%s database=%s authn=%s outbound_http=%s outbound_auth=%s grpc=%s http_idempotency=%s jobs=%s messaging=%s outbox=%s webhooks=%s inbound_webhooks=%s cache=%s object_storage=%s candidate=%s revision=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${outbound_auth}" "${grpc}" "${http_idempotency}" "${jobs}" "${messaging}" "${outbox}" "${webhooks}" "${inbound_webhooks}" "${cache}" "${object_storage}" "${candidate}" "${output_revision}"
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
	if [[ ${full_graph} == true ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-build.log" "runtime-${graph}-build" \
			"${runtime_environment[@]}" make -C "${target}" build
		record_command "${receipt}" "${log_dir}/runtime-${graph}-test.log" "runtime-${graph}-test" \
			"${runtime_environment[@]}" make -C "${target}" test
		if ((${#db_tests[@]} > 0)); then
			record_command "${receipt}" "${log_dir}/runtime-${graph}-db.log" "runtime-${graph}-db" \
				"${runtime_environment[@]}" REQUIRE_DOCKER=1 bash "${target}/scripts/ci/test-integration-db.sh" "${db_tests[@]}"
		fi
		return
	fi
	local -a check_features=()
	if [[ ${database} == postgres ]]; then
		check_features=(--features integration-tests/integration)
	fi
	record_command "${receipt}" "${log_dir}/runtime-${graph}-check.log" "runtime-${graph}-check" \
		"${runtime_environment[@]}" cargo check --workspace --all-targets \
			"${check_features[@]}" --locked --offline --manifest-path "${target}/Cargo.toml"
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

# Visit every runtime graph in inventory order with its profile tuple:
# graph database authn outbound_http outbound_auth http_idempotency jobs
# messaging outbox webhooks inbound_webhooks [grpc [cache]].
each_runtime_graph() {
	local action=$1 graph=0 database authn outbound_http selection http_idempotency
	for database in none postgres; do
		for authn in none oidc-jwt oidc-introspection; do
			for outbound_http in none bounded; do
				((graph += 1))
				runtime_graph_selected "${graph}" || continue
					"${action}" "${graph}" "${database}" "${authn}" "${outbound_http}" none none none none none none none
			done
		done
	done
	for authn in oidc-jwt oidc-introspection; do
		for outbound_http in none bounded; do
			((graph += 1))
			runtime_graph_selected "${graph}" || continue
				"${action}" "${graph}" postgres "${authn}" "${outbound_http}" none postgres none none none none none
		done
	done
	for authn in none oidc-jwt oidc-introspection; do
		for outbound_http in none bounded; do
			((graph += 1))
			runtime_graph_selected "${graph}" || continue
				"${action}" "${graph}" postgres "${authn}" "${outbound_http}" none none postgres none none none none
		done
	done
	for authn in oidc-jwt oidc-introspection; do
		for outbound_http in none bounded; do
			((graph += 1))
			runtime_graph_selected "${graph}" || continue
				"${action}" "${graph}" postgres "${authn}" "${outbound_http}" none postgres postgres none none none none
		done
	done
	for selection in none:none oidc-jwt:none oidc-introspection:none oidc-jwt:postgres oidc-introspection:postgres; do
		IFS=: read -r authn http_idempotency <<<"${selection}"
		for outbound_http in none bounded; do
			if [[ ${outbound_http} == none ]]; then
				((graph += 1))
				runtime_graph_selected "${graph}" || continue
					"${action}" "${graph}" postgres "${authn}" none none "${http_idempotency}" postgres none none none standard-webhooks
				continue
			fi
			((graph += 1))
			if runtime_graph_selected "${graph}"; then
					"${action}" "${graph}" postgres "${authn}" bounded none "${http_idempotency}" postgres none none none standard-webhooks
			fi
			((graph += 1))
			if runtime_graph_selected "${graph}"; then
					"${action}" "${graph}" postgres "${authn}" bounded none "${http_idempotency}" postgres none none durable none
			fi
			((graph += 1))
			if runtime_graph_selected "${graph}"; then
					"${action}" "${graph}" postgres "${authn}" bounded none "${http_idempotency}" postgres none none durable standard-webhooks
			fi
		done
	done
	[[ ${graph} == 46 ]] || { echo "webhook graph inventory ended at ${graph}, expected 46" >&2; return 1; }
	graph=47; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none none none none none nats-jetstream none none none
	graph=48; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres none none none none postgres nats-jetstream postgres none none
	graph=49; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded none postgres postgres nats-jetstream postgres durable standard-webhooks
	graph=50; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none bounded oauth2-client-credentials none none none none none none
	graph=51; runtime_graph_selected "${graph}" && "${action}" "${graph}" none oidc-jwt bounded oauth2-client-credentials none none none none none none
	graph=52; runtime_graph_selected "${graph}" && "${action}" "${graph}" none oidc-introspection bounded oauth2-client-credentials none none none none none none
	graph=53; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded oauth2-client-credentials postgres postgres none none durable standard-webhooks
	graph=54; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none bounded oauth2-client-credentials none none nats-jetstream none none none
	graph=55; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded oauth2-client-credentials postgres postgres nats-jetstream postgres durable standard-webhooks
	# Compile the integration fixture callback with jobs and messaging, without outbox.
	graph=56; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres none none none none postgres nats-jetstream none none none
	graph=57; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none none none none none none none none none enabled
	graph=58; runtime_graph_selected "${graph}" && "${action}" "${graph}" none oidc-jwt none none none none none none none none enabled
	graph=59; runtime_graph_selected "${graph}" && "${action}" "${graph}" none oidc-introspection none none none none none none none none enabled
	graph=60; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none bounded oauth2-client-credentials none none none none none none enabled
	graph=61; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded oauth2-client-credentials postgres postgres nats-jetstream postgres durable standard-webhooks enabled
	# Cache only: no database suite, so this graph does not need Docker.
	graph=62; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none none none none none none none none none none redis
	# Graph 61's profile set plus cache. Focused like 61: locked metadata and cargo check, not a database suite.
	graph=63; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded oauth2-client-credentials postgres postgres nats-jetstream postgres durable standard-webhooks enabled redis
	graph=64; runtime_graph_selected "${graph}" && "${action}" "${graph}" none none none none none none none none none none none none s3
	graph=65; runtime_graph_selected "${graph}" && "${action}" "${graph}" postgres oidc-introspection bounded oauth2-client-credentials postgres postgres nats-jetstream postgres durable standard-webhooks enabled redis s3
	[[ ${graph} == 65 ]] || { echo "profile graph inventory ended at ${graph}, expected 65" >&2; return 1; }
}

# One line per graph for scripts/ci/initializer-matrix.py.
print_graph() {
	printf '%s %s %s %s %s %s %s %s %s %s %s %s %s %s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$9" "${10}" "${11}" "${12:-none}" "${13:-none}" "${14:-none}"
}

run_validation() {
	local work source candidate database authn outbound_http outbound_auth graph=0 common receipt_dir receipt log_dir target_cache
	local started=${SECONDS}
	local -a scrubbed_identity=(env -u SERVICE_NAME -u REPOSITORY -u DESCRIPTION -u CODEOWNER -u DATABASE -u AUTHN -u OUTBOUND_HTTP -u OUTBOUND_AUTH -u GRPC -u HTTP_IDEMPOTENCY -u JOBS -u MESSAGING -u OUTBOX -u WEBHOOKS -u INBOUND_WEBHOOKS -u CACHE -u OBJECT_STORAGE -u AGENT_HARNESS)
	work=$(mktemp -d)
	trap 'rm -rf -- "${work}"' RETURN
	common=$(git -C "${repo}" rev-parse --git-common-dir)
	[[ ${common} == /* ]] || common=${repo}/${common}
	receipt_dir=${common}/codex/template-init
	mkdir -p "${receipt_dir}"
	receipt=$(mktemp "${receipt_dir}/attempt.XXXXXX")
	log_dir=$(mktemp -d "${receipt_dir}/attempt-logs.XXXXXX")
	# Explicit caller caches survive this attempt; only private work is removed.
	# Every initialization and representative shares this one absolute cache,
	# so the locked dependency graph compiles once per attempt, not per init.
	target_cache=${CARGO_TARGET_DIR:-${work}/cargo-target}
	[[ ${target_cache} == /* ]] || target_cache=${PWD}/${target_cache}
	export CARGO_TARGET_DIR=${target_cache}
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
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --self-test
		record_command "${receipt}" "${log_dir}/projections.log" "canonical-projections" \
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}"
	fi
	if [[ ${mode} == image-context ]]; then
		record_command "${receipt}" "${log_dir}/image-context.log" "image-context-metadata" \
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --image-context
	fi
	if [[ ${mode} == quality-projections ]]; then
		record_command "${receipt}" "${log_dir}/quality-projections.log" "quality-projections" \
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --quality-only
	fi
	if [[ ${mode} == full || ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then
		each_runtime_graph run_graph
	fi
	printf 'state=passed\nduration_seconds=%s\n' "$((SECONDS - started))" >>"${receipt}"
}

# The outer call owns the validation lock once. Representatives inherit it
# and run sequentially; focused receipts retain their narrower mode.
if [[ ${mode} == self-test ]]; then
	recorder_self_test
elif [[ ${mode} == list-graphs ]]; then
	each_runtime_graph print_graph
elif [[ ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	run_validation
else
	arguments=(--repo "${repo}")
	if [[ ${mode} == runtime-graphs || ${mode} == artifact-graphs ]]; then
		arguments+=("--${mode}" "${runtime_graphs}")
	elif [[ ${mode} != full ]]; then
		arguments+=("--${mode}")
	fi
	bash "${repo}/scripts/ci/validation-lock.sh" -- bash "$0" "${arguments[@]}"
fi
