#!/usr/bin/env bash
# Preselected bounded follow-up for the first mixed peak-RSS separation.
# Run after precision builds/loads; retain all original and follow-up cells.
set -euo pipefail
cd /root/profiling
test -f evidence/precision-completed.txt
for round in 1 2 3; do
  binaries=(candidate baseline)
  if [[ $round == 2 ]]; then binaries=(baseline candidate); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-mixed-rss-r$round" --binary "$binary" \
      --workload webhook_mix --rate 800 --pool 4 --duration 30
  done
done
python3 scripts/summarize.py > evidence/summary.txt
python3 scripts/analyze.py > evidence/analysis.txt
date -u --iso-8601=seconds > evidence/rss-followup-completed.txt
