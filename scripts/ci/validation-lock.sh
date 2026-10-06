#!/usr/bin/env bash
# Run one command under the repository's validation lock, so two CPU-heavy
# validations never overlap (AGENTS.md forbids concurrent heavy validation).
# The lock is a directory under the Git common directory, shared by every
# worktree. Stale ownership requires deliberate, process-confirmed cleanup.
#
#   validation-lock.sh -- command [args...]
#   validation-lock.sh --self-test
set -euo pipefail

if [[ ${1:-} == --self-test ]]; then
	tmp=$(mktemp -d)
	pids=()
	self_test_cleanup() {
		touch "${tmp}/release" "${tmp}/terminated-release" "${tmp}/group-release"
		for pid in ${pids[@]+"${pids[@]}"}; do
			kill -TERM "${pid}" 2>/dev/null || true
		done
		for pid in ${pids[@]+"${pids[@]}"}; do
			wait "${pid}" 2>/dev/null || true
		done
		rm -rf -- "${tmp}"
	}
	trap self_test_cleanup EXIT
	fail() { echo "validation lock self-test: $*" >&2; exit 1; }
	await_file() {
		local deadline=$((SECONDS + 10))
		until [[ -f $1 ]]; do
			((SECONDS < deadline)) || fail "missing fixture event: $1"
			sleep 0.05
		done
	}
	await_log() {
		local deadline=$((SECONDS + $3))
		until grep -Eq -- "$2" "$1" 2>/dev/null; do
			((SECONDS < deadline)) || fail "missing diagnostic: $2"
			sleep 0.05
		done
	}
	expect_status() {
		local actual=0
		wait "$1" || actual=$?
		[[ ${actual} == "$2" ]] || fail "expected status $2, got ${actual}"
	}
	# Inherited outer ownership must not bypass these isolated fixture locks.
	unset VALIDATION_LOCK_HELD
	set -m
	export VALIDATION_LOCK_DIR="${tmp}/lock" FIXTURE_DIR="${tmp}"
	export VALIDATION_LOCK_TIMEOUT_SECONDS=30
	cat >"${tmp}/Makefile" <<'MAKEFILE'
.PHONY: hold
hold:
	@printf 1 >>"$$FIXTURE_DIR/order"; touch "$$FIXTURE_DIR/ready"; \
	while [ ! -f "$$FIXTURE_DIR/release" ]; do sleep 0.05; done; \
	printf 2 >>"$$FIXTURE_DIR/order"
