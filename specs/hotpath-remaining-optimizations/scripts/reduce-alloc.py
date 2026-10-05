#!/usr/bin/env python3
"""Exact remote-only counter reduction; preserve numerator and denominator."""
import json
import pathlib
import re

root = pathlib.Path('/root/profiling/evidence')
result = []
for cell in sorted((root / 'runs').iterdir()):
    if not (cell / 'hotpath-after.prom').exists():
        continue
    before = (cell / 'hotpath-before.prom').read_text()
    after = (cell / 'hotpath-after.prom').read_text()
    names = ['infra_webhooks::inbound::new', 'infra_jobs::enqueue::prepare',
             'infra_http::observe::make_span', 'infra_http::observe::record',
             'infra_http::webhooks::receive', 'infra_webhooks::inbound::receive_bytes']
    functions = {}
    for name in names:
        totals = {}
        for key, metric in [('bytes', 'hotpath_function_alloc_bytes_total'),
                            ('allocations', 'hotpath_function_alloc_count_total'),
                            ('calls', 'hotpath_function_calls_total')]:
            pattern = re.compile(r'^' + re.escape(metric) + r'\{[^\n]*function="' +
                                 re.escape(name) + r'"[^\n]*\}\s+([^\s]+)', re.M)
            left, right = pattern.findall(before), pattern.findall(after)
            if not left and not right:
                break
            if not left:
                left = ['0']
            assert len(left) == len(right) == 1, (cell.name, name, metric)
            totals[key] = float(right[0]) - float(left[0])
        if len(totals) == 3 and totals['calls']:
            totals['bytes_per_call'] = totals['bytes'] / totals['calls']
            totals['allocations_per_call'] = totals['allocations'] / totals['calls']
            functions[name] = totals
    item = {'name': cell.name, 'functions': functions}
    scopes = ['infra_webhooks::inbound::new', 'infra_jobs::enqueue::prepare']
    if all(name in functions for name in scopes):
        item['construction_preparation_bytes_per_call'] = sum(functions[name]['bytes_per_call'] for name in scopes)
    result.append(item)
(root / 'allocation-comparison.json').write_text(json.dumps(result, indent=2) + '\n')
for item in result:
    print(json.dumps(item))
