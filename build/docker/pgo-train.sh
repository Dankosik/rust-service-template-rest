#!/bin/sh
# Requests the instrumented service answers during a profile-guided image
# build (Dockerfile ARG PGO=train). The compiler optimizes what this run
# executes for speed and everything else for size, so add the routes that
# carry this service's traffic and answer without a database or broker.
set -eu
binary=${1:?path to the instrumented service binary}
requests=${PGO_TRAIN_REQUESTS:-20000}
base=http://127.0.0.1:8080

# BuildKit points OTEL_* and TRACEPARENT at its own trace socket inside RUN,
# and the service reads them as an operator's exporter configuration.
for name in $(env | sed -n 's/^\(OTEL_[A-Za-z0-9_]*\)=.*/\1/p') TRACEPARENT; do
	unset "${name}"
done

APP__HTTP__ADDR=127.0.0.1:8080 APP__HTTP__READINESS_PROPAGATION_DELAY=0s "${binary}" >/dev/null &
pid=$!
trap 'kill "${pid}" 2>/dev/null || true' EXIT

attempt=0
until curl -fs -o /dev/null "${base}/health/ready"; do
	attempt=$((attempt + 1))
	if [ "${attempt}" -ge 100 ] || ! kill -0 "${pid}" 2>/dev/null; then
		echo "pgo-train: the instrumented service did not become ready" >&2
		exit 1
	fi
	sleep 0.1
done

send() {
	curl -s -o /dev/null --parallel --parallel-max 16 "$@"
}
send "${base}/health/live?n=[1-${requests}]"
send "${base}/health/ready?n=[1-${requests}]"
send "${base}/pgo-train/unrouted?n=[1-${requests}]"
send "http://127.0.0.1:9090/metrics?n=[1-200]"

# The counters reach disk only when the process returns from main.
trap - EXIT
kill -TERM "${pid}"
wait "${pid}"
