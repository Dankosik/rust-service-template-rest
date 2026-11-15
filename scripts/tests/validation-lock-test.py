#!/usr/bin/env python3
"""Self-tests for scripts/ci/validation-lock.py."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts/ci/validation-lock.sh"


def clean_env(lock_dir: Path, **extra: str) -> dict[str, str]:
    base = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("VALIDATION_LOCK_") and key != "CI"
    }
    return {**base, "VALIDATION_LOCK_DIR": str(lock_dir), **extra}


class ValidationLockTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="validation-lock-test-")))
        self.lock_dir = self.temp / "lock"

    def test_assert_held_and_reentrant_invocation(self) -> None:
        outside = subprocess.run(
            ["bash", str(ENTRY), "--assert-held"],
            env=clean_env(self.lock_dir),
            capture_output=True,
            text=True,
        )
        self.assertEqual(outside.returncode, 1)

        inside = subprocess.run(
            [
                "bash",
                str(ENTRY),
                "--",
                "bash",
                "-c",
                f'bash "{ENTRY}" --assert-held && bash "{ENTRY}" -- printf "%s" "$VALIDATION_LOCK_TOKEN"',
            ],
            env=clean_env(self.lock_dir),
            capture_output=True,
            text=True,
        )
        self.assertEqual(inside.returncode, 0, inside.stderr)
        self.assertEqual(len(inside.stdout), 32)

    def test_status_and_timeout_when_contended(self) -> None:
        free = subprocess.run(
            ["bash", str(ENTRY), "--status"],
            env=clean_env(self.lock_dir),
            capture_output=True,
            text=True,
        )
        self.assertEqual(free.returncode, 0)
        self.assertEqual(free.stdout.strip(), "free")

        ready_file = self.temp / "ready"
        holder = subprocess.Popen(
            [
                "bash",
                str(ENTRY),
                "--",
                sys.executable,
                "-c",
                f"import pathlib, time; pathlib.Path({str(ready_file)!r}).write_text('ok'); time.sleep(5)",
            ],
            env=clean_env(self.lock_dir),
        )
        try:
            deadline = time.monotonic() + 3.0
            while not ready_file.is_file():
                self.assertLess(time.monotonic(), deadline, "holder never started")
                time.sleep(0.02)

            status = subprocess.run(
                ["bash", str(ENTRY), "--status"],
                env=clean_env(self.lock_dir),
                capture_output=True,
                text=True,
            )
            self.assertEqual(status.returncode, 0)
            payload = json.loads(status.stdout)
            self.assertIn("pid", payload)

            contended = subprocess.run(
                ["bash", str(ENTRY), "--", "true"],
                env=clean_env(self.lock_dir, VALIDATION_LOCK_TIMEOUT_SECONDS="0.1"),
                capture_output=True,
                text=True,
            )
            self.assertEqual(contended.returncode, 75)
            self.assertIn("timed out", contended.stderr)
        finally:
            holder.terminate()
            holder.wait(timeout=5)

    def test_serializes_concurrent_processes_and_propagates_exit_code(self) -> None:
        log = self.temp / "events.txt"
        snippet = (
            "import pathlib, sys, time; "
            f"p = pathlib.Path({str(log)!r}); "
            "f = p.open('a'); f.write(f'start:{sys.argv[1]}\\n'); f.flush(); "
            "time.sleep(0.15); "
            "f.write(f'end:{sys.argv[1]}\\n'); f.close()"
        )
        first = subprocess.Popen(
            ["bash", str(ENTRY), "--", sys.executable, "-c", snippet, "a"],
            env=clean_env(self.lock_dir),
        )
        second = subprocess.Popen(
            ["bash", str(ENTRY), "--", sys.executable, "-c", snippet, "b"],
            env=clean_env(self.lock_dir),
        )
        self.assertEqual(first.wait(timeout=5), 0)
        self.assertEqual(second.wait(timeout=5), 0)
        events = log.read_text(encoding="utf-8").splitlines()
        self.assertIn(events, (["start:a", "end:a", "start:b", "end:b"], ["start:b", "end:b", "start:a", "end:a"]))

        failing = subprocess.run(
            ["bash", str(ENTRY), "--", sys.executable, "-c", "raise SystemExit(17)"],
            env=clean_env(self.lock_dir),
            capture_output=True,
            text=True,
        )
        self.assertEqual(failing.returncode, 17)


if __name__ == "__main__":
    unittest.main()
