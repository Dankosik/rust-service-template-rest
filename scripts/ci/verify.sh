#!/usr/bin/env bash
# Surface-aware verification: classify the changed paths, plan the smallest
# set of make targets that proves them, run the plan under the validation
# lock, and record the outcome.
#
#   verify.sh                 plan and run for the worktree changes since BASE_REF
#   verify.sh --plan          print the plan only
#   verify.sh --files P...    plan for an explicit path list
#   verify.sh --self-test
#
# A passing run writes a receipt under <git-common-dir>/codex/verify named by
# the candidate fingerprint (changed files' content and modes plus HEAD), the
# plan, and the environment; an identical rerun reuses it unless
# VERIFY_FORCE=1. Every run writes an attempt record with per-step state, so a
# failed or interrupted run leaves evidence without granting acceptance. A
# step that changes the candidate invalidates the attempt.
#
# Heavy steps and the source initializer matrix are CI-owned: a local run
# names them and records a partial result instead of running them.
# ALLOW_HEAVY=1 and ALLOW_FULL=1 keep them local.
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

mode=run
locked=false
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
--locked)
	locked=true
	shift
	;;
esac
if [[ ${1:-} == --files ]]; then
	shift
	provided_files=("$@")
fi

fingerprint_candidate() {
	local file mode record hash head
	local -a hash_files=() records=()
	head=$(git rev-parse HEAD)
	while IFS= read -r file; do
		if [[ -L ${file} ]]; then
			hash=$(readlink "${file}" | git hash-object --stdin)
			records+=("mode 120000  ${file}"$'\n'"symlink ${hash}")
		elif [[ -f ${file} ]]; then
			if [[ -x ${file} ]]; then mode=100755; else mode=100644; fi
			records+=("mode ${mode}  ${file}")
			hash_files+=("${file}")
		else
			records+=("deleted  ${file}")
		fi
	done <"${files_path}"
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

# One argument vector owns both execution and the retained continuation command.
# Image kinds bake and assert the commit the run executed against, so the
# retained command proves the same identity as the run.
prepare_command() {
	local kind=$1 argument=$2
	case "${kind}" in
	make) step_command=(make "${argument}") ;;
	lint) step_command=(make lint-changed "PKGS=${argument}") ;;
	test) step_command=(make test-changed "PKGS=${argument}") ;;
	shell) step_command=(make shellcheck "SHELL_FILES=${argument}") ;;
	image-build) step_command=(env "VCS_REF=${execution_head}" make runtime-image-build "RUNTIME_IMAGE=${argument}") ;;
	image-check) step_command=(make runtime-image-check "RUNTIME_IMAGE=${argument}" "RUNTIME_EXPECTED_COMMIT=${execution_head}") ;;
	image-security) step_command=(make container-security "CONTAINER_IMAGE=${argument}") ;;
	migration-validate) step_command=(make migration-validate "RUNTIME_IMAGE=${argument}" "RUNTIME_EXPECTED_COMMIT=${execution_head}") ;;
	*)
		echo "unknown verification command kind: ${kind}" >&2
		return 2
		;;
	esac
	printf -v step_display '%q ' "${step_command[@]}"
	step_display=${step_display% }
}

