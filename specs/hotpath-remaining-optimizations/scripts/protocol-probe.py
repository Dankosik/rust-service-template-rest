#!/usr/bin/env python3
"""Disposable loopback PostgreSQL protocol observation; run only on droplet."""
import asyncio
import base64
import hashlib
import hmac
import json
import os
import pathlib
import signal
import struct
import subprocess
import time
import urllib.request

root = pathlib.Path('/root/remaining/evidence/protocol')
root.mkdir(parents=True, exist_ok=False)
events = []
phase = 'startup'
connection_count = 0
key = b'0123456789abcdef0123456789abcdef'
body = json.dumps({'type': 'benchmark', 'data': 'x' * 1024}, separators=(',', ':')).encode()

def label(query):
    value = query.strip().upper()
    if value.startswith('BEGIN'): return 'begin'
    if value.startswith('COMMIT'): return 'commit'
    if value.startswith('ROLLBACK'): return 'rollback'
    if 'INSERT INTO WEBHOOK_RECEIPTS' in value: return 'receipt'
    if 'INSERT INTO BACKGROUND_JOBS' in value: return 'job'
    if 'PG_NOTIFY' in value: return 'notify'
    if value in ['', ';'] or 'SQLX PING' in value: return 'ping'
    if value.startswith('SELECT 1'): return 'select1'
    return 'other'

async def accept(client_read, client_write):
    global connection_count
    connection_count += 1
    cid = connection_count
    server_read, server_write = await asyncio.open_connection('127.0.0.1', 5433)
    statements, portals, pending, group = {}, {}, [], []
    prefix = await client_read.readexactly(4)
    size = struct.unpack('!I', prefix)[0]
    startup = await client_read.readexactly(size - 4)
    server_write.write(prefix + startup)
    await server_write.drain()

    async def pump(reader, writer, frontend):
        try:
            while True:
                kind = await reader.readexactly(1)
                prefix = await reader.readexactly(4)
                size = struct.unpack('!I', prefix)[0]
                payload = await reader.readexactly(size - 4)
                event = {'time': time.monotonic(), 'connection': cid, 'phase': phase,
                         'direction': 'frontend' if frontend else 'backend',
                         'type': kind.decode(), 'length': size}
                if frontend:
                    if kind == b'Q':
                        command = label(payload.rstrip(b'\0').decode())
                        pending.append([command])
                        event['operation'] = command
                    elif kind == b'P':
                        name, query, _ = payload.split(b'\0', 2)
                        statements[name] = label(query.decode())
                        group.append('prepare_' + statements[name])
                        event['operation'] = statements[name]
                    elif kind == b'B':
                        portal, statement, _ = payload.split(b'\0', 2)
                        portals[portal] = statements.get(statement, 'unknown')
                    elif kind == b'E':
                        portal = payload.split(b'\0', 1)[0]
                        command = portals.get(portal, 'unknown')
                        group.append(command)
                        event['operation'] = command
                    elif kind == b'S':
                        pending.append(list(group))
                        group.clear()
                elif kind == b'Z':
                    event['operations'] = pending.pop(0) if pending else ['startup']
                    event['transaction_status'] = payload.decode()
                # Never retain authentication, binds, rows or message bodies.
                if kind != b'p': events.append(event)
                writer.write(kind + prefix + payload)
                await writer.drain()
        except (asyncio.IncompleteReadError, ConnectionError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()
    await asyncio.gather(pump(client_read, server_write, True),
                         pump(server_read, client_write, False))

def request(path, payload=None, headers=None):
    req = urllib.request.Request('http://127.0.0.1:8080' + path, payload, headers or {})
    with urllib.request.urlopen(req, timeout=9) as response:
        return {'status': response.status, 'body_bytes': len(response.read())}

async def delivery(name, message_id):
    global phase
    phase = name
    timestamp = str(int(time.time()))
    signed = message_id.encode() + b'.' + timestamp.encode() + b'.' + body
    signature = base64.b64encode(hmac.new(key, signed, hashlib.sha256).digest()).decode()
    start = time.monotonic()
    result = await asyncio.to_thread(request, '/webhooks/bench', body, {
        'content-type': 'application/json', 'webhook-id': message_id,
        'webhook-timestamp': timestamp, 'webhook-signature': 'v1,' + signature})
    end = time.monotonic()
    assert result['status'] == 204, result
    return {'name': name, 'id': message_id, 'start': start, 'returned': end,
            'latency_ms': (end - start) * 1000, **result}

async def main():
    global phase
    subprocess.run(['docker', 'exec', 'profiling-postgres', 'psql', '-U', 'app', '-d', 'app',
                    '-c', 'TRUNCATE webhook_receipts, background_jobs CASCADE;'], check=True)
    proxy = await asyncio.start_server(accept, '127.0.0.1', 5544)
    env = dict(os.environ, APP__APP__ENV='local', APP__HTTP__ADDR='127.0.0.1:8080',
               APP__HTTP__READINESS_PROPAGATION_DELAY='0s',
               APP__OBSERVABILITY__METRICS__ADDR='127.0.0.1:9090',
               APP__POSTGRES__ENABLED='true', APP__POSTGRES__MAX_CONNECTIONS='1',
               APP__POSTGRES__DSN='postgres://app:profiling-only@127.0.0.1:5544/app?sslmode=disable',
               APP__INBOUND_WEBHOOKS__ENDPOINTS__BENCH__ACTIVE_KEY='bench_key',
               APP__INBOUND_WEBHOOKS__SECRETS__BENCH_KEY='whsec_' + base64.b64encode(key).decode())
    log = (root / 'service.log').open('wb')
    server = subprocess.Popen(['taskset', '-c', '0,1', '/root/profiling/bin/baseline'],
                              env=env, stdout=log, stderr=subprocess.STDOUT)
    cases = []
    try:
        for _ in range(100):
            try:
                if (await asyncio.to_thread(request, '/health/ready'))['status'] == 200:
                    break
            except OSError:
                await asyncio.sleep(.1)
        else:
            raise RuntimeError('service readiness unavailable')
        cases.append(await delivery('warmup', 'protocol-warmup'))
        await asyncio.sleep(.06)  # Existing per-kind wake debounce is 25 ms; no idle ping yet.
        cases.append(await delivery('new_wake_due', 'protocol-new-one'))
        cases.append(await delivery('new_within_debounce', 'protocol-new-two'))
        cases.append(await delivery('duplicate', 'protocol-new-one'))
        phase = 'settle'
        await asyncio.sleep(.1)
    finally:
        phase = 'shutdown'
        server.send_signal(signal.SIGTERM)
        status = await asyncio.to_thread(server.wait, 35)
        log.close()
        proxy.close()
        await proxy.wait_closed()
        (root / 'events.json').write_text(json.dumps(events, indent=2) + '\n')
        (root / 'cases.json').write_text(json.dumps(cases, indent=2) + '\n')
        (root / 'service-exit.json').write_text(json.dumps({'exit': status}) + '\n')
    for case in cases:
        ready = [event for event in events if event['type'] == 'Z' and event['phase'] == case['name']]
        print(json.dumps({**case, 'ready_events_by_phase': ready}))

asyncio.run(main())
