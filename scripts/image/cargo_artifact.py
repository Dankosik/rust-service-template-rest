#!/usr/bin/env python3
"""Select a unique executable artifact from one successful Cargo JSON build stream."""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Iterable, Sequence
from pathlib import Path

sys.dont_write_bytecode = True


def select_executable(lines: Iterable[str], *, manifest: Path, name: str, kind: str) -> str:
    resolved_manifest = str(manifest.resolve())
    expect_test_profile = kind == "test"
    matches: list[str] = []
    for line in lines:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        profile = message.get("profile")
        executable = message.get("executable")
        if not isinstance(target, dict) or not isinstance(profile, dict):
            continue
        if (
            message.get("manifest_path") == resolved_manifest
            and target.get("name") == name
            and target.get("kind") == [kind]
            and profile.get("test") is expect_test_profile
            and isinstance(executable, str)
            and Path(executable).is_absolute()
        ):
            matches.append(executable)
    if len(matches) != 1:
        raise ValueError(
            f"expected one {manifest} {kind} {name} executable from Cargo, got {len(matches)}"
        )
    return matches[0]


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--kind", choices=("bin", "example", "test"), required=True)
    args = parser.parse_args(argv)
    try:
        executable = select_executable(
            sys.stdin,
            manifest=args.manifest,
            name=args.name,
            kind=args.kind,
        )
    except ValueError as error:
        print(str(error), file=sys.stderr)
        return 1
    print(executable)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
