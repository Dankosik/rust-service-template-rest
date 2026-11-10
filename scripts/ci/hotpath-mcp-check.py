#!/usr/bin/env python3
"""Remote CI compatibility receipt against the actual feature-enabled service."""
import json
import os
import pathlib
import signal
import subprocess
import time
import urllib.error
import urllib.request

MCP_URL = 'http://127.0.0.1:6771/mcp'
MCP_TOKEN = 'profiling-ci-only'
TOOLS = ('profiler_status', 'server', 'functions_timing', 'mutexes')


def mcp_receipt():
    """Initialize one Streamable HTTP session and call every tool in TOOLS."""
    session = None
    sequence = 0
    protocol = '2025-03-26'

    def rpc(method, params, notification=False):
        nonlocal session, sequence
        sequence += 1
        payload = {'jsonrpc': '2.0', 'method': method, 'params': params}
        if not notification:
            payload['id'] = sequence
        headers = {'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream',
                   'MCP-Protocol-Version': protocol, 'Authorization': MCP_TOKEN}
        if session:
            headers['Mcp-Session-Id'] = session
        request = urllib.request.Request(MCP_URL, json.dumps(payload).encode(), headers, method='POST')
        with urllib.request.urlopen(request, timeout=15) as response:
            session = response.headers.get('Mcp-Session-Id', session)
            if notification or response.status == 202:
                return None
            if 'application/json' in response.headers.get('Content-Type', ''):
                result = json.loads(response.read())
            else:
                # Streamable HTTP can keep SSE open after the response. Consume
                # complete events until this RPC's reply, rather than waiting EOF.
                data_lines = []
                while True:
                    raw = response.readline()
                    if not raw:
                        raise RuntimeError('MCP SSE ended without this RPC response')
                    line = raw.decode().rstrip('\r\n')
                    if line.startswith('data:'):
                        data_lines.append(line[5:].lstrip())
                    if not line:
                        data = '\n'.join(data_lines)
                        data_lines = []
                        if not data.strip():
                            continue
                        event = json.loads(data)
                        if event.get('id') == sequence:
                            result = event
                            break
            if 'error' in result:
                raise RuntimeError(result['error'])
            return result['result']

    receipt = {'initialize': rpc('initialize', {'protocolVersion': protocol, 'capabilities': {},
                                                'clientInfo': {'name': 'profiling-ci', 'version': '1'}})}
    protocol = receipt['initialize']['protocolVersion']
    rpc('notifications/initialized', {}, notification=True)
    receipt['tools_list'] = rpc('tools/list', {})
    for name in TOOLS:
        value = rpc('tools/call', {'name': name, 'arguments': {}})
        decoded = []
        for content in value.get('content', []):
            if content.get('type') == 'text':
                try:
                    decoded.append(json.loads(content['text']))
                except json.JSONDecodeError:
                    decoded.append(content['text'])
        receipt[name] = {'isError': value.get('isError', False), 'data': decoded}
    return receipt


evidence = pathlib.Path('.ci/profile-mcp')
evidence.mkdir(parents=True, exist_ok=True)
env = dict(os.environ)
env.update({
    'APP__APP__ENV': 'local', 'APP__HTTP__ADDR': '127.0.0.1:8080',
    'APP__OBSERVABILITY__METRICS__ADDR': '127.0.0.1:9090',
    'APP__HTTP__READINESS_PROPAGATION_DELAY': '0s', 'APP__POSTGRES__ENABLED': 'false',
    'APP__LOG__FORMAT': 'json', 'HOTPATH_MCP_PORT': '6771',
    'HOTPATH_METRICS_PORT': '6770', 'HOTPATH_MCP_AUTH_TOKEN': MCP_TOKEN,
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
        request = urllib.request.Request(MCP_URL, b'{}', {'Content-Type': 'application/json'}, method='POST')
        try:
            urllib.request.urlopen(request, timeout=3)
        except urllib.error.HTTPError as error:
            assert error.code == 401, error.code
        else:
            raise AssertionError('MCP accepted a request without the configured token')
        receipt = mcp_receipt()
        (evidence / 'mcp.json').write_text(json.dumps(receipt, indent=2))
        assert receipt['initialize']['serverInfo']['version'] == '0.28.4'
        names = {tool['name'] for tool in receipt['tools_list']['tools']}
        assert {'profiler_status', 'functions_timing', 'mutexes'} <= names
        for name in TOOLS:
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
