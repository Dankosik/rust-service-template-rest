#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"

emit() {
	printf 'changed_packages=%s\n' "${changed_packages}"
	printf 'lint_packages=%s\n' "${lint_packages}"
	printf 'test_packages=%s\n' "${test_packages}"
	printf 'fallback=%s\n' "${fallback}"
	printf 'fallback_reason=%s\n' "${fallback_reason}"
}

fall_back() {
	fallback=true
	fallback_reason=$1
	lint_packages=''
	test_packages=''
	emit
	exit 0
}

join_sorted() {
	if [[ ! -s $1 ]]; then
		return
	fi
	LC_ALL=C sort -u "$1" | tr '\n' ' ' | sed 's/[[:space:]]*$//'
}

package_name() {
	local manifest=$1
	awk '
		/^\[/ { in_package = ($0 == "[package]") }
		in_package && $1 == "name" {
			sub(/^[^=]*=[[:space:]]*"/, "")
			sub(/".*$/, "")
			print
			exit
		}
	' "${manifest}"
}

self_test() (
	local fixture script output crate
	fixture=$(mktemp -d)
	trap 'rm -rf -- "${fixture}"' EXIT
	mkdir -p "${fixture}/scripts/ci"
	cp "${ROOT_DIR}/scripts/ci/affected-crates.sh" "${fixture}/scripts/ci/"
	script=${fixture}/scripts/ci/affected-crates.sh
	cd "${fixture}"
	printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >Cargo.toml
	# leaf <- mid <- (dev) top; leaf <- extra; leaf <- cfg (named fixture-config); alone
	for crate in leaf mid top extra cfg alone; do
		mkdir -p "crates/${crate}/src"
		: >"crates/${crate}/src/lib.rs"
	done
	printf '[package]\nname = "leaf"\nversion = "0.1.0"\nedition = "2021"\n' >crates/leaf/Cargo.toml
	printf '[package]\nname = "mid"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../leaf" }\n' >crates/mid/Cargo.toml
	printf '[package]\nname = "top"\nversion = "0.1.0"\nedition = "2021"\n\n[dev-dependencies]\nmid = { path = "../mid" }\n' >crates/top/Cargo.toml
	printf '[package]\nname = "extra"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../leaf" }\n' >crates/extra/Cargo.toml
	printf '[package]\nname = "fixture-config"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nleaf = { path = "../leaf" }\n' >crates/cfg/Cargo.toml
	printf '[package]\nname = "alone"\nversion = "0.1.0"\nedition = "2021"\n' >crates/alone/Cargo.toml
	cargo generate-lockfile --offline --quiet

	output=$(printf '%s\n' README.md crates/leaf/README.md | bash "${script}")
	grep -qx 'changed_packages=' <<<"${output}"
	grep -qx 'lint_packages=' <<<"${output}"
	grep -qx 'test_packages=' <<<"${output}"
	grep -qx 'fallback=false' <<<"${output}"

	for file in Cargo.toml Cargo.lock rust-toolchain.toml crates/leaf/Cargo.toml; do
		output=$(printf '%s\n' "${file}" | bash "${script}")
		grep -qx 'fallback=true' <<<"${output}"
		grep -qx 'fallback_reason=workspace_manifest' <<<"${output}"
		grep -qx 'test_packages=' <<<"${output}"
	done

	output=$(printf '%s\n' crates/mid/src/lib.rs | bash "${script}")
	grep -qx 'changed_packages=mid' <<<"${output}"
	grep -qx 'lint_packages=mid' <<<"${output}"
	grep -qx 'test_packages=mid top' <<<"${output}"
	grep -qx 'fallback=false' <<<"${output}"

	output=$(printf '%s\n' crates/mid/build.rs | bash "${script}")
	grep -qx 'test_packages=mid top' <<<"${output}"

	output=$(printf '%s\n' crates/alone/src/lib.rs crates/mid/build.rs crates/mid/src/lib.rs crates/top/src/lib.rs | bash "${script}")
	grep -qx 'test_packages=alone mid top' <<<"${output}"

	output=$(printf '%s\n' crates/cfg/src/lib.rs | bash "${script}")
	grep -qx 'changed_packages=fixture-config' <<<"${output}"
	grep -qx 'test_packages=fixture-config' <<<"${output}"

	output=$(printf '%s\n' crates/mid/tests/it.rs | bash "${script}")
	grep -qx 'changed_packages=mid' <<<"${output}"
	grep -qx 'lint_packages=mid' <<<"${output}"
	grep -qx 'test_packages=mid' <<<"${output}"

	output=$(printf '%s\n' crates/alone/src/lib.rs crates/mid/tests/it.rs crates/mid/tests/fixtures/a.toml | bash "${script}")
	grep -qx 'changed_packages=alone mid' <<<"${output}"
	grep -qx 'lint_packages=alone mid' <<<"${output}"
	grep -qx 'test_packages=alone mid' <<<"${output}"

	output=$(printf '%s\n' crates/leaf/src/lib.rs | bash "${script}")
	grep -qx 'changed_packages=leaf' <<<"${output}"
	grep -qx 'fallback=true' <<<"${output}"
	grep -qx 'fallback_reason=wide_reverse_closure' <<<"${output}"

	for file in scripts/tool.rs env/config/local.toml; do
		output=$(printf '%s\n' "${file}" | bash "${script}")
		grep -qx 'fallback=true' <<<"${output}"
		grep -qx 'fallback_reason=outside_crates' <<<"${output}"
	done

	output=$(printf '%s\n' crates/ghost/src/lib.rs | bash "${script}")
	grep -qx 'fallback=true' <<<"${output}"
	grep -qx 'fallback_reason=missing_manifest' <<<"${output}"

	output=$(printf '%s\n' crates/mid/src/lib.rs | CARGO=missing-cargo-for-test bash "${script}")
	grep -qx 'fallback_reason=cargo_unavailable' <<<"${output}"

	printf 'not toml\n' >crates/alone/Cargo.toml
	output=$(printf '%s\n' crates/mid/src/lib.rs | bash "${script}" 2>/dev/null)
	grep -qx 'fallback=true' <<<"${output}"
	grep -qx 'fallback_reason=cargo_tree_error' <<<"${output}"
)

if [[ ${1:-} == --self-test ]]; then
	self_test
	exit
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "${tmp}"' EXIT
files_path=${tmp}/files
cat >"${files_path}"
LC_ALL=C sort -u -o "${files_path}" "${files_path}"

changed_packages=''
lint_packages=''
test_packages=''
fallback=false
fallback_reason=

if [[ ! -s ${files_path} ]]; then
	emit
	exit
fi

changed_list=${tmp}/changed
production_list=${tmp}/production
test_only_list=${tmp}/test-only
: >"${changed_list}"
: >"${production_list}"
: >"${test_only_list}"

while IFS= read -r file; do
	[[ -n ${file} ]] || continue
	case "${file}" in
	Cargo.toml | Cargo.lock | rust-toolchain.toml | crates/*/Cargo.toml) fall_back workspace_manifest ;;
	esac
	case "${file}" in
	crates/*/src/* | crates/*/tests/* | crates/*/*.rs)
		dir=${file#crates/}
		dir=${dir%%/*}
		manifest=crates/${dir}/Cargo.toml
		[[ -f ${manifest} ]] || fall_back missing_manifest
		package=$(package_name "${manifest}")
		[[ -n ${package} ]] || fall_back missing_manifest
		printf '%s\n' "${package}" >>"${changed_list}"
		case "${file}" in
		crates/*/tests/*) printf '%s\n' "${package}" >>"${test_only_list}" ;;
		*) printf '%s\n' "${package}" >>"${production_list}" ;;
		esac
		;;
	*.rs | env/config/*) fall_back outside_crates ;;
	esac
done <"${files_path}"

changed_packages=$(join_sorted "${changed_list}")
if [[ -z ${changed_packages} ]]; then
	emit
	exit
fi
lint_packages=${changed_packages}

cargo_bin=${CARGO:-cargo}
command -v "${cargo_bin}" >/dev/null 2>&1 || fall_back cargo_unavailable

affected_list=${tmp}/affected
: >"${affected_list}"
if [[ -s ${production_list} ]]; then
	invert_args=()
	LC_ALL=C sort -u -o "${production_list}" "${production_list}"
	while IFS= read -r package; do
		invert_args+=(--invert "${package}")
	done <"${production_list}"
	"${cargo_bin}" tree --locked --workspace "${invert_args[@]}" -e normal,build,dev --prefix none |
		awk 'NF { print $1 }' >"${affected_list}" || fall_back cargo_tree_error
fi
cat "${test_only_list}" >>"${affected_list}"

total=$("${cargo_bin}" tree --locked --workspace --depth 0 --prefix none 2>/dev/null | awk 'NF { print $1 }' | LC_ALL=C sort -u | wc -l) || fall_back cargo_tree_error
count=$(LC_ALL=C sort -u "${affected_list}" | grep -c . || true)
if ((total == 0)); then
	fall_back cargo_tree_error
elif ((count >= 50)) || ((count * 10 >= total * 8)); then
	fall_back wide_reverse_closure
fi

test_packages=$(join_sorted "${affected_list}")
emit
