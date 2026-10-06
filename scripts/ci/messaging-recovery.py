#!/usr/bin/env python3
"""Owned R3/TLS fixture and one-record DLQ controller, not a delivery engine.

No broker endpoint is accepted by a mutation command. All clients, credentials,
stores and the internal network originate here. The lock serializes controllers;
the original broker lifetime, retained across ambiguity, fences native deletion.
"""

import argparse
import base64
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
COMPOSE = ROOT / "test/fixtures/messaging-recovery-compose.yml"
GIB = 1024 ** 3
SESSION_SECONDS = 900
ADMISSION_COOLDOWN_SECONDS = 60
SERVICES = ("nats1", "nats2", "nats3", "client")
LABEL = "recovery.generation"
STARTUP_PHASES = {"tls_material", "auth_generation", "server_config", "native_config_parse",
                  "broker_up", "resource_capture", "verify", "execution_inputs", "topology", "complete"}
BROKER_LOG_BYTES = 128 * 1024
# Fixed fatal prefixes from nats-server v2.15.0 server/server.go. Matching text
# never enters evidence; an unknown message remains an unclassified failure.
BROKER_FATAL_PREFIXES = (
    ("Can't set system account:", "system_account"),
    ("Could not start resolver:", "account_resolver"),
    ("Can't start JetStream:", "jetstream_startup"),
    ("Not allowed to enable JetStream on the system account", "system_account_jetstream"),
    ("Error listening on port:", "client_listener"),
    ("Can't start monitoring:", "monitoring"),
    ("Error starting monitor on ", "monitoring"),
    ("Could not write pidfile:", "pidfile"),
)
BROKER_FATAL_LINE = re.compile(
    r"^(?:\[\d+\]\s+)?(?:\d{4}/\d{2}/\d{2}\s+\d{2}:\d{2}:\d{2}(?:\.\d+)?\s+)?\[FTL\]\s+(.*)$")
# NewServer errors are printed by main.go before ConfigureLogger. These fixed
# prefixes come from v2.15.0 server.go, jwt.go, auth.go and jetstream.go.
BROKER_CONSTRUCTOR_PREFIXES = (
    ("Error processing trusted operator keys", "constructor_operator_keys"),
    ("operators require an account resolver to be configured", "constructor_resolver_missing"),
    ("operators do not allow Accounts to be configured directly", "constructor_static_accounts"),
    ("operators do not allow users to be configured directly", "constructor_static_users"),
    ("operators do not allow authorization callouts to be configured directly", "constructor_static_callouts"),
    ("conflicting options for 'TrustedKeys' and 'TrustedOperators'", "constructor_operator_conflict"),
    ("system_account in config and operator JWT must be identical", "constructor_system_account_mismatch"),
    ("using nats based account resolver - the system account needs to be specified", "constructor_system_account_missing"),
    ("trusted Keys ", "constructor_operator_keys"),
    ("pinned account key ", "constructor_pinned_account_key"),
    ("default sentinel requires operators and accounts", "constructor_sentinel"),
    ("default sentinel JWT not valid", "constructor_sentinel"),
    ("default sentinel must be a bearer token", "constructor_sentinel"),
    ("error resolving system account:", "constructor_system_account_resolution"),
    ("resolver preloads only available for writeable resolver types MEM/DIR/CACHE_DIR", "constructor_resolver_preloads"),
    ("preload account error for ", "constructor_account_preload"),
    ("invalid permissions for user ", "constructor_user_permissions"),
    ("invalid permissions for nkey ", "constructor_nkey_permissions"),
    ("max_payload (", "constructor_payload_limit"),
    ("server name cannot contain spaces", "constructor_server_name"),
    ("lame duck grace period (", "constructor_shutdown_options"),
    ("jetstream cluster requires `server_name` to be set", "constructor_jetstream_cluster"),
    ("jetstream cluster requires `cluster.name` to be set", "constructor_jetstream_cluster"),
    ("jetstream max catchup cannot be negative", "constructor_jetstream_options"),
    ("invalid domain name:", "constructor_jetstream_domain"),
    ("default_js_domain contains ", "constructor_jetstream_domain"),
    ("in non operator mode, `default_js_domain` references non existing account ", "constructor_jetstream_domain"),
)
BROKER_CONSTRUCTOR_LINE = re.compile(r"^nats-server: (.+)$")
BROKER_OPERATOR_VERSION_ERROR = re.compile(
    r"operator .+ (?:expects version .+ got error instead: .+|expected (?:major|minor|update) version \d+ > server (?:major|minor|update) version \d+)")
CONFIG_PATH_ROLES = {
    "/session/node.conf": ("node", "node_config"),
    "/auth/server.conf": ("auth", "auth_config"),
    "/auth/operator.jwt": ("auth", "operator_jwt"),
    "/tls/server.crt": ("node", "server_certificate"),
    "/tls/server.key": ("node", "server_key"),
    "/tls/ca.crt": ("node", "certificate_authority"),
}
CONFIG_FIELDS = frozenset({
    "server_name", "port", "http", "max_payload", "include", "tls", "jetstream", "store_dir",
    "max_file_store", "max_memory_store", "sync_interval", "cluster", "name", "routes",
    "cert_file", "key_file", "ca_file", "verify", "operator", "system_account", "resolver", "resolver_preload",
})
# conf/lex.go and conf/parse.go expose these grammar families without a stable
# error type. Export only the fixed code, never the unexpected token or value.
CONFIG_ERROR_PREFIXES = (
    ("Unexpected EOF", "unexpected_eof"),
    ("Expected a top-level value to end", "top_level_terminator"),
    ("Expected a block-level value to end", "block_value_terminator"),
    ("Expected a block-level to end", "block_terminator"),
    ("Unexpected key separator", "unexpected_key_separator"),
    ("Expected include value", "include_value"),
    ("Expected value but found new line", "missing_value"),
    ("Unexpected array value terminator", "array_value_terminator"),
    ("Expected an array value terminator", "array_terminator"),
    ("Unexpected array end", "unexpected_array_end"),
    ("Unexpected map value terminator", "map_value_terminator"),
    ("Expected a map value terminator", "map_terminator"),
    ("Invalid escape character", "invalid_escape"),
    ("Expected two hexadecimal digits", "invalid_hex_escape"),
    ("Floats must", "invalid_float"),
    ("Expected a digit but", "invalid_number"),
    ("All ISO8601 dates", "invalid_datetime"),
    ("Expected digit in ISO8601 datetime", "invalid_datetime"),
    ("BUG in lexer:", "lexer_internal"),
    ("unknown field", "unknown_field"),
    ("error parsing tls config", "tls_config"),
    ("missing 'key_file' in TLS configuration", "tls_key_missing"),
    ("missing 'cert_file' in TLS configuration", "tls_certificate_missing"),
    ("error parsing X509 certificate/key pair", "tls_key_pair"),
    ("error parsing certificate", "tls_certificate"),
    ("failed to parse root ca certificate", "tls_certificate_authority"),
    ("unsupported minimum TLS version:", "tls_version"),
)
CONFIG_IO_CODES = {"no such file or directory": "io_missing", "permission denied": "io_permission_denied",
                   "read-only file system": "io_read_only", "not a directory": "io_not_directory",
                   "is a directory": "io_is_directory"}


class Refused(RuntimeError):
    """Only closed, credential-free reasons cross the command boundary."""

    def __init__(self, reason, *, command_class="controller", exit_code=None):
        super().__init__(reason)
        self.command_class = command_class
        self.exit_code = exit_code


def require(condition, reason):
    if not condition:
        raise Refused(reason)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def load(path):
    require(path.stat().st_size <= 4 * 1024 * 1024, "manifest_too_large")
    return json.loads(path.read_bytes())


