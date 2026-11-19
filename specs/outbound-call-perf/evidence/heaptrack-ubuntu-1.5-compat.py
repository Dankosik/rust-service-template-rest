#!/usr/bin/env python3
import json
import subprocess
import sys


arguments = sys.argv[1:]
if arguments == ["--version"]:
    sys.exit(subprocess.run(["/usr/bin/heaptrack", *arguments]).returncode)
if arguments and arguments[0] == "-q":
    arguments = arguments[1:]
result = subprocess.run(["/usr/bin/heaptrack", *arguments], text=True, stdout=subprocess.PIPE)
records = []
for line in result.stdout.splitlines():
    if line.startswith("{"):
        try:
            record = json.loads(line)
        except ValueError:
            pass
        else:
            if isinstance(record, dict) and "scenario" in record and "windows" in record:
                records.append(line)
                continue
    print(line, file=sys.stderr)
if result.returncode:
    sys.exit(result.returncode)
if len(records) != 1:
    print(f"expected one caller JSON receipt, observed {len(records)}", file=sys.stderr)
    sys.exit(1)
print(records[0])
