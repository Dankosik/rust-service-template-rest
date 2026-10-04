#!/usr/bin/env python3
"""Remote-only grouped observations; every valid sample stays visible."""
import json
import pathlib
import statistics

root = pathlib.Path('/root/profiling/evidence')
rows = json.loads((root / 'summary.json').read_text())
groups = {}
fields = ['p50_ms', 'p95_ms', 'service_cpu_us_per_request', 'service_cpu_percent',
          'rss_peak_mib', 'requests_per_s', 'generator_cpu_percent', 'service_cpus_steal_percent']
for row in rows:
    if row['binary'] not in ['baseline', 'candidate', 'noname']:
        continue
    case = row['workload']
    if 'pool' in row['name']:
        case = f"sizing-pool{row['pool']}"
    elif 'http-repaired' in row['name']:
        case = 'repaired-HTTP'
    elif 'name-rss' in row['name']:
        case = 'name-diagnosis'
    elif case == 'webhook_new':
        case += '-large' if row['body_bytes'] == 65536 else '-small'
    row['p50_ms'] = row['duration_ms'].get('p(50)', row['duration_ms'].get('med'))
    row['p95_ms'] = row['duration_ms']['p(95)']
    groups.setdefault(case, {}).setdefault(row['binary'], []).append(row)
output = {}
for case, binaries in groups.items():
    output[case] = {}
    for binary, samples in binaries.items():
        output[case][binary] = {
            'names': [sample['name'] for sample in samples], 'samples': len(samples),
            'statistics': {field: {
                'median': statistics.median(sample[field] for sample in samples),
                'min': min(sample[field] for sample in samples),
                'max': max(sample[field] for sample in samples),
            } for field in fields},
            'failed_checks': sum(sample['failed_checks'] for sample in samples),
            'dropped_iterations': sum(sample['dropped_iterations'] for sample in samples),
            'http_failed_rates': [sample['http_failed_rate'] for sample in samples],
        }
result = {'ordinary': output, 'samples': rows,
          'allocations': json.loads((root / 'allocation-comparison.json').read_text()),
          'invalid_cells': json.loads((root / 'invalid-cells.json').read_text())}
(root / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(output, indent=2))
