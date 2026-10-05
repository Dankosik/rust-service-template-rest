#!/usr/bin/env python3
"""Remote-only matching-cohort pool/transaction signals and database CPU."""
import json
import pathlib
import re

root = pathlib.Path('/root/profiling/evidence')
results = []
def total(text, name):
    return sum(float(value) for value in re.findall(r'^' + re.escape(name) + r'(?:\{[^\n]*\})?\s+([^\s]+)', text, re.M))

for cell in sorted((root / 'runs').iterdir()):
    if not (cell / 'metrics-after.prom').exists(): continue
    params = json.loads((cell / 'parameters.json').read_text())
    if not params['workload'].startswith('webhook'): continue
    before = (cell / 'metrics-before.prom').read_text()
    after = (cell / 'metrics-after.prom').read_text()
    signals = {}
    for name in ['db_client_connection_wait_time_seconds', 'postgres_transaction_duration_seconds']:
        count = total(after, name + '_count') - total(before, name + '_count')
        seconds = total(after, name + '_sum') - total(before, name + '_sum')
        signals[name] = {'count': count, 'sum_s': seconds,
                         'mean_ms': seconds / count * 1000 if count else None}
    wait = signals['db_client_connection_wait_time_seconds']
    tx = signals['postgres_transaction_duration_seconds']
    item = {'name': cell.name, 'parameters': params, 'signals': signals,
            'matching_acquire_transaction_population': wait['count'] == tx['count']}
    if wait['count'] == tx['count'] and wait['count']:
        item['acquire_adjusted_transaction_mean_ms'] = tx['mean_ms'] - wait['mean_ms']
    rows = [json.loads(line) for line in (cell / 'resources.jsonl').read_text().splitlines()]
    rows = [row for row in rows if 'database_cpu' in row]
    first, last = rows[0], rows[-1]
    elapsed = last['monotonic'] - first['monotonic']
    item['database_cpu_percent'] = (last['database_cpu']['usage_usec'] - first['database_cpu']['usage_usec']) / 1e6 / elapsed * 100
    item['database_peak_cgroup_memory_mib'] = max(row['database_memory_bytes'] for row in rows) / 2**20
    item['resource_window_s'] = elapsed
    results.append(item)
(root / 'database-comparison.json').write_text(json.dumps(results, indent=2) + '\n')
for item in results:
    if 'pool' in item['name']: print(json.dumps(item))
