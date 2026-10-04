#!/usr/bin/env python3
"""Remote-only terminal receipt audit; no filtering of unfavorable results."""
import json
import pathlib

root = pathlib.Path('/root/profiling/evidence')
rows = json.loads((root / 'summary.json').read_text())
failures = []
for row in rows:
    cell = root / 'runs' / row['name']
    for filename in ['load-exit.json', 'service-exit.json']:
        receipt = json.loads((cell / filename).read_text())
        if receipt['exit'] != 0:
            failures.append({'name': row['name'], 'receipt': filename, 'exit': receipt['exit']})
    if row['failed_checks'] or row['http_failed_rate']:
        failures.append({'name': row['name'], 'failed_checks': row['failed_checks'],
                         'http_failed_rate': row['http_failed_rate']})
invalid = json.loads((root / 'invalid-cells.json').read_text())
result = {'completed_cells': len(rows), 'receipt_failures': failures,
          'invalid_cells': invalid, 'exclusions': [],
          'dropped_iterations_retained': sum(row['dropped_iterations'] for row in rows)}
(root / 'receipt-audit.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
assert not failures and not invalid
