#!/usr/bin/env python3
"""Project supported Cargo context; own an explicitly selected local sccache.

Cargo remains the configuration/compiler authority. Unknown inherited inputs
still execute, but cannot identify a reusable verification result. This helper
does not provision tools, rewrite Cargo configuration, or clean build caches.
"""

import errno
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import tomllib


SCHEMA = 1
ROOT = Path(__file__).resolve().parents[2]
REFUSED = 2
INCOMPLETE = 74
EXHAUSTED = {errno.ENOSPC, errno.EDQUOT}
OUTPUT_KEYS = ("target-dir", "build-dir")
PROGRAM_KEYS = ("rustc", "rustc-wrapper", "rustc-workspace-wrapper", "rustdoc")
SENSITIVE_KEY = re.compile(r"(?:^|[_.-])(token|password|secret|credential)(?:$|[_.-])", re.I)


class ContextError(Exception):
    def __init__(self, reason, role=None):
        super().__init__(reason)
        self.reason = reason
        self.role = role


def digest(value):
    if not isinstance(value, bytes):
        value = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(value).hexdigest()


def path_id(path):
    return digest(str(path))


def file_digest(path):
    with path.open("rb") as source:
        result = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
        return result.hexdigest()


def resolve_path(value, base):
    if not isinstance(value, str) or not value or "\0" in value:
        raise ContextError("unsupported_path")
    # Cargo's build-dir hash algorithm and unknown future templates stay Cargo's.
    if "{" in value or "}" in value:
        raise ContextError("unsupported_output_template")
    path = Path(value)
    return (path if path.is_absolute() else base / path).resolve()


def executable(value, base):
    if not isinstance(value, str) or not value or "\0" in value:
        raise ContextError("unsupported_executable")
    found = str(resolve_path(value, base)) if "/" in value else shutil.which(value)
    if not found or not Path(found).is_file() or not os.access(found, os.X_OK):
        raise ContextError("executable_unavailable")
    # Preserve the invocation name: a rustup symlink must still be called cargo,
    # and sccache's name controls its compiler-wrapper compatibility mode.
    return Path(found).absolute()


def probe(program, arguments, environment=None):
    try:
        result = subprocess.run([str(program), *arguments], env=environment,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=10, check=False)
        if result.returncode == 0 and len(result.stdout) <= 1024 * 1024:
            return result.stdout
    except (OSError, subprocess.TimeoutExpired):
        pass
    return None


def program_identity(program, name=None, environment=None):
    result = {"path_fingerprint": path_id(program.resolve()),
              "content": file_digest(program)}
    if name:
        version = probe(program, ["--version"], environment)
        if version is None:
            raise ContextError("version_unavailable")
        result["version_fingerprint"] = digest(version)
        # Only the product and numeric version are displayed, never arbitrary
        # wrapper output or build metadata supplied by a caller executable.
        match = re.match(rb"^" + name.encode() + rb" ([0-9]+\.[0-9]+\.[0-9]+)(?:[ \r\n-]|$)", version)
        if not match:
            raise ContextError("unsupported_tool_version")
        result["version"] = match[1].decode()
    return result


def flatten(values, base, prefix=()):
    result = {}
    for key, value in values.items():
        location = (*prefix, key)
        if isinstance(value, dict):
            result.update(flatten(value, base, location))
        else:
            result[location] = (value, base)
    return result


def merge(destination, incoming):
    for key, (value, base) in incoming.items():
        previous = destination.get(key)
        if previous and isinstance(previous[0], list) and isinstance(value, list):
            value = previous[0] + value
        destination[key] = (value, base)


