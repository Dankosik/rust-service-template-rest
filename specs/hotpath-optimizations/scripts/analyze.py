#!/usr/bin/env python3
"""Remote-only paired comparison; retain all samples and exact counters."""
import json
import pathlib
import re
import statistics

root = pathlib.Path('/root/profiling/evidence')
rows = json.loads((root / 'summary.json').read_text())
metrics = ['p50_ms', 'p95_ms', 'service_cpu_us_per_request', 'service_cpu_percent',
           'rss_peak_mib', 'requests_per_s', 'generator_cpu_percent', 'service_cpus_steal_percent']
ordinary = {}
for row in rows:
    if row['binary'] not in ['baseline', 'candidate']:
        continue
    case = row['workload']
    if case == 'webhook_new':
        case += '-large' if row['body_bytes'] == 65536 else '-small'
    duration = row['duration_ms']
    row['p50_ms'] = duration.get('p(50)', duration.get('med'))
    row['p95_ms'] = duration['p(95)']
    ordinary.setdefault(case, {}).setdefault(row['binary'], []).append(row)
groups = {}
for case, binaries in ordinary.items():
    groups[case] = {}
    for binary, samples in binaries.items():
        groups[case][binary] = {
            'names': [sample['name'] for sample in samples],
            'samples': len(samples),
            'statistics': {metric: {
                'median': statistics.median(sample[metric] for sample in samples),
                'min': min(sample[metric] for sample in samples),
                'max': max(sample[metric] for sample in samples),
            } for metric in metrics},
            'failed_checks': sum(sample['failed_checks'] for sample in samples),
            'dropped_iterations': sum(sample['dropped_iterations'] for sample in samples),
        }

profiles = []
for cell in sorted((root / 'runs').iterdir()):
    if not (cell / 'hotpath.json').exists():
        continue
    report = json.loads((cell / 'hotpath.json').read_text())
    profiles.append({
        'name': cell.name,
        'function_alloc': [row for row in report['functions_alloc']['data']
                           if row['name'].endswith('::new') or row['name'].endswith('::prepare')],
        'function_timing': [row for row in report['functions_timing']['data']
                            if row['name'].endswith('::new') or row['name'].endswith('::prepare')],
        'server': report['server'],
    })

def counter(text, metric, function):
    pattern = re.compile(r'^' + re.escape(metric) + r'\{[^\n]*function="' +
                         re.escape(function) + r'"[^\n]*\}\s+([^\s]+)', re.M)
    matches = pattern.findall(text)
    if len(matches) != 1:
        raise ValueError(f'{metric}/{function}: expected one sample, got {matches}')
    return float(matches[0])

raw = {}
for binary in ['profile-raw-baseline', 'profile-raw-candidate']:
    cell = root / 'runs' / f'{binary}-large-exact'
    if not (cell / 'hotpath-after.prom').exists():
        continue
    before = (cell / 'hotpath-before.prom').read_text()
    after = (cell / 'hotpath-after.prom').read_text()
    functions = {}
    receiver = 'infra_webhooks::inbound::receive' if binary.endswith('baseline') else 'infra_webhooks::inbound::receive_bytes'
    for function in ['infra_webhooks::inbound::new', 'infra_jobs::enqueue::prepare',
                     'infra_http::webhooks::receive', receiver, 'infra_jobs::enqueue::enqueue']:
        totals = {}
        for key, metric in [('bytes', 'hotpath_function_alloc_bytes_total'),
                            ('allocations', 'hotpath_function_alloc_count_total'),
                            ('calls', 'hotpath_function_calls_total')]:
            totals[key] = counter(after, metric, function) - counter(before, metric, function)
        totals['bytes_per_call'] = totals['bytes'] / totals['calls']
        totals['allocations_per_call'] = totals['allocations'] / totals['calls']
        functions[function] = totals
    scopes = ['infra_webhooks::inbound::new', 'infra_jobs::enqueue::prepare']
    raw[binary] = {'functions': functions,
                   'combined_bytes_per_call': sum(functions[name]['bytes_per_call'] for name in scopes),
                   'combined_allocations_per_call': sum(functions[name]['allocations_per_call'] for name in scopes)}
result = {'ordinary': groups, 'profiles': profiles, 'raw': raw,
          'samples': rows, 'excluded': json.loads((root / 'invalid-cells.json').read_text())}
if len(raw) == 2:
    result['exact_saving_bytes_per_delivery'] = raw['profile-raw-baseline']['combined_bytes_per_call'] - raw['profile-raw-candidate']['combined_bytes_per_call']
    result['exact_saving_allocations_per_delivery'] = raw['profile-raw-baseline']['combined_allocations_per_call'] - raw['profile-raw-candidate']['combined_allocations_per_call']
(root / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
for case, binaries in groups.items():
    for binary, group in binaries.items():
        print(case, binary, json.dumps(group))
print('exact:', json.dumps(raw))
if len(raw) == 2:
    print('saving bytes:', result['exact_saving_bytes_per_delivery'])
