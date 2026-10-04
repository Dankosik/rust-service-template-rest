#!/usr/bin/env python3
"""Remote-only configured JSON/response parity, separate from load metrics."""
import json
import argparse
import os
import pathlib
import signal
import subprocess
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--candidate', default='candidate', choices=['candidate', 'noname'])
parser.add_argument('--output', default='/root/remaining/evidence/signal-parity')
args = parser.parse_args()
root = pathlib.Path(args.output)
root.mkdir(parents=True, exist_ok=False)
cases = [('GET', '/missing', 'parity-missing'),
         ('GET', '/missing?api_key=synthetic&safe=value', 'parity-query'),
         ('PROPFIND', '/missing', 'parity-extension'),
         ('POST', '/webhooks/unknown', 'parity-webhook')]
results = {}
for binary in ['baseline', args.candidate]:
    env = dict(os.environ, APP__APP__ENV='local', APP__APP__COMMIT='signal-parity',
               APP__HTTP__ADDR='127.0.0.1:8080', APP__HTTP__READINESS_PROPAGATION_DELAY='0s',
               APP__OBSERVABILITY__METRICS__ADDR='127.0.0.1:9090',
               APP__POSTGRES__ENABLED='false', APP__LOG__LEVEL='info', APP__LOG__FORMAT='json')
    log_path = root / f'{binary}.log'
    with log_path.open('wb') as log:
        server = subprocess.Popen(['taskset', '-c', '0,1', f'/root/profiling/bin/{binary}'],
                                  env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            for _ in range(100):
                try:
                    with urllib.request.urlopen('http://127.0.0.1:8080/health/ready', timeout=1) as response:
                        if response.status == 200: break
                except OSError: time.sleep(.05)
            else: raise RuntimeError('readiness unavailable')
            responses = []
            for method, path, identity in cases:
                request = urllib.request.Request('http://127.0.0.1:8080' + path,
                    b'{}' if method == 'POST' else None, method=method,
                    headers={'x-request-id': identity, 'user-agent': 'signal-parity',
                             'traceparent': '00-11111111111111111111111111111111-2222222222222222-01'})
                try:
                    response = urllib.request.urlopen(request, timeout=9)
                except urllib.error.HTTPError as error:
                    response = error
                with response:
                    body = response.read().decode()
                    responses.append({'method': method, 'path': path, 'identity': identity,
                                      'status': response.status, 'body': body,
                                      'request_id': response.headers.get('x-request-id')})
        finally:
            server.send_signal(signal.SIGTERM)
            status = server.wait(timeout=35)
            assert status == 0, status
    logs = [json.loads(line) for line in log_path.read_text().splitlines() if line.startswith('{')]
    requests = [item for item in logs if item.get('message') == 'http_request']
    assert len(requests) == len(cases), requests
    def normalize(value):
        if isinstance(value, dict):
            return {key: normalize(item) for key, item in value.items()
                    if key not in ['timestamp', 'duration_ms', 'span_id']}
        if isinstance(value, list): return [normalize(item) for item in value]
        return value
    results[binary] = {'responses': responses, 'http_logs': normalize(requests)}
assert results['baseline'] == results[args.candidate], results
(root / 'comparison.json').write_text(json.dumps(results, indent=2) + '\n')
print('configured JSON HTTP logs and response identity/body/status equal across 4 cases')
