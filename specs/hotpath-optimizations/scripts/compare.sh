#!/usr/bin/env bash
# Run only after candidate implementation/build; no concurrent builds/tests.
set -euo pipefail
cd /root/profiling
for round in 1 2 3; do
  for binary in profile-baseline profile-candidate; do
    python3 scripts/run.py "$binary-large-r$round" --binary "$binary" --workload webhook_new --rate 100 --body-bytes 65536
  done
done
for binary in profile-baseline profile-candidate; do
  python3 scripts/run.py "$binary-large-count" --binary "$binary" --workload webhook_new --rate 100 --body-bytes 65536 --alloc-metric count
done
for round in 1 2 3; do
  binaries=(baseline candidate)
  if [[ $round == 2 ]]; then binaries=(candidate baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-small-r$round" --binary "$binary" --workload webhook_new --rate 100
    python3 scripts/run.py "$binary-large-r$round" --binary "$binary" --workload webhook_new --rate 100 --body-bytes 65536
    python3 scripts/run.py "$binary-duplicate-r$round" --binary "$binary" --workload webhook_duplicate --rate 2000
    python3 scripts/run.py "$binary-mixed-r$round" --binary "$binary" --workload webhook_mix --rate 800 --pool 4
  done
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/comparison-completed.txt
