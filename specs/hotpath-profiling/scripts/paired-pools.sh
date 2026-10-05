#!/usr/bin/env bash
set -euo pipefail
cd /root/profiling
for round in 1 2 3; do
  python3 scripts/run.py pool4-paired-r"$round" --workload webhook_mix --rate 2000 --pool 4
  python3 scripts/run.py pool16-paired-r"$round" --workload webhook_mix --rate 2000 --pool 16
done
python3 scripts/summarize.py > evidence/summary.txt
