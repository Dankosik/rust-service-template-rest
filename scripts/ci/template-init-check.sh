#!/usr/bin/env bash
# Validate all canonical projections and forty-six distinct runtime graphs from
# one private, fixed source candidate. The shared checkout is never staged or committed.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
repo=${ROOT_DIR}
mode=full
runtime_graphs=all


validate_runtime_graphs() {
	local selection=$1 number seen=,
	[[ ${selection} =~ ^([1-9]|[1-3][0-9]|4[0-6])(,([1-9]|[1-3][0-9]|4[0-6]))*$ ]] || return 1
	local -a numbers
	IFS=, read -r -a numbers <<<"${selection}"
	for number in "${numbers[@]}"; do
		[[ ${seen} != *",${number},"* ]] || return 1
		seen+="${number},"
	done
}

runtime_graph_selected() {
	[[ ${mode} != runtime-graphs || ,${runtime_graphs}, == *",${1},"* ]]
}

while (($#)); do
	case "$1" in
	--self-test | --source-checks | --projections-only)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		mode=${1#--}
		shift
		;;
	--runtime-graphs)
		[[ ${mode} == full ]] || { echo "validation modes cannot be combined" >&2; exit 2; }
		validate_runtime_graphs "${2:-}" || { echo "--runtime-graphs requires distinct comma-separated graph IDs 1..46" >&2; exit 2; }
		mode=runtime-graphs
		runtime_graphs=$2
		shift 2
		;;
	--repo)
		[[ -n ${2:-} ]] || { echo "--repo requires a path" >&2; exit 2; }
		repo=$(cd "${2}" && pwd)
		shift 2
		;;
	*)
		echo "usage: $0 [--repo ROOT] [--source-checks|--projections-only|--runtime-graphs IDS|--self-test]" >&2
		exit 2
		;;
	esac
done

if [[ ${mode} == runtime-graphs && ${ALLOW_FULL:-} != 1 && ${CI:-} != true ]]; then
	echo "--runtime-graphs requires ALLOW_FULL=1 (CI sets CI=true)" >&2
	exit 2
fi

if [[ ${mode} == full || ${mode} == runtime-graphs ]]; then
	needs_docker=false
	for number in {13..26} 27 29 30; do
		runtime_graph_selected "${number}" && needs_docker=true
	done
	if [[ ${needs_docker} == true ]] && ! docker info >/dev/null 2>&1; then
		echo "graphs 13-26,27,29,30 need a usable Docker daemon for their retained database suites; select an eligible focused graph when Docker is unavailable" >&2
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
			mkdir -p "${destination}/$(dirname "${relative}")"
			ln -s "${link_target}" "${destination}/${relative}"
			return
			;;
		esac
		echo "candidate path is an unsupported symlink: ${relative}" >&2
		return 2
	fi
	[[ -f ${source} ]] || { echo "candidate path is not a regular file: ${relative}" >&2; return 2; }
	mkdir -p "${destination}/$(dirname "${relative}")"
	cp -p "${source}" "${destination}/${relative}"
}

