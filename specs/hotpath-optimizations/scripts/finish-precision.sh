#!/usr/bin/env bash
# Run remotely after compare.sh exits; preserve completed comparisons.
set -euo pipefail
cd /root/profiling
test -f evidence/comparison-completed.txt
bash scripts/build-raw.sh
for binary in profile-raw-baseline profile-raw-candidate; do
  python3 scripts/run.py "$binary-large-exact" --binary "$binary" --workload webhook_new \
    --rate 100 --body-bytes 65536 --raw-alloc --fixed-ids
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/precision-completed.txt
