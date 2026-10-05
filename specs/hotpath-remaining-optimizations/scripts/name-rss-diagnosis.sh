#!/usr/bin/env bash
# Bounded existing finding diagnosis, after isolated ordinary build finishes.
set -euo pipefail
cd /root/profiling
for round in 1 2 3; do
  binaries=(candidate noname)
  if [[ $round == 2 ]]; then binaries=(noname candidate); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-name-rss-r$round" --binary "$binary" --workload mixed --rate 8000
  done
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/name-rss-diagnosis-completed.txt
