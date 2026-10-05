#!/usr/bin/env bash
# Direct original-baseline comparison of the selected repaired HTTP variant.
# Preselect three pairs; do not pool them to hide earlier failed candidates.
set -euo pipefail
cd /root/profiling
test -f evidence/name-rss-diagnosis-completed.txt
for round in 1 2 3; do
  binaries=(baseline noname)
  if [[ $round == 2 ]]; then binaries=(noname baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-http-repaired-r$round" --binary "$binary" --workload mixed --rate 8000
  done
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/repaired-http-completed.txt
