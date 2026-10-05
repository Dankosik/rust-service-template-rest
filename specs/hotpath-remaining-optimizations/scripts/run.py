#!/usr/bin/env python3
"""One frozen cell: launch, warmup, open workload, live MCP, clean shutdown."""
import argparse
import base64
import json
import os
import pathlib
import signal
import subprocess
import time
import urllib.request
import shutil

p = argparse.ArgumentParser()
p.add_argument('name')
p.add_argument('--binary', default='baseline', choices=['baseline', 'candidate', 'noname', 'profile-baseline', 'profile-candidate', 'profile-noname'])
p.add_argument('--workload', default='mixed')
p.add_argument('--rate', type=int, default=2000)
p.add_argument('--duration', type=int, default=30)
p.add_argument('--body-bytes', type=int, default=1024)
p.add_argument('--pool', type=int, default=4)
p.add_argument('--log-level', default='info')
p.add_argument('--log-health', action='store_true')
p.add_argument('--sampler', default='parentbased_traceidratio')
p.add_argument('--scrape', action='store_true')
p.add_argument('--otlp', action='store_true')
p.add_argument('--cpu-snapshot', action='store_true')
p.add_argument('--generator', choices=['k6', 'wrk2'], default='k6')
p.add_argument('--vus', type=int, default=256)
p.add_argument('--alloc-metric', choices=['bytes', 'count'], default='bytes')
p.add_argument('--id-prefix', default='matched-delivery')
p.add_argument('--raw-alloc', action='store_true')
p.add_argument('--fixed-ids', action='store_true')
args = p.parse_args()
root = pathlib.Path('/root/profiling')
cell = root / 'evidence' / 'runs' / args.name
cell.mkdir(parents=True, exist_ok=False)
(cell / 'parameters.json').write_text(json.dumps(vars(args), indent=2))
for source in ['run.py', 'load.js', 'wrk2.lua', 'mcp.py', 'monitor.py', 'pg_monitor.py']:
    shutil.copyfile(root / 'scripts' / source, cell / source)
database = args.workload.startswith('webhook')
env = dict(os.environ)
env['PATH'] = '/root/.cargo/bin:' + env['PATH']
env.update({
    'APP__APP__ENV': 'local', 'APP__APP__COMMIT': '67be869-dirty-profile-study',
    'APP__HTTP__ADDR': '127.0.0.1:8080',
    'APP__HTTP__READINESS_PROPAGATION_DELAY': '0s',
    'APP__OBSERVABILITY__METRICS__ADDR': '127.0.0.1:9090',
    'APP__LOG__LEVEL': args.log_level, 'APP__LOG__FORMAT': 'json',
    'APP__HTTP__ACCESS_LOG_HEALTH_PROBES': str(args.log_health).lower(),
    'APP__OBSERVABILITY__OTEL__TRACES_SAMPLER': args.sampler,
    'HOTPATH_OUTPUT_FORMAT': 'json', 'HOTPATH_OUTPUT_PATH': str(cell / 'hotpath.json'),
    'HOTPATH_FUNCTIONS_LIMIT': '0', 'HOTPATH_THREADS_LIMIT': '0',
    'HOTPATH_SOURCE_ROOT': '', 'HOTPATH_METRICS_PORT': '6770', 'HOTPATH_MCP_PORT': '6771',
    'HOTPATH_SAMPLY_WRAPPER_BIN': '/root/.cargo/bin/hotpath-samply',
    'HOTPATH_SAMPLY_BIN': '/usr/local/bin/samply',
    'HOTPATH_ALLOC_METRIC': args.alloc_metric,
    'HOTPATH_PROMETHEUS_HOST': '127.0.0.1', 'HOTPATH_PROMETHEUS_PORT': '6772',
})
if args.otlp:
    env['APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT'] = 'http://127.0.0.1:4318'
if database:
    env.update({'APP__POSTGRES__ENABLED': 'true', 'APP__POSTGRES__MAX_CONNECTIONS': str(args.pool),
        'APP__POSTGRES__DSN': 'postgres://app:profiling-only@127.0.0.1:5433/app?sslmode=disable',
        'APP__INBOUND_WEBHOOKS__ENDPOINTS__BENCH__ACTIVE_KEY': 'bench_key',
        'APP__INBOUND_WEBHOOKS__SECRETS__BENCH_KEY': 'whsec_' + base64.b64encode(b'0123456789abcdef0123456789abcdef').decode()})
    subprocess.run(['docker', 'exec', 'profiling-postgres', 'psql', '-U', 'app', '-d', 'app', '-c',
        'TRUNCATE webhook_receipts, background_jobs CASCADE; SELECT pg_stat_statements_reset();'],
        check=True, stdout=(cell / 'database-reset.txt').open('w'))

def get(path, timeout=2):
    with urllib.request.urlopen('http://127.0.0.1:' + path, timeout=timeout) as response:
        return response.read()

server_log = (cell / 'service.log').open('wb')
server = subprocess.Popen(['taskset', '-c', '0,1', str(root / 'bin' / args.binary)],
                          env=env, stdout=server_log, stderr=subprocess.STDOUT)