self_test() (
	local output fixture scratch script attempt_path receipts_before crate
	# plan_section NAME: one section of the plan in ${output}, header included.
	plan_section() { sed -n "/^$1:\$/,/^[^ ]/p" <<<"${output}"; }
	fixture=$(mktemp -d)
	trap 'rm -rf -- "${fixture}"' EXIT
	mkdir -p "${fixture}/scripts/ci" "${fixture}/scripts/lib" "${fixture}/make" "${fixture}/tools"
	cp "${ROOT_DIR}/scripts/ci/"{verify,changed-surfaces,validation-lock,affected-crates,git-changed-paths}.sh "${fixture}/scripts/ci/"
	cp "${ROOT_DIR}/scripts/lib/template_state.py" "${fixture}/scripts/lib/template_state.py"
	cp "${ROOT_DIR}/make/template.mk" "${fixture}/make/template.mk"
	cp "${ROOT_DIR}/tools/versions.env" "${fixture}/tools/versions.env"
	cd "${fixture}"
	# leaf <- mid; alone and other keep leaf's closure under the workspace fallback.
	printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >Cargo.toml
	for crate in leaf mid alone other; do
		mkdir -p "crates/${crate}/src"
		: >"crates/${crate}/src/lib.rs"
		printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "${crate}" >"crates/${crate}/Cargo.toml"
	done
	printf '\n[dependencies]\nleaf = { path = "../leaf" }\n' >>crates/mid/Cargo.toml
	cargo generate-lockfile --offline --quiet
	printf 'include make/template.mk\n' >Makefile
	printf '# Verification fixture\n' >README.md
	printf '# Agent fixture\n' >AGENTS.md
	: >scripts/check-skills.py
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
		files_path=${tmp}/files
		printf '%s\n' plain executable link deleted >"${files_path}"
		base_ref=HEAD
		resolved_base_sha=$(git rev-parse HEAD)
		merge_base_sha=${resolved_base_sha}
		original=$(fingerprint_candidate)
		chmod +x plain
		[[ $(fingerprint_candidate) != "${original}" ]]
		chmod -x plain
		printf 'changed\n' >plain
		[[ $(fingerprint_candidate) != "${original}" ]]
		printf 'plain\n' >plain
		rm link
		ln -s executable link
		[[ $(fingerprint_candidate) != "${original}" ]]
		rm link
		ln -s plain link
		printf 'new\n' >deleted
		[[ $(fingerprint_candidate) != "${original}" ]]
		rm deleted
		[[ $(fingerprint_candidate) == "${original}" ]]
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
	if grep -q 'not applicable' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --plan --files .github/dependabot.yml)
	grep -q '^  none$' <<<"${output}"

	# Source-only admission exists only when make/source.mk is present. The
	# fixture deliberately models that source root, then returns to a derived
	# root with an explicit complete none/core lock.
	: >make/source.mk
	output=$(bash "${script}" --plan --files scripts/init-module.sh)
	grep -q '^  make template-init-check$' <<<"${output}"
	if grep -q 'requires_heavy=true' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --plan --files scripts/tests/template-profile-projections.py)
	grep -q '^  make template-init-check$' <<<"${output}"
	grep -q 'cost_class=cpu requires_heavy=false requires_docker=false' <<<"${output}"
	output=$(bash "${script}" --plan --files crates/infra-bearerauthn/src/claims.rs)
	grep -q '^  make template-init-check$' <<<"${output}"
	# The initializer matrix is CI-owned unless ALLOW_FULL=1 keeps it local; a
	# route with nothing else to prove runs nothing and names CI.
	grep -q '^  make template-init-check$' <<<"$(plan_section ci-owned)"
	output=$(ALLOW_FULL=1 bash "${script}" --plan --files scripts/tests/template-profile-projections.py)
	grep -q '^  make template-init-check$' <<<"$(plan_section commands)"
	if grep -q '^ci-owned:$' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --files scripts/tests/template-profile-projections.py)
	grep -q '^verification not applicable locally: CI owns make template-init-check$' <<<"${output}"
	rm make/source.mk
	cat >template.lock <<EOF
{"schema_version":1,"state":"complete","identity":{"service_name":"fixture-api","repository":"https://github.com/example/fixture-api","description":"Fixture API","codeowner":"@example/platform"},"profiles":{"database":"none","agent_harness":"core"},"source":{"repository":"https://github.com/Dankosik/rust-service-template-rest","checkout_revision":"$(git rev-parse HEAD)","provenance":"local-checkout"}}
EOF
	output=$(bash "${script}" --plan --files Cargo.toml)
	grep -q 'module_initializer=false' <<<"${output}"
	if grep -q 'template-init-check' <<<"${output}"; then return 1; fi
	rm template.lock

	output=$(bash "${script}" --plan --files tools/versions.env)
	grep -q 'make tools-check' <<<"${output}"
	if grep -q 'make test' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files Makefile)
	grep -q 'make verify-check' <<<"${output}"
	grep -q 'make changed-surfaces-check' <<<"${output}"
	grep -q 'make affected-crates-check' <<<"${output}"
	grep -q 'make validation-lock-self-test' <<<"${output}"
	if grep -q 'make test' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files Cargo.lock)
	grep -q '^  make deny$' <<<"${output}"
	grep -q '^  make unused-deps$' <<<"${output}"
	grep -q '^  make build$' <<<"${output}"
	grep -q '^  make lint$' <<<"${output}"
	grep -q '^  make test$' <<<"${output}"
	if grep -q 'lint-changed' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files crates/leaf/src/lib.rs)
	grep -q '^  make fmt-check$' <<<"${output}"
	grep -q "^  make lint-changed PKGS='leaf'$" <<<"${output}"
	grep -q "^  make test-changed PKGS='leaf mid'$" <<<"${output}"
	grep -q '^  make unused-deps$' <<<"${output}"
	if grep -q '^  make test$' <<<"${output}"; then return 1; fi
	if grep -q '^  make lint$' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files crates/leaf/tests/it.rs)
	grep -q "^  make test-changed PKGS='leaf'$" <<<"${output}"
	if grep -q 'mid' <<<"${output}"; then return 1; fi

	# The complete lint configuration changed: the workspace lint replaces the
	# affected one, tests stay affected.
	output=$(bash "${script}" --plan --files crates/leaf/src/lib.rs clippy.toml)
	grep -q '^  make lint$' <<<"${output}"
	grep -q "^  make test-changed PKGS='leaf mid'$" <<<"${output}"
	if grep -q 'lint-changed' <<<"${output}"; then return 1; fi

	# A Rust file without a crate owner runs the workspace.
	output=$(bash "${script}" --plan --files env/config/local.toml)
	grep -q '^  make test$' <<<"${output}"
	grep -q 'outside_crates' <<<"${output}"

	output=$(bash "${script}" --plan --files deny.toml)
	grep -q '^  make deny$' <<<"${output}"
	[[ $(grep -c '^  make ' <<<"${output}") -eq 1 ]]

	output=$(bash "${script}" --plan --files .github/workflows/ci.yml)
	grep -q '^  make actionlint$' <<<"${output}"
	grep -q '^  make zizmor$' <<<"${output}"

	output=$(bash "${script}" --plan --files scripts/ci/fixture.sh)
	grep -q "^  make shellcheck SHELL_FILES='scripts/ci/fixture.sh'$" <<<"${output}"
	grep -q 'requires_docker=true' <<<"${output}"
	# A deleted script selects the surface but leaves nothing to lint.
	output=$(bash "${script}" --plan --files scripts/ci/removed.sh)
	grep -q 'shell: no changed shell source remains' <<<"${output}"
	if grep -q 'make shellcheck' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files api/openapi/service.yaml)
	grep -q '^  make openapi-check$' <<<"${output}"

	output=$(bash "${script}" --plan --files .gitleaks.toml)
	grep -q '^  make secret-scan$' <<<"${output}"

	output=$(bash "${script}" --plan --files scripts/ci/publish-image-metadata.sh .github/actions/publish-image/action.yml)
	grep -q '^  make publish-image-metadata-check$' <<<"${output}"
	grep -q '^  make actionlint$' <<<"${output}"
	if grep -q 'runtime-image-build' <<<"${output}"; then return 1; fi

	output=$(bash "${script}" --plan --files .github/dependabot.yml)
	grep -q 'dependency_automation: GitHub validates' <<<"${output}"

	# The Dockerfile selects one image shared by the lifecycle and scan gates,
	# and the tool-pin check because it carries ARG defaults and FROM digests.
	output=$(bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^  make tools-check$' <<<"${output}"
	grep -q '^  make dockerfile-check$' <<<"${output}"
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"${output}"
	grep -q '^  make runtime-image-check RUNTIME_IMAGE=service:verify$' <<<"${output}"
	grep -q '^  make container-security CONTAINER_IMAGE=service:verify$' <<<"${output}"
	grep -q 'requires_heavy=true' <<<"${output}"
	if grep -q '^  make test$' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --plan --files .dockerignore)
	grep -q '^  make runtime-image-build' <<<"${output}"
	if grep -q 'tools-check' <<<"${output}"; then return 1; fi

	# A migration selects the static history check, the database proof for
	# the runner, and the image rehearsal in place of the plain lifecycle
	# check; the scan stays with the image inputs.
	output=$(bash "${script}" --plan --files migrations/20260918120000_create_widgets.sql)
	grep -q '^  make migration-check$' <<<"${output}"
	grep -q '^  make migration-history-self-test$' <<<"${output}"
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"${output}"
	grep -q '^  make migration-validate RUNTIME_IMAGE=service:verify$' <<<"${output}"
	grep -q '^  make dockerfile-check$' <<<"${output}"
	grep -q '^  make container-security CONTAINER_IMAGE=service:verify$' <<<"${output}"
	if grep -q 'runtime-image-check' <<<"${output}"; then return 1; fi
	if grep -q 'test-integration-db' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --plan --files crates/infra-postgres/src/dsn.rs)
	grep -q '^  make test-integration-db$' <<<"${output}"
	grep -q 'requires_heavy=true' <<<"${output}"
	if grep -q 'migration-validate' <<<"${output}"; then return 1; fi
	output=$(bash "${script}" --plan --files crates/migrate/src/lib.rs)
	grep -q '^  make test-integration-db$' <<<"${output}"
	grep -q '^  make migration-validate RUNTIME_IMAGE=service:verify$' <<<"${output}"

	# Heavy steps are CI-owned unless ALLOW_HEAVY=1 keeps them local; the
	# light steps of the same route stay local.
	output=$(bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^  make dockerfile-check$' <<<"$(plan_section commands)"
	if grep -q 'requires_heavy=true' <<<"$(plan_section commands)"; then return 1; fi
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"$(plan_section ci-owned)"
	grep -q '^  make container-security CONTAINER_IMAGE=service:verify$' <<<"$(plan_section ci-owned)"
	output=$(ALLOW_HEAVY=1 bash "${script}" --plan --files build/docker/Dockerfile)
	grep -q '^  make runtime-image-build RUNTIME_IMAGE=service:verify$' <<<"$(plan_section commands)"
	if grep -q '^ci-owned:$' <<<"${output}"; then return 1; fi

	if VERIFY_DOCKER_COMMAND=missing-docker-for-verify-test bash "${script}" --files scripts/ci/fixture.sh >/dev/null 2>"${TMPDIR:-/tmp}/verify-docker.$$"; then
		echo "verify self-test accepted a container-backed route without Docker" >&2
		return 1
	fi
	grep -q 'Docker is required' "${TMPDIR:-/tmp}/verify-docker.$$"
	rm -f "${TMPDIR:-/tmp}/verify-docker.$$"

	# Execution against stub targets: pass, fail, temporary-file hygiene.
	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'fixture skills check passed\n'
MAKE
	scratch=${fixture}/tmp
	mkdir "${scratch}"
	output=$(TMPDIR="${scratch}" VERIFY_FORCE=1 bash "${script}" --files scripts/check-skills.py)
	grep -q 'fixture skills check passed' <<<"${output}"
	grep -q '^result: pass$' <<<"${output}"
	if grep -q 'reusing exact passing receipt' <<<"${output}"; then return 1; fi
	output=$(TMPDIR="${scratch}" bash "${script}" --files scripts/check-skills.py)
	grep -q 'reusing exact passing receipt' <<<"${output}"
	printf 'check-instructions:\n\t@exit 42\n' >Makefile
	if output=$(TMPDIR="${scratch}" VERIFY_FORCE=1 bash "${script}" --files scripts/check-skills.py 2>&1); then
		echo "verify self-test accepted a failing gate" >&2
		return 1
	fi
	grep -q '^result: fail$' <<<"${output}"
	# Both the unlocked planner and its locked child own temporary files.
	[[ -z $(find "${scratch}" -mindepth 1 -print -quit) ]] || {
		echo "verification leaked temporary files" >&2
		return 1
	}

	# Local steps pass while a CI-owned step remains: the receipt is partial
	# and names CI, never a full verification.
	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'local step ran\n'
MAKE
	: >make/source.mk
	output=$(VERIFY_FORCE=1 bash "${script}" --files scripts/check-skills.py scripts/tests/template-profile-projections.py)
	grep -q 'local step ran' <<<"${output}"
	grep -q '^status: partially_verified$' <<<"${output}"
	grep -q '^ci_owned: make template-init-check$' <<<"${output}"
	grep -q '^gap_or_next_owner: CI$' <<<"${output}"
	rm make/source.mk

	# A failure retains passed and unstarted steps without granting aggregate
	# acceptance; the owner finishes only the missing leaves.
	cat >Makefile <<'MAKE'
tools-check:
	@printf 'tools\n' >>invoked
check-instructions:
	@printf 'skills\n' >>invoked
	@test -f allow-skills
secret-scan:
	@printf 'secrets\n' >>invoked
MAKE
	receipts_before=$(find .git/codex/verify -name '*.receipt' | wc -l)
	if output=$(VERIFY_FORCE=1 bash "${script}" --files tools/versions.env scripts/check-skills.py .gitleaks.toml 2>&1); then
		echo "verify self-test accepted a partially failed plan" >&2
		return 1
	fi
	attempt_path=$(sed -n 's/^verification attempt: //p' <<<"${output}")
	[[ -f ${attempt_path} ]]
	grep -q '^step_state: 1 passed ' "${attempt_path}"
	grep -q '^step_state: 2 failed ' "${attempt_path}"
	grep -q '^step_state: 3 pending$' "${attempt_path}"
	if grep -q '^step_state: 3 running ' "${attempt_path}"; then return 1; fi
	grep -q '^command: make secret-scan$' "${attempt_path}"
	grep -q '^attempt_state: failed$' "${attempt_path}"
	[[ $(cat invoked) == $'tools\nskills' ]]
	[[ $(find .git/codex/verify -name '*.receipt' | wc -l) == "${receipts_before}" ]]
	: >allow-skills
	make check-instructions secret-scan >/dev/null
	[[ $(cat invoked) == $'tools\nskills\nskills\nsecrets' ]]
	[[ $(find .git/codex/verify -name '*.receipt' | wc -l) == "${receipts_before}" ]]
	# An explicitly requested run still executes the entire plan; partial
	# results never become automatic cross-candidate cache hits.
	output=$(VERIFY_FORCE=1 bash "${script}" --files tools/versions.env scripts/check-skills.py .gitleaks.toml)
	attempt_path=$(sed -n 's/^verification attempt: //p' <<<"${output}")
	grep -q '^attempt_state: passed$' "${attempt_path}"
	[[ $(grep -c '^step_state: [123] passed ' "${attempt_path}") == 3 ]]
	grep -q '^result: pass$' <<<"${output}"
	# A step that mutates the selected candidate must not leave reusable success.
	cat >Makefile <<'MAKE'
check-instructions:
	@printf 'changed during verification\n' >>scripts/check-skills.py
MAKE
	receipts_before=$(find .git/codex/verify -name '*.receipt' | wc -l)
	if output=$(VERIFY_FORCE=1 bash "${script}" --files scripts/check-skills.py 2>&1); then
		echo "verify self-test accepted a candidate changed by a gate" >&2
		return 1
	fi
	attempt_path=$(sed -n 's/^verification attempt: //p' <<<"${output}")
	grep -q '^step_state: 1 invalidated$' "${attempt_path}"
	grep -q '^attempt_state: invalidated$' "${attempt_path}"
	if grep -q '^step_state: 1 passed ' "${attempt_path}"; then return 1; fi
	[[ $(find .git/codex/verify -name '*.receipt' | wc -l) == "${receipts_before}" ]]
	# Interrupt only this fixture's verifier; its started step stays unverified.
	cat >Makefile <<'MAKE'
check-instructions:
	@kill -TERM "$$VERIFY_TEST_PID"
MAKE
	if output=$(VERIFY_FORCE=1 bash -c 'export VERIFY_TEST_PID=$$; exec bash "$1" --locked --files scripts/check-skills.py' _ "${script}" 2>&1); then
		echo "verify self-test accepted an interrupted attempt" >&2
		return 1
	fi
	attempt_path=$(sed -n 's/^verification attempt: //p' <<<"${output}")
	grep -q '^step_state: 1 running ' "${attempt_path}"
	if grep -q '^step_state: 1 passed ' "${attempt_path}"; then return 1; fi
	if grep -q '^attempt_state: passed$' "${attempt_path}"; then return 1; fi
	[[ $(find .git/codex/verify -name '*.receipt' | wc -l) == "${receipts_before}" ]]
	# The retained continuation command must replay the executed argument
	# vector exactly; a stub consumes it without running cargo.
	(
		mkdir stub-bin
		cat >stub-bin/make <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@"
SH
		chmod +x stub-bin/make
		export PATH="${fixture}/stub-bin:${PATH}"
		for kind in lint test shell; do
			prepare_command "${kind}" 'a b; unexpected-shell-command'
			actual=$("${step_command[@]}")
			replayed=$(bash -c "${step_display}")
			[[ ${actual} == "${replayed}" ]]
			case "${kind}" in
			lint) grep -q '^PKGS=a b; unexpected-shell-command$' <<<"${replayed}" ;;
			test) grep -q '^PKGS=a b; unexpected-shell-command$' <<<"${replayed}" ;;
			shell) grep -q '^SHELL_FILES=a b; unexpected-shell-command$' <<<"${replayed}" ;;
			esac
		done
		# Image kinds carry the executed commit into the build and the check.
		cat >stub-bin/make <<'SH'
