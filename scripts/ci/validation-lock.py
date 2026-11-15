#!/usr/bin/env python3
"""Serialize local heavy validation commands across processes via fcntl.flock."""

from __future__ import annotations

import fcntl
import json
import os
import secrets
import signal
import subprocess
import sys
import time
from pathlib import Path

SELF = Path(__file__).resolve()


def resolve_lock_dir() -> Path:
    configured = os.environ.get("VALIDATION_LOCK_DIR", "").strip()
    if configured:
        return Path(configured).resolve()
    result = subprocess.run(
        ["git", "rev-parse", "--git-common-dir"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        return (Path(result.stdout.strip()).resolve() / "codex" / "validation.lock")
    return Path("/tmp/rust-service-validation.lock")


def run_child(command: list[str], env: dict[str, str]) -> int:
    child = subprocess.Popen(command, env=env)

    def forward(signum: int, _frame: object) -> None:
        if child.poll() is None:
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass

    previous = {
        sig: signal.signal(sig, forward)
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
    }
    try:
        returncode = child.wait()
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    return 128 + abs(returncode) if returncode < 0 else returncode


def acquire_and_run(command: list[str]) -> int:
    if not command:
        print("usage: validation-lock.sh -- <command> [args...]", file=sys.stderr)
        return 2

    if os.environ.get("VALIDATION_LOCK_HELD") == "1" and os.environ.get("VALIDATION_LOCK_TOKEN"):
        return run_child(command, dict(os.environ))

    if os.environ.get("CI") == "true" and "VALIDATION_LOCK_DIR" not in os.environ:
        env = {
            **os.environ,
            "VALIDATION_LOCK_HELD": "1",
            "VALIDATION_LOCK_TOKEN": secrets.token_hex(16),
        }
        return run_child(command, env)

    try:
        timeout = float(os.environ.get("VALIDATION_LOCK_TIMEOUT_SECONDS", "900"))
    except ValueError:
        print("VALIDATION_LOCK_TIMEOUT_SECONDS must be numeric", file=sys.stderr)
        return 2

    lock_dir = resolve_lock_dir()
    lock_dir.mkdir(parents=True, exist_ok=True)
    lock_path = lock_dir / "lock"
    holder_path = lock_dir / "holder.json"

    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
    os.set_inheritable(fd, False)
    deadline = time.monotonic() + max(0.0, timeout)
    try:
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    holder = holder_path.read_text(encoding="utf-8").strip() if holder_path.is_file() else "unknown"
                    print(
                        f"validation-lock: timed out after {timeout:g}s waiting for {lock_dir} (holder: {holder})",
                        file=sys.stderr,
                    )
                    return 75
                time.sleep(min(0.05, remaining))

        token = secrets.token_hex(16)
        holder_path.write_text(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "started_at": int(time.time()),
                    "command": command,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        env = {
            **os.environ,
            "VALIDATION_LOCK_HELD": "1",
            "VALIDATION_LOCK_TOKEN": token,
        }
        try:
            return run_child(command, env)
        finally:
            holder_path.unlink(missing_ok=True)
            fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def print_status() -> int:
    lock_dir = resolve_lock_dir()
    lock_path = lock_dir / "lock"
    holder_path = lock_dir / "holder.json"
    if not lock_path.is_file():
        print("free")
        return 0
    fd = os.open(lock_path, os.O_RDWR)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            if holder_path.is_file():
                print(holder_path.read_text(encoding="utf-8").strip())
            else:
                print(json.dumps({"state": "held", "lock_dir": str(lock_dir)}))
            return 0
        fcntl.flock(fd, fcntl.LOCK_UN)
        print("free")
        return 0
    finally:
        os.close(fd)


def main(argv: list[str]) -> int:
    if argv == ["--assert-held"]:
        return 0 if (os.environ.get("VALIDATION_LOCK_HELD") == "1" and os.environ.get("VALIDATION_LOCK_TOKEN")) else 1
    if argv == ["--status"]:
        return print_status()
    if argv == ["--self-test"]:
        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        return subprocess.call(
            [sys.executable, "-B", str(SELF.parent.parent / "tests" / "validation-lock-test.py")],
            env=env,
        )
    if argv and argv[0] == "--":
        return acquire_and_run(argv[1:])
    print(
        "usage: validation-lock.sh -- <command> [args...] | --assert-held | --status | --self-test",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
