#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

orig_args=("$@")
mode=run
provided_files=()
case "${1:-}" in
--plan)
	mode=plan
	shift
	;;
--self-test)
	mode=self-test
	shift
	;;
esac
if [[ ${1:-} == --files ]]; then
	shift
	provided_files=("$@")
fi

fingerprint_candidate() {
	local file fmode record hash head
	local -a hash_files=() records=()
	head=$(git rev-parse HEAD)
	while IFS= read -r file; do
		if [[ -L ${file} ]]; then
			hash=$(readlink "${file}" | git hash-object --stdin)
			records+=("mode 120000  ${file}"$'\n'"symlink ${hash}")
		elif [[ -f ${file} ]]; then
			if [[ -x ${file} ]]; then fmode=100755; else fmode=100644; fi
			records+=("mode ${fmode}  ${file}")
			hash_files+=("${file}")
		else
			records+=("deleted  ${file}")
		fi
	done < <(
		cat "${files_path}"
		printf '%s\n' tools/versions.env
		if [[ ${surface_duplication:-false} == true || ${surface_architecture:-false} == true ]]; then
			printf '%s\n' .jscpd.json quality/duplication-baseline.json quality/architecture.json \
				scripts/ci/duplication-check.py scripts/ci/architecture-check.py scripts/tests/quality-checks.py
		fi
	)
	: >"${tmp}/file-hashes"
	if ((${#hash_files[@]})); then
		shasum -a 256 -- "${hash_files[@]}" >"${tmp}/file-hashes" || return
	fi
	{
		printf 'head=%s\n' "${head}"
		printf 'base_ref=%s\nresolved_base_sha=%s\nmerge_base_sha=%s\n' "${base_ref}" "${resolved_base_sha}" "${merge_base_sha}"
		for record in "${records[@]}"; do
			printf '%s\n' "${record}"
			case "${record}" in
			'mode 100644 '* | 'mode 100755 '*)
				IFS= read -r hash || return 1
				printf '%s\n' "${hash}"
				;;
			esac
		done
	} <"${tmp}/file-hashes" | shasum -a 256 | awk '{print $1}'
}

prepare_command() {
	local kind=$1 argument=$2
	case "${kind}" in
	make) step_command=(make "${argument}") ;;
	lint) step_command=(make lint-changed "PKGS=${argument}") ;;
	test) step_command=(make test-changed "PKGS=${argument}") ;;
	shell) step_command=(make shellcheck "SHELL_FILES=${argument}") ;;
	image-build) step_command=(env "VCS_REF=${execution_head}" make runtime-image-build "RUNTIME_IMAGE=${argument}") ;;
	image-check) step_command=(make runtime-image-check "RUNTIME_IMAGE=${argument}" "RUNTIME_EXPECTED_COMMIT=${execution_head}") ;;
	artifacts) step_command=(make template-init-artifacts "ARTIFACT_GRAPHS=${argument}") ;;
	image-sbom) step_command=(make container-sbom "CONTAINER_IMAGE=${argument}") ;;
	image-security) step_command=(make container-security "CONTAINER_IMAGE=${argument}") ;;
	migration-validate) step_command=(make migration-validate "RUNTIME_IMAGE=${argument}" "RUNTIME_EXPECTED_COMMIT=${execution_head}") ;;
	make-vars)
		local -a words
		read -r -a words <<<"${argument}"
		step_command=(make "${words[@]}")
		;;
	*)
		echo "unknown verification command kind: ${kind}" >&2
		return 2
		;;
	esac
	if [[ ${ci_mode:-github-actions} == none ]]; then step_command=(env ALLOW_HEAVY=1 ALLOW_FULL=1 "${step_command[@]}"); fi
	printf -v step_display '%q ' "${step_command[@]}"
	step_display=${step_display% }
}

assert_failed() {
	echo "verify self-test assertion failed at line ${BASH_LINENO[0]}" >&2
	exit 1
}

