# shellcheck shell=bash

sync_cli_parse() {
	local accepted=" $1 "
	shift

	SYNC_MODE=""
	SYNC_REPO="${PWD}"
	SYNC_HARNESS=""

	while (($# > 0)); do
		case "$1" in
		--repo)
			[[ $# -ge 2 ]] || fail "--repo needs a directory"
			SYNC_REPO="$2"
			shift 2
			;;
		--harness)
			[[ $# -ge 2 ]] || fail "--harness needs a selection"
			[[ -z "${SYNC_HARNESS}" ]] || fail "choose at most one --harness selection"
			case "$2" in
			core | all | codex | claude | qwen | cursor | grok | opencode)
				SYNC_HARNESS="$2"
				;;
			*)
				fail "unsupported --harness selection: $2"
				;;
			esac
			shift 2
			;;
		-h | --help)
			usage
			exit 0
			;;
		--*)
			[[ "${accepted}" == *" ${1#--} "* ]] || fail "unknown argument: $1"
			[[ -z "${SYNC_MODE}" ]] || fail "choose exactly one mode"
			SYNC_MODE="${1#--}"
			shift
			;;
		*)
			fail "unknown argument: $1"
			;;
		esac
	done

	[[ -n "${SYNC_MODE}" ]] || {
		usage >&2
		exit 2
	}

	SYNC_REPO=$(CDPATH='' cd -- "${SYNC_REPO}" 2>/dev/null && pwd) ||
		fail "repository directory not found"
}

sync_cli_selected_harness() {
	if [[ -n "${SYNC_HARNESS}" ]]; then
		printf '%s\n' "${SYNC_HARNESS}"
		return 0
	fi
	python3 "${SYNC_REPO}/scripts/lib/template_state.py" profile \
		--repo "${SYNC_REPO}" --field agent_harness
}

sync_cli_require_harness() {
	local required="$1" selected
	selected=$(sync_cli_selected_harness) ||
		fail "cannot determine selected agent harness"
	[[ "${selected}" == all || "${selected}" == "${required}" ]] ||
		fail "${required} adapter is not selected (selected: ${selected})"
}
