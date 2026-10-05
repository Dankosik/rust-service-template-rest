#!/usr/bin/env python3
"""Remote-only evidence reduction. Raw observations remain beside the summary."""
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else '/root/profiling/evidence/runs')
results = []
invalid = []
for cell in sorted(root.iterdir()):
    if not (cell / 'load-exit.json').exists():
        invalid.append({'name': cell.name, 'reason': 'no completed load receipt'})
        continue
    if not (cell / 'load.json').exists():
        continue
    parameters = json.loads((cell / 'parameters.json').read_text())
    metrics = json.loads((cell / 'load.json').read_text())['metrics']
    def values(name):
        metric = metrics.get(name, {})
        return metric.get('values', metric)
    resources = [json.loads(line) for line in (cell / 'resources.jsonl').read_text().splitlines()]
    resources = [row for row in resources if 'service' in row and 'generator' in row]
    first, last = resources[0], resources[-1]
    elapsed = last['monotonic'] - first['monotonic']
    count = values('response_status').get('count', values('http_reqs').get('count', 0))
    cpu = last['service']['cpu_s'] - first['service']['cpu_s']
    generator = last['generator']['cpu_s'] - first['generator']['cpu_s']
    ticks = [b - a for a, b in zip(first['host_cpu_ticks'], last['host_cpu_ticks'])]
    service_ticks = [sum(last['host_per_cpu_ticks'][f'cpu{i}'][j] -
                         first['host_per_cpu_ticks'][f'cpu{i}'][j] for i in [0, 1])
                     for j in range(8)]
    results.append({
        'valid_cell': (cell / 'load-exit.json').exists(),
        'name': cell.name, **parameters, 'requests': count,
        'requests_per_s': count / parameters['duration'],
        'dropped_iterations': values('dropped_iterations').get('count', 0),
        'failed_checks': values('checks').get('fails', 0),
        'http_failed_rate': values('http_req_failed').get('rate', values('http_req_failed').get('value', 0)),
        'duration_ms': values('http_req_duration'),
        'routes_ms': {name: values(name) for name in metrics if name.startswith('latency_')},
        'service_cpu_percent': cpu / elapsed * 100,
        'service_cpu_us_per_request': cpu / count * 1e6,
        'generator_cpu_percent': generator / elapsed * 100,
        'rss_peak_mib': max(row['service']['rss_bytes'] for row in resources) / 2 ** 20,
        'rss_first_mib': first['service']['rss_bytes'] / 2 ** 20,
        'rss_last_mib': last['service']['rss_bytes'] / 2 ** 20,
        'host_steal_percent': ticks[7] / max(1, sum(ticks[:8])) * 100,
        'service_cpus_steal_percent': service_ticks[7] / max(1, sum(service_ticks)) * 100,
        'resource_window_s': elapsed,
        'service_exit': json.loads((cell / 'service-exit.json').read_text()).get('exit')
            if (cell / 'service-exit.json').exists() else None,
    })
path = root.parent / 'summary.json'
path.write_text(json.dumps(results, indent=2))
(root.parent / 'invalid-cells.json').write_text(json.dumps(invalid, indent=2))
for row in results:
    duration = row['duration_ms']
    print(f"{row['name']}: {row['requests_per_s']:.0f} req/s "
          f"p50={duration.get('p(50)', duration.get('med', 0)):.3f} ms "
          f"p95={duration.get('p(95)', 0):.3f} ms "
          f"CPU={row['service_cpu_percent']:.1f}% "
          f"cpu/req={row['service_cpu_us_per_request']:.1f} us "
          f"RSS={row['rss_peak_mib']:.1f} MiB "
          f"generator={row['generator_cpu_percent']:.1f}% "
          f"steal={row['service_cpus_steal_percent']:.2f}% "
          f"drop={row['dropped_iterations']} failed={row['failed_checks']}")
