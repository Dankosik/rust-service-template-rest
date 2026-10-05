#!/usr/bin/env bash
# Preselected bounded follow-up for disjoint small/HTTP peak-RSS ranges.
# All original observations remain; run only after campaign exits.
set -euo pipefail
cd /root/profiling
test -f evidence/campaign-completed.txt
for round in 1 2 3; do
  binaries=(candidate baseline)
  if [[ $round == 2 ]]; then binaries=(baseline candidate); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-small-rss-r$round" --binary "$binary" --workload webhook_new --rate 100
    python3 scripts/run.py "$binary-http-rss-r$round" --binary "$binary" --workload mixed --rate 8000
  done
done
python3 scripts/summarize.py > evidence/summary.txt
python3 scripts/reduce-alloc.py > evidence/allocations.txt
python3 scripts/analyze.py > evidence/analysis.txt
date -u --iso-8601=seconds > evidence/rss-followup-completed.txt
