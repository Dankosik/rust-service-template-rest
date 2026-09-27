#!/usr/bin/env bash
# Verify the owned wire contract and reproducible committed generation.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${root}"
base=${GRPC_BASE_REF:-${BASE_REF:-origin/main}}
git rev-parse --verify "${base}^{commit}" >/dev/null || {
  echo "grpc-check: comparison base is unavailable: ${base}" >&2
  exit 2
}
[[ -s buf.lock ]] || { echo 'grpc-check: pinned buf.lock is required' >&2; exit 2; }

run_buf() { bash scripts/grpc-generate.sh buf "$@"; }
run_buf format --exit-code --diff
run_buf lint
python3 scripts/tests/grpc-protoc.py

temporary=$(mktemp -d)
trap 'rm -rf -- "${temporary}"' EXIT

# Exercise the selected real tool/configuration on valid input before each
# negative control, so a broken invocation cannot count as a detected defect.
fixture=${temporary}/contract-fixture
mkdir -p "${fixture}/api/proto/probe/v1"
cp buf.yaml buf.lock "${fixture}/"
printf 'syntax="proto3";package probe.v1;message Probe{string value=1;}\n' >"${fixture}/api/proto/probe/v1/probe.proto"
run_buf build "${fixture}" --output "${temporary}/fixture.bin"
if run_buf format "${fixture}" --exit-code --diff >"${temporary}/format.diff"; then
  echo 'grpc-check: formatting accepted the deliberately unformatted fixture' >&2
  exit 1
fi
[[ -s ${temporary}/format.diff ]]
run_buf format "${fixture}" --write
run_buf lint "${fixture}"
cp -R "${fixture}" "${temporary}/breaking-fixture"
cp -R "${fixture}" "${temporary}/lint-fixture"
python3 - "${temporary}" <<'PY'
from pathlib import Path
import sys
root = Path(sys.argv[1])
for directory, replacement in [("breaking-fixture", "renamed_value"), ("lint-fixture", "BadName")]:
    source = root / directory / "api/proto/probe/v1/probe.proto"
    source.write_text(source.read_text().replace("string value = 1", f"string {replacement} = 1"))
PY
run_buf build "${temporary}/breaking-fixture" --output "${temporary}/breaking.bin"
if run_buf breaking "${temporary}/breaking-fixture" --against "${fixture}" --error-format json >"${temporary}/breaking.json"; then
  echo 'grpc-check: FILE compatibility accepted a field rename' >&2
  exit 1
fi
run_buf build "${temporary}/lint-fixture" --output "${temporary}/lint.bin"
if run_buf lint "${temporary}/lint-fixture" --error-format json >"${temporary}/lint.json"; then
  echo 'grpc-check: STANDARD lint accepted an invalid field name' >&2
  exit 1
fi
python3 - "${temporary}" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
for filename, expected in [("breaking.json", "FIELD_SAME_NAME"), ("lint.json", "FIELD_LOWER_SNAKE_CASE")]:
    diagnostics = [json.loads(line) for line in (root / filename).read_text().splitlines() if line]
    if not any(item.get("type") == expected for item in diagnostics):
        raise SystemExit(f"grpc-check: {filename} did not report {expected}")
PY

bash scripts/grpc-generate.sh "${temporary}/first"
bash scripts/grpc-generate.sh "${temporary}/second"
diff -r "${temporary}/first" "${temporary}/second"
cmp crates/grpc-contracts/src/descriptor.bin "${temporary}/first/descriptor.bin"
diff -r crates/grpc-contracts/src/generated "${temporary}/first/generated"

# A valid base without this capability is an explicit first addition. A bad
# reference or unreadable existing contract never falls back to the head.
if [[ -n $(git ls-tree --name-only "${base}" -- api/proto) ]]; then
  mkdir -p "${temporary}/base"
  git archive "${base}" -- api/proto buf.yaml buf.lock | tar -x -C "${temporary}/base"
  [[ -s ${temporary}/base/buf.yaml && -s ${temporary}/base/buf.lock ]] || {
    echo 'grpc-check: existing base contract has no readable Buf configuration/lock' >&2
    exit 2
  }
  run_buf breaking --against "${temporary}/base"
else
  echo "grpc-check: ${base} has no protobuf contract; first addition, compatibility baseline absent"
fi
echo 'grpc-check: format, lint, repeat generation, drift and applicable compatibility passed'
