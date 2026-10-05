#!/usr/bin/env python3
"""Run on the droplet; query hotpath MCP through the SSH tunnel return port."""
import argparse
import json
import os
import pathlib
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('output')
parser.add_argument('--url', default='http://127.0.0.1:16771/mcp')
parser.add_argument('--tools', default='profiler_status,server,functions_timing,functions_alloc,threads,tokio_runtime,sql,mutexes,rw_locks,futures')
args = parser.parse_args()
session = None
sequence = 0
protocol = '2025-03-26'

def rpc(method, params, notification=False):
    global session, sequence
    sequence += 1
    payload = {'jsonrpc': '2.0', 'method': method, 'params': params}
    if not notification:
        payload['id'] = sequence
    headers = {'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream',
               'MCP-Protocol-Version': protocol}
    if token := os.environ.get('HOTPATH_MCP_AUTH_TOKEN'):
        headers['Authorization'] = f'Bearer {token}'
    if session:
        headers['Mcp-Session-Id'] = session
    request = urllib.request.Request(args.url, json.dumps(payload).encode(), headers, method='POST')
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

result = {'initialize': rpc('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
    'clientInfo': {'name': 'profiling-research', 'version': '1'}})}
protocol = result['initialize']['protocolVersion']
rpc('notifications/initialized', {}, notification=True)
result['tools_list'] = rpc('tools/list', {})
for name in args.tools.split(','):
    value = rpc('tools/call', {'name': name, 'arguments': {}})
    decoded = []
    for content in value.get('content', []):
        if content.get('type') == 'text':
            try:
                decoded.append(json.loads(content['text']))
            except json.JSONDecodeError:
                decoded.append(content['text'])
    result[name] = {'isError': value.get('isError', False), 'data': decoded}
pathlib.Path(args.output).write_text(json.dumps(result, indent=2))
print(json.dumps({name: {'isError': value['isError']} for name, value in result.items()
                  if isinstance(value, dict) and 'isError' in value}))
