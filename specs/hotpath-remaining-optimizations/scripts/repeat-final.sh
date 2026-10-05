#!/usr/bin/env bash
# Remote-only repeat on baseline + optimization-final.patch, after validation.
# candidate/profile-candidate here contain the retained final source.
set -euo pipefail
cd /root/profiling
test -f /root/remaining/evidence/candidate-ready.txt
for round in 1 2 3; do
  binaries=(profile-baseline profile-candidate)
  if [[ $round == 2 ]]; then binaries=(profile-candidate profile-baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-final-large-r$round" --binary "$binary" \
      --workload webhook_new --rate 100 --body-bytes 65536 --raw-alloc --fixed-ids
    python3 scripts/run.py "$binary-final-http-r$round" --binary "$binary" \
      --workload mixed --rate 2000 --raw-alloc --fixed-ids
  done
done
for round in 1 2 3; do
  binaries=(baseline candidate)
  if [[ $round == 2 ]]; then binaries=(candidate baseline); fi
  for binary in "${binaries[@]}"; do
    python3 scripts/run.py "$binary-final-small-r$round" --binary "$binary" --workload webhook_new --rate 100
    python3 scripts/run.py "$binary-final-large-r$round" --binary "$binary" --workload webhook_new --rate 100 --body-bytes 65536
    python3 scripts/run.py "$binary-final-duplicate-r$round" --binary "$binary" --workload webhook_duplicate --rate 2000
    python3 scripts/run.py "$binary-final-mixed-r$round" --binary "$binary" --workload webhook_mix --rate 800 --pool 4
    python3 scripts/run.py "$binary-final-http-r$round" --binary "$binary" --workload mixed --rate 8000
  done
done
for round in 1 2 3; do
  pools=(4 8 16)
  if [[ $round == 2 ]]; then pools=(16 8 4); fi
  if [[ $round == 3 ]]; then pools=(8 4 16); fi
  for pool in "${pools[@]}"; do
    python3 scripts/run.py "candidate-pool$pool-final-r$round" --binary candidate \
      --workload webhook_mix --rate 2000 --pool "$pool"
  done
done
