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
#   changed-surfaces.sh --all          every surface true (tags, schedule,
#                                      manual runs)
#   changed-surfaces.sh --self-test
#
# Surface table: specs/validation-delivery/research/synthesis.md. A surface
# joins this list with its first artifact and its consumer, never ahead.
set -euo pipefail

names=(
	rust_source cargo_dependencies dependency_policy lint_config openapi tool_manifest
	github_workflows dependency_automation shell secret_scanning
	agent_instructions documentation validation_system no_validation_required
)

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
	local file matched
	reset
	while IFS= read -r file; do
		[[ -n ${file} ]] || continue
		tracking_file=true
		matched=false

		# One path may select several surfaces; each case is independent.
		case "${file}" in
		*.rs | crates/*/src/* | crates/*/tests/* | env/config/*) mark rust_source ;;
		esac
		case "${file}" in
		Cargo.toml | Cargo.lock | crates/*/Cargo.toml | rust-toolchain.toml) mark cargo_dependencies ;;
		esac
		case "${file}" in
		deny.toml) mark dependency_policy ;;
		esac
		case "${file}" in
		clippy.toml | rustfmt.toml | Cargo.toml) mark lint_config ;;
		esac
		case "${file}" in
		.redocly.yaml | api/openapi/*) mark openapi ;;
		esac
		case "${file}" in
		tools/versions.env | scripts/ci/tools-check.sh) mark tool_manifest ;;
		esac
		case "${file}" in
		.github/workflows/* | .github/actions/*) mark github_workflows ;;
		.github/dependabot.yml) mark dependency_automation ;;
		esac
		case "${file}" in
		*.sh) mark shell ;;
		esac
		case "${file}" in
		.gitleaks.toml) mark secret_scanning ;;
		esac
		case "${file}" in
		AGENTS.md | CLAUDE.md | .agents/* | docs/skill-authoring.md | scripts/check-skills.py) mark agent_instructions ;;
		esac
		case "${file}" in
		*.md | docs/* | specs/*) mark documentation ;;
		esac
		case "${file}" in
		.editorconfig | .gitattributes | .gitignore | LICENSE | .github/CODEOWNERS | .github/ISSUE_TEMPLATE/*) mark no_validation_required ;;
		esac
		case "${file}" in
		Makefile | make/*.mk | scripts/ci/changed-surfaces.sh | scripts/ci/git-changed-paths.sh | scripts/ci/affected-crates.sh | scripts/ci/verify.sh | scripts/ci/validation-lock.sh | scripts/ci/measure.sh)
			mark validation_system
			;;
		esac
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
	output="$(printf '%s\n' "${file}" | classify)"
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
	local root output name file
	root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

	assert_case crates/service/src/main.rs \
		"rust_source" \
		"cargo_dependencies lint_config openapi shell documentation validation_system"
	assert_case crates/service/build.rs \
		"rust_source" \
		"cargo_dependencies validation_system"
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
		"cargo_dependencies lint_config" \
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
	assert_case .github/workflows/ci.yml \
		"github_workflows" \
		"dependency_automation documentation"
	assert_case .github/actions/publish-image/action.yml \
		"github_workflows" \
		"dependency_automation"
	assert_case .github/dependabot.yml \
		"dependency_automation" \
		"github_workflows cargo_dependencies"
	assert_case .gitleaks.toml \
		"secret_scanning" \
		"documentation dependency_policy"
	assert_case AGENTS.md \
		"agent_instructions documentation" \
		"rust_source validation_system"
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
	assert_case README.md \
		"documentation" \
		"agent_instructions rust_source cargo_dependencies"
	assert_case docs/roadmap.md \
		"documentation" \
		"agent_instructions"
	assert_case specs/validation-delivery/research/synthesis.md \
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
			"validation_system" \
			"shell rust_source cargo_dependencies"
	done
	for file in changed-surfaces git-changed-paths affected-crates verify validation-lock measure; do
		assert_case "scripts/ci/${file}.sh" \
			"validation_system shell" \
			"tool_manifest rust_source"
	done

	output="$(printf '%s\n' Cargo.toml | classify)"
	has_line "${output}" 'surface_count=2'
	output="$(printf '%s\n' LICENSE | classify)"
	has_line "${output}" 'surface_count=0'
	has_line "${output}" 'classified=true'

	output="$(bash "$0" --all)"
	for name in "${names[@]}"; do
		has_line "${output}" "${name}=true"
	done

	if output="$(printf '%s\n' unknown/new-owner.xyz | classify 2>&1)"; then
		echo "unknown paths must fail closed" >&2
		return 1
	fi
	has_line "${output}" 'classified=false'
	has_line "${output}" 'unclassified_files=unknown/new-owner.xyz'

	# Every tracked file has an owner.
	output="$(git -C "${root}" ls-files | classify)"
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
	reset
	mark "${names[@]}"
	emit
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
