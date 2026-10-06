#!/usr/bin/env bash
# Fixed, opt-in sustained PostgreSQL experiment. Native Rust owns workload and
# result arithmetic; this entry owns source/executable and Compose custody.
set -euo pipefail
export LC_ALL=C

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "${ROOT_DIR}"
if [[ ${ALLOW_HEAVY:-} != 1 ]]; then
	echo "refusing sustained PostgreSQL: explicit ALLOW_HEAVY=1 is required" >&2
	exit 2
fi
if ! bash scripts/ci/validation-lock.sh --assert-held; then
	exec bash scripts/ci/validation-lock.sh -- bash scripts/postgres-sustained.sh "$@"
fi
# shellcheck source=scripts/lib/compose-postgres.sh
source scripts/lib/compose-postgres.sh

output=
case $# in
0) output="${ROOT_DIR}/specs/postgres-sustained-operation/evidence/measurement/run-$(date -u +%Y%m%dT%H%M%SZ)-$$" ;;
2) [[ $1 == --output ]] && output=$2 ;;
*) ;;
esac
[[ -n ${output} && ! -e ${output} ]] || { echo "usage: postgres-sustained.sh [--output NEW_DIRECTORY]" >&2; exit 2; }
[[ ${POSTGRES_SUSTAINED_Q_COMMIT:-} =~ ^[0-9a-f]{40}$ ]] || {
	echo "POSTGRES_SUSTAINED_Q_COMMIT must name the exact accepted integrated Q commit" >&2
	exit 2
}
git merge-base --is-ancestor "${POSTGRES_SUSTAINED_Q_COMMIT}" HEAD || {
	echo "the supplied Q commit is not integrated in this candidate" >&2; exit 2;
}
git diff --quiet HEAD -- || { echo "freeze the tested source in a commit before release variants" >&2; exit 2; }
git ls-files --error-unmatch scripts/postgres-sustained.sh >/dev/null || { echo "laboratory entry is not part of the committed source" >&2; exit 2; }
require_docker
docker_endpoint=${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}')}
[[ ${docker_endpoint} == unix://* ]] || { echo "the laboratory requires the admitted local/CI Unix Docker daemon" >&2; exit 2; }
for command in cargo rustc python3 git ps; do command -v "${command}" >/dev/null || { echo "missing ${command}" >&2; exit 2; }; done
mkdir -p "${output}"
output=$(cd "${output}" && pwd -P)
common=$(git rev-parse --git-common-dir)
common=$(cd "${common}" && pwd -P)
build_root=$(mktemp -d "${common}/postgres-sustained-build.XXXXXXXX")
mkdir -p "${output}/"{attempts,control,reports,replay}
executables="${build_root}/executables"
mkdir -p "${executables}"
source_copy="${build_root}/source"
driver=
collector=
target_created_ms=0
container_id=
daemon_id=
target_identity=
campaign_status=failed
export POSTGRES_SUSTAINED_EVIDENCE="${output}/attempts"
export POSTGRES_SUSTAINED_RESOURCE_SAMPLE="${output}/control/resources.json"

json_get() {
	python3 - "$1" "$2" <<'PY'
import json,sys
value=json.load(open(sys.argv[1]))
for key in sys.argv[2].split('.'):
    value=value[int(key)] if isinstance(value,list) else value[key]
print(json.dumps(value,separators=(',',':')) if isinstance(value,(dict,list,bool)) or value is None else value)
PY
}

now_ms() { python3 -c 'import time; print(time.time_ns()//1_000_000)'; }
free_bytes() { python3 - "$output" <<'PY'
import shutil,sys
print(shutil.disk_usage(sys.argv[1]).free)
PY
}
admit_step() {
	local seconds=$1 now
	now=$(now_ms)
	if ((target_created_ms && (now < target_created_ms || now + (seconds+1200)*1000 > target_created_ms+16200000))); then
		echo "remaining 4h30 envelope cannot fit step and 20-minute cleanup reserve" >&2
		return 1
	fi
}
stop_arrivals() {
	python3 - "${output}/attempts" <<'PY'
from pathlib import Path
import sys
for ready in Path(sys.argv[1]).rglob('*.ready'):
    (ready.parent/'stop').write_text('entry stop\n')
PY
	if [[ -n ${driver} ]]; then
		local second
		for ((second=0; second<15; second++)); do kill -0 "${driver}" 2>/dev/null || break; sleep 1; done
		kill -TERM "${driver}" 2>/dev/null || true
		wait "${driver}" 2>/dev/null || true
		driver=
	fi
	if [[ -n ${collector} ]]; then kill -TERM "${collector}" 2>/dev/null || true; wait "${collector}" 2>/dev/null || true; collector=; fi
}
export_evidence() {
	python3 - "${output}" "${campaign_status}" "${target_created_ms}" <<'PY'
from pathlib import Path
import hashlib,json,os,sys,time
root=Path(sys.argv[1]); files=[]; total=0
for path in sorted(root.rglob('*')):
    if path.is_symlink(): raise SystemExit('evidence contains a symlink')
    if not path.is_file() or path.name=='export.json': continue
    digest=hashlib.sha256()
    with path.open('rb') as stream:
        while chunk:=stream.read(1024*1024): digest.update(chunk)
        os.fsync(stream.fileno())
    size=path.stat().st_size; total+=size
    files.append({'path':str(path.relative_to(root)),'bytes':size,'sha256':digest.hexdigest()})
if total>2*1024**3: raise SystemExit('2 GiB campaign evidence bound exceeded')
with (root/'export.json').open('w') as stream:
    json.dump({'status':sys.argv[2],'target_created_unix_ms':int(sys.argv[3]),'exported_unix_ms':time.time_ns()//1_000_000,'bytes':total,'files':files},stream,indent=2)
    stream.write('\n'); stream.flush(); os.fsync(stream.fileno())
descriptor=os.open(root,os.O_RDONLY); os.fsync(descriptor); os.close(descriptor)
PY
}
cleanup() {
	local status=$? absence=unknown ids volumes networks
	trap - EXIT INT TERM
	stop_arrivals
	# Preserve every attempt before deleting only this task's disposable target.
	export_evidence || status=1
	if compose_postgres_down; then
		if [[ -z ${COMPOSE_PROJECT:-} ]] || {
			ids=$(docker ps -aq --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}") &&
			volumes=$(docker volume ls -q --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}") &&
			networks=$(docker network ls -q --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}") &&
			[[ -z ${ids} && -z ${volumes} && -z ${networks} ]];
		}; then absence=verified; else status=1; fi
	else status=1; fi
	python3 - "${output}/resource-absence.json" "${absence}" "${COMPOSE_PROJECT:-}" <<'PY'
import json,os,sys,time
with open(sys.argv[1],'w') as stream:
    json.dump({'status':sys.argv[2],'compose_project':sys.argv[3],'observed_unix_ms':time.time_ns()//1_000_000},stream)
    stream.write('\n'); stream.flush(); os.fsync(stream.fileno())
PY
	if [[ ${absence} != verified ]]; then
		echo "task resource absence is unverified; retain the registered files for Q recovery" >&2
	fi
	if ((target_created_ms && $(now_ms)-target_created_ms > 16200000)); then
		echo "cumulative laboratory envelope exceeded before teardown completed" >&2
		status=1
	fi
	export_evidence || status=1
	# Build outputs and the source copy remain task-owned at this exact path;
	# this entry never prunes shared caches or deletes an unknown resource.
	printf 'sustained PostgreSQL status=%s evidence=%s build_source=%s\n' "${status}" "${output}" "${build_root}"
	exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Freeze the actual assembled source, including newly authored tracked inputs.
# An interrupted copy or concurrent writer fails the snapshot; no live checkout
# is patched for policy builds.
python3 - "${ROOT_DIR}" "${source_copy}" "${output}" <<'PY'
from pathlib import Path
import hashlib,json,shutil,subprocess,sys
root,dest,out=map(Path,sys.argv[1:]); dest.mkdir()
names=subprocess.check_output(['git','ls-files','--cached','-z'],cwd=root).decode().split('\0')
files=[]
for name in sorted(set(filter(None,names))):
    path=root/name
    if path.is_relative_to(out) or name.startswith('specs/'): continue
    if not path.is_file(): continue
    data=path.read_bytes(); target=dest/name; target.parent.mkdir(parents=True,exist_ok=True); target.write_bytes(data); shutil.copymode(path,target)
    files.append({'path':name,'sha256':hashlib.sha256(data).hexdigest()})
for item in files:
    if hashlib.sha256((root/item['path']).read_bytes()).hexdigest()!=item['sha256']: raise SystemExit('source changed while freezing')
replay=root/'specs/postgres-sustained-operation/evidence/measurement/replay'
for path in replay.iterdir():
    if path.is_file(): shutil.copy2(path,out/'replay'/path.name)
digest=hashlib.sha256(json.dumps(files,separators=(',',':'),sort_keys=True).encode()).hexdigest()
if subprocess.run(['git','diff','--quiet','HEAD','--'],cwd=root).returncode: raise SystemExit('committed source changed while freezing')
(out/'source.json').write_text(json.dumps({'tree_hash':digest,'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'git_tree':subprocess.check_output(['git','rev-parse','HEAD^{tree}'],cwd=root,text=True).strip(),'files':files},indent=2)+'\n')
PY
source_hash=$(json_get "${output}/source.json" tree_hash)
foundation=$(json_get "${output}/replay/manifest.json" foundation)
toolchain=$(rustc --version)
export CARGO_TARGET_DIR="${build_root}/target"
# Keep patch lookup inside the disposable source copy, never its parent Git
# checkout. This repository has no commits and creates no managed worktree.
git init -q "${source_copy}"
for policy in P0 P1 P2 P3 foundation; do
	patch=
	if [[ ${policy} == foundation ]]; then patch="foundation-instrumentation.patch"; elif [[ ${policy} != P0 ]]; then patch="${policy}.patch"; fi
	if [[ -n ${patch} ]]; then (cd "${source_copy}" && git apply "${output}/replay/${patch}"); fi
	python3 - "${source_copy}" "${output}/source.json" "${executables}/${policy}.source.json" "${output}/replay/${patch}" <<'PY'
from pathlib import Path
import hashlib,json,sys
root=Path(sys.argv[1]); base=json.load(open(sys.argv[2])); entries=[]
for item in base['files']: entries.append({'path':item['path'],'sha256':hashlib.sha256((root/item['path']).read_bytes()).hexdigest()})
patch=Path(sys.argv[4]); patch_bytes=patch.read_bytes() if patch.is_file() else b''
Path(sys.argv[3]).write_text(json.dumps({'source_tree_hash':hashlib.sha256(json.dumps(entries,sort_keys=True,separators=(',',':')).encode()).hexdigest(),'policy_patch_sha256':hashlib.sha256(patch_bytes).hexdigest()})+'\n')
PY
	(
		cd "${source_copy}"
		cargo test --release --no-run --locked -p integration-tests --features integration --test postgres_sustained --message-format=json >"${build_root}/${policy}.jsonl"
	)
	python3 - "${build_root}/${policy}.jsonl" "${executables}/${policy}" <<'PY'
import json,shutil,sys
paths=[]
for line in open(sys.argv[1]):
    event=json.loads(line)
    if event.get('reason')=='compiler-artifact' and event.get('target',{}).get('name')=='postgres_sustained' and event.get('executable'): paths.append(event['executable'])
if len(set(paths))!=1: raise SystemExit('release measurement executable identity is ambiguous')
shutil.copy2(paths[0],sys.argv[2])
PY
	if [[ -n ${patch} ]]; then (cd "${source_copy}" && git apply -R "${output}/replay/${patch}"); fi
done
(
	cd "${source_copy}"
	cargo build --release --locked -p migrate --bin migrate
)
cp "${CARGO_TARGET_DIR}/release/migrate" "${executables}/migrate"
cp "${executables}/"*.source.json "${output}/replay/"
python3 - "${source_copy}" "${output}/source.json" <<'PY'
from pathlib import Path
import hashlib,json,sys
root=Path(sys.argv[1]); source=json.load(open(sys.argv[2]))
for item in source['files']:
    if hashlib.sha256((root/item['path']).read_bytes()).hexdigest()!=item['sha256']: raise SystemExit('policy replay did not restore frozen source')
PY
preflight_free=$(free_bytes)
((preflight_free >= 35*1024*1024*1024)) || { echo "35 GiB free is required after release builds" >&2; exit 1; }
docker info --format '{{json .}}' | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["NCPU"]>=2 and d["MemTotal"]>=4*1024**3, "insufficient observed daemon capacity"'

cat >"${output}/control/compose.override.yml" <<'YAML'
services:
  postgres:
    cpus: 2.0
    mem_limit: 1g
    memswap_limit: 1g
    shm_size: 512m
    command: [postgres, -c, shared_buffers=512MB]
    volumes:
      - sustained-data:/var/lib/postgresql
volumes:
  sustained-data:
YAML
target_created_ms=$(now_ms)
compose_postgres_up sustained-postgres --override-file "${output}/control/compose.override.yml"
container_id=$(compose_postgres ps --quiet postgres)
[[ ${container_id} =~ ^[0-9a-f]{64}$ ]] || { echo "container identity missing" >&2; exit 1; }
daemon_id=$(docker info --format '{{.ID}}')
target_identity="${daemon_id}/${container_id}"
image=$(docker inspect --format '{{.Config.Image}}' "${container_id}")
[[ ${image} == 'postgres:18@sha256:4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280' ]] || { echo "laboratory image differs from accepted input" >&2; exit 1; }
docker inspect --format '{{json .HostConfig}}' "${container_id}" | python3 -c 'import json,sys; h=json.load(sys.stdin); assert h["Memory"]==1073741824 and h["MemorySwap"]==1073741824 and h["NanoCpus"]==2000000000, "container envelope mismatch"'

psql_control() { docker exec -i -e "PGOPTIONS=-c statement_timeout=60000 -c lock_timeout=5000" "${container_id}" psql -X -v ON_ERROR_STOP=1 -U app -d "${1}" -At; }
database_dsn() { printf 'postgres://app:app@127.0.0.1:%s/%s?sslmode=disable' "${POSTGRES_HOST_PORT}" "$1"; }
database_create() {
	local name=$1 template=${2:-template0}
	[[ ${name} =~ ^sustained_[a-z_0-9]+$ && ${template} =~ ^(template0|sustained_[a-z_0-9]+)$ ]] || return 2
	admit_step 60
	printf 'CREATE DATABASE "%s" TEMPLATE "%s";\n' "${name}" "${template}" | psql_control postgres >/dev/null
}
database_drop() {
	[[ $1 =~ ^sustained_[a-z_0-9]+$ ]] || return 2
	printf 'DROP DATABASE "%s";\n' "$1" | psql_control postgres >/dev/null
}

# The remaining lifecycle functions below use only the one registered target.
effective_readback() {
	# The native table owner supplies the exact query. Keep its interpretation
	# identical between pre-clone admission and the live controller samples.
	python3 - "${source_copy}/test/tests/postgres_sustained/workload.rs" <<'PYSQL' | psql_control "$1"
from pathlib import Path
import re,sys
source=Path(sys.argv[1]).read_text()
matched=re.findall(r'pub\(super\) const DATABASE_CONFIG_SQL: &str = r#"(.*?)"#;',source,re.S)
if len(matched)!=1: raise SystemExit('native database config authority is ambiguous')
print(matched[0]+';')
PYSQL
}

resource_readback() {
	local active_driver=${1:-0}
	[[ $(docker info --format '{{.ID}}') == "${daemon_id}" ]] || return 1
	docker inspect --format '{{json .}}' "${container_id}" >"${output}/control/container.json.tmp" || return
	docker exec "${container_id}" sh -c '
		cat /sys/fs/cgroup/memory.current
		cat /sys/fs/cgroup/memory.events
		cat /sys/fs/cgroup/cpu.stat
		cat /sys/fs/cgroup/io.stat
		du -sb /var/lib/postgresql
	' >"${output}/control/container-stat.txt.tmp" || return
	docker ps -aq --no-trunc --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" >"${output}/control/project-containers.tmp" || return
	ps -axo pid=,ppid=,rss=,time= >"${output}/control/processes.txt.tmp" || return
	python3 - "${output}" "${target_identity}" "${container_id}" "${active_driver}" "$$" <<'PY'
from pathlib import Path
import json,os,resource,shutil,sys,time
root=Path(sys.argv[1]); control=root/'control'; now=time.time_ns()//1_000_000
container=json.loads((control/'container.json.tmp').read_text()); host=container['HostConfig']
if container['Id']!=sys.argv[3] or not container['State']['Running']: raise SystemExit('target identity or running state lost')
if (host['Memory'],host['MemorySwap'],host['NanoCpus'])!=(1024**3,1024**3,2_000_000_000): raise SystemExit('target resources changed')
lines=(control/'container-stat.txt.tmp').read_text().splitlines(); memory=int(lines[0]); oom=0; cpu=0; reads=0; database=0
for line in lines[1:]:
    parts=line.split()
    if parts[0] in ('oom','oom_kill'): oom+=int(parts[1])
    elif parts[0]=='usage_usec': cpu=int(parts[1])
    elif '/' in line and parts[0].isdigit(): database=int(parts[0])
    elif ':' in parts[0]: reads+=sum(int(word.split('=')[1]) for word in parts[1:] if word.startswith('rbytes='))
def seconds(raw):
    days=0
    if '-' in raw: day,raw=raw.split('-',1); days=int(day)
    value=0.
    for part in raw.split(':'): value=value*60+float(part)
    return value+days*86400
processes={}
for line in (control/'processes.txt.tmp').read_text().splitlines():
    parts=line.split()
    if len(parts)==4: processes[int(parts[0])]={'ppid':int(parts[1]),'rss':int(parts[2])*1024,'cpu':seconds(parts[3])}
def descendants(root):
    selected={root} if root in processes else set()
    while True:
        children={pid for pid,row in processes.items() if row['ppid'] in selected}
        if children.issubset(selected): return selected
        selected|=children
selected=descendants(int(sys.argv[4])); task_processes=descendants(int(sys.argv[5]))
rss=sum(processes[pid]['rss'] for pid in selected)
task_rss=sum(processes[pid]['rss'] for pid in task_processes)+resource.getrusage(resource.RUSAGE_SELF).ru_maxrss*(1 if sys.platform=='darwin' else 1024)
previous_path=control/'resource-clock.json'
previous=json.loads(previous_path.read_text()) if previous_path.exists() else None
mono=time.monotonic(); cores=max(1,(os.cpu_count() or 1)-2); cpu_delta=0.; pg_fraction=0.; fraction=0.; clock_valid=True
if previous:
    elapsed=mono-previous['mono']
    clock_valid=elapsed>0 and abs((now-previous['wall'])/1000-elapsed)<=5
    for pid in selected:
        prior=previous['process_cpu'].get(str(pid),0.)
        cpu_delta+=max(0.,processes[pid]['cpu']-prior)
    fraction=cpu_delta/elapsed/cores if elapsed>0 else 1.
    pg_fraction=max(0,cpu-previous['pg_cpu'])/1e6/elapsed if elapsed>0 else 2.
state={'mono':mono,'wall':now,'pg_cpu':cpu,'process_cpu':{str(pid):processes[pid]['cpu'] for pid in selected}}
previous_path.write_text(json.dumps(state))
evidence=sum(path.stat().st_size for path in root.rglob('*') if path.is_file())
manifest_path=control/'current-manifest.json'
inputs=json.loads(manifest_path.read_text())['effective_inputs'] if manifest_path.exists() else {'inputs_hash':'pre-target'}
load=os.getloadavg()[0]; host_cores=os.cpu_count() or 1
sample={'target_identity':sys.argv[2],'observed_unix_ms':now,'free_disk_bytes':shutil.disk_usage(root).free,'database_bytes':database,'evidence_bytes':evidence,'total_task_memory_bytes':memory+task_rss,'application_rss_bytes':rss,'container_block_read_bytes':reads,'driver_cpu_fraction':fraction,'oom':bool(oom or container['State'].get('OOMKilled')),'unexpected_resources':(control/'project-containers.tmp').read_text().split()!=[sys.argv[3]],'inputs_hash':inputs['inputs_hash'],'uncontended_host':load<host_cores,'clock_valid':clock_valid}
temporary=control/'resources.json.tmp'; temporary.write_text(json.dumps(sample)+'\n'); temporary.replace(control/'resources.json')
with (control/'resource-readbacks.jsonl').open('a') as stream: stream.write(json.dumps({'observed_unix_ms':now,'container_cpu_cores':pg_fraction,'driver_cpu_capacity_cores':cores,'host_load1':load,'host_cpus':host_cores,'sample':sample})+'\n')
if sample['oom'] or sample['unexpected_resources'] or not sample['clock_valid'] or sample['free_disk_bytes']<12*1024**3 or database>20*1024**3 or evidence>2*1024**3 or rss>2*1024**3: raise SystemExit('laboratory resource stop')
PY
}

collect_resources() {
	local active_driver=$1 status=0
	while kill -0 "${active_driver}" 2>/dev/null; do
		if ! admit_step 0 || ! resource_readback "${active_driver}"; then
			python3 - "${output}/attempts" <<'PY'
from pathlib import Path
import sys
for ready in Path(sys.argv[1]).rglob('*.ready'): (ready.parent/'stop').write_text('resource stop\n')
PY
			status=1
			break
		fi
		sleep 5
	done
	return "${status}"
}

write_manifest() {
	local regime=$1 policy=$2 repeat=$3 attempt=$4 counts=$5 instrumentation=$6
	python3 - "${output}" "${regime}" "${policy}" "${repeat}" "${attempt}" "${counts}" "${instrumentation}" "${target_identity}" "${image}" "${toolchain}" "${preflight_free}" "${target_created_ms}" "${foundation}" "${POSTGRES_SUSTAINED_Q_COMMIT}" "${executables}" <<'PY'
from pathlib import Path
import hashlib,json,sys
out=Path(sys.argv[1]); regime,policy,repeat,attempt=sys.argv[2:6]; repeat=int(repeat)
counts=json.load(open(sys.argv[6])); executable='foundation' if sys.argv[7]=='foundation' else policy
exe_dir=Path(sys.argv[15]); identity=json.loads((exe_dir/f'{executable}.source.json').read_text())
source=json.loads((out/'source.json').read_text())
native=[item for item in source['files'] if item['path'].startswith('test/tests/postgres_sustained/')]
database=json.loads((out/'control'/'database-inputs.json').read_text())
fixed={'pg_memory_limit_bytes':1024**3,'shared_buffers_bytes':512*1024**2,'postgres_cpu_count':2,'database_config':database,'workload_hash':hashlib.sha256(json.dumps(native,sort_keys=True).encode()).hexdigest(),'inventory_seed':41001,'inventory_generation':3,**counts,'instrumentation':sys.argv[7],'preflight_free_disk_bytes':int(sys.argv[11]),'target_created_unix_ms':int(sys.argv[12]),'p_foundation':sys.argv[13],'q_commit':sys.argv[14],'p_source_head':source['head'],'p_source_git_tree':source['git_tree']}
stable={k:v for k,v in fixed.items() if k not in ('instrumentation','preflight_free_disk_bytes')}
fixed['inputs_hash']=hashlib.sha256(json.dumps(stable,sort_keys=True).encode()).hexdigest()
manifest={'config':{'attempt_id':attempt,'policy':policy,'regime':regime,'repeat':repeat,'seed':41000+repeat},**identity,'executable_sha256':hashlib.sha256((exe_dir/executable).read_bytes()).hexdigest(),'image_digest':sys.argv[9],'toolchain':sys.argv[10],'features':['integration'],'target_identity':sys.argv[8],'effective_inputs':fixed}
(out/'control'/'current-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
PY
}

last_report() {
	python3 - "${output}/attempts/$1/events.jsonl" "$2" "$3" <<'PY'
from pathlib import Path
import json,sys
matches=[]
for line in open(sys.argv[1]):
    event=json.loads(line)
    if event.get('event')==sys.argv[2]: matches.append(event['report'])
if len(matches)!=1: raise SystemExit('native report missing or repeated')
Path(sys.argv[3]).write_text(json.dumps(matches[0],indent=2)+'\n')
PY
}

run_native() {
	local mode=$1 database=$2 regime=$3 policy=$4 repeat=$5 counts=$6 instrumentation=${7:-observers}
	local executable=${policy} status=0 attempt
	[[ ${instrumentation} != foundation ]] || executable=foundation
	attempt="$(printf '%s' "${regime}" | tr '[:upper:]' '[:lower:]')-${mode}-${executable}-r${repeat}"
	case ${mode} in inventory|inventory-adjust|seed) admit_step 1500 ;; composed) admit_step 1260 ;; cell) admit_step 420 ;; *) admit_step 180 ;; esac
	effective_readback "${database}" >"${output}/control/database-inputs.json"
	write_manifest "${regime}" "${policy}" "${repeat}" "${attempt}" "${counts}" "${instrumentation}"
	resource_readback 0
	DATABASE_URL=$(database_dsn "${database}") POSTGRES_SUSTAINED_MODE="${mode}" \
		POSTGRES_SUSTAINED_MANIFEST="${output}/control/current-manifest.json" \
		"${executables}/${executable}" sustained_postgres --exact --ignored --nocapture --test-threads=1 \
		>"${output}/control/${attempt}.log" 2>&1 &
	driver=$!
	collect_resources "${driver}" & collector=$!
	wait "${driver}" || status=$?
	driver=
	kill -TERM "${collector}" 2>/dev/null || true
	wait "${collector}" 2>/dev/null || true
	collector=
	case ${mode} in
	inventory|inventory-adjust) last_report "${attempt}" inventory_report "${output}/reports/${attempt}.json" ;;
	seed) last_report "${attempt}" seed_preconditioned "${output}/reports/${attempt}.json" ;;
	qualify) last_report "${attempt}" fixed_rate_qualification "${output}/reports/${attempt}.json" ;;
	calibrate) last_report "${attempt}" calibration_complete "${output}/reports/${attempt}.json" ;;
	cell) last_report "${attempt}" cell_report "${output}/reports/${attempt}.json" ;;
	composed) last_report "${attempt}" composed_report "${output}/reports/${attempt}.json" ;;
	esac
	((status==0)) || { echo "native attempt ${attempt} failed; preserve every receipt" >&2; return "${status}"; }
}

freeze_seed() {
	local database=$1
	[[ ${database} =~ ^sustained_seed_(resident|pressured)$ ]] || return 2
	printf "DO \$\$ BEGIN IF EXISTS (SELECT FROM pg_stat_activity WHERE datname='%s') THEN RAISE EXCEPTION 'seed still has connected owners'; END IF; END \$\$; ALTER DATABASE \"%s\" ALLOW_CONNECTIONS false;\n" \
		"${database}" "${database}" | psql_control postgres >/dev/null
}

clone_cell() {
	local regime=$1 start finish
	start=$(now_ms)
	database_create sustained_cell "sustained_seed_${regime}"
	# CREATE DATABASE TEMPLATE does not copy database-level settings or access.
	# Read the effective configuration of the actual clone before each caller.
	effective_readback sustained_cell >"${output}/control/clone-readback.json"
	finish=$(now_ms)
	((finish-start <= 60000)) || { echo "60-second clone/reset envelope exceeded" >&2; return 1; }
	python3 - "${output}/control/clone-readbacks.jsonl" "${regime}" "${start}" "${finish}" "${output}/control/clone-readback.json" <<'PY'
import json,sys
with open(sys.argv[1],'a') as stream: stream.write(json.dumps({'regime':sys.argv[2],'started_unix_ms':int(sys.argv[3]),'completed_unix_ms':int(sys.argv[4]),'database_config':json.load(open(sys.argv[5]))})+'\n')
PY
}

aggregate_report() {
	local mode=$1 attempt=$2 input=$3 destination=$4
	python3 - "${output}/control/current-manifest.json" "${output}/control/aggregate-manifest.json" "${attempt}" "${executables}" <<'PY'
from pathlib import Path
import hashlib,json,sys
manifest=json.load(open(sys.argv[1])); manifest['config']['attempt_id']=sys.argv[3]
exe=Path(sys.argv[4]); manifest.update(json.loads((exe/'P0.source.json').read_text())); manifest['executable_sha256']=hashlib.sha256((exe/'P0').read_bytes()).hexdigest(); manifest['config']['policy']='P0'
Path(sys.argv[2]).write_text(json.dumps(manifest)+'\n')
PY
	POSTGRES_SUSTAINED_MODE="${mode}" POSTGRES_SUSTAINED_REPORTS="${input}" \
		POSTGRES_SUSTAINED_MANIFEST="${output}/control/aggregate-manifest.json" \
		"${executables}/P0" sustained_postgres --exact --ignored --nocapture --test-threads=1 \
		>"${output}/control/${attempt}.log" 2>&1
	python3 - "${output}/attempts/${attempt}/events.jsonl" "${destination}" <<'PY'
from pathlib import Path
import json,sys
events=[json.loads(line) for line in open(sys.argv[1])]
if len(events)!=1 or 'status' not in events[0]: raise SystemExit('aggregate report identity is ambiguous')
Path(sys.argv[2]).write_text(json.dumps(events[0],indent=2)+'\n')
PY
}

for regime in resident pressured; do
	label=Resident
	[[ ${regime} != pressured ]] || label=Pressured
	counts="${output}/control/${regime}-counts.json"
	python3 - "${counts}" "${regime}" <<'PY'
from pathlib import Path
import json,sys,time
live,ordinary,receipts,jobs=(128,260,256,2000) if sys.argv[2]=='resident' else (512,1030,1024,5000)
Path(sys.argv[1]).write_text(json.dumps({'live_bodies':live,'idempotency_rows':ordinary,'receipt_rows':receipts,'jobs_rows':jobs,'seed_started_unix_ms':time.time_ns()//1_000_000})+'\n')
PY
	database="sustained_seed_${regime}"
	database_create "${database}"
	APP__POSTGRES__ENABLED=true APP__POSTGRES__DSN="$(database_dsn "${database}")" \
		"${executables}/migrate" --config "${source_copy}/env/config/local.toml" >"${output}/control/${regime}-migrate.log" 2>&1
	run_native inventory "${database}" "${label}" P0 1 "${counts}"
	inventory_report="${output}/reports/${regime}-inventory-P0-r1.json"
	if [[ $(json_get "${inventory_report}" status) == adjustment_proposed ]]; then
		python3 - "${counts}" "${inventory_report}" <<'PY'
from pathlib import Path
import json,sys
path=Path(sys.argv[1]); counts=json.loads(path.read_text()); report=json.load(open(sys.argv[2]))
proposal=report['proposed_counts']
if not isinstance(proposal,dict): raise SystemExit('missing measured inventory adjustment')
counts.update(proposal); path.write_text(json.dumps(counts)+'\n')
PY
		run_native inventory-adjust "${database}" "${label}" P0 1 "${counts}"
		inventory_report="${output}/reports/${regime}-inventory-adjust-P0-r1.json"
	fi
	[[ $(json_get "${inventory_report}" status) == inventory_prepared ]] || {
		echo "inventory sizing unqualified after the one permitted adjustment" >&2; exit 1;
	}
	run_native seed "${database}" "${label}" P0 1 "${counts}"
	python3 - "${counts}" "${output}/reports/${regime}-seed-P0-r1.json" <<'PY'
from pathlib import Path
import json,sys
path=Path(sys.argv[1]); counts=json.loads(path.read_text()); report=json.load(open(sys.argv[2]))
if report['status']!='seed_qualified': raise SystemExit('physical seed unqualified')
counts['seed_cohort_hash']=report['seed_cohort_hash']; path.write_text(json.dumps(counts)+'\n')
PY
	freeze_seed "${database}"
	clone_cell "${regime}"
	run_native qualify sustained_cell "${label}" P0 1 "${counts}"
	database_drop sustained_cell
done

# Calibration has the fixed reverse order between regimes, not a policy repeat.
calibration_reports=()
for pair in resident:foundation resident:observers pressured:observers pressured:foundation; do
	regime=${pair%:*}; instrumentation=${pair#*:}; label=Resident
	[[ ${regime} != pressured ]] || label=Pressured
	clone_cell "${regime}"
	run_native calibrate sustained_cell "${label}" P0 1 "${output}/control/${regime}-counts.json" "${instrumentation}"
	executable=P0
	[[ ${instrumentation} != foundation ]] || executable=foundation
	calibration_reports+=("${output}/reports/${regime}-calibrate-${executable}-r1.json")
	database_drop sustained_cell
done
python3 - "${output}/reports/calibration-input.json" "${calibration_reports[@]}" <<'PY'
from pathlib import Path
import json,sys
Path(sys.argv[1]).write_text(json.dumps([json.load(open(path)) for path in sys.argv[2:]])+'\n')
PY
aggregate_report calibration-report calibration-summary "${output}/reports/calibration-input.json" "${output}/reports/calibration.json"
[[ $(json_get "${output}/reports/calibration.json" status) == calibration_criteria_met ]] || {
	echo "observer calibration requires its owning design to reopen" >&2; exit 1;
}

cell_reports=()
cells=0
run_cell() {
	local regime=$1 policy=$2 repeat=$3 label=Resident
	[[ ${regime} != pressured ]] || label=Pressured
	((cells < 20)) || { echo "20-cell main matrix limit reached" >&2; return 1; }
	cells=$((cells+1))
	clone_cell "${regime}"
	run_native cell sustained_cell "${label}" "${policy}" "${repeat}" "${output}/control/${regime}-counts.json"
	cell_reports+=("${output}/reports/${regime}-cell-${policy}-r${repeat}.json")
	database_drop sustained_cell
}
matrix_input() {
	python3 - "${output}/reports/matrix-input.json" "${cell_reports[@]}" <<'PY'
from pathlib import Path
import json,sys
Path(sys.argv[1]).write_text(json.dumps([json.load(open(path)) for path in sys.argv[2:]])+'\n')
PY
}
for policy in P0 P1 P2 P3; do run_cell resident "${policy}" 1; done
for policy in P3 P2 P1 P0; do run_cell pressured "${policy}" 1; done
matrix_input
aggregate_report select screening-summary "${output}/reports/matrix-input.json" "${output}/reports/screening.json"
[[ $(json_get "${output}/reports/screening.json" status) == confirmation_required ]] || {
	echo "screening supplies no admissible confirmation; preserve the negative result" >&2; exit 1;
}
python3 - "${output}/reports/screening.json" "${output}/control/confirmation.tsv" <<'PY'
from pathlib import Path
import json,sys
report=json.load(open(sys.argv[1])); cells=report['confirmation_plan']
if not 8<=len(cells)<=12: raise SystemExit('confirmation plan exceeds accepted matrix')
Path(sys.argv[2]).write_text(''.join(f"{cell['regime'].lower()}\t{cell['policy']}\t{cell['repeat']}\n" for cell in cells))
PY
while IFS=$'\t' read -r regime policy repeat; do run_cell "${regime}" "${policy}" "${repeat}"; done <"${output}/control/confirmation.tsv"
matrix_input
aggregate_report select selection-summary "${output}/reports/matrix-input.json" "${output}/reports/selection.json"
[[ $(json_get "${output}/reports/selection.json" status) == policy_selected ]] || {
	echo "no supported measured policy; preserve the complete comparison" >&2; exit 1;
}
selected=$(json_get "${output}/reports/selection.json" selection.policy)
[[ ${selected} =~ ^P[0-3]$ ]] || exit 1
clone_cell pressured
run_native composed sustained_cell Pressured "${selected}" 1 "${output}/control/pressured-counts.json"
database_drop sustained_cell

python3 - "${output}" "${source_hash}" "${POSTGRES_SUSTAINED_Q_COMMIT}" "${foundation}" <<'PY'
from pathlib import Path
import json,sys
out=Path(sys.argv[1]); result=json.loads((out/'reports'/'selection.json').read_text()); policy=result['selection']['policy']
text=f'''# Sustained PostgreSQL measurement

Status: native matrix and composed observations complete; final source selection,
canonical validation, independent review, export and resource absence are separate
delivery requirements.

Selected policy: `{policy}`.
P foundation: `{sys.argv[4]}`.
Q dependency: `{sys.argv[3]}`.
Assembled source inventory: `{sys.argv[2]}`.

All retained runs and paired decisions: [selection](reports/selection.json).
Observation overhead: [calibration](reports/calibration.json).
Composed proof: [behavior report](reports/pressured-composed-{policy}-r1.json).
Raw attempts, invalid attempts and per-minute distributions remain in `attempts/`.
The entry made no automatic retry: zero of the two allowed diagnostic replacement
cells were spent. A later replacement requires a diagnosed external/collector
fault and the original cumulative clock; this report does not authorize it.

Apply the selected constants through the retained replay patch, remove unused
runtime policy paths serially, and run the affected final proof before acceptance.
No production capacity, multi-month durability or network-outbox claim follows.
'''
(out/'measurement-result.md').write_text(text)
PY
campaign_status=observations_complete
