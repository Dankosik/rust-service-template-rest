#!/usr/bin/env bash
# Only after consolidated candidate validation; no concurrent heavy work.
set -euo pipefail
cd /root/profiling
test -f /root/remaining/evidence/candidate-ready.txt
for round in 1 2 3; do
  binaries=(profile-baseline profile-candidate)
  if [[ $round == 2 ]]; then binaries=(profile-candidate profile-baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-large-r$round" --binary "$binary" --workload webhook_new \
      --rate 100 --body-bytes 65536 --raw-alloc --fixed-ids
    python3 scripts/run.py "$binary-http-r$round" --binary "$binary" --workload mixed \
      --rate 2000 --raw-alloc --fixed-ids
  done
done
for round in 1 2 3; do
  python3 scripts/run.py "profile-noname-http-r$round" --binary profile-noname \
    --workload mixed --rate 2000 --raw-alloc --fixed-ids
done
for round in 1 2 3; do
  binaries=(baseline candidate)
  if [[ $round == 2 ]]; then binaries=(candidate baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-small-r$round" --binary "$binary" --workload webhook_new --rate 100
    python3 scripts/run.py "$binary-large-r$round" --binary "$binary" --workload webhook_new --rate 100 --body-bytes 65536
    python3 scripts/run.py "$binary-duplicate-r$round" --binary "$binary" --workload webhook_duplicate --rate 2000
    python3 scripts/run.py "$binary-mixed-r$round" --binary "$binary" --workload webhook_mix --rate 800 --pool 4
    python3 scripts/run.py "$binary-http-r$round" --binary "$binary" --workload mixed --rate 8000
  done
done
for round in 1 2 3; do
  pools=(4 8 16)
  if [[ $round == 2 ]]; then pools=(16 8 4); fi
  if [[ $round == 3 ]]; then pools=(8 4 16); fi
  for pool in "${pools[@]}"; do
    python3 scripts/run.py "candidate-pool$pool-r$round" --binary candidate --workload webhook_mix \
      --rate 2000 --pool "$pool"
  done
done
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/campaign-completed.txt
