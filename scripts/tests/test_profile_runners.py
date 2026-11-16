#!/usr/bin/env python3
"""Command admission and runner contract tests for scripts/profiles/."""

from __future__ import annotations

import http.server
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]


class ProfileRunners(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="profile-runners-")))
        (self.root / "scripts/profiles").mkdir(parents=True)
        (self.root / "scripts/verify").mkdir(parents=True)
        (self.root / "bin").mkdir()
        self.env = dict(os.environ, PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"])
        (self.root / "scripts/verify/lock.py").write_text("import sys\nsys.exit(0)\n", encoding="utf-8")

    def executable(self, path: Path, body: str) -> str:
        path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
        path.chmod(0o755)
        return str(path)

    def run_profile_script(
        self, name: str, *args: str, **settings: str
    ) -> subprocess.CompletedProcess[str]:
        env = dict(self.env)
        for key in (
            "INTEGRATION_COMPOSE_MANAGED",
            "REQUIRE_DOCKER",
            "DATABASE_URL",
            "PGBOUNCER_DATABASE_URL",
            "NATS_URL",
            "CACHE_URL",
            "OBJECT_STORAGE_TEST_ENDPOINT",
            "OAUTH_TEST_KEYCLOAK_URL",
        ):
            env.pop(key, None)
        env.update(settings)
        return subprocess.run(
            ["bash", str(self.root / "scripts/profiles" / name), *args],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    @unittest.skipUnless((ROOT / "scripts/profiles/grpc_check.sh").is_file(), "gRPC profile removed")
    def test_grpc_base_absence_requires_a_readable_tree(self) -> None:
        shutil.copy(ROOT / "scripts/profiles/grpc_check.sh", self.root / "scripts/profiles/grpc_check.sh")
        (self.root / "scripts/profiles/grpc_generate.sh").write_text('mkdir -p "$1"\n', encoding="utf-8")
        (self.root / "crates/grpc-contracts/src/generated").mkdir(parents=True)
        self.executable(self.root / "bin/buf", 'printf "%s\\n" "$*"')

        def git(*args: str) -> str:
            return subprocess.check_output(
                [
                    "git",
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "gc.auto=0",
                    *args,
                ],
                cwd=self.root,
                text=True,
            ).strip()

        git("init", "-q")
        git("commit", "--allow-empty", "-qm", "before protobuf")
        result = self.run_profile_script("grpc_check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("first addition", result.stdout)

        (self.root / "api/proto").mkdir(parents=True)
        (self.root / "api/proto/contract.proto").touch()
        git("add", "api")
        git("commit", "-qm", "protobuf base")
        result = self.run_profile_script("grpc_check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"breaking --against .git#ref={git('rev-parse', 'HEAD')}", result.stdout)

        tree = git("rev-parse", "HEAD^{tree}")
        (self.root / ".git/objects" / tree[:2] / tree[2:]).unlink()
        result = self.run_profile_script("grpc_check.sh", GRPC_BASE_REF="HEAD", BUF="buf")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("first addition", result.stdout)

    @unittest.skipUnless((ROOT / "scripts/profiles/integration.sh").is_file(), "integration runner removed")
    def test_database_proof_requires_profile_only_for_owned_fixtures(self) -> None:
        shutil.copy(ROOT / "scripts/profiles/integration.sh", self.root / "scripts/profiles/integration.sh")
        (self.root / "crates/infra-postgres").mkdir(parents=True)
        template_dir = self.root / "scripts/template"
        template_dir.mkdir(parents=True)
        (template_dir / "state.py").write_text(
            'import os, sys\nprint(os.environ["PROFILE"])\nsys.exit(int(os.environ["PROFILE_STATUS"]))\n',
            encoding="utf-8",
        )
        (self.root / "scripts/profiles/compose.sh").write_text(
            """
require_docker() { :; }
compose_postgres_up() { :; }
compose_pgbouncer_up() { :; }
compose_cleanup() { :; }
compose_up_service() { echo "compose up -d --wait $1" >> events; COMPOSE_LAST_PORT=4222; }
compose_postgres_host_dsn() { echo postgres; }
compose_pgbouncer_host_dsn() { echo pgbouncer; }
""",
            encoding="utf-8",
        )
        self.executable(self.root / "bin/cargo", "echo cargo >> events")
        events = self.root / "events"
        for managed, profile, status, success, nats in (
            ("0", "none", "1", False, False),
            ("0", "none", "0", True, False),
            ("0", "nats-jetstream", "0", True, True),
            ("1", "none", "1", True, False),
        ):
            with self.subTest(managed=managed, profile=profile, status=status):
                events.write_text("", encoding="utf-8")
                result = self.run_profile_script(
                    "integration.sh",
                    "db",
                    PROFILE=profile,
                    PROFILE_STATUS=status,
                    INTEGRATION_COMPOSE_MANAGED=managed,
                    DATABASE_URL="external-postgres",
                    PGBOUNCER_DATABASE_URL="external-pgbouncer",
                )
                self.assertEqual(result.returncode == 0, success, result.stderr)
                recorded = events.read_text(encoding="utf-8").splitlines()
                self.assertEqual("cargo" in recorded, success)
                self.assertEqual("compose up -d --wait nats" in recorded, nats)

    @unittest.skipUnless((ROOT / "scripts/profiles/integration.sh").is_file(), "integration runner removed")
    def test_compile_retained_and_docker_refusal_modes(self) -> None:
        shutil.copy(ROOT / "scripts/profiles/integration.sh", self.root / "scripts/profiles/integration.sh")
        self.executable(self.root / "bin/cargo", 'printf "%s\\n" "$*" >> cargo-calls')
        self.executable(self.root / "bin/docker", "exit 1")
        calls = self.root / "cargo-calls"

        # Empty capability set exits 0 cleanly without compose.sh or Docker.
        empty = self.run_profile_script("integration.sh", "--compile-retained")
        self.assertEqual(empty.returncode, 0, empty.stderr)
        self.assertFalse(calls.exists())

        # OAuth + cache subset compiles without compose.sh or Docker.
        (self.root / "crates/infra-cache").mkdir(parents=True)
        (self.root / "crates/infra-oauth2-client-credentials").mkdir(parents=True)
        subset = self.run_profile_script("integration.sh", "--compile-retained")
        self.assertEqual(subset.returncode, 0, subset.stderr)
        recorded = calls.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(recorded), 2, recorded)
        self.assertTrue(any("infra-cache" in line and "--no-run" in line for line in recorded))
        self.assertTrue(any("infra-oauth2-client-credentials" in line and "--no-run" in line for line in recorded))

        # --compile-services compiles service suites only (not db or oauth).
        calls.unlink()
        (self.root / "crates/infra-postgres").mkdir(parents=True)
        svc_only = self.run_profile_script("integration.sh", "--compile-services")
        self.assertEqual(svc_only.returncode, 0, svc_only.stderr)
        svc_recorded = calls.read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(svc_recorded), 1, svc_recorded)
        self.assertIn("infra-cache", svc_recorded[0])

        # REQUIRE_DOCKER unset refuses with exit 2; REQUIRE_DOCKER=1 fails with exit 1.
        refused = self.run_profile_script("integration.sh", "cache")
        self.assertEqual(refused.returncode, 2, refused.stderr)
        failed = self.run_profile_script("integration.sh", "cache", REQUIRE_DOCKER="1")
        self.assertEqual(failed.returncode, 1, failed.stderr)

        # compile-only URL without --no-run is rejected.
        (self.root / "crates/infra-messaging").mkdir(parents=True)
        bad_compile_url = self.run_profile_script("integration.sh", "messaging", NATS_URL="compile-only")
        self.assertEqual(bad_compile_url.returncode, 2)

    @unittest.skipUnless((ROOT / "scripts/profiles/integration.sh").is_file(), "integration runner removed")
    def test_oauth_wait_http_ready_ignores_proxy_and_stops_when_container_exits(self) -> None:
        shutil.copy(ROOT / "scripts/profiles/integration.sh", self.root / "scripts/profiles/integration.sh")
        (self.root / "crates/infra-oauth2-client-credentials").mkdir(parents=True, exist_ok=True)
        (self.root / "tools").mkdir(parents=True, exist_ok=True)
        (self.root / "tools/versions.env").write_text("KEYCLOAK_IMAGE=quay.io/keycloak/keycloak@sha256:0000\n", encoding="utf-8")
        self.executable(self.root / "bin/cargo", 'printf "%s\\n" "$*" >>"$PWD/oauth-cargo"')

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                self.send_response(200)
                self.end_headers()

            def log_message(self, _format: str, *_args: object) -> None:
                return

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.addCleanup(server.server_close)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.shutdown)
        port = server.server_address[1]

        self.executable(
            self.root / "bin/docker",
            f'case "$1" in info|rm|logs) exit 0 ;; run) echo c1 ;; port) echo "127.0.0.1:{port}" ;; inspect) echo true ;; esac',
        )
        ok = self.run_profile_script(
            "integration.sh", "oauth", HTTP_PROXY="http://127.0.0.1:1", http_proxy="http://127.0.0.1:1"
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)

        self.executable(
            self.root / "bin/docker",
            'case "$1" in info|rm|logs) exit 0 ;; run) echo c2 ;; port) echo "127.0.0.1:1" ;; inspect) echo false ;; esac',
        )
        dead = self.run_profile_script("integration.sh", "oauth")
        self.assertEqual(dead.returncode, 1)


if __name__ == "__main__":
    unittest.main()
