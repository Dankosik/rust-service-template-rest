#!/usr/bin/env bash
# Classify changed paths into validation surfaces. CI and `make verify` read
# the same output, so one owner decides which gates a change selects.
#
#   changed-surfaces.sh < paths        NAME=true|false per surface, then
#                                      classified=, surface_count=,
#                                      unclassified_files=; exit 1 when any
#                                      path has no surface (fail closed)
#   changed-surfaces.sh --union BASE   OR of this classifier and the one at
#                                      BASE, so a pull request that changes
#                                      the classifier is judged by both
#   changed-surfaces.sh --all          every applicable surface true (tags,
#                                      schedule, manual runs); profile and
#                                      source-only surfaces remain bounded
#   changed-surfaces.sh --self-test
#
# Surface table: docs/ci-cd-production-ready.md. A surface joins this list
# with its first artifact and its consumer, never ahead.
set -euo pipefail

names=(
	rust_source cargo_dependencies dependency_policy lint_config openapi tool_manifest
	github_workflows dependency_automation shell runtime_image publication_metadata secret_scanning
	db_integration migrations
	agent_instructions documentation validation_system module_initializer initializer_runtime no_validation_required
)

profile_database() {
	local root database
	root=$(pwd)
	database=$(python3 "${root}/scripts/lib/template_state.py" profile --repo "${root}" --field database) || {
		echo "cannot resolve selected database profile" >&2
		return 2
	}
	case "${database}" in
	none | postgres) printf '%s\n' "${database}" ;;
	*)
		echo "invalid selected database profile: ${database}" >&2
		return 2
		;;
	esac
}

all_surfaces() {
	local database source_only=false
	database=$(profile_database)
	[[ -f make/source.mk ]] && source_only=true
	reset
	mark "${names[@]}"
	if [[ ${database} == none ]]; then
		clear_surface db_integration migrations
	fi
	[[ ${source_only} == true ]] || clear_surface module_initializer initializer_runtime
	emit
}

reset() {
	local name
	unclassified_paths=()
	for name in "${names[@]}"; do
		printf -v "${name}" '%s' false
	done
}

mark() {
	local name
	[[ ${tracking_file:-false} == true ]] && matched=true
	for name in "$@"; do
		printf -v "${name}" '%s' true
	done
}

clear_surface() {
	local name
	for name in "$@"; do
		printf -v "${name}" '%s' false
	done
}

