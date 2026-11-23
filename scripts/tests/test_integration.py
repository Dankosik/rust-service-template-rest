"""Unit tests for integration runner target filtering, nextest parallelism, and shared Postgres sqlx-check."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import integration, lib


class _FakeStack:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    def __enter__(self) -> _FakeStack:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.events.append("stack:down")

    def start(self, service: str, container_port: int, **_kwargs: object) -> int:
        self.events.append(f"start:{service}")
        return 10000 + container_port

    def wait(self, service: str, _port: int, **_kwargs: object) -> None:
        self.events.append(f"wait:{service}")

    def up(self, service: str, container_port: int, **_kwargs: object) -> int:
        self.events.append(f"up:{service}")
        return 10000 + container_port


class IntegrationRunnerTests(unittest.TestCase):
    def test_db_test_args_selects_only_integration_manifest_targets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "test").mkdir(parents=True)
            (root / "test/Cargo.toml").write_text(
                """
                [[test]]
                name = "postgres"
                path = "tests/postgres.rs"
                required-features = ["integration"]

                [[test]]
                name = "orders"
                path = "tests/orders.rs"
                required-features = ["integration"]

                # template:begin temporal:test-integration-db-args-temporal
                [[test]]
                name = "temporal"
                path = "tests/temporal.rs"
                required-features = ["integration"]
                # template:end temporal:test-integration-db-args-temporal

                [[test]]
                name = "outbound_http"
                path = "tests/outbound_http.rs"
                """,
                encoding="utf-8",
            )
            self.assertEqual(integration._db_test_args(root), ["--test", "postgres", "--test", "orders"])

    def test_cmd_db_uses_unified_nextest_overlaps_startup_and_reuses_postgres_for_sqlx_check(self) -> None:
        if not (lib.ROOT / "crates/infra-postgres").is_dir():
            self.skipTest("postgres profile not present")
        events: list[str] = []
        commands: list[list[str]] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            commands.append(list(cmd))
            events.append("run:" + " ".join(cmd[:4]))
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.object(integration.shutil, "which", return_value="/usr/bin/tool"),
            mock.patch.object(integration, "_verify_sqlx_cli"),
            mock.patch.dict(os.environ, {"INTEGRATION_DB_TEST_THREADS": "4"}, clear=False),
        ):
            integration.cmd_db([], with_sqlx_check=True)

        self.assertEqual(events[0], "start:postgres")
        compile_idx = events.index("run:cargo nextest run --locked")
        wait_idx = events.index("wait:postgres")
        self.assertLess(compile_idx, wait_idx, "compilation must overlap with container startup")
        self.assertIn("--no-run", commands[0])
        self.assertIn("integration-tests/integration", commands[0])
        self.assertNotIn("utility_values", " ".join(commands[0]))
        self.assertTrue(any(cmd[:3] == ["sqlx", "database", "create"] for cmd in commands))
        self.assertTrue(any(cmd[:4] == ["cargo", "sqlx", "prepare", "--workspace"] for cmd in commands))
        self.assertEqual(events.count("start:postgres"), 1, "sqlx-check must reuse the running Postgres container")

    def test_cmd_oauth_overlaps_keycloak_startup_with_compilation(self) -> None:
        if not (lib.ROOT / "crates/infra-oauth2-client-credentials").is_dir():
            self.skipTest("outbound-auth profile not present")
        events: list[str] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            events.append("compile" if "--no-run" in cmd else "test")
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.dict(os.environ, {}, clear=False),
        ):
            os.environ.pop("OAUTH_TEST_KEYCLOAK_URL", None)
            integration.cmd_oauth([])

        self.assertEqual(events, ["start:keycloak", "compile", "wait:keycloak", "test", "stack:down"])

    # template:begin temporal:test-integration-temporal
    def test_cmd_temporal_overlaps_startup_with_compilation_and_runs_both_targets(self) -> None:
        if not (lib.ROOT / "crates/infra-temporal").is_dir():
            self.skipTest("temporal profile not present")
        events: list[str] = []
        commands: list[tuple[list[str], dict[str, str]]] = []

        def fake_run(cmd: list[str], **kwargs: object) -> mock.MagicMock:
            env = kwargs.get("env")
            commands.append((list(cmd), dict(env) if isinstance(env, dict) else {}))
            events.append("compile" if "--no-run" in cmd else "test")
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.dict(os.environ, {}, clear=False),
        ):
            os.environ.pop("TEMPORAL_ADDRESS", None)
            integration.cmd_temporal([])

        self.assertEqual(
            events,
            ["start:temporal", "compile", "compile", "wait:temporal", "test", "test", "stack:down"],
        )
        self.assertEqual(commands[2][1]["TEMPORAL_ADDRESS"], "127.0.0.1:17233")
        self.assertIn("infra-temporal", commands[2][0])
        self.assertIn("integration-tests", commands[3][0])

    def test_cmd_compile_includes_both_temporal_targets(self) -> None:
        if not (lib.ROOT / "crates/infra-temporal").is_dir():
            self.skipTest("temporal profile not present")
        commands: list[list[str]] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            commands.append(list(cmd))
            return mock.MagicMock(returncode=0)

        with mock.patch.object(lib, "run", side_effect=fake_run):
            integration.cmd_compile("services")

        self.assertIn(
            ["cargo", "test", "--locked", "-p", "infra-temporal", "--features", "integration", "--test", "temporal", "--no-run"],
            commands,
        )
        self.assertIn(
            ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", "--test", "temporal", "--no-run"],
            commands,
        )
    # template:end temporal:test-integration-temporal


if __name__ == "__main__":
    unittest.main()
