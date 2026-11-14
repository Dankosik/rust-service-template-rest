"""Select the service's OpenAPI executable from the current Cargo build."""

import json
from pathlib import Path
import sys


def main():
    manifest = str(Path("crates/service/Cargo.toml").resolve())
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
                and target["name"] == "openapi"
                and target["kind"] == ["bin"]
                and message["profile"]["test"] is False
                and isinstance(executable, str) and Path(executable).is_absolute()):
            matches.append(executable)
    if len(matches) != 1:
        sys.exit(f"expected one service OpenAPI executable from Cargo, got {len(matches)}")
    print(matches[0])


if __name__ == "__main__":
    main()