emit() {
	local name count=0 classified=true unclassified=''
	for name in "${names[@]}"; do
		printf '%s=%s\n' "${name}" "${!name}"
		if [[ ${name} != no_validation_required && ${!name} == true ]]; then ((count += 1)); fi
	done
	if ((${#unclassified_paths[@]})); then
		classified=false
		unclassified=$(
			IFS=,
			echo "${unclassified_paths[*]}"
		)
	fi
	printf 'classified=%s\nsurface_count=%s\nunclassified_files=%s\n' "${classified}" "${count}" "${unclassified}"
}

has_line() {
	[[ $'\n'"$1"$'\n' == *$'\n'"$2"$'\n'* ]]
}

classify() {
	local file matched database source_only=false p9_retained=false
	database=$(profile_database)
	[[ -f make/source.mk ]] && source_only=true
	[[ -f test/tests/http_idempotency/mounted.rs ]] && p9_retained=true
	reset
	while IFS= read -r file; do
		[[ -n ${file} ]] || continue
		tracking_file=true
		matched=false

		# One path may select several surfaces; each case is independent.
		case "${file}" in
		*.rs | crates/*/src/* | crates/*/tests/* | test/src/* | test/tests/* | env/config/*) mark rust_source ;;
		esac
		case "${file}" in
		Cargo.toml | Cargo.lock | crates/*/Cargo.toml | test/Cargo.toml | rust-toolchain.toml) mark cargo_dependencies ;;
		esac
		# Database-backed proof: the adapter, the runner, the test crate and
		# its fixtures, the compose file, and the scripts that drive them.
		if [[ ${database} == postgres ]]; then case "${file}" in
		crates/infra-postgres/* | crates/infra-idempotency-store/* | crates/infra-jobs/* | crates/jobs-worker/* | crates/migrate/* | test/* | env/docker-compose.yml | scripts/ci/test-integration-db.sh | scripts/lib/compose-postgres.sh)
			mark db_integration
			;;
		esac
		# P9 mounts the seam and the authentication engine against a real
		# database; retained only while the introspection-only fixture exists.
		if [[ ${p9_retained} == true ]]; then case "${file}" in
		crates/infra-http/* | crates/infra-bearerauthn/*)
			mark db_integration
			;;
		esac; fi
		# The migration set and everything that rehearses it against the image.
		case "${file}" in
		migrations/*.sql | crates/migrate/* | env/docker-compose.yml | scripts/ci/migration-validate.sh | scripts/ci/migration-history-check.sh | scripts/lib/compose-postgres.sh)
			# A migration source changes the image payload. The image surface
			# stays profile-neutral; postgres is what admits this extra source.
			mark migrations runtime_image
			;;
		esac; fi
		case "${file}" in
		deny.toml) mark dependency_policy ;;
		esac
		case "${file}" in
		clippy.toml | rustfmt.toml | Cargo.toml) mark lint_config ;;
		esac
		case "${file}" in
		.redocly.yaml | api/openapi/*) mark openapi ;;
		esac
		# The Dockerfile carries tool pins too (ARG defaults, FROM digests).
		case "${file}" in
		tools/versions.env | scripts/ci/tools-check.sh | build/docker/Dockerfile) mark tool_manifest ;;
		esac
		case "${file}" in
		.dockerignore | build/docker/* | scripts/ci/runtime-image-*.sh) mark runtime_image ;;
		esac
		case "${file}" in
		.github/workflows/* | .github/actions/*) mark github_workflows ;;
		.github/dependabot.yml) mark dependency_automation ;;
		esac
		case "${file}" in
		*.sh) mark shell ;;
		esac
		case "${file}" in
		.github/actions/publish-image/* | scripts/ci/publish-image-metadata.sh) mark publication_metadata ;;
		esac
		case "${file}" in
		.gitleaks.toml) mark secret_scanning ;;
		esac
		# Instructions, roles, skills, their generated harness carriers, the
		# workflow and harness documents, and the scripts that check them.
		case "${file}" in
		template.lock | AGENTS.md | CLAUDE.md | QWEN.md | Grok.md | opencode.json | .agents/* | .claude/* | .codex/* | .cursor/* | .qwen/* | .grok/* | .opencode/* | docs/skill-authoring.md | docs/agent-harness.md | docs/agent-harness/* | docs/spec-first-workflow.md | docs/spec-first-workflow/* | docs/prompt-composition.md | docs/prompt-maintenance.md | docs/subagent-brief-template.md | scripts/check-skills.py | scripts/agent-roles-sync.sh | scripts/codex-agents-sync.sh | scripts/harness-skills-sync.sh | scripts/lib/sync-cli.sh)
			mark agent_instructions
			;;
		esac
		case "${file}" in
		*.md | docs/* | specs/*) mark documentation ;;
		esac
		case "${file}" in
		.editorconfig | .gitattributes | .gitignore | LICENSE | .github/CODEOWNERS | .github/ISSUE_TEMPLATE/*) mark no_validation_required ;;
		esac
		case "${file}" in
		template.lock | Makefile | make/*.mk | scripts/ci/changed-surfaces.sh | scripts/ci/git-changed-paths.sh | scripts/ci/affected-crates.sh | scripts/ci/verify.sh | scripts/ci/validation-lock.sh | scripts/ci/measure.sh)
			mark validation_system
			;;
		esac
		# These surfaces exist only in the source template. A derived service
		# cannot select the source-only matrix because make/source.mk was removed.
		# The runtime matrix proves what can change an initialized service's build
		# and tests or the initializer itself; the canonical projections alone
		# prove projected text.
		if [[ ${source_only} == true ]]; then case "${file}" in
		Cargo.toml | Cargo.lock | rust-toolchain.toml | template.lock | Makefile | make/*.mk | \
		api/openapi/* | env/config/* | .github/workflows/ci.yml | \
		scripts/init-module.sh | scripts/template-sync.sh | scripts/lib/template_*.py | scripts/lib/template_profiles.json | \
		template-owned.paths | \
		scripts/ci/template-init-check.sh | scripts/tests/template-* | \
		crates/config/src/* | crates/config/Cargo.toml | crates/service/src/* | crates/service/tests/* | crates/service/Cargo.toml | \
		crates/infra-bearerauthn/* | crates/infra-egress-dns/* | crates/infra-outbound-http/* | crates/infra-idempotency-store/* | crates/infra-http/Cargo.toml | crates/infra-http/src/authn.rs | crates/infra-http/src/idempotency/* | crates/infra-http/src/harden.rs | crates/infra-http/src/lib.rs | crates/infra-http/src/problem.rs | \
		crates/infra-postgres/* | crates/migrate/* | crates/infra-jobs/* | crates/jobs-worker/* | \
		test/* | migrations/*)
			mark module_initializer initializer_runtime
			;;
		build/docker/Dockerfile | README.md | CONTRIBUTING.md | SECURITY.md | .gitleaks.toml | \
		.github/CODEOWNERS | .github/ISSUE_TEMPLATE/* | .github/dependabot.yml | \
		.github/workflows/cd.yml | .github/actions/publish-image/action.yml | \
		scripts/ci/changed-surfaces.sh | scripts/ci/verify.sh | scripts/ci/runtime-image-build.sh | \
		.agents/* | AGENTS.md | CLAUDE.md | QWEN.md | Grok.md | opencode.json | \
		.claude/* | .codex/* | .cursor/* | .qwen/* | .grok/* | .opencode/* | \
		docs/repo-architecture.md | docs/architecture/* | docs/configuration-source-policy.md | docs/production-contract.md | \
		docs/first-production-feature.md | docs/project-structure-and-module-organization.md | \
		docs/backend-library-selection.md | docs/backend-utility-recipes.md | \
		docs/build-test-and-development-commands.md | docs/ci-cd-production-ready.md | docs/railway-deployment-profile.md | \
		docs/validation/* | docs/template-sync.md | docs/authentication.md | docs/outbound-http.md | docs/http-idempotency.md | docs/background-jobs.md)
			mark module_initializer
			;;
		esac; fi
		if [[ ${matched} != true ]]; then unclassified_paths+=("${file}"); fi
	done
	tracking_file=false
	emit
	((${#unclassified_paths[@]} == 0))
}

union_classify() {
	local base_ref=$1 tmp files old_files base_script current old current_status old_status name value count=0
	local current_classified current_unclassified line
	tmp=$(mktemp -d)
	trap 'rm -rf -- "${tmp}"' RETURN
	files=${tmp}/files
	old_files=${tmp}/old-files
	base_script=${tmp}/changed-surfaces.sh
	cat >"${files}"
	: >"${old_files}"
	while IFS= read -r file; do
		[[ ${file} != scripts/ci/changed-surfaces.sh ]] || continue
		git cat-file -e "${base_ref}:${file}" 2>/dev/null && printf '%s\n' "${file}" >>"${old_files}"
	done <"${files}"
	git show "${base_ref}:scripts/ci/changed-surfaces.sh" >"${base_script}" 2>/dev/null || {
		echo "classifier base is unavailable: ${base_ref}" >&2
		return 2
	}
	set +e
	current=$(classify <"${files}")
	current_status=$?
	old=$(bash "${base_script}" <"${old_files}")
	old_status=$?
	set -e
	if ((old_status != 0)); then
		echo "base classifier failed: ${base_ref}" >&2
		return "${old_status}"
	fi
	for name in "${names[@]}"; do
		value=false
		if has_line "${current}" "${name}=true" || has_line "${old}" "${name}=true"; then value=true; fi
		printf '%s=%s\n' "${name}" "${value}"
		if [[ ${name} != no_validation_required && ${value} == true ]]; then ((count += 1)); fi
	done
	while IFS= read -r line; do
		case "${line}" in
		classified=*) current_classified=${line#*=} ;;
		unclassified_files=*) current_unclassified=${line#*=} ;;
		esac
	done <<<"${current}"
	printf 'classified=%s\nsurface_count=%s\nunclassified_files=%s\n' \
		"${current_classified}" \
		"${count}" \
		"${current_unclassified}"
	return "${current_status}"
}

assert_case() {
	local file=$1 true_names=$2 false_names=$3 output name
	output="$(printf '%s\n' "${file}" | (cd "${classifier_root}" && bash scripts/ci/changed-surfaces.sh))"
	for name in ${true_names}; do
		has_line "${output}" "${name}=true" || {
			printf '%s: expected %s=true\n%s\n' "${file}" "${name}" "${output}" >&2
			return 1
		}
	done
	for name in ${false_names}; do
		has_line "${output}" "${name}=false" || {
			printf '%s: expected %s=false\n%s\n' "${file}" "${name}" "${output}" >&2
			return 1
		}
	done
}

self_test() {
	local root output name file source_fixture derived_fixture head classifier_root
	root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
	source_fixture=$(mktemp -d)
	derived_fixture=$(mktemp -d)
	trap 'rm -rf -- "${source_fixture}" "${derived_fixture}"' RETURN
	for fixture in "${source_fixture}" "${derived_fixture}"; do
		mkdir -p "${fixture}/scripts/ci" "${fixture}/scripts/lib"
		cp "${root}/scripts/ci/changed-surfaces.sh" "${fixture}/scripts/ci/changed-surfaces.sh"
		cp "${root}/scripts/lib/template_state.py" "${fixture}/scripts/lib/template_state.py"
	done
	mkdir -p "${source_fixture}/make"
	: >"${source_fixture}/make/source.mk"
	head=$(git -C "${root}" rev-parse HEAD)
	cat >"${derived_fixture}/template.lock" <<EOF
{"schema_version":1,"state":"complete","identity":{"service_name":"fixture-api","repository":"https://github.com/example/fixture-api","description":"Fixture API","codeowner":"@example/platform"},"profiles":{"database":"none","agent_harness":"core"},"source":{"repository":"https://github.com/Dankosik/rust-service-template-rest","checkout_revision":"${head}","provenance":"local-checkout"}}
EOF
	classifier_root=${source_fixture}

	assert_case template.lock \
		"agent_instructions validation_system module_initializer initializer_runtime" \
		"cargo_dependencies shell documentation"

	assert_case crates/service/src/main.rs \
		"rust_source module_initializer initializer_runtime" \
		"cargo_dependencies lint_config openapi shell documentation validation_system"
	assert_case crates/config/build.rs \
		"rust_source" \
		"cargo_dependencies validation_system"
	for file in crates/infra-bearerauthn/src/claims.rs crates/infra-http/src/authn.rs; do
		assert_case "${file}" \
			"rust_source module_initializer initializer_runtime" \
			"cargo_dependencies documentation db_integration"
	done
	assert_case crates/infra-http/src/idempotency/mod.rs \
		"rust_source module_initializer initializer_runtime" \
		"cargo_dependencies documentation db_integration"
	assert_case crates/infra-idempotency-store/src/lib.rs \
		"rust_source db_integration module_initializer initializer_runtime" \
		"cargo_dependencies migrations documentation"
	assert_case crates/infra-jobs/src/lib.rs \
		"rust_source db_integration module_initializer initializer_runtime" \
		"cargo_dependencies migrations documentation"
	assert_case crates/jobs-worker/src/main.rs \
		"rust_source db_integration module_initializer initializer_runtime" \
		"cargo_dependencies migrations documentation"
	assert_case crates/jobs-worker/Cargo.toml \
		"cargo_dependencies db_integration module_initializer initializer_runtime" \
		"rust_source documentation"
	# P9 only mounts infra-http and infra-bearerauthn against a real
	# database while the introspection-only fixture is retained.
	mkdir -p "${classifier_root}/test/tests/http_idempotency"
	: >"${classifier_root}/test/tests/http_idempotency/mounted.rs"
	assert_case crates/infra-http/src/authn.rs \
		"rust_source module_initializer initializer_runtime db_integration" \
		"cargo_dependencies documentation"
	assert_case crates/infra-bearerauthn/src/claims.rs \
		"rust_source module_initializer initializer_runtime db_integration" \
		"cargo_dependencies documentation"
	rm -rf "${classifier_root}/test/tests/http_idempotency"
	for file in crates/infra-egress-dns/src/lib.rs crates/infra-outbound-http/src/lib.rs; do
		assert_case "${file}" \
			"rust_source module_initializer initializer_runtime" \
			"cargo_dependencies documentation"
	done
	for file in crates/config/Cargo.toml crates/service/Cargo.toml crates/infra-http/Cargo.toml crates/infra-egress-dns/Cargo.toml crates/infra-outbound-http/Cargo.toml; do
		assert_case "${file}" \
			"cargo_dependencies module_initializer initializer_runtime" \
			"rust_source documentation"
	done
	assert_case docs/authentication.md \
		"documentation module_initializer" \
		"rust_source cargo_dependencies initializer_runtime"
	assert_case docs/outbound-http.md \
		"documentation module_initializer" \
		"rust_source cargo_dependencies initializer_runtime"
	assert_case docs/http-idempotency.md \
		"documentation module_initializer" \
		"rust_source cargo_dependencies initializer_runtime"
	assert_case docs/background-jobs.md \
		"documentation module_initializer" \
		"rust_source cargo_dependencies initializer_runtime"
	assert_case docs/architecture/async.md \
		"documentation module_initializer" \
		"rust_source cargo_dependencies initializer_runtime"
	assert_case crates/service/tests/lifecycle.rs \
		"rust_source" \
		"cargo_dependencies documentation"
	assert_case crates/infra-http/src/fixtures/response.json \
		"rust_source" \
		"documentation"
	assert_case env/config/local.toml \
		"rust_source" \
		"cargo_dependencies lint_config documentation"
	assert_case Cargo.toml \
		"cargo_dependencies lint_config module_initializer initializer_runtime" \
		"rust_source dependency_policy documentation"
	assert_case Cargo.lock \
		"cargo_dependencies" \
		"rust_source lint_config"
	assert_case crates/health/Cargo.toml \
		"cargo_dependencies" \
		"rust_source lint_config"
	assert_case rust-toolchain.toml \
		"cargo_dependencies" \
		"rust_source lint_config tool_manifest"
	assert_case deny.toml \
		"dependency_policy" \
		"cargo_dependencies rust_source"
	assert_case clippy.toml \
		"lint_config" \
		"cargo_dependencies rust_source"
	assert_case rustfmt.toml \
		"lint_config" \
		"cargo_dependencies rust_source"
	assert_case api/openapi/service.yaml \
		"openapi" \
		"rust_source documentation"
	assert_case .redocly.yaml \
		"openapi" \
		"rust_source documentation"
	assert_case tools/versions.env \
		"tool_manifest" \
		"shell validation_system cargo_dependencies"
	assert_case scripts/ci/tools-check.sh \
		"tool_manifest shell" \
		"validation_system"
	assert_case build/docker/Dockerfile \
		"runtime_image tool_manifest module_initializer" \
		"rust_source cargo_dependencies shell validation_system initializer_runtime"
	assert_case .dockerignore \
		"runtime_image" \
		"tool_manifest no_validation_required"
	assert_case scripts/ci/runtime-image-check.sh \
		"runtime_image shell" \
		"tool_manifest validation_system"
	assert_case scripts/ci/runtime-image-build.sh \
		"runtime_image shell" \
		"tool_manifest validation_system"
	assert_case .github/workflows/ci.yml \
		"github_workflows module_initializer initializer_runtime" \
		"dependency_automation documentation"
	assert_case .github/actions/publish-image/action.yml \
		"github_workflows publication_metadata" \
		"dependency_automation runtime_image"
	assert_case scripts/ci/publish-image-metadata.sh \
		"publication_metadata shell" \
		"github_workflows validation_system runtime_image"
	assert_case .github/workflows/cd.yml \
		"github_workflows" \
		"publication_metadata"
	assert_case .github/dependabot.yml \
		"dependency_automation" \
		"github_workflows cargo_dependencies"
	assert_case .gitleaks.toml \
		"secret_scanning" \
		"documentation dependency_policy"
	assert_case AGENTS.md \
		"agent_instructions documentation module_initializer" \
		"rust_source validation_system initializer_runtime"
	assert_case CLAUDE.md \
		"agent_instructions documentation" \
		"rust_source"
	assert_case .agents/skills/rust-coder/SKILL.md \
		"agent_instructions documentation" \
		"rust_source"
	assert_case .agents/skills/rust-coder/LICENSE \
		"agent_instructions" \
		"documentation no_validation_required"
	assert_case docs/skill-authoring.md \
		"agent_instructions documentation" \
		"rust_source"
	assert_case scripts/check-skills.py \
		"agent_instructions" \
		"shell documentation"
	for file in QWEN.md Grok.md docs/agent-harness.md docs/agent-harness/cursor.md docs/spec-first-workflow.md docs/spec-first-workflow/phases/intake.md docs/prompt-composition.md .agents/roles/worker-agent.toml .agents/contracts/specialist-neighbors.json .claude/agents/worker-agent.md .codex/config.toml .cursor/rules/agent-harness.mdc .qwen/settings.json .grok/roles/worker-agent.toml .opencode/plugins/task-subagents.js; do
		assert_case "${file}" \
			"agent_instructions" \
			"rust_source shell validation_system github_workflows"
	done
	assert_case opencode.json \
		"agent_instructions" \
		"documentation no_validation_required"
	for file in scripts/agent-roles-sync.sh scripts/codex-agents-sync.sh scripts/harness-skills-sync.sh scripts/lib/sync-cli.sh; do
		assert_case "${file}" \
			"agent_instructions shell" \
			"validation_system tool_manifest"
	done
	assert_case crates/infra-postgres/src/dsn.rs \
		"rust_source db_integration" \
		"migrations cargo_dependencies"
	assert_case crates/infra-postgres/Cargo.toml \
		"cargo_dependencies db_integration" \
		"rust_source migrations"
	assert_case crates/migrate/src/main.rs \
		"rust_source db_integration migrations" \
		"cargo_dependencies"
	assert_case migrations/20260918120000_create_widgets.sql \
		"migrations" \
		"rust_source db_integration documentation"
	assert_case migrations/README.md \
		"documentation" \
		"migrations db_integration"
	assert_case test/tests/postgres.rs \
		"rust_source db_integration" \
		"migrations"
	assert_case test/fixtures/migrations/widgets/20260918000001_create_widgets.sql \
		"db_integration" \
		"rust_source migrations"
	assert_case test/Cargo.toml \
		"cargo_dependencies db_integration" \
		"rust_source"
	assert_case test/README.md \
		"documentation db_integration" \
		"rust_source"
	assert_case env/docker-compose.yml \
		"db_integration migrations runtime_image" \
		"rust_source"
	assert_case scripts/ci/test-integration-db.sh \
		"db_integration shell" \
		"migrations validation_system"
	assert_case scripts/ci/migration-validate.sh \
		"migrations runtime_image shell" \
		"db_integration"
	assert_case scripts/ci/migration-history-check.sh \
		"migrations shell" \
		"db_integration validation_system"
	assert_case scripts/lib/compose-postgres.sh \
		"db_integration migrations shell" \
		"validation_system"
	assert_case README.md \
		"documentation" \
		"agent_instructions rust_source cargo_dependencies"
	assert_case docs/roadmap.md \
		"documentation" \
		"agent_instructions"
	assert_case specs/rust-skills/research/synthesis.md \
		"documentation" \
		"agent_instructions"
	assert_case .github/pull_request_template.md \
		"documentation" \
		"github_workflows"
	for file in LICENSE .gitignore .editorconfig .gitattributes .github/CODEOWNERS .github/ISSUE_TEMPLATE/bug_report.yml; do
		assert_case "${file}" \
			"no_validation_required" \
			"documentation rust_source github_workflows"
	done
	for file in Makefile make/template.mk make/service.mk; do
		assert_case "${file}" \
			"validation_system module_initializer initializer_runtime" \
			"shell rust_source cargo_dependencies"
	done
	assert_case scripts/tests/template-profile-projections.py \
		"module_initializer initializer_runtime" \
		"rust_source cargo_dependencies shell github_workflows db_integration"
	for file in changed-surfaces git-changed-paths affected-crates verify validation-lock measure; do
		assert_case "scripts/ci/${file}.sh" \
			"validation_system shell" \
			"tool_manifest rust_source initializer_runtime"
	done

	output="$(printf '%s\n' Cargo.toml | (cd "${classifier_root}" && bash scripts/ci/changed-surfaces.sh))"
	has_line "${output}" 'surface_count=4'
	output="$(printf '%s\n' LICENSE | (cd "${classifier_root}" && bash scripts/ci/changed-surfaces.sh))"
	has_line "${output}" 'surface_count=0'
	has_line "${output}" 'classified=true'

	# A complete no-DB lock controls the derived fixture; source-only routing
	# remains absent because the fixture has no make/source.mk.
	output=$(printf '%s\n' Cargo.toml | (cd "${derived_fixture}" && bash scripts/ci/changed-surfaces.sh))
	has_line "${output}" 'module_initializer=false'
	has_line "${output}" 'initializer_runtime=false'
	has_line "${output}" 'db_integration=false'
	has_line "${output}" 'migrations=false'

	output="$(cd "${source_fixture}" && bash scripts/ci/changed-surfaces.sh --all)"
	for name in "${names[@]}"; do
		has_line "${output}" "${name}=true"
	done
	output="$(cd "${derived_fixture}" && bash scripts/ci/changed-surfaces.sh --all)"
	has_line "${output}" 'db_integration=false'
	has_line "${output}" 'migrations=false'
	has_line "${output}" 'module_initializer=false'
	has_line "${output}" 'initializer_runtime=false'

	if output="$(printf '%s\n' unknown/new-owner.xyz | (cd "${classifier_root}" && bash scripts/ci/changed-surfaces.sh) 2>&1)"; then
		echo "unknown paths must fail closed" >&2
		return 1
	fi
	has_line "${output}" 'classified=false'
	has_line "${output}" 'unclassified_files=unknown/new-owner.xyz'

	# Every tracked file has an owner.
	output="$(git -C "${root}" ls-files | (cd "${classifier_root}" && bash scripts/ci/changed-surfaces.sh))"
	has_line "${output}" 'classified=true'

	# The union keeps a surface either classifier selects. When HEAD predates
	# the classifier (first commit), the union must refuse rather than guess.
	if git -C "${root}" cat-file -e HEAD:scripts/ci/changed-surfaces.sh 2>/dev/null; then
		output="$(printf '%s\n' scripts/ci/changed-surfaces.sh README.md | (cd "${root}" && bash "$0" --union HEAD))"
		has_line "${output}" 'shell=true'
		has_line "${output}" 'validation_system=true'
		has_line "${output}" 'documentation=true'
		has_line "${output}" 'rust_source=false'
	else
		if output="$(printf '%s\n' README.md | (cd "${root}" && bash "$0" --union HEAD) 2>&1)"; then
			echo "union must refuse a base without the classifier" >&2
			return 1
		fi
		has_line "${output}" 'classifier base is unavailable: HEAD'
	fi
}

case "${1:-}" in
--all)
	all_surfaces
	;;
--self-test)
	self_test
	;;
--union)
	[[ -n ${2:-} ]] || {
		echo "usage: $0 --union BASE_REF" >&2
		exit 2
	}
	union_classify "$2"
	;;
"")
	classify
	;;
*)
	echo "usage: $0 [--all|--self-test|--union BASE_REF]" >&2
	exit 2
	;;
esac
