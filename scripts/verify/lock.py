#!/usr/bin/env python3
"""Serialize local heavy validation commands across processes via fcntl.flock."""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import secrets
import signal
import subprocess
import sys
import time
from collections.abc import Iterator, Sequence
from pathlib import Path

SELF = Path(__file__).resolve()
sys.dont_write_bytecode = True


class LockError(RuntimeError):
    def __init__(self, exit_code: int, message: str) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def resolve_lock_dir(cwd: Path | None = None) -> Path:
    configured = os.environ.get("VALIDATION_LOCK_DIR", "").strip()
    if configured:
        return Path(configured).resolve()
    result = subprocess.run(
        ["git", "rev-parse", "--git-common-dir"],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        raw = Path(result.stdout.strip())
        base = raw if raw.is_absolute() else ((cwd or Path.cwd()) / raw)
        return base.resolve() / "codex" / "validation.lock"
    return Path("/tmp/rust-service-validation.lock")


def is_held() -> bool:
    return os.environ.get("VALIDATION_LOCK_HELD") == "1" and bool(
        os.environ.get("VALIDATION_LOCK_TOKEN")
    )


def run_child(command: Sequence[str], env: dict[str, str]) -> int:
    child = subprocess.Popen(list(command), env=env)

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


@contextlib.contextmanager
def hold(command_desc: Sequence[str], *, cwd: Path | None = None) -> Iterator[dict[str, str]]:
    if is_held():
        yield dict(os.environ)
        return

    if os.environ.get("CI") == "true" and "VALIDATION_LOCK_DIR" not in os.environ:
        token = secrets.token_hex(16)
        prev_held = os.environ.get("VALIDATION_LOCK_HELD")
        prev_token = os.environ.get("VALIDATION_LOCK_TOKEN")
        os.environ["VALIDATION_LOCK_HELD"] = "1"
        os.environ["VALIDATION_LOCK_TOKEN"] = token
        try:
            yield dict(os.environ)
        finally:
            if prev_held is None:
                os.environ.pop("VALIDATION_LOCK_HELD", None)
            else:
                os.environ["VALIDATION_LOCK_HELD"] = prev_held
            if prev_token is None:
                os.environ.pop("VALIDATION_LOCK_TOKEN", None)
            else:
                os.environ["VALIDATION_LOCK_TOKEN"] = prev_token
        return

    try:
        timeout = float(os.environ.get("VALIDATION_LOCK_TIMEOUT_SECONDS", "900"))
    except ValueError:
        raise LockError(2, "VALIDATION_LOCK_TIMEOUT_SECONDS must be numeric")

    lock_dir = resolve_lock_dir(cwd=cwd)
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
                    holder = (
                        holder_path.read_text(encoding="utf-8").strip()
                        if holder_path.is_file()
                        else "unknown"
                    )
                    raise LockError(
                        75,
                        f"validation-lock: timed out after {timeout:g}s waiting for {lock_dir} (holder: {holder})",
                    )
                time.sleep(min(0.05, remaining))

        token = secrets.token_hex(16)
        holder_path.write_text(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "started_at": int(time.time()),
                    "command": list(command_desc),
                }
            )
            + "\n",
            encoding="utf-8",
        )
        prev_held = os.environ.get("VALIDATION_LOCK_HELD")
        prev_token = os.environ.get("VALIDATION_LOCK_TOKEN")
        os.environ["VALIDATION_LOCK_HELD"] = "1"
        os.environ["VALIDATION_LOCK_TOKEN"] = token
        try:
            yield dict(os.environ)
        finally:
            if prev_held is None:
                os.environ.pop("VALIDATION_LOCK_HELD", None)
            else:
                os.environ["VALIDATION_LOCK_HELD"] = prev_held
            if prev_token is None:
                os.environ.pop("VALIDATION_LOCK_TOKEN", None)
            else:
                os.environ["VALIDATION_LOCK_TOKEN"] = prev_token
            holder_path.unlink(missing_ok=True)
            fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def acquire_and_run(command: Sequence[str]) -> int:
    if not command:
        print("usage: lock.py -- <command> [args...]", file=sys.stderr)
        return 2
    try:
        with hold(command) as env:
            return run_child(command, env)
    except LockError as error:
        print(str(error), file=sys.stderr)
        return error.exit_code


def print_status(cwd: Path | None = None) -> int:
    lock_dir = resolve_lock_dir(cwd=cwd)
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


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if args == ["--assert-held"]:
        return 0 if is_held() else 1
    if args == ["--status"]:
        return print_status()
    if args == ["--self-test"]:
        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        return subprocess.call(
            [sys.executable, "-B", "-m", "unittest", str(SELF.parents[1] / "tests" / "test_lock.py")],
            env=env,
        )
    if args and args[0] == "--":
        return acquire_and_run(args[1:])
    print(
        "usage: lock.py -- <command> [args...] | --assert-held | --status",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