#!/usr/bin/env bash
printf 'VCS_REF=%s\n' "${VCS_REF:-}"
printf '%s\n' "$@"
SH
		execution_head='fixture-source-head'
		for kind in image-build image-check image-security migration-validate; do
			prepare_command "${kind}" 'fixture:tag; unexpected-shell-command'
			actual=$("${step_command[@]}")
			replayed=$(bash -c "${step_display}")
			[[ ${actual} == "${replayed}" ]]
			case "${kind}" in
			image-build)
				grep -q '^VCS_REF=fixture-source-head$' <<<"${replayed}"
				grep -q '^RUNTIME_IMAGE=fixture:tag; unexpected-shell-command$' <<<"${replayed}"
				;;
			image-check | migration-validate)
				grep -q '^RUNTIME_IMAGE=fixture:tag; unexpected-shell-command$' <<<"${replayed}"
				grep -q '^RUNTIME_EXPECTED_COMMIT=fixture-source-head$' <<<"${replayed}"
				;;
			image-security) grep -q '^CONTAINER_IMAGE=fixture:tag; unexpected-shell-command$' <<<"${replayed}" ;;
			esac
		done
	)
)

if [[ ${mode} == self-test ]]; then
	# Which steps are CI-owned depends on these; CI and a workstation test the
	# same routes.
	CI='' ALLOW_FULL='' ALLOW_HEAVY='' self_test
	exit
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "${tmp}"' EXIT
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