class Context:
    def __init__(self, command):
        self.root = Path.cwd().resolve()
        selected_mode = os.environ.get("BUILD_CACHE", "inherit")
        self.mode = selected_mode if selected_mode in {"inherit", "sccache"} else "unknown"
        self.command = command
        self.unknown = []
        self.refusal = None
        self.refusal_role = None
        self.refusal_key = None
        self.configs = []
        self.values = {}
        self.paths = {}
        self.programs = {}
        self.cache_program = None
        self.cache_size = None
        self.minimum = None
        self.identity_inputs = {"schema": SCHEMA, "mode": self.mode,
                                "checkout": path_id(self.root)}
        if self.mode == "unknown":
            self.reject("invalid_build_cache")

    def unclear(self, reason):
        reason = reason.lower()
        if reason not in self.unknown:
            self.unknown.append(reason)

    def reject(self, reason, role=None):
        if self.refusal is None:
            self.refusal = reason.lower()
            self.refusal_role = role

    def load_config(self, path, stack=(), optional=False):
        canonical = path.resolve()
        if canonical in stack or len(stack) >= 16:
            raise ContextError("config_include_cycle_or_depth")
        if not path.exists():
            self.configs.append({"path_fingerprint": path_id(canonical), "content": "absent"})
            if optional:
                return {}
            raise ContextError("config_unavailable")
        content = path.read_bytes()
        values = tomllib.loads(content.decode())
        self.configs.append({"path_fingerprint": path_id(canonical), "content": digest(content)})
        includes = values.pop("include", [])
        if not isinstance(includes, list):
            raise ContextError("unsupported_config_include")
        result = {}
        for entry in includes:
            optional = False
            if isinstance(entry, dict):
                if set(entry) - {"path", "optional"} or not isinstance(entry.get("optional", False), bool):
                    raise ContextError("unsupported_config_include")
                optional = entry.get("optional", False)
                entry = entry.get("path")
            if not isinstance(entry, str) or not entry.endswith(".toml"):
                raise ContextError("unsupported_config_include")
            merge(result, self.load_config(path.parent / entry, (*stack, canonical), optional))
        merge(result, flatten(values, path.parent.parent))
        return result

    def read_configuration(self, arguments):
        home = resolve_path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")), self.root)
        self.paths["cargo_cache"] = home
        directories = [home, *(parent / ".cargo" for parent in reversed((self.root, *self.root.parents)))]
        seen = set()
        for directory in directories:
            # The extensionless spelling has precedence when both exist.
            path = directory / ("config" if (directory / "config").exists() else "config.toml")
            if path.resolve() in seen:
                continue
            seen.add(path.resolve())
            merge(self.values, self.load_config(path, optional=True))

        for key in (*OUTPUT_KEYS, *PROGRAM_KEYS):
            env_key = "CARGO_BUILD_" + key.upper().replace("-", "_")
            if env_key in os.environ:
                self.values[("build", key)] = (os.environ[env_key], self.root)

        target_override = None
        cli_values = {}
        index = 0
        while index < len(arguments):
            argument = arguments[index]
            if argument == "--":
                break
            if argument in {"--config", "--target-dir", "--manifest-path"}:
                index += 1
                if index == len(arguments):
                    raise ContextError("missing_cargo_option_value")
                value = arguments[index]
            elif argument.startswith(("--config=", "--target-dir=", "--manifest-path=")):
                argument, value = argument.split("=", 1)
            else:
                if argument == "-C" or argument.startswith(("-Z", "+")):
                    self.unclear("unsupported_cargo_option")
                index += 1
                continue
            if argument == "--config":
                if "=" in value:
                    loaded = flatten(tomllib.loads(value), self.root)
                else:
                    loaded = self.load_config(resolve_path(value, self.root))
                merge(self.values, loaded)
                merge(cli_values, loaded)
            elif argument == "--target-dir":
                target_override = value
            elif resolve_path(value, self.root) != self.root / "Cargo.toml":
                self.unclear("unsupported_manifest_root")
            index += 1

        # These direct Cargo variables also override the corresponding config
        # key. Refuse ambiguity between --config and a conflicting special env
        # input rather than guessing a compiler/output selection.
        for key, env_key in (("rustc", "RUSTC"), ("rustdoc", "RUSTDOC"),
                             ("rustc-wrapper", "RUSTC_WRAPPER"),
                             ("rustc-workspace-wrapper", "RUSTC_WORKSPACE_WRAPPER"),
                             ("target-dir", "CARGO_TARGET_DIR")):
            if env_key in os.environ:
                if ("build", key) in cli_values and cli_values[("build", key)][0] != os.environ[env_key]:
                    self.unclear("ambiguous_special_environment_override")
                self.values[("build", key)] = (os.environ[env_key], self.root)
        if target_override is not None:
            self.values[("build", "target-dir")] = (target_override, self.root)

        for key in self.values:
            if any(SENSITIVE_KEY.search(part) for part in key):
                self.unclear("secret_bearing_cargo_config")
            if key[0] == "env" and len(key) > 1:
                name = key[1]
                if name.startswith(("SCCACHE_", "CARGO_", "RUSTC", "RUSTDOC")) or name in {"PATH", "RUSTFLAGS", "RUSTUP_TOOLCHAIN"}:
                    self.unclear("cargo_env_changes_build_context")

    def select_paths_and_programs(self):
        for key, role in (("target-dir", "target"), ("build-dir", "build")):
            default = str(self.root / "target") if role == "target" else str(self.paths.get("target", self.root / "target"))
            value, base = self.values.get(("build", key), (default, self.root))
            if isinstance(value, str):
                value = value.replace("{workspace-root}", str(self.root)).replace("{cargo-cache-home}", str(self.paths["cargo_cache"]))
            try:
                path = resolve_path(value, base)
                self.paths[role] = path
                if path == self.root or self.root not in path.parents:
                    self.unclear("output_ownership_unknown")
            except ContextError as error:
                self.paths[role] = None
                self.unclear(error.reason)
        try:
            trees = subprocess.run(["git", "worktree", "list", "--porcelain", "-z"],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
            if trees.returncode:
                self.unclear("worktree_ownership_unavailable")
            else:
                for field in trees.stdout.split(b"\0"):
                    if field.startswith(b"worktree "):
                        other = Path(os.fsdecode(field[9:])).resolve()
                        if other != self.root:
                            for role in ("target", "build"):
                                path = self.paths.get(role)
                                if path and (path == other or other in path.parents):
                                    self.reject("output_aliases_other_worktree", role)
        except OSError:
            self.unclear("worktree_ownership_unavailable")
        for key in PROGRAM_KEYS:
            value, base = self.values.get(("build", key), (key if key in {"rustc", "rustdoc"} else "", self.root))
            if value == "":
                self.programs[key] = None
            else:
                try:
                    program = executable(value, base)
                    self.programs[key] = program
                    self.identity_inputs[key] = program_identity(program, key if key in {"rustc", "rustdoc"} else None)
                except (OSError, ContextError) as error:
                    self.unclear(error.reason if isinstance(error, ContextError) else "program_unreadable")
        wrappers_selected = any(self.values.get(("build", key), ("", None))[0]
                                for key in ("rustc-wrapper", "rustc-workspace-wrapper"))
        self.identity_inputs["wrapper_mode"] = "inherited" if wrappers_selected else "none"

    def select_cache(self):
        if self.mode == "inherit":
            if any(self.programs.get(key) for key in PROGRAM_KEYS if "wrapper" in key):
                # An inherited arbitrary wrapper or shared daemon has config/state
                # outside this helper's supported private native-cache boundary.
                self.unclear("inherited_wrapper_context_unknown")
            return
        if self.mode != "sccache":
            self.reject("invalid_BUILD_CACHE")
            return
        for key in os.environ:
            if key.startswith("SCCACHE_"):
                self.reject("conflicting_SCCACHE_setting")
                self.refusal_key = key if re.fullmatch(r"SCCACHE_[A-Z0-9_]{1,64}", key) else "SCCACHE_setting"
                return
        if sys.platform not in {"darwin", "linux"} or not hasattr(socket, "AF_UNIX"):
            self.reject("unsupported_cache_platform")
            return
        if not os.environ.get("BUILD_CACHE_DIR"):
            self.reject("missing_BUILD_CACHE_DIR")
            return
        path = resolve_path(os.environ["BUILD_CACHE_DIR"], self.root)
        self.paths["compiler_cache"] = path
        if path == self.root or path in self.root.parents or any(
                output and (path == output or path in output.parents or output in path.parents)
                for output in (self.paths.get("target"), self.paths.get("build"), self.paths.get("cargo_cache"))):
            self.reject("cache_output_overlap", "compiler_cache")
            return
        self.check_cache_owner()
        size = os.environ.get("BUILD_CACHE_SIZE", "1G")
        match = re.fullmatch(r"([1-9][0-9]*)([KMG]?)", size)
        if not match:
            self.reject("invalid_BUILD_CACHE_SIZE")
            return
        self.cache_size = int(match[1]) * 1024 ** {"": 0, "K": 1, "M": 2, "G": 3}[match[2]]
        if self.cache_size > 2**63 - 1:
            self.reject("invalid_BUILD_CACHE_SIZE")
            return
        pin = re.findall(r"^SCCACHE_VERSION=([0-9.]+)$", (ROOT / "tools/versions.env").read_text(), re.M)
        if len(pin) != 1:
            self.reject("cache_pin_unavailable")
            return
        self.cache_program = executable(os.environ.get("BUILD_CACHE_BIN", "sccache"), self.root)
        # --version does not load server state; scrub all native settings anyway.
        native = {key: value for key, value in os.environ.items() if not key.startswith("SCCACHE_")}
        identity = program_identity(self.cache_program, "sccache", native)
        if identity["version"] != pin[0]:
            self.reject("cache_version_mismatch")
        for key in ("rustc-wrapper", "rustc-workspace-wrapper"):
            selected = self.programs.get(key)
            if selected and (key == "rustc-workspace-wrapper" or selected.resolve() != self.cache_program.resolve()):
                self.reject("conflicting_" + key.replace("-", "_"))
        self.identity_inputs.update(wrapper_mode="sccache", sccache=identity,
                                    cache_config={"kind": "private-local-disk", "size": self.cache_size,
                                                  "client_side": True, "foreground": True})

    def check_cache_owner(self):
        path = self.paths["compiler_cache"]
        if not path.exists():
            return
        if not path.is_dir() or path.stat().st_uid != os.getuid():
            raise ContextError("cache_not_task_owned", "compiler_cache")
        owner = path / ".build-context-owner.json"
        if owner.exists():
            try:
                recorded = json.loads(owner.read_text())
            except (ValueError, UnicodeError) as error:
                raise ContextError("cache_owner_invalid", "compiler_cache") from error
            if recorded != {"schema": SCHEMA, "checkout": path_id(self.root)}:
                raise ContextError("cache_owned_by_other_checkout", "compiler_cache")
        elif any(path.iterdir()):
            raise ContextError("cache_not_task_owned", "compiler_cache")

    def resolve(self):
        if self.root != ROOT:
            self.unclear("unsupported_working_directory")
        requirement = os.environ.get("BUILD_MIN_FREE_BYTES")
        if requirement is not None:
            if not re.fullmatch(r"[0-9]{1,100}", requirement):
                self.reject("invalid_BUILD_MIN_FREE_BYTES")
            else:
                self.minimum = int(requirement)
        try:
            configured = shlex.split(os.environ.get("CARGO", "cargo"))
            raw_flags = os.environ.get("CARGO_FLAGS", "--locked")
            flags = shlex.split(raw_flags)
            if any(char in raw_flags for char in "|&;<>`$\n"):
                self.unclear("unsupported_cargo_flags")
            if len(configured) != 1 or any(char in configured[0] for char in "|&;<>`$\n"):
                self.unclear("unsupported_CARGO_command")
            command = self.command or [*configured, *flags]
            self.identity_inputs["cargo_flags"] = digest(flags)
            self.read_configuration(command[1:])
            self.select_paths_and_programs()
            if len(configured) == 1 and command and Path(command[0]).name == Path(configured[0]).name:
                cargo = executable(command[0], self.root)
                self.identity_inputs["cargo"] = program_identity(cargo, "cargo")
            else:
                self.unclear("unsupported_CARGO_command")
            self.select_cache()
        except (ContextError, OSError, ValueError) as error:
            reason = error.reason if isinstance(error, ContextError) else "configuration_unreadable"
            self.unclear(reason)
            if self.mode == "sccache":
                self.reject(reason, getattr(error, "role", None))
        relevant = {key: value for key, value in os.environ.items()
                    if (key.startswith(("CARGO_", "RUST", "SCCACHE_"))
                        or key in {"PATH", "CC", "CXX", "AR", "LD", "CFLAGS", "CXXFLAGS", "LDFLAGS", "CI"})
                    and key not in {"CARGO_MAKEFLAGS", "CARGO_MANIFEST_DIR"}}
        # Only the hash is retained. Credential files and arbitrary environment
        # dumps are never read or persisted.
        self.identity_inputs.update(configs=self.configs, environment=digest(relevant),
                                    outputs={role: path_id(path) if path else "unknown" for role, path in self.paths.items()})
        if self.unknown and self.mode == "sccache":
            self.reject(self.unknown[0])
        return self

    def describe(self):
        filesystems = {}
        observations = []
        refusal, role = self.refusal, self.refusal_role
        for name, path in self.paths.items():
            observed = {"role": name, "path_fingerprint": path_id(path) if path else "unknown",
                        "timestamp": int(time.time())}
            try:
                if path is None:
                    raise OSError(errno.ENOENT, "unknown output")
                ancestor = path
                while not ancestor.exists():
                    if ancestor == ancestor.parent:
                        raise OSError(errno.ENOENT, "no existing ancestor")
                    ancestor = ancestor.parent
                info = ancestor.stat()
                if not stat.S_ISDIR(info.st_mode) or not os.access(ancestor, os.W_OK | os.X_OK):
                    refusal, role = refusal or "location_unwritable", role or name
                filesystem = str(info.st_dev)
                if filesystem not in filesystems:
                    space = os.statvfs(ancestor)
                    filesystems[filesystem] = space.f_bavail * space.f_frsize
                available = filesystems[filesystem]
                observed.update(filesystem=filesystem, available_bytes=available, state="observed")
                if available == 0:
                    refusal, role = refusal or "resource_exhausted", role or name
                elif self.minimum is not None and available < self.minimum:
                    refusal, role = refusal or "insufficient_capacity", role or name
            except OSError:
                observed.update(state="unavailable", available_bytes=None)
                if self.minimum is not None:
                    refusal, role = refusal or "capacity_unavailable", role or name
            observations.append(observed)
        if self.minimum is not None and not observations:
            refusal = refusal or "capacity_unavailable"
        return {"schema": SCHEMA, "identity": digest(self.identity_inputs), "mode": self.mode,
                "known": not self.unknown, "unknown": self.unknown,
                "admission": "refused" if refusal else "ready", "reason": refusal or "ready",
                "role": role, "key": self.refusal_key,
                "minimum_free_bytes": self.minimum, "resources": observations,
                "context": self.identity_inputs}


def evidence(kind, value):
    line = kind + ": " + json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    destination = os.environ.get("BUILD_CONTEXT_EVIDENCE")
    if destination:
        with open(destination, "a") as retained:
            retained.write(line)
    # Retained context has complete safe fingerprints. Terminal progress stays
    # bounded and never repeats the config-source inventory at every leaf.
    if kind == "build_context":
        visible = {key: value[key] for key in ("schema", "identity", "mode", "known", "admission", "reason", "role", "key", "minimum_free_bytes", "resources")}
        line = kind + ": " + json.dumps(visible, sort_keys=True, separators=(",", ":")) + "\n"
    print(line, end="", file=sys.stderr, flush=True)


def socket_ready(path):
    try:
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(0.2)
            client.connect(str(path))
        return True
    except OSError:
        return False


def native_stats(program, environment, server, path):
    if server.poll() is not None or not socket_ready(path):
        return {"state": "unavailable"}
    output = probe(program, ["--show-stats", "--stats-format", "json"], environment)
    if output is None or server.poll() is not None or not socket_ready(path):
        return {"state": "unavailable"}
    try:
        stats = json.loads(output)["stats"]
        if not isinstance(stats, dict):
            return {"state": "unavailable"}
        counters = {}
        for name in ("compile_requests", "requests_executed", "cache_hits", "cache_misses",
                     "cache_errors", "cache_read_errors", "cache_write_errors",
                     "requests_not_cacheable", "requests_not_compile", "requests_unsupported_compiler"):
            value = stats.get(name)
            if isinstance(value, int) and value >= 0:
                counters[name] = value
            elif isinstance(value, dict):
                # Native language maps carry known numeric counters, not paths.
                entries = value.get("counts", value)
                if isinstance(entries, dict) and all(isinstance(number, int) and number >= 0 for number in entries.values()):
                    counters[name] = sum(entries.values())
        return {"state": "observed", "counters": counters}
    except (ValueError, KeyError, TypeError):
        return {"state": "unavailable"}


def cached_run(context, command):
    cache = context.paths["compiler_cache"]
    cache.mkdir(parents=True, exist_ok=True)
    context.check_cache_owner()
    owner = cache / ".build-context-owner.json"
    if not owner.exists():
        with owner.open("x") as record:
            json.dump({"schema": SCHEMA, "checkout": path_id(context.root)}, record)
    # A short private path fits macOS sockaddr_un even for a deeply nested root.
    directory = Path(tempfile.mkdtemp(prefix="build-cache-", dir="/tmp"))
    endpoint = directory / "server.sock"
    config = directory / "config.toml"
    server = None
    joined = True
    cancelled = []
    previous = {}
    def cancellation(number, _frame):
        cancelled.append(number)
    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[number] = signal.signal(number, cancellation)
    result = REFUSED
    stats = {"state": "unavailable"}
    native = None
    failure = None
    try:
        config.write_text("[cache.disk]\ndir = " + json.dumps(str(cache)) + "\nsize = " + str(context.cache_size) + "\n")
        native = {key: value for key, value in os.environ.items() if not key.startswith("SCCACHE_")}
        native.update(SCCACHE_CONF=str(config), SCCACHE_CACHED_CONF=str(directory / "cached.toml"),
                      SCCACHE_SERVER_UDS=str(endpoint), SCCACHE_NO_DAEMON="1",
                      SCCACHE_IDLE_TIMEOUT="0", SCCACHE_CLIENT_SIDE="1")
        # The storage-only server needs none of Cargo's provider credentials.
        # Compiler clients retain their normal build environment; remote cache
        # selection is refused before either process can start.
        server_env = {key: value for key, value in native.items() if key.startswith("SCCACHE_")}
        server_env.update(PATH=os.environ.get("PATH", ""), HOME=str(directory),
                          TMPDIR=str(directory), SCCACHE_START_SERVER="1")
        server_env.pop("SCCACHE_CLIENT_SIDE")
        server = subprocess.Popen([str(context.cache_program)], env=server_env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        joined = False
        deadline = time.monotonic() + 10
        while server.poll() is None and not socket_ready(endpoint) and not cancelled and time.monotonic() < deadline:
            time.sleep(0.05)
        if cancelled:
            result = 128 + cancelled[0]
        elif server.poll() is not None or not socket_ready(endpoint):
            evidence("build_result", {"class": "cache_start_unavailable", "exit": REFUSED})
        else:
            native["RUSTC_WRAPPER"] = str(context.cache_program)
            result = subprocess.call(command, env=native)
            if server.poll() is not None:
                # A client may have tried its own foreground restart. E1 still
                # owns that group; never present the lost original server as a
                # joined successful cache invocation.
                result = INCOMPLETE
            else:
                stats = native_stats(context.cache_program, native, server, endpoint)
    except (OSError, ValueError, ContextError) as error:
        failure = error
    finally:
        try:
            if server is not None:
                try:
                    if native is not None and socket_ready(endpoint):
                        probe(context.cache_program, ["--stop-server"], native)
                finally:
                    # Even failed diagnostics or a failed stop request must
                    # reach the terminal owner. No shared daemon is adopted.
                    try:
                        server.wait(timeout=10)
                        joined = True
                    except subprocess.TimeoutExpired:
                        # E1 retains the foreground group and its generation;
                        # no blanket kill or removal pretends it has completed.
                        result = INCOMPLETE
        finally:
            for number, handler in previous.items():
                signal.signal(number, handler)
            if joined:
                shutil.rmtree(directory)
            else:
                evidence("build_result", {"class": "custody_incomplete", "exit": INCOMPLETE})
    if not joined:
        return INCOMPLETE
    if failure is not None:
        raise failure
    evidence("cache_stats", stats)
    if cancelled:
        result = 128 + cancelled[0]
    return result


def run(command):
    cargo_arguments = command[1:command.index("--")] if "--" in command else command[1:]
    if not any(flag in cargo_arguments for flag in ("--locked", "--frozen")):
        evidence("build_result", {"class": "missing_locked_flag", "exit": REFUSED})
        return REFUSED
    # Public direct execution uses the same supervisor as Make. Re-entry inside
    # an admitted group is verified by E1 rather than a boolean bypass.
    lock = ROOT / "scripts/ci/validation-lock.sh"
    custody = subprocess.run(["bash", str(lock), "--assert-held"], check=False)
    if custody.returncode == 1 and not any(os.environ.get(key) for key in
                                           ("VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_CHILD")):
        os.execvpe("bash", ["bash", str(lock), "--", sys.executable, str(Path(__file__).resolve()), "--run", "--", *command], os.environ)
    if custody.returncode:
        return custody.returncode
    context = Context(command).resolve()
    description = context.describe()
    evidence("build_context", description)
    if description["admission"] != "ready":
        evidence("build_result", {"class": description["reason"], "role": description["role"], "exit": REFUSED})
        return REFUSED
    result = cached_run(context, command) if context.mode == "sccache" else subprocess.call(command)
    # Child output is streamed untouched, never guessed to be storage evidence:
    # a test printing ENOSPC is still just a failed command. Owned I/O below has
    # a real errno; other failures retain current capacity and their actual exit.
    after = Context(command).resolve().describe()
    if after["identity"] != description["identity"] or after["known"] != description["known"]:
        evidence("build_result", {"class": "context_changed", "exit": result or REFUSED})
        return result or REFUSED
    evidence("build_result", {"class": "passed" if result == 0 else "custody_incomplete" if result == INCOMPLETE else "command_failed",
                              "exit": result, "resources": after["resources"], "known": after["known"]})
    return result if result >= 0 else 128 - result


def main():
    arguments = sys.argv[1:]
    if arguments == ["--describe"]:
        description = Context(None).resolve().describe()
        if os.environ.get("BUILD_CONTEXT_EVIDENCE"):
            evidence("build_context", description)
        print(json.dumps(description, sort_keys=True, separators=(",", ":")))
        return 0
    if len(arguments) >= 3 and arguments[:2] == ["--run", "--"]:
        return run(arguments[2:])
    print("usage: build-context.py --describe | --run -- cargo [arguments...]", file=sys.stderr)
    return REFUSED


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ContextError as error:
        try:
            evidence("build_result", {"class": error.reason, "role": error.role, "exit": REFUSED})
        except OSError:
            print("build_result: " + json.dumps({"class": error.reason, "role": error.role, "exit": REFUSED}), file=sys.stderr)
        sys.exit(REFUSED)
    except ValueError:
        # Malformed caller/configuration input never exposes parser excerpts,
        # executable arguments or private paths through a traceback.
        print('build_result: {"class":"invalid_context_input","exit":2}', file=sys.stderr)
        sys.exit(REFUSED)
    except OSError as error:
        # Never stringify an OSError: it can contain a private path or command.
        record = {"class": "resource_exhausted" if error.errno in EXHAUSTED else "owned_io_failed", "exit": REFUSED}
        try:
            evidence("build_result", record)
        except OSError:
            print("build_result: " + json.dumps(record), file=sys.stderr)
        sys.exit(REFUSED)