snapshot_candidate() {
	local destination=$1 relative
	mkdir -p "${destination}"
	while IFS= read -r -d '' relative; do copy_path "${relative}" "${destination}"; done < <(git -C "${repo}" ls-files -z)
	while IFS= read -r relative || [[ -n ${relative} ]]; do
		[[ -z ${relative} || ${relative} == \#* ]] && continue
		if [[ ${relative} == */ ]]; then
			[[ ${relative} == evals/template-initializer/ || ${relative} == specs/template-initializer/ || ${relative} == crates/infra-bearerauthn/ || ${relative} == crates/infra-egress-dns/ || ${relative} == crates/infra-outbound-http/ || ${relative} == crates/infra-webhooks/ || ${relative} == crates/infra-idempotency-store/ || ${relative} == crates/infra-http/src/idempotency/ || ${relative} == test/tests/http_idempotency/ || ${relative} == crates/infra-jobs/ || ${relative} == crates/jobs-worker/ || ${relative} == test/tests/jobs/ || ${relative} == test/tests/webhooks/ || ${relative} == test/src/bin/ || ${relative} == docs/universal-disciplines/ || ${relative} == evals/rust-reliability/ ]] || {
				echo "candidate directory is not authorized: ${relative}" >&2; return 2
			}
			while IFS= read -r -d '' nested; do
				nested=${nested#"${repo}"/}
				git -C "${repo}" check-ignore -q -- "${nested}" && continue
				copy_path "${nested}" "${destination}"
			done < <(find -P "${repo}/${relative}" \( -type f -o -type l \) -print0)
			continue
		fi
		git -C "${repo}" check-ignore -q -- "${relative}" && {
			echo "authorized candidate path is ignored: ${relative}" >&2; return 2
		}
		copy_path "${relative}" "${destination}"
	done <"${candidate_paths}"
	git -C "${destination}" init -q -b main
	git -C "${destination}" config user.email template-init-check@example.invalid
	git -C "${destination}" config user.name template-init-check
	git -C "${destination}" add -A
	git -C "${destination}" commit -qm 'fixed template candidate'
	git -C "${destination}" rev-parse HEAD
}

record_command() {
	local receipt=$1 log=$2 label=$3 started=${SECONDS}
	shift 3
	printf 'command=%q ' "$@" >>"${receipt}"
	printf '\nstatus=running label=%s\n' "${label}" >>"${receipt}"
	if "$@" >"${log}" 2>&1; then
		printf 'status=passed label=%s log_sha256=%s output=%s duration_seconds=%s\n' "${label}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" "$((SECONDS - started))" >>"${receipt}"
		cat "${log}"
		return 0
	else
		local status=$?
		printf 'status=failed label=%s exit_code=%s log_sha256=%s output=%s duration_seconds=%s\n' "${label}" "${status}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" "$((SECONDS - started))" >>"${receipt}"
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
	for number in {1..46}; do
		runtime_graph_selected "${number}" || { echo "default full mode skipped graph ${number}" >&2; return 1; }
	done
	mode=runtime-graphs
	runtime_graphs="3,4,5,6,$(seq -s, 9 46)"
	validate_runtime_graphs "${runtime_graphs}"
	for number in {1..46}; do
		case "${number}" in
		1 | 2 | 7 | 8) if runtime_graph_selected "${number}"; then echo "subset selected graph ${number}" >&2; return 1; fi ;;
		*) runtime_graph_selected "${number}" || { echo "subset skipped graph ${number}" >&2; return 1; } ;;
		esac
	done
	for invalid in '' 0 47 01 '1,1' '2,,3' '1,'; do
		if validate_runtime_graphs "${invalid}"; then echo "invalid graph selection accepted" >&2; return 1; fi
	done
	printf 'template initializer graph selection self-test: pass\n'
	printf 'template initializer recorder self-test: pass\n'
}

record_source_suites() {
	record_command "${receipt}" "${log_dir}/source-purity.log" "source-purity" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-owned-purity.py" --repo "${source}"
	record_command "${receipt}" "${log_dir}/source-init-safety.log" "source-init-safety" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-init-safety.py" --source "${source}"
	record_command "${receipt}" "${log_dir}/source-sync-canary.log" "source-sync-canary" \
		"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-sync-canary.py" --source "${source}"
}

run_graph() {
	local graph=$1 database=$2 authn=$3 outbound_http=$4 http_idempotency=$5 jobs=$6 webhooks=$7 inbound_webhooks=$8
	local target output_revision openapi_sha256 cargo_lock_sha256 identity description full_graph
	local -a db_tests=()
	if [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		identity="matrix-w${graph}"
		description="Webhook matrix ${database} ${authn} ${outbound_http} ${http_idempotency} ${webhooks} ${inbound_webhooks}"
	elif [[ ${jobs} == none ]]; then
		identity="matrix-${database}-${authn}-${outbound_http}-${http_idempotency}-core"
		description="Matrix ${database} ${authn} ${outbound_http} ${http_idempotency} core"
	else
		identity="matrix-${database}-${authn}-${outbound_http}-${http_idempotency}-jobs-core"
		description="Matrix ${database} ${authn} ${outbound_http} ${http_idempotency} jobs core"
	fi
	if [[ ${http_idempotency} == postgres ]]; then
		db_tests+=(--test http_idempotency)
	fi
	if [[ ${jobs} == postgres ]]; then
		db_tests+=(--test jobs)
	fi
	if ((graph <= 26)); then
		full_graph=true
	else
		case "${graph}" in
		27 | 29 | 30) full_graph=true ;;
		*) full_graph=false ;;
		esac
	fi
	if [[ ${full_graph} == true ]] && [[ ${webhooks} != none || ${inbound_webhooks} != none ]]; then
		db_tests+=(--test webhooks)
	fi
	target=${work}/runtime-${graph}-${database}-${authn}-${outbound_http}-${http_idempotency}-${jobs}-${webhooks}-${inbound_webhooks}
	git clone --quiet --no-local "${source}" "${target}"
	printf 'runtime_graph=%s database=%s authn=%s outbound_http=%s http_idempotency=%s jobs=%s webhooks=%s inbound_webhooks=%s harness=core candidate=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${http_idempotency}" "${jobs}" "${webhooks}" "${inbound_webhooks}" "${candidate}" >>"${receipt}"
	record_command "${receipt}" "${log_dir}/runtime-${graph}-init.log" "runtime-${graph}-init" \
		"${scrubbed_identity[@]}" bash "${source}/scripts/init-module.sh" --repo "${target}" \
		--service-name "${identity}" \
		--repository "https://github.com/example/${identity}" \
		--description "${description}" \
		--codeowner @example/platform --database "${database}" --authn "${authn}" \
		--outbound-http "${outbound_http}" --http-idempotency "${http_idempotency}" --jobs "${jobs}" \
		--webhooks "${webhooks}" --inbound-webhooks "${inbound_webhooks}" --agent-harness core
	git -C "${target}" config user.email template-init-check@example.invalid
	git -C "${target}" config user.name template-init-check
	git -C "${target}" add -A
	git -C "${target}" commit -qm "initialized ${database}/${authn}/${outbound_http}/${http_idempotency}/${jobs}/core"
	output_revision=$(git -C "${target}" rev-parse HEAD)
	openapi_sha256=$(shasum -a 256 "${target}/api/openapi/service.yaml" | awk '{print $1}')
	cargo_lock_sha256=$(shasum -a 256 "${target}/Cargo.lock" | awk '{print $1}')
	printf 'status=passed label=runtime-%s-initialized output_revision=%s openapi_sha256=%s cargo_lock_sha256=%s\n' \
		"${graph}" "${output_revision}" "${openapi_sha256}" "${cargo_lock_sha256}" >>"${receipt}"
	printf 'template initializer runtime_graph=%s database=%s authn=%s outbound_http=%s http_idempotency=%s jobs=%s webhooks=%s inbound_webhooks=%s candidate=%s revision=%s\n' \
		"${graph}" "${database}" "${authn}" "${outbound_http}" "${http_idempotency}" "${jobs}" "${webhooks}" "${inbound_webhooks}" "${candidate}" "${output_revision}"
	if ((graph > 26)); then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-metadata.log" "runtime-${graph}-metadata" \
			"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" cargo metadata --locked --offline --format-version 1 --manifest-path "${target}/Cargo.toml"
	fi
	if [[ ${full_graph} == true ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-build.log" "runtime-${graph}-build" \
			"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" make -C "${target}" build
		record_command "${receipt}" "${log_dir}/runtime-${graph}-test.log" "runtime-${graph}-test" \
			"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" make -C "${target}" test
		if ((${#db_tests[@]} > 0)); then
			record_command "${receipt}" "${log_dir}/runtime-${graph}-db.log" "runtime-${graph}-db" \
				"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" REQUIRE_DOCKER=1 bash "${target}/scripts/ci/test-integration-db.sh" "${db_tests[@]}"
		fi
		return
	fi
	record_command "${receipt}" "${log_dir}/runtime-${graph}-check.log" "runtime-${graph}-check" \
		"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" cargo check --workspace --all-targets \
			--features integration-tests/integration --locked --offline --manifest-path "${target}/Cargo.toml"
	record_command "${receipt}" "${log_dir}/runtime-${graph}-provider.log" "runtime-${graph}-provider" \
		"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" make -C "${target}" test-package PKG=infra-webhooks
	if [[ ${inbound_webhooks} == standard-webhooks ]]; then
		record_command "${receipt}" "${log_dir}/runtime-${graph}-contract.log" "runtime-${graph}-contract" \
			"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" cargo test -p "${identity}" --test openapi --locked --offline --manifest-path "${target}/Cargo.toml"
		record_command "${receipt}" "${log_dir}/runtime-${graph}-lifecycle.log" "runtime-${graph}-lifecycle" \
			"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" cargo test -p "${identity}" --test lifecycle \
				inert_inbound_webhook_route_rejects_unknown_endpoint_without_signature_work --locked --offline --manifest-path "${target}/Cargo.toml"
	fi
}

run_validation() {
	local work source candidate database authn outbound_http graph=0 common receipt_dir receipt log_dir target_cache
	local started=${SECONDS}
	local -a scrubbed_identity=(env -u SERVICE_NAME -u REPOSITORY -u DESCRIPTION -u CODEOWNER -u DATABASE -u AUTHN -u OUTBOUND_HTTP -u HTTP_IDEMPOTENCY -u JOBS -u WEBHOOKS -u INBOUND_WEBHOOKS -u AGENT_HARNESS)
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
	printf 'candidate=%s\nmode=%s\nstate=running\n' "${candidate}" "${mode}" >"${receipt}"
	if [[ ${mode} == runtime-graphs ]]; then printf 'requested_runtime_graphs=%s\n' "${runtime_graphs}" >>"${receipt}"; fi
	printf 'template initializer fixed candidate: %s\n' "${candidate}"
	printf 'template initializer receipt: %s\n' "${receipt}"

	if [[ ${mode} == full || ${mode} == source-checks ]]; then record_source_suites; fi
	if [[ ${mode} == full || ${mode} == projections-only ]]; then
		record_command "${receipt}" "${log_dir}/projection-self-test.log" "projection-self-test" \
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}" --self-test
		record_command "${receipt}" "${log_dir}/projections.log" "canonical-projections" \
			"${scrubbed_identity[@]}" python3 "${source}/scripts/tests/template-profile-projections.py" --source "${source}"
	fi
	if [[ ${mode} == full || ${mode} == runtime-graphs ]]; then
		for database in none postgres; do
			for authn in none oidc-jwt oidc-introspection; do
				for outbound_http in none bounded; do
					((graph += 1))
					runtime_graph_selected "${graph}" || continue
					run_graph "${graph}" "${database}" "${authn}" "${outbound_http}" none none none none
				done
			done
		done
		for authn in oidc-jwt oidc-introspection; do
			for outbound_http in none bounded; do
				((graph += 1))
				runtime_graph_selected "${graph}" || continue
				run_graph "${graph}" postgres "${authn}" "${outbound_http}" postgres none none none
			done
		done
		for authn in none oidc-jwt oidc-introspection; do
			for outbound_http in none bounded; do
				((graph += 1))
				runtime_graph_selected "${graph}" || continue
				run_graph "${graph}" postgres "${authn}" "${outbound_http}" none postgres none none
			done
		done
		for authn in oidc-jwt oidc-introspection; do
			for outbound_http in none bounded; do
				((graph += 1))
				runtime_graph_selected "${graph}" || continue
				run_graph "${graph}" postgres "${authn}" "${outbound_http}" postgres postgres none none
			done
		done
		for selection in none:none oidc-jwt:none oidc-introspection:none oidc-jwt:postgres oidc-introspection:postgres; do
			IFS=: read -r authn http_idempotency <<<"${selection}"
			for outbound_http in none bounded; do
				if [[ ${outbound_http} == none ]]; then
					((graph += 1))
					runtime_graph_selected "${graph}" || continue
					run_graph "${graph}" postgres "${authn}" none "${http_idempotency}" postgres none standard-webhooks
					continue
				fi
				((graph += 1))
				if runtime_graph_selected "${graph}"; then
					run_graph "${graph}" postgres "${authn}" bounded "${http_idempotency}" postgres none standard-webhooks
				fi
				((graph += 1))
				if runtime_graph_selected "${graph}"; then
					run_graph "${graph}" postgres "${authn}" bounded "${http_idempotency}" postgres durable none
				fi
				((graph += 1))
				if runtime_graph_selected "${graph}"; then
					run_graph "${graph}" postgres "${authn}" bounded "${http_idempotency}" postgres durable standard-webhooks
				fi
			done
		done
		[[ ${graph} == 46 ]] || { echo "webhook graph inventory ended at ${graph}, expected 46" >&2; return 1; }
	fi
	printf 'state=passed\nduration_seconds=%s\n' "$((SECONDS - started))" >>"${receipt}"
}

# The outer call owns the validation lock once. Representatives inherit it
# and run sequentially; focused receipts retain their narrower mode.
if [[ ${mode} == self-test ]]; then
	recorder_self_test
elif [[ ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	run_validation
else
	arguments=(--repo "${repo}")
	if [[ ${mode} == runtime-graphs ]]; then
		arguments+=(--runtime-graphs "${runtime_graphs}")
	elif [[ ${mode} != full ]]; then
		arguments+=("--${mode}")
	fi
	bash "${repo}/scripts/ci/validation-lock.sh" -- bash "$0" "${arguments[@]}"
fi