surfaces_path=${tmp}/surfaces
bash ./scripts/ci/changed-surfaces.sh <"${files_path}" >"${surfaces_path}"
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

# CI runs every heavy step and the source initializer matrix on the surfaces
# that select them, so a local run leaves them there rather than occupying
# the workstation.
ci_owned() {
	local requires_heavy=$1 display=$2
	[[ ${CI:-} != true ]] || return 1
	if [[ ${requires_heavy} == true ]]; then
		[[ ${ALLOW_HEAVY:-} != 1 ]]
		return
	fi
	[[ ${display} == 'make template-init-check' && ${ALLOW_FULL:-} != 1 ]]
}

# add_command KIND ARGUMENT REASON DISPLAY COST_CLASS REQUIRES_HEAVY REQUIRES_DOCKER
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

# Cheap owners first, so a plan fails fast on the inexpensive gate.
if is_true tool_manifest; then add_command make tools-check "tool manifest changed" "make tools-check" cheap false false; fi
if is_true validation_system; then
	add_command make changed-surfaces-check "validation routing changed" "make changed-surfaces-check" cheap false false
	add_command make affected-crates-check "validation routing changed" "make affected-crates-check" cpu false false
	add_command make validation-lock-self-test "validation routing changed" "make validation-lock-self-test" cheap false false
	add_command make verify-check "validation routing changed" "make verify-check" cpu false false
