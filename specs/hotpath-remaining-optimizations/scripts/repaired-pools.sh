#!/usr/bin/env bash
# Confirm selected sizing on the exact final repaired source, same budget.
set -euo pipefail
cd /root/profiling
for round in 1 2 3; do
  pools=(4 8 16)
  if [[ $round == 2 ]]; then pools=(16 8 4); fi
  if [[ $round == 3 ]]; then pools=(8 4 16); fi
  for pool in "${pools[@]}"; do
    python3 scripts/run.py "noname-pool$pool-final-r$round" --binary noname \
      --workload webhook_mix --rate 2000 --pool "$pool"
  done
done
python3 scripts/summarize.py > evidence/summary.txt
python3 scripts/reduce-db.py > evidence/database-summary.txt
date -u --iso-8601=seconds > evidence/repaired-pools-completed.txt
