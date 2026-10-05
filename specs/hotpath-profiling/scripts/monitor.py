#!/usr/bin/env python3
"""Remote-only Linux /proc resource sampling; includes generator headroom."""
import json
import os
import pathlib
import sys
import time
import subprocess

service, generator, output = int(sys.argv[1]), int(sys.argv[2]), pathlib.Path(sys.argv[3])
hz = os.sysconf('SC_CLK_TCK')
container = subprocess.check_output(['docker', 'inspect', '--format', '{{.Id}}', 'profiling-postgres'], text=True).strip()
database_cgroup = pathlib.Path('/sys/fs/cgroup/system.slice') / f'docker-{container}.scope'

def process(pid):
    raw = pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return {'state': raw[0], 'cpu_s': (int(raw[11]) + int(raw[12])) / hz,
            'rss_bytes': int(raw[21]) * os.sysconf('SC_PAGE_SIZE'),
            'threads': int(raw[17])}

with output.open('w') as stream:
    while pathlib.Path(f'/proc/{generator}').exists():
        row = {'monotonic': time.monotonic(), 'wall': time.time()}
        for name, pid in [('service', service), ('generator', generator)]:
            try:
                row[name] = process(pid)
            except (OSError, ValueError):
                pass
        cpu = pathlib.Path('/proc/stat').read_text().splitlines()[0].split()[1:]
        row['host_cpu_ticks'] = list(map(int, cpu))
        if database_cgroup.exists():
            row['database_cpu'] = {key: int(value) for key, value in
                (line.split() for line in (database_cgroup / 'cpu.stat').read_text().splitlines())}
            row['database_memory_bytes'] = int((database_cgroup / 'memory.current').read_text())
        row['host_per_cpu_ticks'] = {
            line.split()[0]: list(map(int, line.split()[1:]))
            for line in pathlib.Path('/proc/stat').read_text().splitlines()
            if line.startswith('cpu') and line.split()[0] != 'cpu'}
        try:
            row['service_io'] = pathlib.Path(f'/proc/{service}/io').read_text()
        except OSError:
            pass
        stream.write(json.dumps(row) + '\n')
        stream.flush()
        if row.get('generator', {}).get('state') == 'Z':
            break
        time.sleep(.25)