fi
if is_true module_initializer; then
	add_command make template-init-check "canonical projections and twelve runtime representatives" "make template-init-check" cpu false false
fi

workspace_rust=false
if is_true cargo_dependencies; then workspace_rust=true; fi
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
fi
if is_true rust_source || is_true lint_config; then
	add_command make fmt-check "Rust source or formatting configuration changed" "make fmt-check" cheap false false
fi
if [[ ${workspace_rust} == true ]]; then
	reason="dependency, lockfile, or toolchain changes can affect every crate"
	if is_true cargo_dependencies; then :; elif [[ -n ${affected_reason} ]]; then reason="Rust changes need the workspace oracle (${affected_reason})"; fi
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
if is_true openapi; then add_command make openapi-check "OpenAPI document or its lint configuration changed" "make openapi-check" cpu false false; fi
if is_true github_workflows; then
	add_command make actionlint "GitHub workflow or action source changed" "make actionlint" cheap false false
	add_command make zizmor "GitHub workflow or action source changed" "make zizmor" cheap false false
fi
if is_true dependency_automation; then add_na dependency_automation "GitHub validates the Dependabot schema; Dependency Review stays a CI gate"; fi
if is_true shell; then
	shell_files=$(awk '/\.sh$/ { print }' "${files_path}" | while IFS= read -r file; do if [[ -f ${file} ]]; then printf '%s ' "${file}"; fi; done)
	shell_files=${shell_files% }
	if [[ -n ${shell_files} ]]; then add_command shell "${shell_files}" "shell sources changed" "make shellcheck SHELL_FILES='${shell_files}'" docker false true; else add_na shell "no changed shell source remains"; fi