MAKEFILE
	# A real make goal exercises safe command identity; its variable stays private.
	bash "$0" -- make --no-print-directory -f "${tmp}/Makefile" hold PRIVATE=do-not-log-this >"${tmp}/first.log" 2>&1 &
	first_pid=$!
	pids+=("${first_pid}")
	await_file "${tmp}/ready"
	await_log "${tmp}/lock/owner" '^child_pid=[0-9]+$' 3
	grep -q '^make_target=hold$' "${tmp}/lock/owner" || fail "make target missing"
	for field in pid worktree candidate acquired_at executable child_pid; do
		grep -q "^${field}=." "${tmp}/lock/owner" || fail "owner ${field} missing"
	done
	bash "$0" -- sh -c 'printf 3 >>"$1"' -- "${tmp}/order" >"${tmp}/second.log" 2>&1 &
	second_pid=$!
	pids+=("${second_pid}")
	await_log "${tmp}/second.log" 'waiting elapsed=[01]s timeout=30s.*owner_state=readable' 3
	# Keep the actual lock contended through one production progress interval.
	await_log "${tmp}/second.log" 'waiting elapsed=1[01]s timeout=30s.*make_target=hold' 13
	[[ $(<"${tmp}/order") == 1 ]] || fail "contending command ran before acquisition"
	if grep -q 'do-not-log-this' "${tmp}/lock/owner" "${tmp}/first.log" "${tmp}/second.log"; then
		fail "command arguments leaked"
	fi
	touch "${tmp}/release"
	expect_status "${first_pid}" 0
	expect_status "${second_pid}" 0
	[[ $(<"${tmp}/order") == 123 && ! -d ${tmp}/lock ]] || fail "serialization or owner cleanup failed"
	grep -q 'validation lock acquired' "${tmp}/second.log" || fail "acquisition not reported"

	# A nested wrapper inherits custody and returns the command's nonzero result.
	bash "$0" -- bash "$0" -- sh -c 'exit 37' >"${tmp}/nested.log" 2>&1 &
	pids+=("$!")
	expect_status "${pids[${#pids[@]} - 1]}" 37
	[[ ! -d ${tmp}/lock ]] || fail "nested ownership leaked lock"

	# Missing metadata is diagnostic only. An unacquired waiter never cleans it.
	mkdir "${tmp}/lock"
	VALIDATION_LOCK_TIMEOUT_SECONDS=0 bash "$0" -- touch "${tmp}/unexpected" >"${tmp}/timeout.log" 2>&1 &
	pids+=("$!")
	expect_status "${pids[${#pids[@]} - 1]}" 75
	grep -q 'owner_state=missing' "${tmp}/timeout.log" || fail "missing owner not reported"
	grep -q 'validation lock timed out' "${tmp}/timeout.log" || fail "timeout not reported"
	[[ ! -e ${tmp}/unexpected && -d ${tmp}/lock ]] || fail "timeout ran command or released foreign lock"
	for signal in INT TERM; do
		bash "$0" -- touch "${tmp}/unexpected" >"${tmp}/cancel-${signal}.log" 2>&1 &
		waiter_pid=$!
		pids+=("${waiter_pid}")
		await_log "${tmp}/cancel-${signal}.log" 'validation lock waiting' 3
		kill -s "${signal}" "${waiter_pid}"
		if [[ ${signal} == INT ]]; then expected=130; else expected=143; fi
		expect_status "${waiter_pid}" "${expected}"
		grep -q "validation lock cancelled signal=${signal}" "${tmp}/cancel-${signal}.log" || fail "cancellation not reported"
		[[ ! -e ${tmp}/unexpected && -d ${tmp}/lock ]] || fail "cancel ran command or released foreign lock"
	done
	rmdir "${tmp}/lock"

	# Stale wrapper metadata must not let another command overlap a live child.
	rm "${tmp}/ready" "${tmp}/release"
	bash "$0" -- make --no-print-directory -f "${tmp}/Makefile" hold >"${tmp}/stale-owner.log" 2>&1 &
	holder_pid=$!
	pids+=("${holder_pid}")
	await_file "${tmp}/ready"
	bash -c 'exit 0' &
	dead_pid=$!
	wait "${dead_pid}"
	printf 'pid=%s\ncommand=legacy-secret-must-not-print\n' "${dead_pid}" >"${tmp}/lock/owner"
	VALIDATION_LOCK_TIMEOUT_SECONDS=0 bash "$0" -- touch "${tmp}/unexpected" >"${tmp}/stale.log" 2>&1 &
	pids+=("$!")
	expect_status "${pids[${#pids[@]} - 1]}" 75
	grep -q 'owner_state=stale-or-unreachable.*automatic_reclaim=refused' "${tmp}/stale.log" || fail "stale owner not refused"
	if grep -q 'legacy-secret' "${tmp}/stale.log"; then fail "legacy argv leaked"; fi
	[[ ! -e ${tmp}/unexpected && -f ${tmp}/lock/owner ]] || fail "stale owner was reclaimed"
	kill -0 "${holder_pid}" || fail "stale diagnostic killed owner"
	touch "${tmp}/release"
	expect_status "${holder_pid}" 0

	# A command may need time to finish after TERM; its result and lock survive.
	cat >"${tmp}/slow-stop.sh" <<'CHILD'
trap 'touch "$FIXTURE_DIR/terminating"; while [ ! -f "$FIXTURE_DIR/terminated-release" ]; do sleep 0.05; done; exit 37' TERM
touch "$FIXTURE_DIR/child-ready"
while true; do sleep 0.05; done
CHILD
	bash "$0" -- bash "${tmp}/slow-stop.sh" >"${tmp}/custody.log" 2>&1 &
	holder_pid=$!
	pids+=("${holder_pid}")
	await_file "${tmp}/child-ready"
	kill -TERM "${holder_pid}"
	await_file "${tmp}/terminating"
	kill -0 "${holder_pid}" || fail "wrapper exited before child"
	VALIDATION_LOCK_TIMEOUT_SECONDS=0 bash "$0" -- touch "${tmp}/unexpected" >"${tmp}/custody-wait.log" 2>&1 &
	pids+=("$!")
	expect_status "${pids[${#pids[@]} - 1]}" 75
	[[ ! -e ${tmp}/unexpected && -f ${tmp}/lock/owner ]] || fail "cancelled owner released live child"
	touch "${tmp}/terminated-release"
	expect_status "${holder_pid}" 37
	[[ ! -d ${tmp}/lock ]] || fail "terminated child left lock behind"
	# The direct command may exit while ordinary work still shares its job group.
	cat >"${tmp}/group-child.sh" <<'GROUP_CHILD'
