"""Select a unique non-test executable from one successful Cargo build."""

import argparse
import json
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--kind", choices=("bin", "example"), required=True)
    args = parser.parse_args()
    manifest = str(args.manifest.resolve())
    matches = []
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
            continue
        target = message["target"]
        executable = message["executable"]
        if (message["manifest_path"] == manifest
                and target["name"] == args.name
                and target["kind"] == [args.kind]
                and message["profile"]["test"] is False
                and isinstance(executable, str) and Path(executable).is_absolute()):
            matches.append(executable)
    if len(matches) != 1:
        sys.exit(f"expected one {args.manifest} {args.kind} {args.name} executable from Cargo, got {len(matches)}")
    print(matches[0])


if __name__ == "__main__":
    main()