def atomic(path, value, exclusive=False):
    """Commit private state before an effect; no-clobber immutable selections."""
    raw = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        if exclusive:
            os.link(temporary, path)
            os.unlink(temporary)
        else:
            os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def private_text(path, text):
    with path.open("x") as output:
        os.chmod(path, 0o600)
        output.write(text)


def run(command, *, timeout=30, env=None, input=None, check=True, tick=None, command_class=None,
        private_output=None, private_output_limit=None):
    """Finite native command; suppress provider stderr and secret-bearing argv."""
    if command_class is None:
        command_class = {"openssl": "openssl", "yq": "config_query"}.get(command[0], "native_command")
        if command[0] == "docker" and len(command) > 1:
            command_class = {name: "docker_" + name for name in
                             ("compose", "exec", "inspect", "image", "network", "volume", "ps")}.get(command[1], "docker")
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        with subprocess.Popen(command, stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
                              stdout=stdout, stderr=stderr, env=env, start_new_session=True) as process:
            if input is not None:
                process.stdin.write(input)
                process.stdin.close()
            until = time.monotonic() + timeout
            try:
                while process.poll() is None:
                    if time.monotonic() >= until:
                        raise Refused("native_command_deadline")
                    require(os.fstat(stdout.fileno()).st_size <= 4 * 1024 * 1024
                            and os.fstat(stderr.fileno()).st_size <= 4 * 1024 * 1024, "native_output_limit")
                    if tick:
                        tick()
                    time.sleep(0.2)
            except BaseException as error:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=2)
                if isinstance(error, Refused):
                    error.command_class = command_class
                    error.exit_code = process.returncode
                raise
            finally:
                if private_output is not None:
                    # Parser text can contain configuration values. It is bounded,
                    # private and outside the evidence upload whitelist.
                    with contextlib.suppress(OSError):
                        with private_output.open("xb") as output:
                            os.chmod(private_output, 0o600)
                            remaining = private_output_limit
                            for stream in (stdout, stderr):
                                if remaining is not None and remaining <= 0:
                                    break
                                stream.seek(0)
                                raw = stream.read(4 * 1024 * 1024 if remaining is None else remaining - 1)
                                output.write(raw)
                                output.write(b"\n")
                                if remaining is not None:
                                    remaining -= len(raw) + 1
            stdout.seek(0)
            data = stdout.read(4 * 1024 * 1024 + 1)
            if len(data) > 4 * 1024 * 1024:
                raise Refused("native_output_limit", command_class=command_class, exit_code=process.returncode)
            if check and process.returncode != 0:
                raise Refused("native_command_failed", command_class=command_class, exit_code=process.returncode)
            return process.returncode, data


def native_json(command, **kwargs):
    return json.loads(run(command, **kwargs)[1])


def broker_fatal(raw):
    for number, line in enumerate(raw.decode(errors="replace").splitlines(), 1):
        matched = BROKER_FATAL_LINE.fullmatch(line)
        if matched:
            category = next((category for prefix, category in BROKER_FATAL_PREFIXES
                             if matched[1].startswith(prefix)), "unclassified")
            return {"category": category, "line": number}
        constructor = BROKER_CONSTRUCTOR_LINE.fullmatch(line)
        if constructor:
            category = next((category for prefix, category in BROKER_CONSTRUCTOR_PREFIXES
                             if constructor[1].startswith(prefix)), "unclassified")
            if category == "unclassified" and BROKER_OPERATOR_VERSION_ERROR.fullmatch(constructor[1]):
                category = "constructor_operator_version"
            return {"category": category, "line": number}
    return {"category": "unclassified", "line": None}


def generated_config_field(directory, role, line, column):
    relative = {"node": "nats1.conf", "auth": "auth/server.conf"}.get(role)
    if relative is None or line is None or line < 1:
        return "unknown"
    source = directory / relative
    try:
        metadata = source.lstat()
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != directory.stat().st_uid
                or metadata.st_mode & 0o077 or not source.resolve().is_relative_to(directory.resolve())):
            return "unknown"
        with source.open("rb") as generated:
            raw = generated.read(4 * 1024 * 1024 + 1)
        if len(raw) > 4 * 1024 * 1024:
            return "unknown"
        lines = raw.decode().splitlines()
        if line > len(lines):
            return "unknown"
        text = lines[line - 1]
        if column is not None and not 1 <= column <= len(text) + 1:
            return "unknown"
        fields = [match for match in re.finditer(r"(?:^|[,{])\s*([a-z_]+)(?=\s*[:{]|\s)", text)
                  if match[1] in CONFIG_FIELDS]
        if column is not None:
            fields = [match for match in fields if match.start(1) < column]
        return fields[-1][1] if fields and column is not None else fields[0][1] if fields else "unknown"
    except (OSError, UnicodeError):
        return "unknown"


def config_diagnostic(text, directory):
    empty = {"config_role": "unknown", "line": None, "column": None, "field": "unknown",
             "parser_code": "unknown", "path_role": "unknown"}
    for raw_line in text.splitlines():
        body = raw_line.removeprefix("nats-server: ")
        role, path_role, line, column = "node", "node_config", None, None
        included = re.fullmatch(r"error parsing include file '([^'\r\n]+)', (.*)", body)
        if included:
            include_path = "/auth/server.conf" if included[1] == "../auth/server.conf" else included[1]
            role, path_role = (CONFIG_PATH_ROLES[include_path] if include_path in
                               {"/session/node.conf", "/auth/server.conf"} else ("other", "other"))
            body = included[2]
        positioned = re.fullmatch(r"(/session/node\.conf|/auth/server\.conf):(\d{1,10})(?::(\d{1,10}))?:\s*(.*)", body)
        if positioned:
            role, path_role = CONFIG_PATH_ROLES[positioned[1]]
            line = int(positioned[2])
            column = int(positioned[3]) if positioned[3] else None
            body = positioned[4]
        lexical = re.fullmatch(r"Parse error on line (\d{1,10}): '(.*)'", body)
        if lexical:
            line, body = int(lexical[1]), lexical[2]
        variable = re.fullmatch(r"variable reference for '[^'\r\n]*' on line (\d{1,10}) (can not be found|could not be parsed:.*)", body)
        code = next((code for prefix, code in CONFIG_ERROR_PREFIXES if body.startswith(prefix)), "unknown")
        if lexical and code == "unknown":
            code = "lexer_unknown"
        if variable:
            line = int(variable[1])
            code = "variable_not_found" if variable[2] == "can not be found" else "variable_parse_failed"
        io_error = None
        for known_path, known_roles in CONFIG_PATH_ROLES.items():
            io_error = re.search(r"\b(?:open|read|stat) " + re.escape(known_path) + r": (" +
                                 "|".join(re.escape(reason) for reason in CONFIG_IO_CODES) + r")$", body)
            if io_error:
                if not positioned and not included:
                    role, path_role = known_roles
                else:
                    path_role = known_roles[1]
                code = CONFIG_IO_CODES[io_error[1]]
                break
        if positioned or lexical or variable or io_error:
            return {"config_role": role, "line": line, "column": column,
                    "field": generated_config_field(directory, role, line, column),
                    "parser_code": code, "path_role": path_role}
        if included:
            return {**empty, "config_role": role, "path_role": path_role, "parser_code": "include_unclassified"}
        if code != "unknown" and raw_line.startswith("nats-server: "):
            return {**empty, "parser_code": code}
    return empty


