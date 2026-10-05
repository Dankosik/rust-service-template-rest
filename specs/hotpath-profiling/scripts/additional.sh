#!/usr/bin/env bash
set -euo pipefail
cd /root/profiling
python3 scripts/run.py mixed-32k --generator wrk2 --rate 32000
python3 scripts/run.py mixed-48k --generator wrk2 --rate 48000
python3 scripts/run.py diagnostics-control-8k --rate 8000
for round in 1 2 3; do
  python3 scripts/run.py pool4-paired-r"$round" --workload webhook_mix --rate 2000 --pool 4
  python3 scripts/run.py pool16-paired-r"$round" --workload webhook_mix --rate 2000 --pool 16
done
for round in 1 2 3; do
  python3 scripts/run.py size-small-r"$round" --workload webhook_new --rate 100
  python3 scripts/run.py size-large-r"$round" --workload webhook_new --rate 100 --body-bytes 65536
done
python3 scripts/run.py webhook-burst-large --workload webhook_burst --rate 1000 --body-bytes 65536
python3 scripts/run.py webhook-overload512 --workload webhook_mix --rate 4000 --vus 512
taskset -c 3 python3 scripts/otlp_stub.py > evidence/otlp-stub.log 2>&1 &
sink_pid=$!
trap 'kill "$sink_pid" || true' EXIT
for round in 1 2 3; do
  python3 scripts/run.py otlp-default-r"$round" --generator wrk2 --rate 8000 --otlp
  python3 scripts/run.py otlp-always-on-r"$round" --generator wrk2 --rate 8000 --otlp --sampler always_on
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/additional-completed.txt