fi
if is_true agent_instructions; then add_command make check-instructions "agent instructions, skills, roles, or carriers changed" "make check-instructions" cheap false false; fi
if is_true publication_metadata; then add_command make publish-image-metadata-check "publication naming or promotion changed" "make publish-image-metadata-check" cheap false false; fi
if is_true secret_scanning; then add_command make secret-scan "secret scanning policy changed" "make secret-scan" cpu false false; fi
if is_true migrations; then
	add_command make migration-check "migration set or its runner changed" "make migration-check" cpu false false
	add_command make migration-history-self-test "migration set or its runner changed" "make migration-history-self-test" cheap false false
fi
if is_true db_integration; then
	add_command make test-integration-db "database adapter, runner, or database proof changed" "make test-integration-db" docker true true
fi
if is_true runtime_image || is_true migrations; then
	image=${VERIFY_RUNTIME_IMAGE:-service:verify}
	if is_true runtime_image; then
		add_command make dockerfile-check "runtime image inputs changed" "make dockerfile-check" docker false true
	fi
	add_command image-build "${image}" "one image is shared by the selected runtime gates" "make runtime-image-build RUNTIME_IMAGE=${image}" docker true true
	# The rehearsal contains the lifecycle check, with the profile enabled.
	if is_true migrations; then
		add_command migration-validate "${image}" "migration set or its rehearsal changed" "make migration-validate RUNTIME_IMAGE=${image}" docker true true
	else
		add_command image-check "${image}" "runtime image lifecycle changed" "make runtime-image-check RUNTIME_IMAGE=${image}" docker true true
	fi
	if is_true runtime_image; then
		add_command image-security "${image}" "runtime image inputs changed" "make container-security CONTAINER_IMAGE=${image}" docker true true
	fi
