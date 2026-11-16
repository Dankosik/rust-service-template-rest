#!/usr/bin/env python3
"""Execute npx under the 7-day publish-age cooldown and --ignore-scripts policy."""

from __future__ import annotations

import os
import re
import shutil
import sys
from collections.abc import Mapping, Sequence
from datetime import datetime, timedelta, timezone

sys.dont_write_bytecode = True

DEFAULT_COOLDOWN_DAYS = 7
_DAYS_RE = re.compile(r"^[0-9]+$")


def resolve_before(*, now: datetime | None = None, cooldown_days: int = DEFAULT_COOLDOWN_DAYS) -> str:
    current = now if now is not None else datetime.now(timezone.utc)
    cutoff = current.astimezone(timezone.utc) - timedelta(days=cooldown_days)
    return cutoff.strftime("%Y-%m-%dT%H:%M:%SZ")


def prepare_environment(
    environ: Mapping[str, str],
    *,
    now: datetime | None = None,
) -> dict[str, str]:
    raw_days = environ.get("NPX_COOLDOWN_DAYS", str(DEFAULT_COOLDOWN_DAYS)).strip()
    if not _DAYS_RE.fullmatch(raw_days):
        raise ValueError(f"NPX_COOLDOWN_DAYS must be a non-negative integer, got {raw_days!r}")
    cooldown_days = int(raw_days)
    return {
        **environ,
        "npm_config_prefer_offline": "true",
        "npm_config_ignore_scripts": "true",
        "npm_config_audit": "false",
        "npm_config_fund": "false",
        "npm_config_update_notifier": "false",
        "npm_config_before": resolve_before(now=now, cooldown_days=cooldown_days),
    }


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] in ("-h", "--help") or args[0].startswith("-"):
        print("usage: npx.py <pkg@version> [args...]", file=sys.stderr)
        return 2
    try:
        env = prepare_environment(os.environ)
    except ValueError as error:
        print(f"npx: {error}", file=sys.stderr)
        return 2
    npx_bin = shutil.which("npx", path=env.get("PATH"))
    if npx_bin is None:
        print(f"npx: Node.js (npx) is required for {args[0]}", file=sys.stderr)
        return 2
    try:
        os.execvpe(npx_bin, [npx_bin, "--yes", *args], env)
    except OSError as error:
        print(f"npx: cannot execute {npx_bin}: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