load = None
try:
    for _ in range(100):
        if server.poll() is not None:
            raise RuntimeError(f'service failed: {server.returncode}')
        try:
            if get('8080/health/ready') == b'ok':
                break
        except OSError:
            pass
        time.sleep(.1)
    else:
        raise RuntimeError('service did not become ready')
    load_env = dict(env, WORKLOAD=args.workload, RATE=str(args.rate), DURATION='5s',
                    RUN_ID=args.id_prefix + '-warmup', BODY_BYTES=str(args.body_bytes), VUS=str(args.vus),
                    SCRAPE='1' if args.scrape else '0', FIXED_ID_WIDTH='10' if args.fixed_ids else '0')
    if args.generator == 'k6':
        warmup_command = ['taskset', '-c', '4-7', 'k6', 'run', '--quiet', str(cell / 'load.js')]
    else:
        warmup_command = ['taskset', '-c', '4-7', 'wrk2', '-t4', '-c64', '-d15s', '-R', str(args.rate),
            '-s', str(cell / 'wrk2.lua'), '--latency', 'http://127.0.0.1:8080']
    subprocess.run(warmup_command, env=load_env, check=True, stdout=(cell / 'warmup.txt').open('w'), stderr=subprocess.STDOUT)
    (cell / 'metrics-before.prom').write_bytes(get('9090/metrics'))
    if args.raw_alloc:
        (cell / 'hotpath-before.prom').write_bytes(get('6772/metrics', timeout=12))
    load_env.update(DURATION=f'{args.duration}s', RUN_ID=args.id_prefix)
    started = time.monotonic()
    if args.generator == 'k6':
        load_command = ['taskset', '-c', '4-7', 'k6', 'run', '--quiet', '--summary-export', str(cell / 'load.json'), str(cell / 'load.js')]
    else:
        load_command = ['taskset', '-c', '4-7', 'wrk2', '-t4', '-c64', f'-d{args.duration}s', '-R', str(args.rate),
            '-s', str(cell / 'wrk2.lua'), '--latency', 'http://127.0.0.1:8080']
    load = subprocess.Popen(load_command, env=load_env,
        stdout=(cell / 'load.txt').open('w'), stderr=subprocess.STDOUT)
    monitor = subprocess.Popen(['taskset', '-c', '3', 'python3', str(root / 'scripts/monitor.py'),
                               str(server.pid), str(load.pid), str(cell / 'resources.jsonl')])
    pg_monitor = subprocess.Popen(['taskset', '-c', '3', 'python3', str(root / 'scripts/pg_monitor.py'),
        str(load.pid), str(cell / 'pg-waits.jsonl')]) if database else None
    if args.binary.startswith('profile'):
        time.sleep(min(10, args.duration / 2))
        if args.cpu_snapshot:
            subprocess.run(['python3', str(root / 'scripts/mcp.py'), str(cell / 'mcp-cpu-start.json'),
                            '--tools', 'functions_cpu_snapshot'], check=True)
        subprocess.run(['python3', str(root / 'scripts/mcp.py'), str(cell / 'mcp-live.json')],
                       check=True, stdout=(cell / 'mcp-receipt.txt').open('w'), stderr=subprocess.STDOUT)
    load_status = load.wait(timeout=args.duration + 30)
    monitor.wait(timeout=5)
    if pg_monitor:
        pg_monitor.wait(timeout=5)
    if args.generator == 'wrk2':
        text = (cell / 'load.txt').read_text()
        wrk_result = json.loads(next(line[len('WRK_JSON '):] for line in text.splitlines() if line.startswith('WRK_JSON ')))
        (cell / 'wrk.json').write_text(json.dumps(wrk_result, indent=2))
        (cell / 'load.json').write_text(json.dumps({'metrics': {
            'response_status': {'count': wrk_result['requests']},
            'http_req_duration': {'p(50)': wrk_result['p50_ms'], 'p(95)': wrk_result['p95_ms'], 'p(99)': wrk_result['p99_ms']},
            'checks': {'fails': wrk_result['status5xx'] + wrk_result['other']},
            'http_req_failed': {'rate': (wrk_result['status5xx'] + wrk_result['other']) / max(1, wrk_result['requests'])}
        }}))
    (cell / 'load-exit.json').write_text(json.dumps({'exit': load_status,
        'elapsed_s': time.monotonic() - started, 'service_pid': server.pid}))
    (cell / 'metrics-after.prom').write_bytes(get('9090/metrics'))
    if args.binary.startswith('profile'):
        subprocess.run(['python3', str(root / 'scripts/mcp.py'), str(cell / 'mcp-final.json'),
            '--tools', 'profiler_status,server,functions_timing,functions_alloc,threads,tokio_runtime,sql,mutexes,rw_locks,functions_cpu'],
            check=True, stdout=(cell / 'mcp-final-receipt.txt').open('w'), stderr=subprocess.STDOUT)
    if database:
        subprocess.run(['docker', 'exec', 'profiling-postgres', 'psql', '-U', 'app', '-d', 'app',
            '-c', 'SELECT version(); SHOW fsync; SHOW synchronous_commit; '
                  'SELECT query,calls,total_exec_time,mean_exec_time,rows,shared_blks_hit,shared_blks_read,wal_bytes '
                  'FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 15; '
                  'SELECT count(*) AS receipts FROM webhook_receipts; SELECT count(*) AS jobs FROM background_jobs; '
                  'SELECT avg(pg_column_size(payload)) AS stored_payload_bytes FROM background_jobs;'],
            check=True, stdout=(cell / 'postgres.txt').open('w'))
    if args.raw_alloc:
        (cell / 'hotpath-after.prom').write_bytes(get('6772/metrics', timeout=12))
finally:
    if load is not None and load.poll() is None:
        load.terminate()
        try:
            load.wait(timeout=5)
        except subprocess.TimeoutExpired:
            load.kill()
            load.wait()
    if server.poll() is None:
        server.send_signal(signal.SIGTERM)
    try:
        status = server.wait(timeout=35)
    except subprocess.TimeoutExpired:
        server.kill()
        status = server.wait()
    server_log.close()
    (cell / 'service-exit.json').write_text(json.dumps({'exit': status}))
