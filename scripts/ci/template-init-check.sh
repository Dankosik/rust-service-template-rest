#!/usr/bin/env bash
# Build one private, fixed source candidate, then prove every supported
# initializer output. The shared checkout is never staged or committed.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
repo=${ROOT_DIR}
self_test=false
source_checks=false

while (($#)); do
	case "$1" in
	--self-test)
		[[ ${self_test} == false && ${source_checks} == false ]] || { echo "--self-test cannot combine with another mode" >&2; exit 2; }
		self_test=true
		shift
		;;
	--source-checks)
		[[ ${source_checks} == false && ${self_test} == false ]] || { echo "--source-checks cannot combine with another mode" >&2; exit 2; }
		source_checks=true
		shift
		;;
	--repo)
		[[ -n ${2:-} ]] || { echo "--repo requires a path" >&2; exit 2; }
		repo=$(cd "${2}" && pwd)
		shift 2
		;;
	*)
		echo "usage: $0 [--repo ROOT] [--source-checks|--self-test]" >&2
		exit 2
		;;
	esac
done

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
			[[ ${relative} == evals/template-initializer/ || ${relative} == specs/template-initializer/ ]] || {
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
	local receipt=$1 log=$2 label=$3
	shift 3
	printf 'command=%q ' "$@" >>"${receipt}"
	printf '\nstatus=running label=%s\n' "${label}" >>"${receipt}"
	if "$@" >"${log}" 2>&1; then
		printf 'status=passed label=%s log_sha256=%s output=%s\n' "${label}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" >>"${receipt}"
		cat "${log}"
		return 0
	else
		local status=$?
		printf 'status=failed label=%s exit_code=%s log_sha256=%s output=%s\n' "${label}" "${status}" \
			"$(shasum -a 256 "${log}" | awk '{print $1}')" "${log}" >>"${receipt}"
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

run_source_checks() {
	local work source candidate common receipt_dir receipt log_dir
	local -a scrubbed_identity=(env -u SERVICE_NAME -u REPOSITORY -u DESCRIPTION -u CODEOWNER -u DATABASE -u AGENT_HARNESS)
	work=$(mktemp -d)
	trap 'rm -rf -- "${work}"' RETURN
	common=$(git -C "${repo}" rev-parse --git-common-dir)
	[[ ${common} == /* ]] || common=${repo}/${common}
	receipt_dir=${common}/codex/template-init
	mkdir -p "${receipt_dir}"
	receipt=$(mktemp "${receipt_dir}/attempt.XXXXXX")
	log_dir=$(mktemp -d "${receipt_dir}/attempt-logs.XXXXXX")
	source=${work}/source
	candidate=$(snapshot_candidate "${source}")
	printf 'candidate=%s\nmode=source-checks\nstate=running\n' "${candidate}" >"${receipt}"
	printf 'template initializer fixed candidate: %s\n' "${candidate}"
	printf 'template initializer receipt: %s\n' "${receipt}"
	record_source_suites
	printf 'state=passed\n' >>"${receipt}"
}

run_matrix() {
	local work source candidate database harness target cell=0 common receipt_dir receipt log_dir target_cache tools_root output_revision
	local -a scrubbed_identity=(env -u SERVICE_NAME -u REPOSITORY -u DESCRIPTION -u CODEOWNER -u DATABASE -u AGENT_HARNESS)
	work=$(mktemp -d)
	trap 'rm -rf -- "${work}"' RETURN
	common=$(git -C "${repo}" rev-parse --git-common-dir)
	[[ ${common} == /* ]] || common=${repo}/${common}
	receipt_dir=${common}/codex/template-init
	mkdir -p "${receipt_dir}"
	receipt=$(mktemp "${receipt_dir}/attempt.XXXXXX")
	log_dir=$(mktemp -d "${receipt_dir}/attempt-logs.XXXXXX")
	# Explicit caller caches survive this attempt; only private work is removed.
	target_cache=${CARGO_TARGET_DIR:-${work}/cargo-target}
	tools_root=${TOOLS_ROOT:-${work}/tools}
	source=${work}/source
	candidate=$(snapshot_candidate "${source}")
	printf 'candidate=%s\nstate=running\n' "${candidate}" >"${receipt}"
	printf 'template initializer fixed candidate: %s\n' "${candidate}"
	printf 'template initializer receipt: %s\n' "${receipt}"

	record_source_suites

	for database in none postgres; do
		for harness in core codex claude qwen cursor grok opencode all; do
			((cell += 1))
			target=${work}/cell-${cell}-${database}-${harness}
			git clone --quiet --no-local "${source}" "${target}"
			printf 'cell=%s database=%s harness=%s candidate=%s\n' "${cell}" "${database}" "${harness}" "${candidate}" >>"${receipt}"
			record_command "${receipt}" "${log_dir}/cell-${cell}-init.log" "cell-${cell}-init" \
				"${scrubbed_identity[@]}" bash "${source}/scripts/init-module.sh" --repo "${target}" \
				--service-name "matrix-${database}-${harness}" \
				--repository "https://github.com/example/matrix-${database}-${harness}" \
				--description "Matrix ${database} ${harness}" \
				--codeowner @example/platform --database "${database}" --agent-harness "${harness}"
			git -C "${target}" config user.email template-init-check@example.invalid
			git -C "${target}" config user.name template-init-check
			git -C "${target}" add -A
			git -C "${target}" commit -qm "initialized ${database}/${harness}"
			output_revision=$(git -C "${target}" rev-parse HEAD)
			printf 'status=passed label=cell-%s-initialized output_revision=%s\n' "${cell}" "${output_revision}" >>"${receipt}"
			printf 'template initializer cell=%s database=%s harness=%s candidate=%s revision=%s\n' \
				"${cell}" "${database}" "${harness}" "${candidate}" "${output_revision}"
			record_command "${receipt}" "${log_dir}/cell-${cell}-build.log" "cell-${cell}-build" \
				"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" TOOLS_ROOT="${tools_root}" make -C "${target}" build
			record_command "${receipt}" "${log_dir}/cell-${cell}-check.log" "cell-${cell}-check" \
				"${scrubbed_identity[@]}" CARGO_TARGET_DIR="${target_cache}" TOOLS_ROOT="${tools_root}" ALLOW_FULL=1 ALLOW_HEAVY=1 make -C "${target}" check
		done
	done
	printf 'state=passed\n' >>"${receipt}"
}

# The outer call owns the validation lock once. Cells inherit it and execute
# serially, so no Cargo-heavy or container-backed proofs overlap.
if [[ ${self_test} == true ]]; then
	recorder_self_test
elif [[ ${source_checks} == true && ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	run_source_checks
elif [[ ${source_checks} == true ]]; then
	bash "${repo}/scripts/ci/validation-lock.sh" -- bash "$0" --repo "${repo}" --source-checks
elif [[ ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	run_matrix
else
	bash "${repo}/scripts/ci/validation-lock.sh" -- bash "$0" --repo "${repo}"
fi