self_test() (
	unset VALIDATION_LOCK_TOKEN VALIDATION_LOCK_DOMAIN VALIDATION_LOCK_HELD VALIDATION_LOCK_DIR
	local output fixture scratch script attempt_path receipts_before crate path
	plan_section() { sed -n "/^$1:\$/,/^[^ ]/p" <<<"${output}"; }
	fixture=$(mktemp -d)
	trap 'rm -rf -- "${fixture}"' EXIT
	unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_BUILD_DIR
	export CARGO=cargo CARGO_FLAGS=--locked CARGO_HOME=${fixture}/cargo-home
	export VALIDATION_LOCK_DIR=${fixture}/validation.lock
	mkdir -p "${CARGO_HOME}" "${fixture}/scripts/ci" "${fixture}/scripts/lib" "${fixture}/scripts/tests" "${fixture}/make" "${fixture}/tools"
	cp "${ROOT_DIR}/scripts/ci/"{verify,changed-surfaces,validation-lock,affected-crates,git-changed-paths}.sh "${fixture}/scripts/ci/"
	cp "${ROOT_DIR}/scripts/lib/"{template_state.py,template_init.py,template_profiles.json} "${fixture}/scripts/lib/"
	if [[ -f ${ROOT_DIR}/make/source.mk ]]; then
		cp "${ROOT_DIR}/scripts/ci/"{initializer-matrix.py,template-init-check.sh} "${fixture}/scripts/ci/"
	fi
	cp "${ROOT_DIR}/scripts/ci/validation-lock.py" "${fixture}/scripts/ci/"
	cp "${ROOT_DIR}/scripts/tests/validation-lock-test.py" "${fixture}/scripts/tests/"
	cp "${ROOT_DIR}/make/template.mk" "${fixture}/make/template.mk"
	cp "${ROOT_DIR}/tools/versions.env" "${fixture}/tools/versions.env"
	: >"${fixture}/scripts/ci/monitoring-rules.sh"
	: >"${fixture}/scripts/ci/native-transport-regressions.py"
	cd "${fixture}"
	printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >Cargo.toml
	for crate in leaf mid alone other; do
		mkdir -p "crates/${crate}/src"
		: >"crates/${crate}/src/lib.rs"
		printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "${crate}" >"crates/${crate}/Cargo.toml"
	done
	printf '\n[dependencies]\nleaf = { path = "../leaf" }\n' >>crates/mid/Cargo.toml
	cat >Cargo.lock <<'LOCK'
version = 4

[[package]]
name = "alone"
version = "0.1.0"

[[package]]
name = "leaf"
version = "0.1.0"

[[package]]
name = "mid"
version = "0.1.0"
dependencies = ["leaf"]

[[package]]
name = "other"
version = "0.1.0"
LOCK
	printf 'include make/template.mk\n' >Makefile
	printf '# Verification fixture\n' >README.md
	printf '# Agent fixture\n' >AGENTS.md
	mkdir -p .agents/roles
	: >.agents/roles/worker-agent.toml
	: >.gitleaks.toml
	: >scripts/ci/fixture.sh
	git init -q
	git add -A
	git -c user.name=verify-test -c user.email=verify-test@example.invalid -c commit.gpgsign=false commit -qm fixture
	script=${fixture}/scripts/ci/verify.sh

	(
		tmp=$(mktemp -d)
		trap 'rm -rf -- "${tmp}"' EXIT
		cd "${tmp}"
		git init -q
		git -c user.name=verify-test -c user.email=verify-test@example.invalid -c commit.gpgsign=false commit -qm fixture --allow-empty
		printf 'plain\n' >plain
		printf 'executable\n' >executable
		chmod +x executable
		ln -s plain link
		: >tools_placeholder
		mkdir -p tools
		: >tools/versions.env
		files_path=${tmp}/files
		printf '%s\n' plain executable link deleted >"${files_path}"
		base_ref=HEAD
		resolved_base_sha=$(git rev-parse HEAD)
		merge_base_sha=${resolved_base_sha}
		original=$(fingerprint_candidate)
		chmod +x plain
		[[ $(fingerprint_candidate) != "${original}" ]] || assert_failed
		chmod -x plain
		printf 'changed\n' >plain
		[[ $(fingerprint_candidate) != "${original}" ]] || assert_failed
		printf 'plain\n' >plain
		rm link
		ln -s executable link
		[[ $(fingerprint_candidate) != "${original}" ]] || assert_failed
		rm link
		ln -s plain link
		printf 'new\n' >deleted
		[[ $(fingerprint_candidate) != "${original}" ]] || assert_failed
		rm deleted
		[[ $(fingerprint_candidate) == "${original}" ]] || assert_failed
		mkdir quality
		printf '{"cases": []}\n' >quality/duplication-baseline.json
		surface_duplication=true
		original=$(fingerprint_candidate)
		printf '{"cases": ["changed"]}\n' >quality/duplication-baseline.json
		[[ $(fingerprint_candidate) != "${original}" ]] || assert_failed
	)
	bash "${ROOT_DIR}/scripts/ci/git-changed-paths.sh" --self-test

	if CI='' ALLOW_FULL='' make check >"${TMPDIR:-/tmp}/verify-full-guard.$$" 2>&1; then
		echo "verify self-test accepted make check without ALLOW_FULL=1" >&2
		return 1
	fi
	rm -f "${TMPDIR:-/tmp}/verify-full-guard.$$"

	output=$(bash "${script}" --plan --files .agents/roles/worker-agent.toml)
	grep -q '^  make check-instructions$' <<<"${output}"
	output=$(bash "${script}" --plan --files README.md)
	grep -q 'documentation=true' <<<"${output}"
	grep -q '^  make docs-check$' <<<"${output}"
	grep -q 'requires_docker=true' <<<"${output}"
	output=$(bash "${script}" --plan --files .github/dependabot.yml)
	grep -q '^  none$' <<<"${output}"

	if [[ -f ${ROOT_DIR}/make/source.mk ]]; then
		: >make/source.mk
		output=$(bash "${script}" --plan --files scripts/init-module.sh)
		grep -q '^  make template-init-check$' <<<"${output}"
		grep -q '^  make template-init-artifacts ARTIFACT_GRAPHS=1,7,47,65$' <<<"$(plan_section ci-owned)"
		output=$(ALLOW_FULL=1 bash "${script}" --plan --files scripts/ci/initializer-matrix.py)
		grep -q '^  make template-init-check$' <<<"$(plan_section commands)"
		grep -q '^  make template-init-artifacts ARTIFACT_GRAPHS=1,7,47,65$' <<<"$(plan_section ci-owned)"
		output=$(bash "${script}" --files scripts/tests/template-fixture.json)
		grep -q '^verification not applicable locally: CI owns make template-init-check' <<<"${output}"
		output=$(ALLOW_HEAVY=1 bash "${script}" --plan --files crates/infra-messaging/src/lib.rs)
		grep -q '^  make template-init-artifacts ARTIFACT_GRAPHS=47,65$' <<<"$(plan_section commands)"
		output=$(bash "${script}" --plan --files docs/decisions/outbound-http-decisions.md)
		grep -q '^  make template-init-projections$' <<<"$(plan_section commands)"
		rm make/source.mk
	fi

	cat >template.lock <<EOF
{"schema_version":1,"state":"complete","identity":{"service_name":"fixture-api","repository":"https://github.com/example/fixture-api","description":"Fixture API","codeowner":"@example/platform"},"profiles":{"database":"none","agent_harness":"core"},"source":{"repository":"https://github.com/Dankosik/rust-service-template-rest","checkout_revision":"$(git rev-parse HEAD)","provenance":"local-checkout"}}
EOF
	output=$(bash "${script}" --plan --files Cargo.toml)
	grep -q 'module_initializer=false' <<<"${output}"
	if grep -q 'template-init-check\|template-init-artifacts' <<<"${output}"; then return 1; fi
	rm template.lock

	for path in .jscpd.json quality/duplication-baseline.json scripts/ci/duplication-check.py; do
		output=$(bash "${script}" --plan --files "${path}")
		grep -q '^  make duplication-check$' <<<"${output}"
		grep -q '^  make quality-check-self-test$' <<<"${output}"
		if grep -q '^  make architecture-check$' <<<"${output}"; then return 1; fi
	done
	output=$(bash "${script}" --plan --files quality/architecture.json)
	grep -q '^  make architecture-check$' <<<"${output}"
	grep -q '^  make quality-check-self-test$' <<<"${output}"
	if grep -q '^  make duplication-check$' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files Cargo.lock)
	grep -q '^  make deny$' <<<"${output}"
	grep -q '^  make unused-deps$' <<<"${output}"
	grep -q '^  make build$' <<<"${output}"
	grep -q '^  make lint$' <<<"${output}"
	grep -q '^  make test$' <<<"${output}"

	output=$(bash "${script}" --plan --files crates/leaf/src/lib.rs)
	grep -q '^  make fmt-check$' <<<"${output}"
	grep -q "^  make lint-changed PKGS='leaf'$" <<<"${output}"
	grep -q "^  make test-changed PKGS='leaf mid'$" <<<"${output}"
	grep -q '^  make unused-deps$' <<<"${output}"
	if grep -q '^  make test$' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files crates/leaf/src/lib.rs clippy.toml)
	grep -q '^  make lint$' <<<"${output}"
	grep -q "^  make test-changed PKGS='leaf mid'$" <<<"${output}"
	if grep -q 'lint-changed' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files scripts/ci/fixture.sh)
	grep -q "^  make shellcheck SHELL_FILES='scripts/ci/fixture.sh'$" <<<"${output}"
	output=$(bash "${script}" --plan --files scripts/ci/removed.sh)
	grep -q 'shell: no changed shell source remains' <<<"${output}"

	output=$(bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^  make dockerfile-check$' <<<"$(plan_section commands)"
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"$(plan_section ci-owned)"
	output=$(ALLOW_HEAVY=1 bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"$(plan_section commands)"

	output=$(REPOSITORY_CI=none bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^ci_mode: none$' <<<"${output}"
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"$(plan_section commands)"
	grep -q "^  make secret-scan BASE_REF=$(git rev-parse HEAD)\$" <<<"$(plan_section commands)"
	if REPOSITORY_CI=gitlab bash "${script}" --plan --files README.md >/dev/null 2>&1; then
		echo "verify self-test accepted an unknown REPOSITORY_CI" >&2
		return 1
	fi

	if VERIFY_DOCKER_COMMAND=missing-docker-for-verify-test bash "${script}" --files scripts/ci/fixture.sh >/dev/null 2>"${TMPDIR:-/tmp}/verify-docker.$$"; then
		echo "verify self-test accepted a container-backed route without Docker" >&2
		return 1
	fi
	grep -q 'Docker is required' "${TMPDIR:-/tmp}/verify-docker.$$"
	rm -f "${TMPDIR:-/tmp}/verify-docker.$$"

	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'fixture skills check passed\n'
MAKE
	scratch=${fixture}/tmp
	mkdir "${scratch}"
	output=$(TMPDIR="${scratch}" VERIFY_FORCE=1 bash "${script}" --files .agents/roles/worker-agent.toml)
	grep -q 'fixture skills check passed' <<<"${output}"
	grep -q '^result: pass$' <<<"${output}"
	output=$(TMPDIR="${scratch}" bash "${script}" --files .agents/roles/worker-agent.toml)
	grep -q 'reusing exact passing receipt:' <<<"${output}"
	[[ -z $(find "${scratch}" -mindepth 1 -print -quit) ]] || {
		echo "verification leaked temporary files" >&2
		return 1
	}

	if [[ -f ${ROOT_DIR}/make/source.mk ]]; then
		: >make/source.mk
		output=$(VERIFY_FORCE=1 bash "${script}" --files .agents/roles/worker-agent.toml scripts/tests/template-fixture.json)
		grep -q '^status: partially_verified$' <<<"${output}"
		grep -q '^ci_owned: make template-init-check;make template-init-artifacts ARTIFACT_GRAPHS=1,7,47,65$' <<<"${output}"
		rm make/source.mk
	fi

	mkdir "${fixture}/plan-tools"
	for binary in npx go; do
		printf '#!/bin/sh\necho "unexpected tool invocation" >&2\nexit 97\n' >"${fixture}/plan-tools/${binary}"
		chmod +x "${fixture}/plan-tools/${binary}"
	done
	export PATH="${fixture}/plan-tools:${PATH}"

	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'skills ALLOW_HEAVY=%s ALLOW_FULL=%s CI=%s\n' "$(ALLOW_HEAVY)" "$(ALLOW_FULL)" "$(CI)"
secret-scan:
	@printf 'range scan since %s\n' "$(BASE_REF)"
MAKE
	output=$(REPOSITORY_CI=none VERIFY_FORCE=1 bash "${script}" --files .agents/roles/worker-agent.toml)
	grep -q '^skills ALLOW_HEAVY=1 ALLOW_FULL=1 CI=$' <<<"${output}"
	grep -q '^status: verified$' <<<"${output}"

	# Candidate mutation during a step invalidates the attempt and writes no receipt.
	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'changed during verification\n' >>.agents/roles/worker-agent.toml
MAKE
	receipts_before=$(find .git/codex/verify -name '*.receipt' 2>/dev/null | wc -l)
	if output=$(VERIFY_FORCE=1 bash "${script}" --files .agents/roles/worker-agent.toml 2>&1); then
		echo "verify self-test accepted a candidate changed by a gate" >&2
		return 1
	fi
	attempt_path=$(sed -n 's/^verification attempt: //p' <<<"${output}")
	grep -q '^step_state: 1 invalidated$' "${attempt_path}"
	grep -q '^attempt_state: invalidated$' "${attempt_path}"
	[[ $(find .git/codex/verify -name '*.receipt' 2>/dev/null | wc -l) == "${receipts_before}" ]] || assert_failed

	# Step failure stops execution and writes no receipt.
	cat >Makefile <<'MAKE'
tools-check:
	@:
monitoring-rules:
	@:
duplication-check architecture-check quality-check-self-test:
	@:
check-instructions:
	@exit 42
secret-scan:
	@touch should-not-run
MAKE
	if output=$(VERIFY_FORCE=1 bash "${script}" --files tools/versions.env .agents/roles/worker-agent.toml .gitleaks.toml 2>&1); then
		echo "verify self-test accepted a failing gate" >&2
		return 1
	fi
	[[ ! -e should-not-run ]] || assert_failed
	grep -q '^result: fail$' <<<"${output}"
	[[ $(find .git/codex/verify -name '*.receipt' 2>/dev/null | wc -l) == "${receipts_before}" ]] || assert_failed
)

if [[ ${mode} == self-test ]]; then
	CI='' ALLOW_FULL='' ALLOW_HEAVY='' REPOSITORY_CI='' VERIFY_RUNTIME_IMAGE='' self_test
	exit
fi

tmp=$(mktemp -d)
cleanup_tmp() {
	local status=$?
	trap - EXIT HUP INT TERM
	rm -rf -- "${tmp}"
	exit "${status}"
}
trap cleanup_tmp EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
files_path=${tmp}/files
base_ref=${BASE_REF:-origin/main}

if ((${#provided_files[@]})); then
	printf '%s\n' "${provided_files[@]}" >"${files_path}"
	if git rev-parse --verify "${base_ref}^{commit}" >/dev/null 2>&1; then
		resolved_base_sha=$(git rev-parse "${base_ref}^{commit}")
		merge_base_sha=$(git merge-base HEAD "${resolved_base_sha}")
	else
		resolved_base_sha=$(git rev-parse HEAD)
		merge_base_sha=${resolved_base_sha}
	fi
else
	git rev-parse --verify "${base_ref}^{commit}" >/dev/null 2>&1 || {
		echo "verification base is unavailable: ${base_ref}; set BASE_REF to a readable commit" >&2
		exit 2
	}
	resolved_base_sha=$(git rev-parse "${base_ref}^{commit}")
	merge_base_sha=$(git merge-base HEAD "${resolved_base_sha}")
	bash ./scripts/ci/git-changed-paths.sh --worktree "${base_ref}" >"${files_path}"
fi
LC_ALL=C sort -u -o "${files_path}" "${files_path}"

if [[ ! -s ${files_path} ]]; then
	echo "no changed files; verification is not applicable"
	exit
fi
if grep -n '[[:space:]]' "${files_path}" >/dev/null; then
	echo "verification does not support changed paths containing whitespace" >&2
	exit 2
fi

ci_mode=${REPOSITORY_CI:-}
if [[ -z ${ci_mode} && -f make/service.mk ]]; then
	ci_mode=$(sed -n 's/^REPOSITORY_CI[[:space:]]*?*=[[:space:]]*//p' make/service.mk | tail -n 1)
	read -r ci_mode _ <<<"${ci_mode}" || true
fi
ci_mode=${ci_mode:-github-actions}
case "${ci_mode}" in
github-actions | none) ;;
*)
	echo "unsupported REPOSITORY_CI: ${ci_mode}; expected github-actions or none" >&2
	exit 2
	;;
esac

surfaces_path=${tmp}/surfaces
bash ./scripts/ci/changed-surfaces.sh --union "${merge_base_sha}" <"${files_path}" >"${surfaces_path}"
while IFS='=' read -r name value; do
	printf -v "surface_${name}" '%s' "${value}"
done <"${surfaces_path}"

is_true() {
	local variable=surface_$1
	[[ ${!variable:-false} == true ]]
}

kinds=()
arguments=()
reasons=()
displays=()
cost_classes=()
heavy_requirements=()
docker_requirements=()
keys=''
not_applicable=()
ci_owned_displays=()
ci_owned_details=()

ci_owned() {
	local requires_heavy=$1 display=$2
	[[ ${CI:-} != true && ${ci_mode} != none ]] || return 1
	if [[ ${requires_heavy} == true ]]; then
		[[ ${ALLOW_HEAVY:-} != 1 ]]
		return
	fi
	[[ ${display} == 'make template-init-check' && ${ALLOW_FULL:-} != 1 ]]
}

add_command() {
	local kind=$1 argument=$2 reason=$3 display=$4 cost_class=$5 requires_heavy=$6 requires_docker=$7 key
	key="${kind}|${argument}"
	case $'\n'"${keys}" in *$'\n'"${key}"$'\n'*) return ;; esac
	keys="${keys}${key}"$'\n'
	if ci_owned "${requires_heavy}" "${display}"; then
		ci_owned_displays[${#ci_owned_displays[@]}]=${display}
		ci_owned_details[${#ci_owned_details[@]}]=$(printf '    because %s\n    cost_class=%s requires_heavy=%s requires_docker=%s' \
			"${reason}" "${cost_class}" "${requires_heavy}" "${requires_docker}")
		return
	fi
	kinds[${#kinds[@]}]=${kind}
	arguments[${#arguments[@]}]=${argument}
	reasons[${#reasons[@]}]=${reason}
	displays[${#displays[@]}]=${display}
	cost_classes[${#cost_classes[@]}]=${cost_class}
	heavy_requirements[${#heavy_requirements[@]}]=${requires_heavy}
	docker_requirements[${#docker_requirements[@]}]=${requires_docker}
}

add_na() {
	not_applicable[${#not_applicable[@]}]="$1: $2"
}

if is_true monitoring_rules && [[ -f scripts/ci/monitoring-rules.sh ]]; then
	add_command make monitoring-rules "monitoring rules, fixtures, or their pinned route changed" "make monitoring-rules" cheap false false
fi

if is_true tool_manifest; then add_command make tools-check "tool manifest changed" "make tools-check" cheap false false; fi
if is_true validation_system; then
	add_command make changed-surfaces-check "validation routing changed" "make changed-surfaces-check" cheap false false
	add_command make affected-crates-check "validation routing changed" "make affected-crates-check" cpu false false
	add_command make validation-lock-self-test "validation routing changed" "make validation-lock-self-test" cheap false false
	add_command make verify-check "validation routing changed" "make verify-check" cpu false false
fi
if is_true duplication; then
	add_command make duplication-check "Rust source, clone admission, or detector inputs changed" "make duplication-check" cpu false false
fi
if is_true architecture; then
	add_command make architecture-check "declared graph, boundary policy, or checker inputs changed" "make architecture-check" cheap false false
fi
if is_true initializer_runtime; then
	add_command make template-init-check "canonical projections and the initializer runtime graph inventory" "make template-init-check" cpu false true
elif is_true module_initializer; then
	add_command make template-init-projections "projected text changed and no runtime input did" "make template-init-projections" cpu false false
fi

# template:begin grpc:verify-grpc-schema
if is_true grpc_schema; then
	add_command make grpc-check "protobuf schema, generator, or committed output changed" "make grpc-check" cpu true false
	if [[ ${ci_mode} == none ]]; then
		add_command make grpc-generator-deny "no CI checks the standalone generator's dependency policy" "make grpc-generator-deny" cpu false false
	fi
fi
# template:end grpc:verify-grpc-schema

workspace_rust=false
if is_true cargo_dependencies; then workspace_rust=true; fi
if [[ ${ci_mode} == none ]] && is_true rust_source; then workspace_rust=true; fi
affected_lint=''
affected_tests=''
affected_fallback=false
affected_reason=''
if is_true rust_source && [[ ${workspace_rust} != true ]]; then
	affected_path=${tmp}/affected
	bash ./scripts/ci/affected-crates.sh <"${files_path}" >"${affected_path}"
	while IFS='=' read -r name value; do
		case "${name}" in
		lint_packages) affected_lint=${value} ;;
		test_packages) affected_tests=${value} ;;
		fallback) affected_fallback=${value} ;;
		fallback_reason) affected_reason=${value} ;;
		esac
	done <"${affected_path}"
	if [[ ${affected_fallback} == true ]]; then workspace_rust=true; fi
fi

if is_true rust_source || is_true cargo_dependencies; then
	add_command make unused-deps "dependency declarations or their users changed" "make unused-deps" cpu false false
fi
if is_true cargo_dependencies || is_true dependency_policy; then
	add_command make deny "dependency graph or policy changed" "make deny" cpu false false
	add_command make dependency-drift "dependency graph or policy changed" "make dependency-drift" cheap false false
fi
if is_true rust_source || is_true lint_config; then
	add_command make fmt-check "Rust source or formatting configuration changed" "make fmt-check" cheap false false
elif [[ ${ci_mode} == none ]] && is_true cargo_dependencies; then
	add_command make fmt-check "no CI formats the workspace after a dependency change" "make fmt-check" cheap false false
fi
if [[ ${workspace_rust} == true ]]; then
	reason="dependency, lockfile, or toolchain changes can affect every crate"
	if is_true cargo_dependencies; then :; elif [[ -n ${affected_reason} ]]; then reason="Rust changes need the workspace oracle (${affected_reason})"; elif [[ ${ci_mode} == none ]]; then reason="no CI runs the workspace route of a push"; fi
	add_command make lint "${reason}" "make lint" cpu false false
	add_command make build "${reason}" "make build" cpu false false
	add_command make test "${reason}" "make test" cpu false false
else
	if is_true lint_config; then
		add_command make lint "the complete lint configuration changed" "make lint" cpu false false
	elif [[ -n ${affected_lint} ]]; then
		add_command lint "${affected_lint}" "crate owners changed" "make lint-changed PKGS='${affected_lint}'" cpu false false
	fi
	if [[ -n ${affected_tests} ]]; then
		add_command test "${affected_tests}" "affected crates and their dependents changed" "make test-changed PKGS='${affected_tests}'" cpu false false
	fi
fi
if is_true duplication || is_true architecture; then
	add_command make quality-check-self-test "readability policy, source, graph, or checker integration changed" "make quality-check-self-test" cpu false false
	if [[ -f make/source.mk ]]; then
		add_command make template-quality-projections "readability checks must remain usable after profile projection and rename" "make template-quality-projections" cpu false false
	fi
fi
if is_true cargo_dependencies && [[ -f scripts/ci/native-transport-regressions.py ]]; then
	add_command make native-transport-regressions "patched native transport graph or source changed" "make native-transport-regressions" cpu true false
fi
if [[ ${ci_mode} == none ]] && { is_true rust_source || is_true cargo_dependencies; }; then
	add_command make profiling-check "no CI compiles the optional profiling configurations" "make profiling-check" cpu false false
fi
if is_true openapi; then add_command make openapi-check "OpenAPI document or its lint configuration changed" "make openapi-check" cpu false false; fi
if [[ ${ci_mode} == none ]] && is_true openapi; then
	add_command make-vars "openapi-breaking-since BASE_REF=${merge_base_sha}" "no CI compares the document with the merge base" "make openapi-breaking-since BASE_REF=${merge_base_sha}" cpu false false
fi
if is_true github_workflows; then
	add_command make actionlint "GitHub workflow or action source changed" "make actionlint" cheap false false
	add_command make zizmor "GitHub workflow or action source changed" "make zizmor" cheap false false
fi
if is_true dependency_automation; then
	if [[ ${ci_mode} == none ]]; then
		add_na dependency_automation "no CI validates the Dependabot schema or runs Dependency Review"
	else
		add_na dependency_automation "GitHub validates the Dependabot schema; Dependency Review stays a CI gate"
	fi
fi
if is_true shell; then
	shell_files=$(awk '/\.sh$/ { print }' "${files_path}" | while IFS= read -r file; do if [[ -f ${file} ]]; then printf '%s ' "${file}"; fi; done)
	shell_files=${shell_files% }
	if [[ -n ${shell_files} ]]; then add_command shell "${shell_files}" "shell sources changed" "make shellcheck SHELL_FILES='${shell_files}'" docker false true; else add_na shell "no changed shell source remains"; fi
fi
if is_true python; then add_command make python-lint "Python source or its lint configuration changed" "make python-lint" cheap false false; fi
if is_true agent_instructions; then add_command make check-instructions "agent instructions, skills, roles, or carriers changed" "make check-instructions" cheap false false; fi
if is_true publication_metadata; then add_command make publish-image-metadata-check "publication naming or promotion changed" "make publish-image-metadata-check" cheap false false; fi
if [[ ${ci_mode} == none ]]; then
	add_command make-vars "secret-scan BASE_REF=${merge_base_sha}" "no CI scans the commits since the merge base" "make secret-scan BASE_REF=${merge_base_sha}" cpu false false
elif is_true secret_scanning; then
	add_command make secret-scan "secret scanning policy changed" "make secret-scan" cpu false false
fi
if is_true migrations; then
	add_command make migration-check "migration set or its runner changed" "make migration-check" cpu false false
	add_command make migration-history-self-test "migration set or its runner changed" "make migration-history-self-test" cheap false false
fi
if is_true db_integration; then
	add_command make test-integration-db "database adapter, runner, or database proof changed" "make test-integration-db" docker true true
	if [[ ${ci_mode} == none ]]; then
		add_command make integration-compile "no CI compiles the integration suites the change does not run" "make integration-compile" cpu false false
	fi
	add_command make sqlx-check "a checked statement, a migration, or the query metadata changed" "make sqlx-check" docker true true
fi
if is_true messaging_integration; then
	add_command make test-integration-messaging "JetStream adapter, bridge, Compose, or messaging proof changed" "make test-integration-messaging" docker true true
	if [[ ${ci_mode} == none ]]; then
		add_command make messaging-go-compat "no CI compares the wire with the pinned Go service" "make messaging-go-compat" cpu true false
	fi
fi
if is_true cache_integration; then
	add_command make test-integration-cache "cache adapter, Compose, or cache proof changed" "make test-integration-cache" docker true true
fi
if is_true object_storage_integration; then
	add_command make test-integration-object-storage "object storage adapter, Compose, or emulator proof changed" "make test-integration-object-storage" docker true true
fi
if is_true oauth_integration; then
	add_command make test-integration-oauth "OAuth adapter, its transport, or Keycloak proof changed" "make test-integration-oauth" docker true true
fi
# template:begin runtime-progress:verify-runtime-progress
if is_true runtime_progress; then
	add_command make runtime-progress-proof "the finite production-runtime CPU quota envelope changed" "make runtime-progress-proof" docker true true
fi
# template:end runtime-progress:verify-runtime-progress
if is_true runtime_image || is_true migrations; then
	image=${VERIFY_RUNTIME_IMAGE:-service:verify}
	if is_true runtime_image; then
		add_command make dockerfile-check "runtime image inputs changed" "make dockerfile-check" docker false true
	fi
	add_command image-build "${image}" "one image is shared by the selected runtime gates" "make runtime-image-build RUNTIME_IMAGE=${image}" docker true true
	if is_true migrations; then
		add_command migration-validate "${image}" "migration set or its rehearsal changed" "make migration-validate RUNTIME_IMAGE=${image}" docker true true
	else
		add_command image-check "${image}" "runtime image lifecycle changed" "make runtime-image-check RUNTIME_IMAGE=${image}" docker true true
	fi
	if is_true runtime_image; then
		add_command image-security "${image}" "runtime image inputs changed" "make container-security CONTAINER_IMAGE=${image}" docker true true
		add_command image-sbom "${image}" "admitted runtime inventory conversion" "make container-sbom CONTAINER_IMAGE=${image}" docker true true
	fi
fi
if is_true initializer_artifacts; then
	artifact_plan=$(python3 scripts/ci/initializer-matrix.py --artifacts-only --union "${merge_base_sha}" <"${files_path}")
	artifact_graphs=${artifact_plan#artifact_graphs=}
	if [[ -n ${artifact_graphs} ]]; then
		add_command artifacts "${artifact_graphs}" "selected initialized release binary sets" "make template-init-artifacts ARTIFACT_GRAPHS=${artifact_graphs}" docker true true
	fi
fi
if is_true documentation; then add_command make docs-check "Markdown changed" "make docs-check" docker false true; fi
if [[ ${ci_mode} == none ]]; then
	if is_true rust_source || is_true cargo_dependencies || is_true github_workflows; then
		add_na codeql "no code scanning; GitHub code scanning of a private repository needs a paid plan"
	fi
	if is_true cargo_dependencies || is_true tool_manifest || is_true github_workflows || is_true runtime_image; then
		add_na dependency_review "make deny covers Rust advisories and licenses and container-security the image; workflow actions do not run"
	fi
	if is_true runtime_image || is_true publication_metadata; then
		add_na publication "cd.yml publication, signing, and attestation run only from GitHub Actions"
	fi
	add_na history_secret_scan "the scheduled full-history scan does not run; run ALLOW_HEAVY=1 make secret-scan-history before a release"
fi

print_plan() {
	if [[ ${ci_mode} == none ]]; then echo "ci_mode: none"; fi
	echo "files:"
	sed 's/^/  /' "${files_path}"
	echo "surfaces:"
	sed 's/^/  /' "${surfaces_path}"
	echo "commands:"
	if ((${#kinds[@]} == 0)); then echo "  none"; else
		for i in "${!kinds[@]}"; do
			printf '  %s\n    because %s\n    cost_class=%s requires_heavy=%s requires_docker=%s\n' \
				"${displays[$i]}" "${reasons[$i]}" "${cost_classes[$i]}" "${heavy_requirements[$i]}" "${docker_requirements[$i]}"
		done
	fi
	if ((${#ci_owned_displays[@]})); then
		echo "ci-owned:"
		for i in "${!ci_owned_displays[@]}"; do
			printf '  %s\n%s\n' "${ci_owned_displays[$i]}" "${ci_owned_details[$i]}"
		done
	fi
	if ((${#not_applicable[@]})); then
		echo "not applicable:"
		printf '  %s\n' "${not_applicable[@]}"
	fi
}

if [[ ${mode} == plan ]]; then
	print_plan
	exit
fi

ci_owned_summary=''
if ((${#ci_owned_displays[@]})); then ci_owned_summary=$(
	IFS='; '
	echo "${ci_owned_displays[*]}"
); fi

if ((${#kinds[@]} == 0)); then
	print_plan
	if [[ -n ${ci_owned_summary} ]]; then
		echo "verification not applicable locally: CI owns ${ci_owned_summary}"
	else
		echo "verification not applicable: no executable checks for changed surfaces"
	fi
	exit 0
fi

requires_docker=false
for i in "${!kinds[@]}"; do
	[[ ${docker_requirements[$i]} == true ]] && requires_docker=true
done

blocked() {
	printf 'claim: surface-aware verification\nresult: blocked\nstatus: blocked\ngap_or_next_owner: %s\n' "$1" >&2
	exit 2
}

for binary in git make shasum python3; do command -v "${binary}" >/dev/null 2>&1 || blocked "required binary is unavailable: ${binary}"; done
if is_true rust_source || is_true cargo_dependencies || is_true dependency_policy || is_true lint_config || is_true openapi || is_true validation_system || is_true module_initializer || is_true tool_manifest || is_true duplication || is_true architecture; then
	command -v cargo >/dev/null 2>&1 || blocked "required binary is unavailable: cargo"
fi
if is_true openapi || is_true migrations || is_true duplication || is_true architecture; then command -v npx >/dev/null 2>&1 || blocked "required binary is unavailable: npx"; fi
if is_true github_workflows || is_true secret_scanning || [[ ${ci_mode} == none ]]; then command -v go >/dev/null 2>&1 || blocked "required binary is unavailable: go"; fi
docker_command=${VERIFY_DOCKER_COMMAND:-docker}
if [[ ${requires_docker} == true ]]; then
	command -v "${docker_command}" >/dev/null 2>&1 || blocked "Docker is required"
	"${docker_command}" info >/dev/null 2>&1 || blocked "Docker is required and the daemon is unavailable"
fi

if ! bash ./scripts/ci/validation-lock.sh --assert-held; then
	rm -rf -- "${tmp}"
	trap - EXIT HUP INT TERM
	exec bash ./scripts/ci/validation-lock.sh -- bash "$0" ${orig_args[@]+"${orig_args[@]}"}
fi

candidate=$(fingerprint_candidate)
execution_head=$(git rev-parse HEAD)
command_summary=$(
	IFS='; '
	echo "${displays[*]}"
)
plan_input=${tmp}/plan
for i in "${!kinds[@]}"; do printf '%s|%s|%s|%s\n' "${displays[$i]}" "${cost_classes[$i]}" "${heavy_requirements[$i]}" "${docker_requirements[$i]}"; done >"${plan_input}"
plan=$(shasum -a 256 "${plan_input}" | awk '{print $1}')
rust_environment=$(rustc --version 2>/dev/null || echo unavailable)
cargo_environment=$(cargo --version 2>/dev/null || echo unavailable)
tool_manifest_hash=$(shasum -a 256 tools/versions.env 2>/dev/null | awk '{print substr($1, 1, 12)}' || echo unavailable)
docker_environment=not-used
if [[ ${requires_docker} == true ]]; then docker_environment=$("${docker_command}" version --format '{{.Client.Version}}/{{.Server.Version}}' 2>/dev/null || echo unavailable); fi
environment_detail="$(uname -srm); ${rust_environment}; ${cargo_environment}; docker=${docker_environment}; ALLOW_FULL=${ALLOW_FULL:-}; ALLOW_HEAVY=${ALLOW_HEAVY:-}; tools=${tool_manifest_hash}"
if [[ ${ci_mode} == none ]]; then environment_detail+="; REPOSITORY_CI=none"; fi
environment=$(printf '%s\n' "${environment_detail}" | shasum -a 256 | awk '{print $1}')
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
receipt_dir=${common_dir}/codex/verify
receipt=${receipt_dir}/${candidate}-${plan}-${environment}.receipt

print_plan

if [[ -f ${receipt} && ${VERIFY_FORCE:-} != 1 ]]; then
	echo "reusing exact passing receipt: ${receipt}"
	cat "${receipt}"
	exit 0
fi

mkdir -p "${receipt_dir}"
attempt=$(mktemp "${receipt_dir}/attempt-${candidate:0:12}.XXXXXX")
{
	printf 'candidate: %s\nbase_ref: %s\nresolved_base_sha: %s\nmerge_base_sha: %s\n' \
		"${candidate}" "${base_ref}" "${resolved_base_sha}" "${merge_base_sha}"
	printf 'environment: %s\nattempt_state: running\n' "${environment_detail}"
	print_plan
	for i in "${!kinds[@]}"; do
		prepare_command "${kinds[$i]}" "${arguments[$i]}"
		printf 'step: %s\ncommand: %s\nstep_state: %s pending\n' "$((i + 1))" "${step_display}" "$((i + 1))"
	done
} >"${attempt}"
echo "verification attempt: ${attempt}"

started=$(date +%s)
for i in "${!kinds[@]}"; do
	command_started=$(date +%s)
	printf 'step_state: %s running started_at=%s\n' "$((i + 1))" "${command_started}" >>"${attempt}"
	echo "==> ${displays[$i]}"
	prepare_command "${kinds[$i]}" "${arguments[$i]}"
	if "${step_command[@]}"; then
		candidate_after=$(fingerprint_candidate)
		if [[ ${candidate_after} != "${candidate}" ]]; then
			printf 'step_state: %s invalidated\nattempt_state: invalidated\n' "$((i + 1))" >>"${attempt}"
			echo "verification candidate changed during ${displays[$i]}; see ${attempt}" >&2
			exit 1
		fi
		printf 'step_state: %s passed duration=%ss\n' "$((i + 1))" "$(($(date +%s) - command_started))" >>"${attempt}"
	else
		command_exit=$?
		duration=$(($(date +%s) - started))
		printf 'step_state: %s failed exit_code=%s duration=%ss\nattempt_state: failed\n' \
			"$((i + 1))" "${command_exit}" "$(($(date +%s) - command_started))" >>"${attempt}"
		printf 'claim: surface-aware verification\nresult: fail\ncandidate: %s\nscope: %s\ncommand: %s\ninputs: %s\nenvironment: %s\nduration: %ss\nstatus: not_verified\ngap_or_next_owner: failed command\n' \
			"${candidate}" "$(tr '\n' ',' <"${files_path}")" "${displays[$i]}" "$(tr '\n' ',' <"${files_path}")" "${environment_detail}" "${duration}" >&2
		echo "partial results and pending plan: ${attempt}" >&2
		exit 1
	fi
	printf '<== %s (%ss)\n' "${displays[$i]}" "$(($(date +%s) - command_started))"
done
duration=$(($(date +%s) - started))
candidate_after=$(fingerprint_candidate)
if [[ ${candidate_after} != "${candidate}" ]]; then
	printf 'attempt_state: invalidated\n' >>"${attempt}"
	printf 'claim: surface-aware verification\nresult: invalidated\ncandidate: %s\nstatus: not_verified\ngap_or_next_owner: candidate changed during verification\n' "${candidate}" >&2
	exit 1
fi

receipt_tmp=${attempt}.receipt.tmp
{
	printf 'claim: surface-aware verification\n'
	printf 'result: pass\n'
	printf 'candidate: %s\n' "${candidate}"
	printf 'base_ref: %s\nresolved_base_sha: %s\nmerge_base_sha: %s\n' "${base_ref}" "${resolved_base_sha}" "${merge_base_sha}"
	printf 'scope: %s\n' "$(tr '\n' ',' <"${files_path}")"
	printf 'command: make verify [%s]\n' "${command_summary}"
	printf 'inputs: %s\n' "$(tr '\n' ',' <"${files_path}")"
	printf 'environment: %s\n' "${environment_detail}"
	printf 'duration: %ss\n' "${duration}"
	if [[ ${ci_mode} == none ]]; then printf 'ci_mode: none\n'; fi
	if [[ -n ${ci_owned_summary} ]]; then printf 'status: partially_verified\n'; else printf 'status: verified\n'; fi
	if ((${#not_applicable[@]})); then printf 'not_applicable: %s\n' "$(
		IFS='; '
		echo "${not_applicable[*]}"
	)"; fi
	if [[ -n ${ci_owned_summary} ]]; then
		printf 'ci_owned: %s\ngap_or_next_owner: CI\n' "${ci_owned_summary}"
	else
		printf 'gap_or_next_owner: none\n'
	fi
} >"${receipt_tmp}"
mv "${receipt_tmp}" "${receipt}"
printf 'attempt_state: passed\nreceipt: %s\n' "${receipt}" >>"${attempt}"
cat "${receipt}"