trap 'touch "$FIXTURE_DIR/group-terminating"' TERM
touch "$FIXTURE_DIR/group-ready"
while [ ! -f "$FIXTURE_DIR/group-release" ]; do sleep 0.05; done
GROUP_CHILD
	cat >"${tmp}/group-parent.sh" <<'GROUP_PARENT'
bash "$FIXTURE_DIR/group-child.sh" &
while [ ! -f "$FIXTURE_DIR/group-ready" ]; do sleep 0.05; done
exit 23
GROUP_PARENT
	bash "$0" -- bash "${tmp}/group-parent.sh" >"${tmp}/group.log" 2>&1 &
	holder_pid=$!
	pids+=("${holder_pid}")
	await_file "${tmp}/group-ready"
	await_log "${tmp}/lock/owner" '^child_pid=[0-9]+$' 3
	command_pid=$(awk -F= '$1 == "child_pid" { print $2 }' "${tmp}/lock/owner")
	deadline=$((SECONDS + 10))
	while kill -0 "${command_pid}" 2>/dev/null; do
		((SECONDS < deadline)) || fail "direct fixture command did not exit"
		sleep 0.05
	done
	kill -0 "${holder_pid}" || fail "wrapper released ordinary group work"
	kill -TERM "${holder_pid}"
	await_file "${tmp}/group-terminating"
	VALIDATION_LOCK_TIMEOUT_SECONDS=0 bash "$0" -- touch "${tmp}/unexpected" >"${tmp}/group-wait.log" 2>&1 &
	pids+=("$!")
	expect_status "${pids[${#pids[@]} - 1]}" 75
	[[ ! -e ${tmp}/unexpected && -f ${tmp}/lock/owner ]] || fail "ordinary group work lost custody"
	touch "${tmp}/group-release"
	expect_status "${holder_pid}" 23
	[[ ! -d ${tmp}/lock ]] || fail "finished group left lock behind"
	echo "validation lock self-test passed"
	exit
fi

if [[ ${1:-} != -- || $# -lt 2 ]]; then
	echo "usage: $0 -- command [args...]" >&2
	exit 2
fi
shift

if [[ ${VALIDATION_LOCK_HELD:-} == 1 ]]; then
	exec "$@"
fi

root=$(git rev-parse --show-toplevel)
common_dir=$(git rev-parse --git-common-dir)
[[ ${common_dir} == /* ]] || common_dir=${root}/${common_dir}
lock_dir=${VALIDATION_LOCK_DIR:-${common_dir}/codex/validation.lock}
owner_file=${lock_dir}/owner
timeout=${VALIDATION_LOCK_TIMEOUT_SECONDS:-900}
if [[ ! ${timeout} =~ ^[0-9]+$ || ${#timeout} -gt 9 ]]; then
	echo "validation lock timeout must be a non-negative integer (at most 9 digits)" >&2
	exit 2
fi
timeout=$((10#${timeout}))
started=${SECONDS}
next_report=0
acquired=0
child_pid=
signal_count=0
cancel_signal=
cancel_status=0

sanitize() {
	LC_ALL=C printf '%s' "${1:0:512}" | LC_ALL=C tr -c '[:print:]' '?'
}

owner_diagnostic() {
	local owner_pid= key value fields= receipt
	if ! receipt=$(cat "${owner_file}" 2>/dev/null); then
		printf 'owner_state=missing\n' >&2
		return
	fi
	# Never print an old receipt's full command or any unrecognized field.
	while IFS='=' read -r key value; do
		case ${key} in
		pid) owner_pid=${value} ;;
		worktree | candidate | acquired_at | executable | make_target | child_pid) ;;
		*) continue ;;
		esac
		printf -v fields '%s %s=%q' "${fields}" "${key}" "$(sanitize "${value}")"
	done <<<"${receipt}"
	if [[ ! ${owner_pid} =~ ^[1-9][0-9]*$ ]]; then
		printf 'owner_state=missing-pid%s\n' "${fields}" >&2
	elif kill -0 "${owner_pid}" 2>/dev/null; then
		printf 'owner_state=readable%s\n' "${fields}" >&2
	else
		printf 'owner_state=stale-or-unreachable%s automatic_reclaim=refused\n' "${fields}" >&2
	fi
}

on_signal() {
	cancel_signal=$1
	cancel_status=$2
	signal_count=$((signal_count + 1))
	if [[ -n ${child_pid} ]]; then
		printf 'validation lock cancelled signal=%s child_pid=%s; retaining lock until child and group exit\n' "${cancel_signal}" "${child_pid}" >&2
		kill -s "${cancel_signal}" -- "-${child_pid}" 2>/dev/null || true
	fi
}

cancel_before_command() {
	if [[ -n ${cancel_signal} ]]; then
		printf 'validation lock cancelled signal=%s elapsed=%ss timeout=%ss acquired=%s; command not started\n' "${cancel_signal}" "$((SECONDS - started))" "${timeout}" "${acquired}" >&2
		exit "${cancel_status}"
	fi
}

cleanup() {
	# A killed wrapper leaves its directory behind. A waiter cannot prove the
	# child is gone from a dead wrapper PID, so only this owner releases it.
	if ((acquired)) && [[ -z ${child_pid} ]]; then
		rm -f "${owner_file}"
		rmdir "${lock_dir}" 2>/dev/null || true
	fi
}
trap cleanup EXIT
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM
trap 'on_signal HUP 129' HUP
mkdir -p "$(dirname "${lock_dir}")"

while true; do
	cancel_before_command
	if mkdir "${lock_dir}" 2>/dev/null; then
		acquired=1
		break
	fi
	cancel_before_command
	elapsed=$((SECONDS - started))
	if ((elapsed >= next_report)); then
		printf 'validation lock waiting elapsed=%ss timeout=%ss lock=%q ' "${elapsed}" "${timeout}" "${lock_dir}" >&2
		owner_diagnostic
		next_report=$((elapsed + 10))
	fi
	if ((elapsed >= timeout)); then
		printf 'validation lock timed out elapsed=%ss timeout=%ss; command not started\n' "${elapsed}" "${timeout}" >&2
		exit 75
	fi
	sleep 1
done
cancel_before_command

executable=${1##*/}
make_target=
if [[ ${executable} == make || ${executable} == gmake ]]; then
	skip_argument=0
	for argument in "${@:2}"; do
		if ((skip_argument)); then
			skip_argument=0
			continue
		fi
		case ${argument} in
		-C | --directory | -f | --file | --makefile | -I | --include-dir | -o | --old-file | --assume-old | -W | --what-if | --new-file | --assume-new | -E | --eval)
			skip_argument=1
			;;
		-* | *=*) ;;
		*)
			# Only the first explicit make goal, never assignments or script text.
			if [[ ${argument} =~ ^[a-zA-Z_][a-zA-Z0-9_.:/-]*$ ]]; then
				make_target=${argument}
				break
			fi
			;;
		esac
	done