def resource_snapshot(path, deadline):
    def budget():
        remaining = deadline - time.time()
        require(remaining > 0, "resource_admission_deadline")
        return min(30, remaining)

    free = shutil.disk_usage(path).free
    cpus = os.cpu_count() or 1
    load_one = os.getloadavg()[0]
    info = native_json(["docker", "info", "--format", "{{json .}}"], timeout=budget())
    running = run(["docker", "ps", "-q"], timeout=budget())[1].decode().split()
    competitors = native_json(["docker", "inspect", *running], timeout=budget()) if running else []
    memory_limits = [item["HostConfig"]["Memory"] for item in competitors]
    return {"sampled_at": time.time(), "free_disk_bytes": free, "host_cpus": cpus, "host_load_one": load_one,
            "docker_memory_bytes": info["MemTotal"], "docker_cpus": info["NCPU"],
            "competing_containers": len(competitors), "competing_memory_limits_bytes": sum(memory_limits),
            "container_memory_limits_bounded": all(memory_limits),
            "available_container_memory_bytes": info["MemTotal"] - sum(memory_limits),
            "fixture_memory_limit_bytes": 2688 * 1024 ** 2, "fixture_cpu_limit": 2.5,
            "retained_data_limit_bytes": GIB, "disk_floor_bytes": 2 * GIB,
            "required_free_disk_bytes": 3 * GIB, "required_container_memory_bytes": 3 * GIB + GIB // 2,
            "required_docker_cpus": 3, "max_host_load_one": cpus}


