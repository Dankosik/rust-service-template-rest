#!/usr/bin/env python3
"""Report hand-written code that a newer toolchain's std or lints now cover.

Run once after a toolchain bump (`make modernize-scan SINCE=<old toolchain>`).
It prints candidates and never fails on them: about half of the mechanical
suggestions in the round-3 pass were rejected on judgment, so a gate would
cost more than it catches. Pedantic and default lints already fail `make lint`
once `rust-version` moves; this covers what they do not:

1. clippy lints new since SINCE, with their default level;
2. one clippy pass with the nursery and restriction groups, filtered to the
   lints that replace a hand-written form with a std one (plus every lint new
   since SINCE);
3. the ast-grep rules in tools/modernize/rules, for repository patterns no
   clippy lint covers. ast-grep does not look inside macro arguments.
"""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
from pathlib import Path

# Allow-by-default lints whose suggestion is a std construct; the rest of the
# nursery and restriction groups are style or policy and were noise (4,000+
# hits, a handful useful) in the round-3 pass.
STD_FORM_LINTS = {
    "clippy::deref_by_slicing",
    "clippy::equatable_if_let",
    "clippy::iter_on_empty_collections",
    "clippy::iter_on_single_items",
    "clippy::map_with_unused_argument_over_ranges",
    "clippy::or_fun_call",
    "clippy::redundant_clone",
    "clippy::tuple_array_conversions",
}
# Already decided against in this repository; do not resurface them.
DECLINED_LINTS = {"clippy::option_if_let_else", "clippy::return_and_then"}


def lint_levels(toolchain: str | None) -> dict[str, str]:
    """Map each clippy lint name to its default level for a toolchain."""
    command = ["clippy-driver", "-W", "help", "-"]
    if toolchain:
        command = ["rustup", "run", toolchain, *command]
    output = subprocess.run(
        command, input="", capture_output=True, text=True, check=True
    ).stdout
    levels = {}
    for line in output.splitlines():
        fields = line.split()
        # Group lines list members instead of a level; skip them.
        if (
            len(fields) >= 2
            and fields[0].startswith("clippy::")
            and fields[1] in {"allow", "warn", "deny", "forbid"}
        ):
            levels[fields[0].replace("-", "_")] = fields[1]
    return levels


def new_lints(since: str) -> set[str]:
    current = lint_levels(None)
    previous = lint_levels(since)
    added = sorted(set(current) - set(previous))
    print(f"== clippy lints new since {since}: {len(added)}")
    for name in added:
        print(f"  {name} ({current[name]})")
    return set(added)


def clippy_candidates(cargo: list[str], wanted: set[str], new: set[str]) -> int:
    command = [
        *cargo,
        "clippy",
        "--workspace",
        "--all-targets",
        "--locked",
        "--message-format=json",
        "--",
        "-W",
        "clippy::nursery",
        "-W",
        "clippy::restriction",
        "-A",
        "clippy::blanket_clippy_restriction_lints",
    ]
    print(f"== clippy nursery/restriction, filtered to {len(wanted)} lints")
    run = subprocess.run(command, capture_output=True, text=True, check=False)
    if run.returncode != 0:
        sys.stderr.write(run.stderr)
        return run.returncode
    seen = set()
    # A lint new since SINCE is unvetted: show its count and first places only.
    sampled: dict[str, list[str]] = {}
    for line in run.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message["message"]
        code = (diagnostic.get("code") or {}).get("code", "")
        if code not in wanted or not diagnostic["spans"]:
            continue
        span = diagnostic["spans"][0]
        if span["file_name"].startswith(("vendor/", "/")):
            continue
        key = (span["file_name"], span["line_start"], code)
        if key in seen:
            continue
        seen.add(key)
        place = f"{key[0]}:{key[1]}: {code}: {diagnostic['message']}"
        if code in new and code not in STD_FORM_LINTS:
            sampled.setdefault(code, []).append(place)
        else:
            print(f"  {place}")
    for code, places in sorted(sampled.items()):
        print(f"  {code}: {len(places)} hits (new lint; first three shown)")
        for place in places[:3]:
            print(f"    {place}")
    if not seen:
        print("  none")
    return 0


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    os.chdir(root)
    cargo = shlex.split(os.environ.get("CARGO", "cargo"))
    ast_grep = shlex.split(os.environ.get("AST_GREP", "ast-grep"))
    since = os.environ.get("SINCE", "").strip()

    wanted = set(STD_FORM_LINTS)
    new: set[str] = set()
    if since:
        new = new_lints(since)
        wanted |= new
    else:
        print("== clippy lints new since the previous toolchain: skipped (set SINCE=<toolchain>)")
    wanted -= DECLINED_LINTS

    status = clippy_candidates(cargo, wanted, new)
    if status:
        return status

    print("== ast-grep repository patterns")
    sys.stdout.flush()
    return subprocess.run(
        [
            *ast_grep,
            "scan",
            "--config",
            "tools/modernize/sgconfig.yml",
            "--report-style",
            "short",
            ".",
        ],
        check=False,
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