fi
{
	printf 'pid=%s\n' "$$"
	printf 'worktree=%s\n' "$(sanitize "${root}")"
	printf 'candidate=%s\n' "$(git rev-parse HEAD)"
	printf 'acquired_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
	printf 'executable=%s\n' "$(sanitize "${executable}")"
	[[ -z ${make_target} ]] || printf 'make_target=%s\n' "$(sanitize "${make_target}")"
} >"${owner_file}"
printf 'validation lock acquired elapsed=%ss timeout=%ss lock=%q ' "$((SECONDS - started))" "${timeout}" "${lock_dir}" >&2
owner_diagnostic
cancel_before_command

export VALIDATION_LOCK_HELD=1
# A separate job group receives cancellation without signalling this wrapper.
# Monitor mode also prevents an asynchronous child from inheriting ignored INT.
set -m
"$@" &
child_pid=$!
set +m
printf 'child_pid=%s\n' "${child_pid}" >>"${owner_file}"
# Close the signal/launch gap: a signal before $! was recorded is forwarded now.
if [[ -n ${cancel_signal} ]]; then
	kill -s "${cancel_signal}" -- "-${child_pid}" 2>/dev/null || true
fi
while true; do
	wait_generation=${signal_count}
	if wait "${child_pid}"; then
		child_status=0
	else
		child_status=$?
	fi
	# A trapped signal interrupts wait before the child terminates. Wait again,
	# including when it exited during the trap, to recover its actual status.
	if ((wait_generation == signal_count)); then
		break
	fi
done
# A shell or make may finish before ordinary children in its group. Keep the
# lock through those children too. Reaped-or-orphaned zombies do no work and
# cannot be waited for by this shell; ps only distinguishes them from live work.
group_has_live_work() {
	local processes
	kill -0 -- "-${child_pid}" 2>/dev/null || return 1
	processes=$(ps -eo pgid=,stat=) || return 0
	awk -v group="${child_pid}" '$1 == group && $2 !~ /^[ZX]/ { live = 1 } END { exit !live }' <<<"${processes}"
}
while group_has_live_work; do
	sleep 0.1
done
child_pid=
exit "${child_status}"
