"""Select a unique non-test executable from one successful Cargo build."""

import argparse
import json
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path("crates/service/Cargo.toml"))
    parser.add_argument("--name", default="openapi")
    parser.add_argument("--kind", choices=("bin", "example"), default="bin")
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
        label = "service OpenAPI" if not sys.argv[1:] else f"{args.manifest} {args.kind} {args.name}"
        sys.exit(f"expected one {label} executable from Cargo, got {len(matches)}")
    print(matches[0])


if __name__ == "__main__":
    main()
