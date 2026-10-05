#!/usr/bin/env python3
"""Remote synthetic DB wait-event sampling; not a lock-duration measurement."""
import json
import pathlib
import sys
import time
import psycopg2

generator = int(sys.argv[1])
connection = psycopg2.connect(host='127.0.0.1', port=5433, dbname='app', user='app',
    password='profiling-only', application_name='measurement-observer')
connection.autocommit = True
with pathlib.Path(sys.argv[2]).open('w') as stream:
    while pathlib.Path(f'/proc/{generator}').exists():
        state = pathlib.Path(f'/proc/{generator}/stat').read_text().rsplit(')', 1)[1].split()[0]
        if state == 'Z':
            break
        with connection.cursor() as cursor:
            cursor.execute("SELECT state,wait_event_type,wait_event,count(*) FROM pg_stat_activity "
                           "WHERE datname='app' AND backend_type='client backend' "
                           "AND pid<>pg_backend_pid() GROUP BY state,wait_event_type,wait_event")
            stream.write(json.dumps({'wall': time.time(), 'states': cursor.fetchall()}) + '\n')
            stream.flush()
        time.sleep(.1)
connection.close()
