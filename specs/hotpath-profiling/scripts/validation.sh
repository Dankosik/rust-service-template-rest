#!/usr/bin/env bash
# After profiling. No load generator or profiler runs concurrently with this.
set -euo pipefail
source /root/.cargo/env
cd /root/profiling/source
export CARGO_BUILD_JOBS=6
make build > /root/profiling/evidence/make-build.log 2>&1
make test > /root/profiling/evidence/make-test.log 2>&1
ALLOW_HEAVY=1 make test-integration-db > /root/profiling/evidence/integration-db.log 2>&1
make fmt-check > /root/profiling/evidence/final-fmt.log 2>&1
shellcheck -x /root/profiling/scripts/*.sh > /root/profiling/evidence/shellcheck-final.txt
date -u --iso-8601=seconds > /root/profiling/evidence/validation-completed.txt
