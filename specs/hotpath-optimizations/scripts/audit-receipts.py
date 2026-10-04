#!/usr/bin/env python3
"""Remote-only custody audit; not an additional service behavior test."""
import hashlib
import json
import pathlib

root = pathlib.Path('/root/profiling/evidence/runs')
receipts = []
for cell in sorted(root.iterdir()):
    load = json.loads((cell / 'load-exit.json').read_text())
    service = json.loads((cell / 'service-exit.json').read_text())
    assert load['exit'] == 0, (cell.name, load)
    assert service['exit'] == 0, (cell.name, service)
    receipts.append({'name': cell.name, 'load_exit': load['exit'],
                     'service_exit': service['exit'],
                     'excluded': (cell / 'excluded.json').exists(),
                     'load_sha256': hashlib.sha256((cell / 'load.js').read_bytes()).hexdigest()})
assert len(receipts) == 41, len(receipts)
assert sum(not row['excluded'] for row in receipts) == 40
(root.parent / 'run-receipts.json').write_text(json.dumps(receipts, indent=2) + '\n')
print('41 complete cells; 40 retained comparison cells; all load/service exits 0')
