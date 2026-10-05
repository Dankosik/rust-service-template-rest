#!/usr/bin/env python3
"""Remote-only operation-sequence reduction; HTTP phase is not lease ownership."""
import json
import pathlib

root = pathlib.Path('/root/remaining/evidence/protocol')
events = json.loads((root / 'events.json').read_text())
cases = json.loads((root / 'cases.json').read_text())
ready = [event for event in events if event['type'] == 'Z']
results = []
for case in cases:
    if case['name'] == 'warmup':
        continue
    start = next(i for i, event in enumerate(ready)
                 if event['phase'] == case['name'] and event['operations'] == ['begin'])
    finish = next(i for i in range(start, len(ready)) if ready[i]['operations'] == ['commit'])
    sequence = ready[start:finish + 1]
    assert len({event['connection'] for event in sequence}) == 1
    cid = sequence[0]['connection']
    returned = ready[finish + 1]
    assert returned['connection'] == cid and returned['operations'] == []
    assert returned['transaction_status'] == 'I'
    sent_begin = next(event for event in events
                      if event['direction'] == 'frontend' and event.get('operation') == 'begin'
                      and event['phase'] == case['name'])
    return_sync = [event for event in events
                   if event['direction'] == 'frontend' and event['type'] == 'S'
                   and event['connection'] == cid
                   and sequence[-1]['time'] <= event['time'] <= returned['time']]
    assert len(return_sync) == 1, return_sync
    results.append({**case, 'connection': cid,
                    'request_operations': [event['operations'] for event in sequence],
                    'request_ready_exchanges': len(sequence),
                    'return_ready_exchanges': 1,
                    'total_exchanges_including_return': len(sequence) + 1,
                    'observed_begin_to_commit_ack_ms': (sequence[-1]['time'] - sent_begin['time']) * 1000,
                    'observed_commit_ack_to_return_ready_ms': (returned['time'] - sequence[-1]['time']) * 1000,
                    'observed_begin_to_return_ready_ms': (returned['time'] - sent_begin['time']) * 1000})
expected = {'new_wake_due': 5, 'new_within_debounce': 4, 'duplicate': 3}
for result in results:
    assert result['request_ready_exchanges'] == expected[result['name']], result
output = {'kind': 'bounded warmed baseline protocol observation, pool=1',
          'limitation': 'proxy timing includes observation transport; not a direct-service latency measurement',
          'raw': ['events.json', 'cases.json'], 'cases': results}
(root / 'summary.json').write_text(json.dumps(output, indent=2) + '\n')
print(json.dumps(output, indent=2))
