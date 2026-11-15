#!/usr/bin/env bash
# Emit unique changed paths. Renames include both names. A copy from an
# unchanged path contributes only the added name.
#
#   git-changed-paths.sh --worktree BASE_REF   commits since the merge base, the
#                                              index, the worktree, untracked files
#   git-changed-paths.sh --diff BASE HEAD      two commits
#   git-changed-paths.sh --self-test
set -euo pipefail

usage() {
	echo "usage: $0 --worktree BASE_REF | --diff BASE HEAD | --self-test" >&2
	exit 2
}

tracked_paths() {
	git diff --name-only -z --no-renames "$@" | LC_ALL=C tr '\0' '\n'
}

require_commit() {
	local ref=$1
	git rev-parse --verify "${ref}^{commit}" >/dev/null 2>&1 || {
		echo "verification base is unavailable: ${ref}; set BASE_REF to a readable commit" >&2
		exit 2
	}
}

worktree_paths() {
	local base_ref=$1
	require_commit "${base_ref}"
	{
		tracked_paths "${base_ref}...HEAD" || return
		tracked_paths || return
		tracked_paths --cached || return
		git ls-files --others --exclude-standard
	} | LC_ALL=C sort -u
}

diff_paths() {
	local base=$1 head=$2
	require_commit "${base}"
	require_commit "${head}"
	tracked_paths "${base}" "${head}" | LC_ALL=C sort -u
}

self_test() {
	local tmp repo output script
	tmp=$(mktemp -d)
	trap 'rm -rf -- "${tmp}"' RETURN
	script=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/git-changed-paths.sh
	repo=${tmp}/repo
	git init -q "${repo}"
	git -C "${repo}" config user.name changed-paths
	git -C "${repo}" config user.email changed-paths@example.invalid
	git -C "${repo}" config commit.gpgsign false
	mkdir -p "${repo}/crates/old/src"
	printf 'pub fn old() {}\n' >"${repo}/crates/old/src/lib.rs"
	git -C "${repo}" add crates/old/src/lib.rs
	git -C "${repo}" commit -qm start
	mkdir -p "${repo}/crates/new/src"
	git -C "${repo}" mv crates/old/src/lib.rs crates/new/src/lib.rs
	git -C "${repo}" commit -qm move
	output=$(cd "${repo}" && bash "${script}" --diff HEAD~1 HEAD)
	grep -qx 'crates/old/src/lib.rs' <<<"${output}"
	grep -qx 'crates/new/src/lib.rs' <<<"${output}"
	[[ $(grep -c . <<<"${output}") -eq 2 ]]
	printf 'untracked\n' >"${repo}/notes.md"
	output=$(cd "${repo}" && bash "${script}" --worktree HEAD~1)
	grep -qx 'notes.md' <<<"${output}"
	grep -qx 'crates/new/src/lib.rs' <<<"${output}"
	cp "${repo}/crates/new/src/lib.rs" "${repo}/copy with space\\.rs"
	git -C "${repo}" add .
	git -C "${repo}" commit -qm copy
	output=$(cd "${repo}" && bash "${script}" --diff HEAD~1 HEAD)
	[[ ${output} == $'copy with space\\.rs\nnotes.md' ]]
	printf 'staged\n' >"${repo}/notes.md"
	git -C "${repo}" add notes.md
	printf 'untracked\n' >"${repo}/notes.md"
	output=$(cd "${repo}" && bash "${script}" --worktree HEAD)
	[[ ${output} == notes.md ]]
}

case "${1:-}" in
--self-test)
	self_test
	;;
--worktree)
	[[ $# -eq 2 ]] || usage
	worktree_paths "$2"
	;;
--diff)
	[[ $# -eq 3 ]] || usage
	diff_paths "$2" "$3"
	;;
*)
	usage
	;;
esac