def resource_admission(path, *, deadline, cooldown=False):
    samples = []
    for attempt in range(2 if cooldown else 1):
        snapshot = resource_snapshot(path, deadline)
        conditions = (
            (snapshot["free_disk_bytes"] >= 3 * GIB, "fixture_requires_2GiB_floor_plus_1GiB_data"),
            (snapshot["container_memory_limits_bounded"], "unbounded_container_memory"),
            (snapshot["available_container_memory_bytes"] >= 3 * GIB + GIB // 2, "insufficient_container_memory"),
            (snapshot["docker_cpus"] >= 3, "insufficient_docker_cpu_capacity"),
            (snapshot["host_load_one"] <= snapshot["host_cpus"], "host_load_above_cpu_count"),
        )
        reason = next((reason for admitted, reason in conditions if not admitted), None)
        wait = (ADMISSION_COOLDOWN_SECONDS if cooldown and attempt == 0
                and reason == "host_load_above_cpu_count" else 0)
        record = {"attempt": attempt + 1, "admitted": reason is None, "reason": reason,
                  "cooldown_seconds": wait, "deadline": deadline, "sample": snapshot}
        samples.append(record)
        # Emit only the numeric resource projection, never raw Docker metadata.
        print(json.dumps({"event": "messaging_recovery_resource_admission", **record}, sort_keys=True),
              file=sys.stderr, flush=True)
        if reason is None:
            return {**snapshot, "samples": samples}
        if wait:
            require(time.time() + wait < deadline, "resource_admission_deadline")
            time.sleep(wait)
        else:
            raise Refused(reason)
    raise Refused("resource_admission_unresolved")


def image_pin(service):
    return run(["yq", "-r", f".services.{service}.image", str(ROOT / "env/docker-compose.yml")])[1].decode().strip()


def artifact_identity(path, architecture):
    """Refuse host Mach-O and wrong-architecture artifacts before any startup."""
    with path.open("rb") as binary:
        header = binary.read(20)
    machines = {"amd64": 62, "arm64": 183}
    require(len(header) == 20 and header[:4] == b"\x7fELF" and header[4:6] == b"\x02\x01",
            "artifact_requires_64bit_little_endian_linux_elf")
    require(architecture in machines and int.from_bytes(header[18:20], "little") == machines[architecture],
            "artifact_runtime_architecture_mismatch")
    return {"sha256": sha(path.read_bytes()), "architecture": architecture, "format": "ELF64"}


class Session:
    def __init__(self, path):
        self.path = path.resolve()
        self.data = load(self.path / "session.json")
        require(self.data["version"] == 1, "session_version")
        require(re.fullmatch(r"[a-f0-9]{24}", self.data["generation"]), "session_generation")
        require(self.data["project"] == "messaging-recovery-" + self.data["generation"], "session_project")
        require(self.data["directory"] == str(self.path), "session_moved")
        require(self.path.stat().st_uid == os.getuid() and self.path.stat().st_mode & 0o077 == 0,
                "session_directory_not_private")
        self.last_resource_check = 0.0

    def save(self):
        atomic(self.path / "session.json", self.data)

    @contextlib.contextmanager
    def lock(self):
        with (self.path / "controller.lock").open("a") as owner:
            try:
                fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise Refused("controller_busy") from error
            yield

    def compose(self, *args, **kwargs):
        # Make exports recipe inputs; only this session's private env file owns
        # RECOVERY_* interpolation, including a nested rehearsal's bind source.
        environment = {key: value for key, value in os.environ.items() if not key.startswith("RECOVERY_")}
        return run(["docker", "compose", "--env-file", str(self.path / "compose.env"),
                    "-p", self.data["project"], "-f", str(COMPOSE), *args], env=environment, **kwargs)

    def startup_phase(self, phase):
        require(phase in STARTUP_PHASES, "invalid_startup_phase")
        self.data["startup_phase"] = phase
        self.save()

    def remember_startup_failure(self, error):
        if "startup_failure" not in self.data:
            phase = self.data.get("startup_phase")
            self.data["startup_failure"] = {
                "phase": phase if phase in STARTUP_PHASES else "unknown",
                "command_class": error.command_class if isinstance(error, Refused) else "controller",
                "exit_code": error.exit_code if isinstance(error, Refused) else None,
            }
            self.save()

    @contextlib.contextmanager
    def startup_diagnostics(self):
        try:
            yield
        except BaseException as error:
            try:
                self.remember_startup_failure(error)
                containers = {}
                state_status = "observed"
                identities = {item["id"]: name for name, item in self.data["containers"].items()}
                observed = []
                try:
                    observed = native_json(["docker", "inspect", *identities], timeout=5) if identities else []
                    for item in observed:
                        require(item["Id"] in identities, "diagnostic_container_not_owned")
                        state = item["State"]
                        containers[identities[item["Id"]]] = {
                            "Status": state.get("Status"), "Running": state.get("Running"),
                            "ExitCode": state.get("ExitCode"), "OOMKilled": state.get("OOMKilled"),
                            "Health.Status": state.get("Health", {}).get("Status"),
                        }
                except (Refused, OSError, ValueError, KeyError):
                    state_status = "unavailable"
                result = {**self.data["startup_failure"], "container_state_status": state_status, "containers": containers}
                if result["phase"] == "broker_up":
                    result["broker_fatals"] = self.capture_broker_fatals(observed)
                self.evidence("startup-failure", result)
            except (Refused, OSError, ValueError, KeyError):
                print(json.dumps({"event": "startup_failure_evidence_unavailable"}), file=sys.stderr)
            raise

    def capture_broker_fatals(self, observed):
        by_id = {item["Id"]: item for item in observed}
        result = {}
        for name in ("nats1", "nats2", "nats3"):
            original = self.data["containers"].get(name)
            if original is None:
                continue
            report = {"category": "unclassified", "line": None, "capture_exit_code": None, "broker_exit_code": None}
            result[name] = report
            try:
                item = by_id[original["id"]]
                labels = item["Config"]["Labels"]
                require(labels.get(LABEL) == self.data["generation"]
                        and labels.get("com.docker.compose.project") == self.data["project"]
                        and labels.get("com.docker.compose.service") == name, "diagnostic_broker_not_owned")
                report["broker_exit_code"] = item["State"].get("ExitCode")
                remaining = self.data["expires_at"] - time.time()
                require(remaining > 0, "diagnostic_deadline")
                output = self.path / f"broker-{name}.output"
                require(not output.exists(), "diagnostic_output_already_exists")
                code, _ = run(["docker", "logs", "--tail", "80", original["id"]],
                              timeout=min(5, remaining), check=False, command_class="broker_logs",
                              private_output=output, private_output_limit=BROKER_LOG_BYTES)
                report["capture_exit_code"] = code
                if code == 0:
                    with output.open("rb") as private:
                        report.update(broker_fatal(private.read(BROKER_LOG_BYTES)))
            except (Refused, OSError, ValueError, KeyError) as error:
                if isinstance(error, Refused):
                    report["capture_exit_code"] = error.exit_code
        return result

    def test_native_config(self):
        remaining = self.data["expires_at"] - time.time()
        require(remaining > 25, "insufficient_config_test_budget")
        output = self.path / "native-config-test.output"
        code, _ = self.compose("run", "--rm", "--no-deps", "-T", "--pull", "never",
                               "--name", self.data["project"] + "-config-test", "nats1",
                               "timeout", "-k", "5", "20", "nats-server", "-t", "-c", "/session/node.conf",
                               timeout=min(30, remaining), check=False,
                               command_class="nats_config_test", private_output=output)
        # Only a known generated-file location and integer line can escape the
        # private parser text; neither the failing line nor error message does.
        text = ""
        if output.exists():
            with output.open("rb") as private:
                text = private.read(8 * 1024 * 1024 + 2).decode(errors="replace")
        diagnostic = config_diagnostic(text, self.path)
        success = re.compile(r"^nats-server: configuration file /session/node\.conf is valid \([^\r\n()]+\)$")
        lines = text.splitlines()
        config_success = any(success.fullmatch(line) for line in lines)
        unknown_flag = "flag provided but not defined" in text
        usage = any(line.startswith("Usage: nats-server") for line in lines)
        native_refusal = (diagnostic["line"] is not None or diagnostic["parser_code"] != "unknown"
                          or any(line.startswith("nats-server: ") and not success.fullmatch(line) for line in lines))
        # NATS main.go's usage callback exits zero even for an unknown flag.
        # Only its explicit canonical -t success message establishes this check.
        valid = code == 0 and config_success and not (unknown_flag or usage or native_refusal)
        category = "valid" if valid else "native_test_failed"
        if not valid and diagnostic["config_role"] in {"node", "auth"}:
            category = diagnostic["config_role"] + "_config_rejected"
        elif unknown_flag:
            category = "unsupported_config_test_flag"
        elif usage:
            category = "config_test_usage"
        elif code == 0 and not config_success:
            category = "config_success_unobserved"
        self.evidence("native-config-test", {"category": category, **diagnostic,
                                            "exit_code": code, "config_success": config_success,
                                            "unknown_flag_error": unknown_flag, "usage_printed": usage,
                                            "native_refusal": native_refusal})
        if not valid:
            raise Refused("native_config_test_failed", command_class="nats_config_test", exit_code=code)

    def capture_resources(self):
        """Record the exact identities also when native startup partly failed."""
        generation = self.data["generation"]
        ids = self.compose("ps", "-a", "-q", timeout=10)[1].decode().split()
        if ids:
            for item in native_json(["docker", "inspect", *ids]):
                name = item["Config"]["Labels"]["com.docker.compose.service"]
                require(item["Config"]["Labels"].get(LABEL) == generation, "startup_identity")
                self.data["containers"][name] = {"id": item["Id"], "started_at": item["State"]["StartedAt"], "image": item["Image"]}
        ids = run(["docker", "network", "ls", "-q", "--filter", f"label={LABEL}={generation}"])[1].decode().split()
        if ids:
            require(len(ids) == 1, "startup_network_count")
            network = native_json(["docker", "network", "inspect", ids[0]])[0]
            self.data["network"] = {"id": network["Id"], "name": network["Name"]}
        self.data["volumes"] = run(["docker", "volume", "ls", "-q", "--filter", f"label={LABEL}={generation}"])[1].decode().split()
        self.save()

    def resources(self):
        containers = native_json(["docker", "inspect", *[item["id"] for item in self.data["containers"].values()]])
        return {item["Config"]["Labels"]["com.docker.compose.service"]: item for item in containers}

    def verify(self, *, idle=True):
        require(self.data["status"] == "active", "session_not_active")
        require(time.time() < self.data["expires_at"], "session_expired_stop_required")
        current = self.resources()
        require(set(current) == set(self.data["containers"]), "container_set_changed")
        for name, original in self.data["containers"].items():
            item = current[name]
            stopped = name in self.data.get("stopped_nodes", [])
            require(item["Id"] == original["id"] and (item["State"]["Running"] or stopped), "original_process_unavailable")
            require(item["State"]["StartedAt"] == original["started_at"] and item["RestartCount"] == 0,
                    "broker_lifetime_changed")
            require(item["Config"]["Labels"].get(LABEL) == self.data["generation"], "container_generation")
            require(item["Image"] == original["image"], "container_image_changed")
            require(not item["HostConfig"].get("PortBindings"), "external_route_detected")
            require(set(item["NetworkSettings"]["Networks"]) == {self.data["network"]["name"]}, "network_changed")
        network = native_json(["docker", "network", "inspect", self.data["network"]["id"]])[0]
        require(network["Internal"] and network["Labels"].get(LABEL) == self.data["generation"], "network_not_owned")
        require(set(network["Containers"]) == {item["Id"] for item in current.values() if item["State"]["Running"]},
                "unowned_client_on_network")
        if idle:
            known = {worker["exec_id"] for worker in self.data.get("workers", {}).values()}
            require(set(current["client"].get("ExecIDs") or []) <= known, "previous_client_still_running")
        self.tick(force=True)

    def tick(self, force=False):
        require(time.time() < self.data["expires_at"], "session_deadline")
        if not force and time.monotonic() - self.last_resource_check < 5:
            return
        self.last_resource_check = time.monotonic()
        require(shutil.disk_usage(self.path).free >= 2 * GIB, "disk_floor_stop_required")
        # The native NATS quota caps its three volumes at 384 MiB. Measure all
        # writable Docker layers/volumes as well, including PostgreSQL when selected.
        retained = Path(self.data.get("budget_root", self.path))
        used = sum(file.stat().st_size for file in retained.rglob("*") if file.is_file())
        for name, item in self.data["containers"].items():
            if name in self.data.get("paused_nodes", []):
                used += 128 * 1024 ** 2  # The existing native store quota remains in force while paused.
                continue
            directory = "/data" if name.startswith("nats") else "/var/lib/postgresql" if name == "postgres" else "/tmp"
            result = run(["docker", "exec", item["id"], "du", "-sk", directory], timeout=5, check=False)
            if result[0] == 0:
                used += int(result[1].split()[0]) * 1024
            elif name.startswith("nats"):
                used += 128 * 1024 ** 2  # Retained stopped-node volume, bounded by native quota.
        require(used <= GIB, "retained_data_limit_stop_required")

    def client_exec(self, *args, timeout=35, check=True, input=None, environment=None):
        env_args = ["--env-file", str(environment)] if environment else []
        return run(["docker", "exec", "-i", *env_args, self.data["containers"]["client"]["id"], *args],
                   timeout=min(timeout, max(1, self.data["expires_at"] - time.time())),
                   check=check, input=input, tick=self.tick)

    def nats(self, *args, timeout=15, check=True):
        return self.client_exec("/artifacts/nats", "--no-context", *args, timeout=timeout, check=check)

    def sql(self, database, statement):
        require(re.fullmatch(r"[a-z][a-z0-9_]{0,40}", database), "database_name")
        return self.client_exec("psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-d", database,
                                "-c", statement)[1].decode().strip()

    def rows(self, database, query):
        return json.loads(self.sql(database, "SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM (" + query + ") t"))

    def worker_environment(self, name, database, durable):
        require(name in {"worker", "publisher", "consumer", "replay"}, "worker_name")
        require(re.fullmatch(r"[a-z][a-z0-9_]{0,40}", database), "database_name")
        connection = load(self.path / "connection.json")
        secret = (self.path / "postgres.password").read_text().strip()
        port = {"worker": 8080, "publisher": 8081, "consumer": 8082, "replay": 8083}[name]
        values = {
            "APP__APP__ENV": "local", "APP__POSTGRES__ENABLED": "true",
            "APP__POSTGRES__DSN": f"postgres://recovery:{secret}@postgres:5432/{database}?sslmode=disable",
            "DATABASE_URL": f"postgres://recovery:{secret}@postgres:5432/{database}?sslmode=disable",
            "APP__POSTGRES__MAX_CONNECTIONS": "6", "APP__JOBS__MAX_WORKERS": "1",
            "APP__MESSAGING__URLS": ",".join(connection["servers"]),
            "APP__MESSAGING__CREDENTIALS_FILE": "/session/worker.creds",
            "APP__MESSAGING__ROOT_CA_PATH": "/session/tls/ca.crt",
            "APP__MESSAGING__SOURCE_STREAM": self.data["source_stream"],
            "APP__MESSAGING__MAX_PAYLOAD_BYTES": "64 KiB",
            "APP__MESSAGING__CONSUMER_DURABLE": durable,
            "APP__MESSAGING__CONSUMER_FILTER_SUBJECT": "recovery.counter.*",
            "APP__MESSAGING__DLQ_SUBJECT": self.data["dlq_subject"],
            "APP__MESSAGING__CONSUMER_CONCURRENCY": "1",
            "APP__HTTP__ADDR": f"127.0.0.1:{port}",
            "APP__OBSERVABILITY__METRICS__ADDR": f"127.0.0.1:{port + 1000}",
            "APP__LOG__FORMAT": "json",
        }
        path = self.path / f"{name}.env"
        require(not path.exists(), "worker_environment_already_exists")
        private_text(path, "".join(f"{key}={value}\n" for key, value in values.items()))
        return path

    def worker_start(self, name, database, durable="recovery_effect"):
        self.verify()
        require(name not in self.data.get("workers", {}), "worker_already_running")
        env_path = self.path / f"{name}.env"
        if not env_path.exists():
            self.worker_environment(name, database, durable)
        role = "consumer" if name == "replay" else name
        client = self.data["containers"]["client"]["id"]
        before = set(native_json(["docker", "inspect", client])[0].get("ExecIDs") or [])
        log = self.path / f"{name}.log"
        log.unlink(missing_ok=True)
        wrapper = 'echo $$ > /session/"$1".pid; exec /artifacts/messaging_recovery "$2" > /session/"$1".log 2>&1'
        run(["docker", "exec", "-d", "--env-file", str(env_path), client, "sh", "-c", wrapper, "worker", name, role])
        after = set(native_json(["docker", "inspect", client])[0].get("ExecIDs") or [])
        new = after - before
        require(len(new) == 1, "worker_execution_identity")
        self.data.setdefault("workers", {})[name] = {"exec_id": new.pop(), "database": database, "durable": durable}
        self.save()
        until = time.monotonic() + 25
        while time.monotonic() < until:
            self.tick()
            if log.exists() and '"jobs_worker_ready"' in log.read_text():
                pid = int((self.path / f"{name}.pid").read_text())
                stat = self.client_exec("cat", f"/proc/{pid}/stat")[1].decode().split()
                self.data["workers"][name].update({"pid": pid, "start_ticks": stat[21]})
                self.save()
                self.evidence("worker-start", {"role": name, "database": database, "durable": durable,
                                               "pool_max": 6, "ordinary_slots": 1, "publisher_slots": 1})
                return
            current = native_json(["docker", "inspect", client])[0].get("ExecIDs") or []
            require(self.data["workers"][name]["exec_id"] in current, "worker_start_failed")
            time.sleep(0.2)
        raise Refused("worker_start_deadline")

    def workload_start(self):
        """One finite ingress process, tracked by the same session worker custody."""
        self.verify()
        require("load" not in self.data.get("workers", {}), "workload_already_running")
        client = self.data["containers"]["client"]["id"]
        before = set(native_json(["docker", "inspect", client])[0].get("ExecIDs") or [])
        wrapper = ('echo $$ > /session/load.pid; exec /artifacts/messaging_recovery load '
                   '/session/measurement-plan.json /session/load.jsonl > /session/load.log 2>&1')
        run(["docker", "exec", "-d", "--env-file", str(self.path / "worker.env"), client,
             "sh", "-c", wrapper])
        after = set(native_json(["docker", "inspect", client])[0].get("ExecIDs") or [])
        new = after - before
        require(len(new) == 1, "workload_execution_identity")
        self.data.setdefault("workers", {})["load"] = {"exec_id": new.pop(), "database": "producer"}
        self.save()
        until = time.monotonic() + 10
        while time.monotonic() < until:
            self.tick()
            if (self.path / "load.pid").exists():
                pid = int((self.path / "load.pid").read_text())
                stat = self.client_exec("cat", f"/proc/{pid}/stat")[1].decode().split()
                self.data["workers"]["load"].update({"pid": pid, "start_ticks": stat[21]})
                self.save()
                return
            time.sleep(0.2)
        raise Refused("workload_start_deadline")

    def worker_stop(self, name):
        worker = self.data.get("workers", {}).get(name)
        if not worker:
            return
        require("pid" in worker and "start_ticks" in worker, "worker_identity_unresolved_stop_session")
        code, raw = self.client_exec("cat", f"/proc/{worker['pid']}/stat", check=False)
        if code == 0:
            require(raw.decode().split()[21] == worker["start_ticks"], "worker_pid_replaced")
            self.client_exec("sh", "-c", 'kill -TERM "$1"', "stop", str(worker["pid"]))
        until = time.monotonic() + 40
        while time.monotonic() < until:
            self.tick()
            running = native_json(["docker", "inspect", self.data["containers"]["client"]["id"]])[0].get("ExecIDs") or []
            if worker["exec_id"] not in running:
                del self.data["workers"][name]
                self.save()
                self.evidence("worker-stop", {"role": name, "joined": True})
                return
            time.sleep(0.2)
        raise Refused("worker_stop_deadline_stop_session")

    def request(self, subject, body, *, headers=(), check=True):
        return self.request_bytes(subject, json.dumps(body, separators=(",", ":")).encode(), headers=headers, check=check)

    def request_bytes(self, subject, body, *, headers=(), check=True):
        args = ["request", "--raw", "--no-templates"]
        for key, value in headers:
            args += ["--header", f"{key}:{value}"]
        args += ["--force-stdin", subject]
        code, raw = self.client_exec("/artifacts/nats", "--no-context", *args, input=body, timeout=15, check=check)
        if code:
            return None
        return json.loads(raw)

    @contextlib.contextmanager
    def operation(self, kind, selection=None):
        self.verify()
        inflight = self.path / "inflight.json"
        require(not inflight.exists(), "abandoned_operation_reconcile_or_stop")
        # Persist before either publication or deletion. In this generation no
        # stream can ever be replaced, even after a later successful readback.
        if kind in {"redrive", "retire"}:
            self.data["topology_frozen"] = True
            self.save()
        atomic(inflight, {"version": 1, "operation": kind, "selection": selection,
                          "generation": self.data["generation"], "started_at": time.time()}, exclusive=True)
        try:
            yield
        except BaseException:
            # The file is custody, not a lease: neither PID death nor elapsed
            # time permits topology replacement or a blind second operation.
            raise
        else:
            inflight.unlink()
        finally:
            (self.path / "grant.json").unlink(missing_ok=True)

    def selection(self, name):
        require(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", name), "selection_name")
        path = self.path / "selections" / (name + ".json")
        selected = load(path)
        require(selected["version"] == 1 and selected["generation"] == self.data["generation"], "selection_generation")
        require(selected["record"]["stream"] == self.data["dlq_stream"], "selection_stream")
        require(selected["source_stream"] == self.data["source_stream"], "selection_destination")
        return path, selected

    def inspect(self, sequence, name, payload=False):
        require(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", name), "selection_name")
        with self.operation("inspect", name):
            _, raw = self.client_exec("/artifacts/dlq_recovery", "inspect", "/session/connection.json",
                                      self.data["dlq_stream"], str(sequence), f"/session/selections/{name}.json",
                                      *(["--payload"] if payload else []))
        report = json.loads(raw)
        self.evidence("inspect", report)
        return report

    def redrive(self, name):
        path, _ = self.selection(name)
        with self.operation("redrive", name):
            atomic(self.path / "grant.json", {"version": 1, "generation": self.data["generation"],
                   "client_hostname": "client-" + self.data["generation"], "selection_sha256": sha(path.read_bytes()),
                   "operation": "redrive"}, exclusive=True)
            code, _ = self.client_exec("/artifacts/dlq_recovery", "redrive", f"/session/selections/{name}.json", check=False)
            require(code == 0, "redrive_unresolved_consult_state")
        report = load(path.with_suffix(".state.json"))
        self.evidence("redrive", report)
        return report

    def exact_record(self, selected):
        record = selected["record"]
        response = self.request(f"$JS.API.STREAM.MSG.GET.{record['stream']}", {"seq": record["sequence"]})
        if response.get("error", {}).get("err_code") == 10037:
            return "absent"
        require("error" not in response, "record_read_unresolved")
        actual = response["message"]
        payload = base64.b64decode(actual.get("data", ""), validate=True)
        header_bytes = base64.b64decode(actual.get("hdrs", ""), validate=True)
        # Native RFC3339 output may omit trailing zeroes in its fraction. Compare
        # nanosecond precision without a float or Python datetime truncation.
        same = (actual["seq"] == record["sequence"] and actual["subject"] == record["subject"]
                and timestamp(actual["time"]) == timestamp(record["stored_at"])
                and header_bytes == base64.b64decode(record["headers_base64"], validate=True)
                and sha(payload) == record["payload_sha256"]
                and base64.b64encode(payload).decode() == record["payload_base64"])
        return "retained" if same else "stale"

    def retire(self, name):
        path, selected = self.selection(name)
        state_path = path.with_suffix(".state.json")
        state = load(state_path)
        require(state["version"] == 1 and state["selection_sha256"] == sha(path.read_bytes()), "state_identity")
        ack = state.get("ack") or {}
        if state["publication"] != "confirmed" or ack.get("stream") != self.data["source_stream"] or ack.get("sequence", 0) <= 0:
            state["retirement"] = "refused"
            atomic(state_path, state)
            return state
        with self.operation("retire", name):
            current = self.exact_record(selected)
            if current != "retained":
                state["retirement"] = current
            else:
                state["retirement"] = "unknown"
                atomic(state_path, state)
                record = selected["record"]
                response = self.request(f"$JS.API.STREAM.MSG.DELETE.{record['stream']}",
                                        {"seq": record["sequence"], "no_erase": False}, check=False)
                if response and response.get("success") is True:
                    state["retirement"] = "retired"
                else:
                    # Still inside the original lifetime, which stays frozen
                    # even if a delayed delete lands after this readback.
                    try:
                        observed = self.exact_record(selected)
                        state["retirement"] = "retired" if observed == "absent" else observed
                    except (Refused, ValueError, KeyError):
                        state["retirement"] = "unknown"
            atomic(state_path, state)
        self.evidence("retire", state)
        return state

    def reconcile(self):
        self.verify()
        path = self.path / "inflight.json"
        require(path.exists(), "no_abandoned_operation")
        pending = load(path)
        require(pending["generation"] == self.data["generation"], "operation_generation")
        result = {"operation": pending["operation"], "topology_frozen": self.data["topology_frozen"]}
        if pending.get("selection") and pending["operation"] in {"redrive", "retire"}:
            selected_path, selected = self.selection(pending["selection"])
            state_path = selected_path.with_suffix(".state.json")
            if state_path.exists():
                state = load(state_path)
                current = self.exact_record(selected)
                state["retirement"] = "retired" if current == "absent" and state["retirement"] == "unknown" else current
                atomic(state_path, state)
                result.update(state)
        self.evidence("reconcile", result)
        (self.path / "grant.json").unlink(missing_ok=True)
        path.unlink()
        return result

    def evidence(self, operation, result):
        # Selection bytes and credentials stay in private source manifests.
        directory = self.path / "evidence"
        atomic(directory / f"{time.time_ns()}-{operation}.json",
               {"generation": self.data["generation"], "operation": operation, "result": result}, exclusive=True)

    def seed(self, suffix="selected"):
        require(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", suffix), "seed_identity")
        logical_id = f"dlq-example-{self.data['generation']}-{suffix}"
        payload = {"counter_id": "demonstration", "delta": 1}
        with self.operation("seed"):
            result = self.request(self.data["dlq_subject"], payload, headers=(
                ("Message-Id", logical_id), ("Event-Type", "recovery.counter.incremented"), ("Event-Schema", "v1"),
                ("Created-At", "2026-01-01T00:00:00Z"), ("Nats-Msg-Id", "dlq-" + logical_id),
                ("Original-Subject", self.data["subject"]), ("Dead-Letter-Reason", "permanent"),
                ("Nats-Expected-Stream", self.data["dlq_stream"])))
            require(result.get("stream") == self.data["dlq_stream"] and result.get("seq", 0) > 0, "seed_ack_unresolved")
        self.evidence("seed", {"logical_id": logical_id, "payload": payload, "ack": result})
        return result

    def stop(self):
        # Only exact identities from this session. Failure to observe stopped
        # processes leaves the manifest open; never recycle its directory.
        ids = run(["docker", "ps", "-aq", "--no-trunc", "--filter",
                   f"label={LABEL}={self.data['generation']}"])[1].decode().split()
        if ids:
            present = native_json(["docker", "inspect", *ids])
            require(all(item["Config"]["Labels"].get(LABEL) == self.data["generation"]
                        and item["Config"]["Labels"].get("com.docker.compose.project") == self.data["project"]
                        and item["Config"]["Labels"].get("com.docker.compose.service") in (*SERVICES, "postgres")
                        for item in present),
                    "cleanup_identity")
            if self.data["status"] != "starting":
                require(set(ids) <= {item["id"] for item in self.data["containers"].values()}, "cleanup_replaced_container")
            else:
                # Compose can die between create and identity readback. These
                # labels were issued before creation to this never-reused
                # generation; adopt only its exact allowed resources for stop.
                for item in present:
                    name = item["Config"]["Labels"]["com.docker.compose.service"]
                    self.data["containers"][name] = {"id": item["Id"], "started_at": item["State"]["StartedAt"], "image": item["Image"]}
                self.save()
            paused = [item["Id"] for item in present if item["State"].get("Paused")]
            if paused:
                run(["docker", "unpause", *paused], timeout=10)
            run(["docker", "stop", "--time", "10", *ids], timeout=30, check=False)
            present = native_json(["docker", "inspect", *ids])
            running = [item["Id"] for item in present if item["State"]["Running"]]
            if running:
                run(["docker", "kill", *running], timeout=10)
            require(all(not item["State"]["Running"] for item in native_json(["docker", "inspect", *ids])),
                    "lifetime_termination_unobserved")
        self.data["status"] = "terminated"
        self.data["terminated_at"] = time.time()
        self.save()
        if ids:
            run(["docker", "rm", *ids], timeout=20)
        network_ids = run(["docker", "network", "ls", "-q", "--no-trunc", "--filter",
                           f"label={LABEL}={self.data['generation']}"])[1].decode().split()
        for network_id in network_ids:
            network = native_json(["docker", "network", "inspect", network_id])[0]
            require(network["Labels"].get(LABEL) == self.data["generation"] and not network["Containers"],
                    "cleanup_network_identity")
            run(["docker", "network", "rm", network["Id"]], timeout=10)
        volumes = run(["docker", "volume", "ls", "-q", "--filter",
                       f"label={LABEL}={self.data['generation']}"])[1].decode().split()
        for name in volumes:
            volume = native_json(["docker", "volume", "inspect", name])[0]
            require(volume["Labels"].get(LABEL) == self.data["generation"], "cleanup_volume_identity")
            run(["docker", "volume", "rm", name], timeout=10)
        self.data["status"] = "stopped"
        self.save()
        self.evidence("stop", {"lifetime": "terminated", "resources": "removed"})
        return {"generation": self.data["generation"], "status": "stopped"}


def demonstrate(session):
    """One native B4 observation, with a neighbor as the wrong-delete oracle."""
    with session.lock():
        selected = session.seed()
        neighbor = session.seed("neighbor")
        inspected = session.inspect(selected["seq"], "selected")
        require(inspected["inspection"] == "restorable", "demo_inspection")
        session.inspect(neighbor["seq"], "neighbor")
        first = session.redrive("selected")
        require(first["publication"] == "confirmed" and first["retirement"] == "retained", "demo_publication")
        retry = session.redrive("selected")
        require(first["ack"] == retry["ack"], "demo_stable_publication")
        retired = session.retire("selected")
        require(retired["publication"] == "confirmed" and retired["retirement"] == "retired", "demo_retirement")
        _, selected_manifest = session.selection("selected")
        _, neighbor_manifest = session.selection("neighbor")
        require(session.exact_record(selected_manifest) == "absent" and session.exact_record(neighbor_manifest) == "retained",
                "demo_exact_record")
        result = {"publication": "confirmed", "retirement": "retired", "neighbor": "retained",
                  "publication_id": inspected["publication_id"], "source_ack": first["ack"]}
        session.evidence("dlq-demo", result)
        return result


def timestamp(value):
    match = re.fullmatch(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(?:Z|\+00:00)", value)
    require(match is not None, "non_utc_timestamp")
    return match[1], (match[2] or "").ljust(9, "0")


def prepare_auth(session):
    """Use the pinned native CLI's offline store; no host credential store."""
    try:
        session.compose("up", "-d", "--pull", "never", "client", timeout=30)
    except BaseException as error:
        session.remember_startup_failure(error)
        raise
    finally:
        try:
            session.startup_phase("resource_capture")
            session.capture_resources()
        except Exception:
            if "startup_failure" not in session.data:
                raise
    session.startup_phase("auth_generation")
    def auth(*args):
        return session.client_exec("env", "-u", "NATS_CREDS", "/artifacts/nats", "--no-context", "auth", *args)
    auth("operator", "add", "recovery", "--no-signing-key")
    auth("account", "add", "recovery", "--operator", "recovery", "--jetstream",
                 "--js-disk", str(384 * 1024 ** 2), "--js-memory", str(16 * 1024 ** 2), "--defaults")
    auth("user", "add", "admin", "recovery", "--operator", "recovery", "--defaults",
                 "--credential", "/session/admin.creds")
    denied = ["$JS.API.STREAM." + operation + ".>" for operation in
              ("CREATE", "UPDATE", "DELETE", "PURGE", "RESTORE", "SNAPSHOT", "MSG.DELETE")]
    flags = [flag for subject in denied for flag in ("--pub-deny", subject)]
    auth("user", "add", "worker", "recovery", "--operator", "recovery", "--defaults",
                 "--credential", "/session/worker.creds", *flags)
    claims = {}
    for file in (session.path / "auth-state").rglob("*.jwt"):
        token = file.read_text().strip()
        encoded = token.split(".")[1]
        claim = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
        kind = claim.get("nats", {}).get("type")
        if kind in {"operator", "account"}:
            claims[(kind, claim["name"])] = (claim, token)
    operator, operator_token = claims[("operator", "recovery")]
    system, system_token = claims[("account", "SYSTEM")]
    account, account_token = claims[("account", "recovery")]
    require(operator["nats"]["system_account"] == system["sub"], "fixture_auth_system_account")
    private_text(session.path / "auth/operator.jwt", operator_token + "\n")
    private_text(session.path / "auth/server.conf", 'operator: "/auth/operator.jwt"\n'
                 + f'system_account: "{system["sub"]}"\nresolver: MEMORY\nresolver_preload: {{\n'
                 + f'  "{system["sub"]}": "{system_token}"\n  "{account["sub"]}": "{account_token}"\n}}\n')
    session.data["credential_identity"] = {"operator": operator["sub"], "account": account["sub"],
                                          "worker_stream_mutation_denied": denied}
    session.save()


def start(path, artifacts, *, postgres=False, empty_streams=False, deadline=None, budget_root=None):
    path = path.absolute()
    artifacts = artifacts.resolve()
    require(not path.exists(), "session_directory_must_be_new")
    started_at = time.time()
    expires = min(deadline or started_at + SESSION_SECONDS, started_at + SESSION_SECONDS)
    admission = resource_admission(path.parent, deadline=expires, cooldown=deadline is None)
    binaries = ("nats", "dlq_recovery", *(["messaging_recovery", "migrate"] if postgres else []))
    for name in binaries:
        require((artifacts / name).is_file() and os.access(artifacts / name, os.X_OK), "prebuilt_linux_artifact_missing")
    nats_image = image_pin("nats")
    # Client ABI matches the canonical Debian PostgreSQL image in combined
    # profiles. Messaging-only projections select the installed NATS image;
    # their supplied executables must consequently be static Linux artifacts.
    postgres_image = image_pin("postgres")
    client_image = postgres_image if postgres_image != "null" else nats_image
    for image in {nats_image, client_image}:
        require("@sha256:" in image, "canonical_image_pin_missing")
        run(["docker", "image", "inspect", image])  # No implicit download.
    runtime = native_json(["docker", "image", "inspect", client_image])[0]
    artifacts_identity = {name: artifact_identity(artifacts / name, runtime["Architecture"]) for name in binaries}
    require(not postgres or postgres_image != "null", "combined_profile_unavailable")
    generation = secrets.token_hex(12)
    project = "messaging-recovery-" + generation
    path.mkdir(mode=0o700)
    for child in ("tls", "auth", "selections", "evidence"):
        (path / child).mkdir(mode=0o700)
    username, password = "recovery", secrets.token_hex(32)
    private_text(path / "postgres.password", password + "\n")
    require(expires - time.time() > 60, "insufficient_session_budget")
    nodes = [f"nats{index}-{generation}" for index in range(1, 4)]
    environment = {"RECOVERY_GENERATION": generation, "RECOVERY_SESSION": str(path),
                   "RECOVERY_UID": str(os.getuid()), "RECOVERY_GID": str(os.getgid()),
                   "RECOVERY_ARTIFACTS": str(artifacts), "RECOVERY_NATS_IMAGE": nats_image,
                   "RECOVERY_CLIENT_IMAGE": client_image, "RECOVERY_POSTGRES_IMAGE": postgres_image,
                   "RECOVERY_LIFETIME": str(int(expires - time.time())),
                   "RECOVERY_USER": username, "RECOVERY_PASSWORD": password,
                   **{f"RECOVERY_NODE{index}": node for index, node in enumerate(nodes, 1)}}
    require(all("\n" not in value and "'" not in value for value in environment.values()), "unsupported_fixture_path")
    private_text(path / "compose.env", "".join(f"{key}='{value}'\n" for key, value in environment.items()))
    data = {"version": 1, "generation": generation, "project": project, "directory": str(path),
            "created_at": time.time(), "expires_at": expires, "status": "starting",
            "budget_root": str(budget_root or path), "workers": {},
            "source_stream": "RECOVERY", "dlq_stream": "RECOVERY_DLQ", "subject": "recovery.counter.incremented",
            "dlq_subject": "recovery.dlq", "topology_frozen": False, "containers": {}, "volumes": [],
            "network": None, "resource_admission": admission, "client_tls": True, "route_tls": True,
            "artifacts": artifacts_identity, "client_image_architecture": runtime["Architecture"]}
    atomic(path / "session.json", data, exclusive=True)
    session = Session(path)
    with session.lock(), session.startup_diagnostics():
        session.startup_phase("tls_material")
        tls = path / "tls"
        run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=recovery-ca",
             "-keyout", str(tls / "ca.key"), "-out", str(tls / "ca.crt")])
        run(["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=" + nodes[0],
             "-keyout", str(tls / "server.key"), "-out", str(tls / "server.csr")])
        private_text(tls / "extensions", "subjectAltName=" + ",".join("DNS:" + node for node in nodes)
                     + ",DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth,clientAuth\n")
        run(["openssl", "x509", "-req", "-days", "1", "-in", str(tls / "server.csr"), "-CA", str(tls / "ca.crt"),
             "-CAkey", str(tls / "ca.key"), "-CAcreateserial", "-extfile", str(tls / "extensions"), "-out", str(tls / "server.crt")])
        for certificate in tls.iterdir():
            certificate.chmod(0o600)
        session.startup_phase("auth_generation")
        prepare_auth(session)
        data = session.data
        session.startup_phase("server_config")
        for index, node in enumerate(nodes, 1):
            routes = ", ".join(json.dumps(f"nats-route://{peer}:6222") for peer in nodes if peer != node)
            # NATS joins includes to /session; ../auth reaches the separate mount.
            private_text(path / f"nats{index}.conf", f'''server_name: {node}
port: 4222
http: 127.0.0.1:8222
max_payload: 1048576
include "../auth/server.conf"
tls {{ cert_file: "/tls/server.crt", key_file: "/tls/server.key", ca_file: "/tls/ca.crt" }}
jetstream {{ store_dir: "/data", max_file_store: 128MB, max_memory_store: 16MB, sync_interval: always }}
cluster {{
  name: "{project}"
  port: 6222
  routes: [{routes}]
  tls {{ cert_file: "/tls/server.crt", key_file: "/tls/server.key", ca_file: "/tls/ca.crt", verify: true }}
}}
''')
        atomic(path / "connection.json", {"servers": [f"tls://{node}:4222" for node in nodes],
               "root_ca_path": "/session/tls/ca.crt", "credentials_file": "/session/admin.creds", "allow_plaintext": False,
               "source_stream": data["source_stream"], "max_payload_bytes": 65536, "generation": generation}, exclusive=True)
        session.startup_phase("native_config_parse")
        session.test_native_config()
        services = [*SERVICES, *(["postgres"] if postgres else [])]
        try:
            # Capture exact resources even on partial Compose startup. No later
            # generation is allowed to reuse this directory or these volumes.
            session.startup_phase("broker_up")
            session.compose("up", "-d", "--wait", "--wait-timeout", "60", "--pull", "never", *services, timeout=75)
        except BaseException as error:
            session.remember_startup_failure(error)
            raise
        finally:
            try:
                session.startup_phase("resource_capture")
                session.capture_resources()
            except Exception:
                if "startup_failure" not in session.data:
                    raise
        require(set(data["containers"]) == set(services), "startup_incomplete_stop_required")
        data["status"] = "active"
        session.save()
        session.startup_phase("verify")
        session.verify()
        session.startup_phase("execution_inputs")
        session.evidence("execution-inputs", {"nats_cli": session.nats("--version")[1].decode().strip(),
                         "images": {key: value["image"] for key, value in data["containers"].items()},
                         "client_kernel": session.client_exec("uname", "-sm")[1].decode().strip()})
        session.startup_phase("topology")
        if not empty_streams:
            for stream, subject in ((data["source_stream"], "recovery.counter.*"), (data["dlq_stream"], data["dlq_subject"])):
                response = session.request(f"$JS.API.STREAM.CREATE.{stream}", {
                    "name": stream, "subjects": [subject], "storage": "file", "num_replicas": 3,
                    "retention": "limits", "discard": "new", "max_bytes": 16 * 1024 ** 2,
                    "max_msg_size": 73728 if stream == data["source_stream"] else 81920,
                    "max_msgs": 10000, "duplicate_window": 120_000_000_000, "no_ack": False})
                require("error" not in response, "stream_create_failed")
            effective = {}
            until = time.monotonic() + 30
            while time.monotonic() < until:
                effective = {stream: session.request(f"$JS.API.STREAM.INFO.{stream}", {})
                             for stream in (data["source_stream"], data["dlq_stream"])}
                if all(value.get("cluster", {}).get("leader") and len(value.get("cluster", {}).get("replicas", [])) == 2
                       and all(replica["current"] for replica in value["cluster"]["replicas"]) for value in effective.values()):
                    break
                time.sleep(0.2)
            else:
                raise Refused("r3_not_current")
            session.evidence("topology", effective)
            for value in effective.values():
                config = value["config"]
                require(config["storage"] == "file" and config["num_replicas"] == 3
                        and not config.get("no_ack", False) and config.get("persist_mode") in (None, "default")
                        and config["discard"] == "new" and config["max_bytes"] == 16 * 1024 ** 2,
                        "effective_stream_configuration")
        session.startup_phase("complete")
        return {"generation": generation, "status": "active", "session": str(path)}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = ("start", "demo", "status", "inspect", "redrive", "retire", "reconcile", "stop", "seed")
    # template:begin outbox:recovery-rehearsal-command
    commands += ("rehearse", "measure")
    # template:end outbox:recovery-rehearsal-command
    parser.add_argument("command", choices=commands)
    parser.add_argument("--session", required=True, type=Path)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--postgres", action="store_true")
    parser.add_argument("--empty-streams", action="store_true")
    parser.add_argument("--sequence", type=int)
    parser.add_argument("--selection", default="selected")
    parser.add_argument("--payload", action="store_true")
    args = parser.parse_args()
    try:
        # template:begin outbox:recovery-rehearsal-dispatch
        if args.command in {"rehearse", "measure"}:
            from messaging_recovery_scenarios import rehearse
            from messaging_recovery_capacity import measure
            require(args.artifacts is not None, "artifacts_required_no_implicit_build")
            execute = rehearse if args.command == "rehearse" else measure
            result = execute(sys.modules[__name__], args.session, args.artifacts)
            print(json.dumps(result, sort_keys=True))
            return 0 if result.get("status") == "observed" else 2
        # template:end outbox:recovery-rehearsal-dispatch
        if args.command in {"start", "demo"}:
            require(args.artifacts is not None, "artifacts_required_no_implicit_build")
            result = start(args.session, args.artifacts, postgres=args.postgres, empty_streams=args.empty_streams)
            if args.command == "demo":
                session = Session(args.session)
                try:
                    result = demonstrate(session)
                finally:
                    with session.lock():
                        session.stop()
        else:
            session = Session(args.session)
            with session.lock():
                if args.command == "stop":
                    result = session.stop()
                elif args.command == "status":
                    result = {key: session.data[key] for key in ("generation", "status", "expires_at", "topology_frozen")}
                elif args.command == "inspect":
                    require(args.sequence is not None and args.sequence > 0, "positive_sequence_required")
                    result = session.inspect(args.sequence, args.selection, args.payload)
                elif args.command == "redrive":
                    result = session.redrive(args.selection)
                elif args.command == "retire":
                    result = session.retire(args.selection)
                elif args.command == "reconcile":
                    result = session.reconcile()
                else:
                    result = session.seed()
        print(json.dumps(result, sort_keys=True))
        return 0
    except (Refused, OSError, ValueError, KeyError) as error:
        reason = str(error) if isinstance(error, Refused) else "invalid_or_unavailable_session_input"
        must_stop = args.command in {"start", "demo"} or reason in {
            "session_deadline", "session_expired_stop_required", "disk_floor_stop_required", "retained_data_limit_stop_required"}
        if must_stop and (args.session / "session.json").exists():
            try:
                session = Session(args.session)
                with session.lock():
                    session.stop()
            except (Refused, OSError, ValueError, KeyError):
                reason += ":cleanup_unresolved_use_stop"
        print(json.dumps({"error": reason, "custody": "retained_until_reconciled_or_stopped"}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
