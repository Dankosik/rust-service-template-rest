#!/usr/bin/env python3
"""Run only on the droplet; retain totals alongside complete test logs."""
import json
import pathlib
import re

root = pathlib.Path('/root/optimization/evidence')
results = {}
for name in ['test-candidate', 'test-integration-db']:
    content = (root / f'{name}.log').read_text()
    suites = [tuple(map(int, match)) for match in re.findall(
        r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', content)]
    results[name] = {
        'passed': sum(row[0] for row in suites),
        'failed': sum(row[1] for row in suites),
        'ignored': sum(row[2] for row in suites),
        'suites': len(suites),
        'ignored_lines': [line for line in content.splitlines() if ' ... ignored' in line],
        'failure_lines': [line for line in content.splitlines() if 'FAILED' in line],
    }
results['candidate_ready'] = (root / 'candidate-ready.txt').read_text().strip()
(root / 'validation.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
