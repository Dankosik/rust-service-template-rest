"""Shared primitives for repository automation scripts (stdlib-only)."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]

PG_ENV_VARS = (
    "PGHOSTADDR",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGDATABASE",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGAPPNAME",
    "PGOPTIONS",
    "PGPASSFILE",
)

_EPHEMERAL_COMPOSE_PORTS = {
    "POSTGRES_PORT": "127.0.0.1:0",
    "PGBOUNCER_PORT": "127.0.0.1:0",
    "NATS_PORT": "127.0.0.1:0",
    "VALKEY_PORT": "127.0.0.1:0",
    "VERSITYGW_PORT": "127.0.0.1:0",
    "KEYCLOAK_PORT": "127.0.0.1:0",
}


class ScriptError(RuntimeError):
    def __init__(self, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def clean_pg_env(extra: Mapping[str, str] | None = None) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k not in PG_ENV_VARS}
    if extra:
        env.update(extra)
    return env


def run(
    cmd: Sequence[str],
    *,
    cwd: Path = ROOT,
    env: Mapping[str, str] | None = None,
    capture: bool = False,
    check: bool = True,
    input_bytes: bytes | None = None,
    binary: bool = False,
) -> subprocess.CompletedProcess[Any]:
    result = subprocess.run(
        list(cmd),
        cwd=cwd,
        env=dict(env) if env is not None else None,
        input=input_bytes,
        capture_output=capture,
        text=not binary,
        check=False,
    )
    if check and result.returncode != 0:
        detail = ""
        if capture and result.stderr:
            detail = (
                result.stderr.decode("utf-8", errors="replace").strip()
                if binary
                else str(result.stderr).strip()
            )
        suffix = f"\n{detail}" if detail else ""
        raise ScriptError(f"command exited {result.returncode}: {' '.join(cmd)}{suffix}")
    return result


def git(*args: str, cwd: Path = ROOT, binary: bool = False) -> Any:
    return run(["git", "-C", os.fspath(cwd), *args], cwd=cwd, capture=True, binary=binary).stdout


def cargo_metadata(
    root: Path = ROOT,
    *,
    no_deps: bool = True,
    offline: bool = False,
    filter_platform: str | None = None,
) -> dict[str, Any]:
    cmd = ["cargo", "metadata", "--locked", "--format-version", "1", "--manifest-path", str(root / "Cargo.toml")]
    if no_deps:
        cmd.append("--no-deps")
    if offline:
        cmd.append("--offline")
    if filter_platform:
        cmd.extend(["--filter-platform", filter_platform])
    return json.loads(run(cmd, cwd=root, capture=True).stdout)


def read_service_meta(root: Path = ROOT) -> tuple[str, str]:
    pkg = tomllib.loads((root / "crates/service/Cargo.toml").read_text(encoding="utf-8")).get("package", {})
    name = pkg["name"]
    version = pkg["version"]
    if isinstance(version, dict) and version.get("workspace") is True:
        version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    if not isinstance(name, str) or not isinstance(version, str):
        raise ScriptError("unable to read service name/version from Cargo.toml", exit_code=2)
    return name, version


def read_mise_config(root: Path = ROOT) -> dict[str, Any]:
    path = root / "mise.toml"
    if not path.is_file():
        return {}
    return tomllib.loads(path.read_text(encoding="utf-8"))


def mise_env(name: str, root: Path = ROOT) -> str:
    if value := os.environ.get(name):
        return value
    env_table = read_mise_config(root).get("env", {})
    value = env_table.get(name, "")
    if not isinstance(value, str) or not value:
        raise ScriptError(f"missing {name} in environment and mise.toml [env]", exit_code=2)
    return value


def require_docker(*, allow_refusal: bool = False) -> None:
    try:
        probe = subprocess.run(["docker", "info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        available = probe.returncode == 0
    except OSError:
        available = False
    if available:
        return
    if os.environ.get("REQUIRE_DOCKER") == "1" or not allow_refusal:
        raise ScriptError("docker is required but unavailable", exit_code=1 if os.environ.get("REQUIRE_DOCKER") == "1" else 2)
    raise ScriptError("docker is unavailable; set REQUIRE_DOCKER=1 to fail instead of refusing", exit_code=2)


def wait_http_ready(url: str, *, container: str | None = None, timeout_s: int = 60, req_timeout_s: float = 3.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=req_timeout_s) as resp:
                if 200 <= resp.status < 300:
                    return
        except (urllib.error.URLError, TimeoutError, OSError):
            pass
        if container is not None:
            state = subprocess.run(
                ["docker", "inspect", "-f", "{{.State.Running}}", container],
                capture_output=True,
                text=True,
                check=False,
            )
            if state.returncode != 0 or state.stdout.strip() != "true":
                break
        time.sleep(0.5)
    raise ScriptError(f"endpoint did not become ready within {timeout_s}s: {url}")


class ComposeStack:
    """Ephemeral Docker Compose stack with guaranteed cleanup on exit and signals."""

    def __init__(self, prefix: str, *, root: Path = ROOT, compose_file: Path | None = None) -> None:
        self.root = root
        self.compose_file = compose_file or (root / "env/docker-compose.yml")
        self.project = f"{prefix}-{os.getpid()}-{int(time.time())}"
        self.active = False
        self._prev_handlers: dict[int, Any] = {}

    @property
    def network(self) -> str:
        return f"{self.project}_default"

    def _env(self, extra: Mapping[str, str] | None = None) -> dict[str, str]:
        env = clean_pg_env(_EPHEMERAL_COMPOSE_PORTS)
        if extra:
            env.update(extra)
        return env

    def _cmd(self, args: Sequence[str], *, profile: str | None = None) -> list[str]:
        cmd = ["docker", "compose", "-p", self.project, "-f", os.fspath(self.compose_file)]
        if profile:
            cmd.extend(["--profile", profile])
        cmd.extend(args)
        return cmd

    def __enter__(self) -> ComposeStack:
        def _handle(signum: int, _frame: Any) -> None:
            raise SystemExit(128 + signum)

        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            self._prev_handlers[sig] = signal.getsignal(sig)
            signal.signal(sig, _handle)
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        try:
            self.down(strict=exc_type is None)
        finally:
            for sig, handler in self._prev_handlers.items():
                signal.signal(sig, handler)
            self._prev_handlers.clear()

    def start(
        self,
        service: str,
        container_port: int,
        *,
        extra_env: Mapping[str, str] | None = None,
        profile: str | None = None,
    ) -> int:
        self.active = True
        env = self._env(extra_env)
        res = run(self._cmd(["up", "-d", service], profile=profile), cwd=self.root, env=env, check=False)
        if res.returncode != 0:
            run(self._cmd(["ps", "--all"], profile=profile), cwd=self.root, env=env, check=False)
            run(self._cmd(["logs", "--no-color", "--tail", "100", service], profile=profile), cwd=self.root, env=env, check=False)
            raise ScriptError(f"compose up failed for {service}")
        return self.port(service, container_port, extra_env=extra_env, profile=profile)

    def wait(
        self,
        service: str,
        port: int,
        *,
        timeout_s: int | None = None,
        extra_env: Mapping[str, str] | None = None,
        profile: str | None = None,
        ready_url: str | None = None,
        ready_timeout_s: int = 180,
    ) -> None:
        env = self._env(extra_env)
        if ready_url is not None:
            cid = run(self._cmd(["ps", "-q", service], profile=profile), cwd=self.root, env=env, capture=True).stdout.strip()
            try:
                wait_http_ready(f"http://127.0.0.1:{port}{ready_url}", container=cid or None, timeout_s=ready_timeout_s)
            except ScriptError:
                run(self._cmd(["logs", "--no-color", "--tail", "100", service], profile=profile), cwd=self.root, env=env, check=False)
                raise
            return
        wait_args = ["up", "-d", "--no-recreate", "--wait"]
        if timeout_s is not None:
            wait_args.extend(["--wait-timeout", str(timeout_s)])
        wait_args.append(service)
        res = run(self._cmd(wait_args, profile=profile), cwd=self.root, env=env, check=False)
        if res.returncode != 0:
            run(self._cmd(["ps", "--all"], profile=profile), cwd=self.root, env=env, check=False)
            run(self._cmd(["logs", "--no-color", "--tail", "100", service], profile=profile), cwd=self.root, env=env, check=False)
            raise ScriptError(f"compose wait failed for {service}")

    def up(
        self,
        service: str,
        container_port: int,
        *,
        timeout_s: int | None = None,
        extra_env: Mapping[str, str] | None = None,
        profile: str | None = None,
        ready_url: str | None = None,
        ready_timeout_s: int = 180,
    ) -> int:
        if ready_url is not None:
            port = self.start(service, container_port, extra_env=extra_env, profile=profile)
            self.wait(
                service,
                port,
                extra_env=extra_env,
                profile=profile,
                ready_url=ready_url,
                ready_timeout_s=ready_timeout_s,
            )
            return port
        self.active = True
        env = self._env(extra_env)
        up_args = ["up", "-d", "--wait"]
        if timeout_s is not None:
            up_args.extend(["--wait-timeout", str(timeout_s)])
        up_args.append(service)
        res = run(self._cmd(up_args, profile=profile), cwd=self.root, env=env, check=False)
        if res.returncode != 0:
            run(self._cmd(["ps", "--all"], profile=profile), cwd=self.root, env=env, check=False)
            run(self._cmd(["logs", "--no-color", "--tail", "100", service], profile=profile), cwd=self.root, env=env, check=False)
            raise ScriptError(f"compose up failed for {service}")
        return self.port(service, container_port, extra_env=extra_env, profile=profile)

    def restart(
        self,
        service: str,
        container_port: int,
        *,
        extra_env: Mapping[str, str] | None = None,
        timeout_s: int = 30,
    ) -> int:
        env = self._env(extra_env)
        run(self._cmd(["restart", "--timeout", "10", service]), cwd=self.root, env=env)
        return self.up(service, container_port, timeout_s=timeout_s, extra_env=extra_env)

    def port(
        self,
        service: str,
        container_port: int,
        *,
        extra_env: Mapping[str, str] | None = None,
        profile: str | None = None,
    ) -> int:
        out = run(
            self._cmd(["port", service, str(container_port)], profile=profile),
            cwd=self.root,
            env=self._env(extra_env),
            capture=True,
        ).stdout.strip()
        raw_port = out.splitlines()[0].rsplit(":", 1)[-1] if out else ""
        if not raw_port.isdigit():
            raise ScriptError(f"failed to resolve compose {service} port {container_port} (got {out!r})")
        return int(raw_port)

    def down(self, *, strict: bool = False) -> None:
        if not self.active:
            return
        self.active = False
        res = subprocess.run(
            self._cmd(["down", "-v", "--remove-orphans"], profile="*"),
            cwd=self.root,
            env=self._env(),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if strict and res.returncode != 0:
            raise ScriptError(f"compose cleanup incomplete for {self.project}")


def run_cli(fn: Any) -> int:
    try:
        return int(fn() or 0)
    except ScriptError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return exc.exit_code