fi
if is_true documentation; then add_command make docs-check "Markdown changed" "make docs-check" docker false true; fi

print_plan() {
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

for binary in git make shasum; do command -v "${binary}" >/dev/null 2>&1 || blocked "required binary is unavailable: ${binary}"; done
if is_true rust_source || is_true cargo_dependencies || is_true dependency_policy || is_true lint_config || is_true openapi || is_true validation_system || is_true module_initializer || is_true tool_manifest; then
	command -v cargo >/dev/null 2>&1 || blocked "required binary is unavailable: cargo"
fi
if is_true openapi; then command -v npx >/dev/null 2>&1 || blocked "required binary is unavailable: npx"; fi
if is_true github_workflows || is_true secret_scanning; then command -v go >/dev/null 2>&1 || blocked "required binary is unavailable: go"; fi
docker_command=${VERIFY_DOCKER_COMMAND:-docker}
if [[ ${requires_docker} == true ]]; then
	command -v "${docker_command}" >/dev/null 2>&1 || blocked "Docker is required"
	"${docker_command}" info >/dev/null 2>&1 || blocked "Docker is required and the daemon is unavailable"
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
environment=$(printf '%s\n' "${environment_detail}" | shasum -a 256 | awk '{print $1}')
common_dir=$(git rev-parse --git-common-dir)
[[ ${common_dir} == /* ]] || common_dir=${ROOT_DIR}/${common_dir}
receipt_dir=${common_dir}/codex/verify
receipt=${receipt_dir}/${candidate}-${plan}-${environment}.receipt

if [[ -f ${receipt} && ${VERIFY_FORCE:-} != 1 ]]; then
	echo "reusing exact passing receipt: ${receipt}"
	cat "${receipt}"
	exit
fi

if [[ ${locked} != true ]]; then
	args=(bash "$0" --locked)
	if ((${#provided_files[@]})); then args+=(--files "${provided_files[@]}"); fi
	rm -rf -- "${tmp}"
	exec bash ./scripts/ci/validation-lock.sh -- "${args[@]}"
fi

print_plan

# Partial runs retain evidence, never cross-candidate cache hits: only a
# complete passing attempt writes a receipt, and only for this exact
# candidate, plan, and environment.
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

receipt_tmp=${receipt}.tmp.$$
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
