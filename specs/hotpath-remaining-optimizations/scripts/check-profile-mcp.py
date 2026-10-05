#!/usr/bin/env python3
"""Remote CI compatibility receipt against the actual feature-enabled service."""
import json
import os
import pathlib
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

evidence = pathlib.Path('.ci/profile-mcp')
evidence.mkdir(parents=True, exist_ok=True)
env = dict(os.environ)
env.update({
    'APP__APP__ENV': 'local', 'APP__HTTP__ADDR': '127.0.0.1:8080',
    'APP__OBSERVABILITY__METRICS__ADDR': '127.0.0.1:9090',
    'APP__HTTP__READINESS_PROPAGATION_DELAY': '0s', 'APP__POSTGRES__ENABLED': 'false',
    'APP__LOG__FORMAT': 'json', 'HOTPATH_MCP_PORT': '6771',
    'HOTPATH_METRICS_PORT': '6770', 'HOTPATH_MCP_AUTH_TOKEN': 'profiling-ci-only',
    'HOTPATH_OUTPUT_FORMAT': 'json', 'HOTPATH_OUTPUT_PATH': str(evidence / 'profile.json'),
})
with (evidence / 'service.log').open('wb') as log:
    server = subprocess.Popen(['target/debug/service'], env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(150):
            if server.poll() is not None:
                raise RuntimeError(f'profile service exited early: {server.returncode}')
            try:
                with urllib.request.urlopen('http://127.0.0.1:8080/health/ready', timeout=1) as response:
                    if response.status == 200:
                        break
            except OSError:
                pass
            time.sleep(.1)
        else:
            raise RuntimeError('profile service readiness timeout')
        request = urllib.request.Request('http://127.0.0.1:6771/mcp', b'{}',
                                         {'Content-Type': 'application/json'}, method='POST')
        try:
            urllib.request.urlopen(request, timeout=3)
        except urllib.error.HTTPError as error:
            assert error.code == 401, error.code
        else:
            raise AssertionError('MCP accepted a request without the configured token')
        output = evidence / 'mcp.json'
        subprocess.run([sys.executable, 'specs/hotpath-remaining-optimizations/scripts/mcp.py',
                        str(output), '--url', 'http://127.0.0.1:6771/mcp',
                        '--tools', 'profiler_status,server,functions_timing,mutexes'],
                       env=env, check=True, timeout=60)
        receipt = json.loads(output.read_text())
        assert receipt['initialize']['serverInfo']['version'] == '0.28.4'
        names = {tool['name'] for tool in receipt['tools_list']['tools']}
        assert {'profiler_status', 'functions_timing', 'mutexes'} <= names
        for name in ['profiler_status', 'server', 'functions_timing', 'mutexes']:
            assert not receipt[name]['isError'], name
        print('MCP0.28.4 auth denial, initialize, session, tools/list and tools/call passed')
    finally:
        if server.poll() is None:
            server.send_signal(signal.SIGTERM)
        try:
            status = server.wait(timeout=35)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()
            raise RuntimeError('profile service failed to drain')
        (evidence / 'exit.json').write_text(json.dumps({'exit': status}) + '\n')
        assert status == 0, status
