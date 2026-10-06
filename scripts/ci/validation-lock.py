#!/usr/bin/env python3
"""Local validation queue. The gate is an immutable hard link, never a PID lock.

Only this file owns admission, process scopes and typed daemon custody. Queue
metadata is private and diagnostic command identities never contain raw argv.
"""

import contextlib
import ctypes
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import re
import secrets
import signal
import stat
import struct
import subprocess
import sys
import time

PROTOCOL = 2
LABEL = "dev.rust-service.validation-owner"
POLL = 0.05
INTERRUPTED = 0
SELF = Path(__file__).resolve()
SAFE_TARGETS = frozenset({
    "build", "test", "test-package", "test-changed", "lint", "lint-changed",
    "check", "check-unlocked", "verify", "template-init-check", "sqlx-check",
    "sqlx-prepare", "test-integration-db", "test-integration-messaging",
    "test-integration-cache", "test-integration-object-storage", "test-integration-oauth",
    "runtime-image-build", "runtime-image-check", "runtime-progress-proof",
    "migration-validate", "dockerfile-check", "container-security", "container-sbom",
    "shellcheck", "docs-check", "grpc-generate", "grpc-check",
})
SAFE_SCRIPTS = frozenset(target + ".sh" for target in SAFE_TARGETS)


class Refusal(Exception):
    pass


class Deadline(Exception):
    pass


def interrupt(signum, _frame):
    global INTERRUPTED
    if not INTERRUPTED:
        INTERRUPTED = signum


@contextlib.contextmanager
def launch_signals_blocked():
    # A signal accepted before this critical section prevents launch. A signal
    # pending inside it is delivered after the launch decision, then forwarded.
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def safe_open(path, flags=os.O_RDONLY):
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    st = os.fstat(fd)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid():
        os.close(fd)
        raise Refusal("ownership-unavailable: unsafe metadata file")
    return fd


def read_json(path):
    fd = safe_open(path)
    with os.fdopen(fd, "r") as stream:
        return json.load(stream)


def atomic_json(path, value):
    path = Path(path)
    tmp = path.with_name(path.name + "." + secrets.token_hex(8))
    try:
        fd = safe_open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
        with os.fdopen(fd, "w") as stream:
            json.dump(value, stream, separators=(",", ":"))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(tmp, path)
        sync_dir(path.parent)
    finally:
        with contextlib.suppress(FileNotFoundError):
            tmp.unlink()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def private_dir(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = path.lstat()
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.getuid():
        raise Refusal("ownership-unavailable: unsafe queue directory")
    os.chmod(path, 0o700)


def command_output(args, timeout=5):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, timeout=timeout, check=False)
    if result.returncode:
        raise Refusal("ownership-unavailable: native readback failed: " + Path(args[0]).name)
    return result.stdout.strip()


def boot_id():
    if sys.platform == "linux":
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    if sys.platform == "darwin":
        return command_output(["sysctl", "-n", "kern.boottime"])
    raise Refusal("ownership-unavailable: unsupported process identity platform")


def process_identity(pid):
    """Kernel start identity, including sub-second reuse protection on Darwin."""
    if sys.platform == "linux":
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            return None
        return fields[19]
    if sys.platform == "darwin":
        # SDK sys/proc_info.h: PROC_PIDTBSDINFO (3), proc_bsdinfo is 136 bytes;
        # the final uint64 pair is pbi_start_tvsec, pbi_start_tvusec.
        lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                    ctypes.c_void_p, ctypes.c_int]
        buf = ctypes.create_string_buffer(136)
        count = lib.proc_pidinfo(pid, 3, 0, buf, len(buf))
        if count == 0 and ctypes.get_errno() == errno.ESRCH:
            return None
        if count != len(buf):
            raise Refusal("process identity unavailable")
        return ":".join(str(v) for v in struct.unpack_from("=QQ", buf.raw, 120))
    raise Refusal("process identity unavailable")


def session_members(sid):
    """A successful complete native snapshot is required to assert absence."""
    output = command_output(["ps", "-axo", "pid=,stat="])
    members = []
    for line in output.splitlines():
        pid_text, state = line.split(None, 1)
        if state.startswith("Z"):
            continue  # Reaped-or-waitable zombies cannot execute work.
        pid = int(pid_text)
        try:
            if os.getsid(pid) == sid:
                members.append(pid)
        except ProcessLookupError:
            continue
        except PermissionError as error:
            raise Refusal("process session observation unavailable") from error
    return members


def scope_absent(scope, current_boot):
    if scope["boot"] != current_boot:
        return True
    if session_members(scope["sid"]):
        return False
    # A process can fork while the native listing is assembled. Kernel group
    # absence closes that snapshot race for the anchored ordinary group.
    try:
        os.killpg(scope["sid"], 0)
    except ProcessLookupError:
        return True
    except PermissionError as error:
        raise Refusal("process group absence is unavailable") from error
    return False


def signal_scope(scope, signum, current_boot):
    if scope["boot"] != current_boot:
        return
    members = session_members(scope["sid"])
    if not members:
        return
    # The launch sentinel anchors the whole session until all descendants exit.
    # Once that identity is lost, never signal a recyclable numeric group.
    if process_identity(scope["pid"]) != scope["identity"]:
        raise Refusal("scope anchor unavailable; termination is unknown")
    groups = set()
    for pid in members:
        try:
            groups.add(os.getpgid(pid))
        except ProcessLookupError:
            pass
    if process_identity(scope["pid"]) != scope["identity"]:
        raise Refusal("scope anchor changed during cancellation")
    for pgid in groups:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(pgid, signum)


def close_except(retained):
    last = 3
    for fd in sorted(fd for fd in retained if fd >= 3):
        os.closerange(last, fd)
        last = fd + 1
    os.closerange(last, os.sysconf("SC_OPEN_MAX"))


def prepare_scope(command, environment, completion, lease_path, current_boot):
    """Prepare a native session with no command effect until the launch byte."""
    barrier_read, barrier_write = os.pipe()
    ready_read, ready_write = os.pipe()
    lease = safe_open(lease_path, os.O_RDWR | os.O_CREAT | os.O_EXCL)
    fcntl.flock(lease, fcntl.LOCK_EX)
    pid = os.fork()
    if pid == 0:
        try:
            close_except({0, 1, 2, barrier_read, ready_write, lease})
            os.setsid()
            signal.signal(signal.SIGINT, lambda *_: None)
            signal.signal(signal.SIGTERM, lambda *_: None)
            ready = {"pid": os.getpid(), "sid": os.getsid(0),
                     "identity": process_identity(os.getpid()), "boot": current_boot}
            os.write(ready_write, json.dumps(ready).encode() + b"\n")
            os.close(ready_write)
            launch = os.read(barrier_read, 1)
            if launch != b"L":
                os._exit(0)
            # Reset ignored Python handlers in the actual executable child.
            def child_signals():
                signal.signal(signal.SIGINT, signal.SIG_DFL)
                signal.signal(signal.SIGTERM, signal.SIG_DFL)
            try:
                child = subprocess.Popen(command, env=environment, preexec_fn=child_signals)
                result = child.wait()
                result = 128 - result if result < 0 else result
            except FileNotFoundError:
                result = 127
            except OSError:
                result = 126
            atomic_json(completion, {"exit": result, "terminal": True})
            while any(member != os.getpid() for member in session_members(os.getsid(0))):
                time.sleep(POLL)
            # Keep the session identity pinned until the guardian retires its
            # signal capability. Guardian death also closes this pipe, allowing
            # a genuinely terminal orphan scope to finish and be reconciled.
            os.read(barrier_read, 1)
            os.close(barrier_read)
            os._exit(0)
        except BaseException:
            # No fabricated completion receipt if the sole observer is lost.
            os._exit(1)
    os.close(barrier_read)
    os.close(ready_write)
    os.close(lease)
    with os.fdopen(ready_read, "rb") as stream:
        ready = stream.readline(2048)
    if not ready:
        os.close(barrier_write)
        os.waitpid(pid, 0)
        raise Refusal("ownership-unavailable: command session preparation failed")
    return json.loads(ready), barrier_write


def lease_held(path):
    fd = safe_open(path)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(fd, fcntl.LOCK_UN)
        return False
    finally:
        os.close(fd)


def safe_command(command):
    executable = Path(command[0]).name
    if executable in {"bash", "sh", "python3"} and len(command) > 1:
        script = Path(command[1])
        if script.parent.as_posix() in {"scripts/ci", "scripts/lib"} and script.name in SAFE_SCRIPTS:
            return executable + " " + script.name
    if executable == "make" and len(command) > 1 and command[1] in SAFE_TARGETS:
        return "make " + command[1]
    return "command-" + secrets.token_hex(6)


class Queue:
    def __init__(self, gate, deadline=None):
        self.started = time.monotonic()
        supplied = Path(gate).absolute()
        self.gate = supplied.parent.resolve() / supplied.name
        self.directory = self.gate.with_name(self.gate.name + ".queue")
        self.deadline = deadline
        self.boot = boot_id()
        private_dir(self.directory)
        self.tickets = self.directory / "tickets"
        self.owners = self.directory / "owners"
        private_dir(self.tickets)
        private_dir(self.owners)
        self.mutex = safe_open(self.directory / "mutex", os.O_RDWR | os.O_CREAT)
        self.check_filesystem()

    def check_filesystem(self):
        if sys.platform == "darwin":
            # Darwin sys/mount.h: the 64-bit statfs structure has f_fstypename
            # at offset 72 (16 bytes), followed by two MAXPATHLEN buffers.
            native = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
            native.statfs.argtypes = [ctypes.c_char_p, ctypes.c_void_p]
            buffer = ctypes.create_string_buffer(2168)
            if native.statfs(os.fsencode(self.directory), buffer) != 0:
                raise Refusal("ownership-unavailable: filesystem observation failed")
            filesystem = buffer.raw[72:88].split(b"\0", 1)[0].decode("ascii")
        else:
            filesystem = command_output(["stat", "-f", "-c", "%T", str(self.directory)])
        if filesystem.lower() not in {"apfs", "hfs", "ext2/ext3", "ext4", "tmpfs", "overlayfs", "xfs", "btrfs", "zfs"}:
            raise Refusal("ownership-unavailable: unverified local filesystem")

    @contextlib.contextmanager
    def locked(self, interruptible=False):
        while True:
            try:
                fcntl.flock(self.mutex, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if interruptible and INTERRUPTED:
                    raise InterruptedError
                if self.deadline is not None and time.monotonic() >= self.deadline:
                    raise Deadline
                time.sleep(POLL)
        try:
            yield
        finally:
            fcntl.flock(self.mutex, fcntl.LOCK_UN)

    def check_deadline(self):
        if INTERRUPTED:
            raise InterruptedError
        if self.deadline is not None and time.monotonic() >= self.deadline:
            raise Deadline

    def owner_dir(self, token):
        if not re.fullmatch(r"[0-9a-f]{32}", token):
            raise Refusal("invalid owner token")
        return self.owners / token

    def load_gate(self):
        try:
            inode = self.gate.lstat()
        except FileNotFoundError:
            return None
        if stat.S_ISDIR(inode.st_mode):
            return {"protocol": "legacy", "stage": "legacy-held", "identity_confidence": "unknown"}
        if not stat.S_ISREG(inode.st_mode):
            raise Refusal("gate is not an owned regular file")
        gate = read_json(self.gate)
        if gate.get("protocol") != PROTOCOL or gate.get("domain") != str(self.gate):
            raise Refusal("gate protocol/domain unavailable")
        owner = self.owner_dir(gate["token"])
        expected = (owner / "gate").stat(follow_symlinks=False)
        if (inode.st_dev, inode.st_ino) != (expected.st_dev, expected.st_ino):
            raise Refusal("gate generation does not match its owner")
        state = read_json(owner / "state.json")
        if state.get("token") != gate["token"]:
            raise Refusal("owner generation changed")
        return {**gate, **state, "inode": [inode.st_dev, inode.st_ino],
                "identity_confidence": "lease-held" if lease_held(owner / "lease") else "lease-unheld"}

    def save_state(self, token, state):
        state["token"] = token
        atomic_json(self.owner_dir(token) / "state.json", state)

    def authenticate(self, allow_cancel=False):
        token = os.environ.get("VALIDATION_LOCK_TOKEN", "")
        with self.locked():
            gate = self.load_gate()
            if not gate or gate.get("token") != token:
                raise Refusal("ownership-unavailable: inherited owner is stale or absent")
            if not lease_held(self.owner_dir(token) / "lease"):
                raise Refusal("ownership-unavailable: guardian lease is not live")
            if gate["guardian"]["identity"] != process_identity(gate["guardian"]["pid"]):
                raise Refusal("ownership-unavailable: guardian identity changed")
            if gate["scope"]["boot"] != self.boot or os.getsid(0) != gate["scope"]["sid"]:
                raise Refusal("ownership-unavailable: caller is outside the command session")
            if gate["stage"] != "running" and not (allow_cancel and gate["stage"] == "cancelling"):
                raise Refusal("ownership-unavailable: owner no longer accepts effects")
            return gate

    def register_ticket(self, command):
        with self.locked(interruptible=True):
            self.check_deadline()
            sequence_path = self.directory / "sequence.json"
            try:
                sequence = read_json(sequence_path)["next"]
            except FileNotFoundError:
                sequence = 1
            atomic_json(sequence_path, {"next": sequence + 1})
            token = secrets.token_hex(16)
            fd = safe_open(self.tickets / (token + ".lease"), os.O_RDWR | os.O_CREAT | os.O_EXCL)
            fcntl.flock(fd, fcntl.LOCK_EX)
            try:
                candidate = command_output(["git", "rev-parse", "HEAD"])
            except (Refusal, FileNotFoundError):
                candidate = "unavailable"
            ticket = {"sequence": sequence, "token": token, "command": safe_command(command), "candidate": candidate}
            atomic_json(self.tickets / (token + ".json"), ticket)
            return ticket, fd

    def remove_ticket(self, token):
        # Lease inodes are retained, never replaced while another actor can hold one.
        with contextlib.suppress(FileNotFoundError):
            (self.tickets / (token + ".json")).unlink()

    def live_tickets(self):
        tickets = []
        for path in self.tickets.glob("*.json"):
            ticket = read_json(path)
            if lease_held(self.tickets / (ticket["token"] + ".lease")):
                tickets.append(ticket)
            else:
                owner = self.owner_dir(ticket["token"])
                if not (owner / "gate").exists():
                    path.unlink()
                    continue
                # A guardian can die after preparation but before linking the
                # gate. EOF closes its launch barrier; require actual scope
                # absence before discarding that unpublished registration.
                prepared = read_json(owner / "gate")
                active = self.load_gate()
                if (not active or active.get("token") != ticket["token"]) and not lease_held(owner / "lease") and scope_absent(prepared["scope"], self.boot):
                    path.unlink()
                else:
                    tickets.append(ticket)
        return sorted(tickets, key=lambda item: item["sequence"])

    def quarantine(self, token, reason):
        with self.locked():
            gate = self.load_gate()
            if gate and gate.get("token") == token:
                state = read_json(self.owner_dir(token) / "state.json")
                state["stage"] = "quarantined"
                state["quarantine"] = reason
                self.save_state(token, state)

    def status(self):
        with self.locked():
            return {"protocol": PROTOCOL, "domain": str(self.gate),
                    "gate": self.load_gate(), "tickets": self.live_tickets()}

    def release(self, token, guardian=False):
        with self.locked():
            gate = self.load_gate()
            if not gate or gate.get("token") != token:
                return False
            owner = self.owner_dir(token)
            if not guardian and lease_held(owner / "lease"):
                return False
            if not scope_absent(gate["scope"], self.boot):
                return False
            for record in gate["resources"]:
                if record["state"] != "complete":
                    return False
                for observer in record.get("observers", []):
                    if not scope_absent(observer["scope"], self.boot):
                        return False
                    if not (owner / observer["receipt"]).exists():
                        return False
            # Same-token/inode check and unlink are serialized with every admission.
            current = self.gate.lstat()
            if [current.st_dev, current.st_ino] != gate["inode"]:
                return False
            self.gate.unlink()
            sync_dir(self.gate.parent)
            self.remove_ticket(token)
            return True

    def reconcile(self):
        with self.locked():
            gate = self.load_gate()
            if not gate:
                self.live_tickets()
                return True
            if gate.get("protocol") == "legacy":
                return False  # PID-only legacy metadata never proves termination.
            token = gate["token"]
            if lease_held(self.owner_dir(token) / "lease"):
                return False
        try:
            ordinary_absent = scope_absent(gate["scope"], self.boot)
        except (Refusal, OSError, subprocess.TimeoutExpired):
            self.quarantine(token, "ordinary process identity/absence cannot be observed")
            return False
        if not ordinary_absent:
            self.quarantine(token, "ordinary session still exists; await its actual termination")
            return False
        for record in gate["resources"]:
            try:
                self.resource_cleanup(token, record["token"])
            except (Refusal, subprocess.TimeoutExpired, OSError):
                self.quarantine(token, "resource " + record["token"] + " requires native terminal readback")
                return False
        if self.release(token):
            return True
        self.quarantine(token, "protected observer or terminal receipt remains unresolved")
        return False

    def resource(self, owner_token, resource_token):
        state = read_json(self.owner_dir(owner_token) / "state.json")
        for record in state["resources"]:
            if record["token"] == resource_token:
                return record
        raise Refusal("unknown resource token")

    def update_resource(self, owner_token, record):
        with self.locked():
            state = read_json(self.owner_dir(owner_token) / "state.json")
            for index, existing in enumerate(state["resources"]):
                if existing["token"] == record["token"]:
                    # Observer publication may race a native readback. Never
                    # replace its durable custody with an earlier snapshot.
                    record["observers"] = existing["observers"]
                    state["resources"][index] = record
                    self.save_state(owner_token, state)
                    return
            raise Refusal("resource generation unavailable")

    def docker(self, args, daemon=None):
        timeout = 30
        if self.deadline is not None:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                raise Deadline
            timeout = min(timeout, remaining)
        if daemon is not None and self.daemon() != daemon:
            raise Refusal("Docker daemon/context identity changed")
        return command_output(["docker", *args], timeout=timeout)

    def daemon(self):
        context = self.docker(["context", "show"])
        identity = self.docker(["info", "--format", "{{.ID}}"])
        if not identity or not context:
            raise Refusal("Docker daemon identity unavailable")
        return {"context": context, "id": identity}

    def inspect_container(self, name, daemon):
        ids = self.docker(["ps", "-aq", "--filter", "name=^/" + name + "$"], daemon).splitlines()
        if not ids:
            return None
        records = json.loads(self.docker(["inspect", *ids], daemon))
        exact = [record for record in records if record.get("Name", "").lstrip("/") == name]
        if len(exact) != 1:
            raise Refusal("container identity is ambiguous")
        return exact[0]

    def add_resource(self, kind, name, files=(), adopted=False):
        gate = self.authenticate()
        token = gate["token"]
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,160}", name):
            raise Refusal("invalid resource name")
        if token[:12] not in name and not adopted:
            raise Refusal("resource name must contain the owner token prefix")
        daemon = self.daemon()
        if kind == "container":
            if self.inspect_container(name, daemon):
                raise Refusal("container name already exists")
        elif kind == "compose":
            if not files or any(not Path(path).is_file() for path in files):
                raise Refusal("Compose files are required")
            if self.docker(["ps", "-aq", "--filter", "label=com.docker.compose.project=" + name], daemon):
                raise Refusal("Compose project already exists")
            for resource in ("network", "volume"):
                if self.docker([resource, "ls", "-q", "--filter", "label=com.docker.compose.project=" + name], daemon):
                    raise Refusal("Compose project already has native resources")
        elif kind != "buildkit":
            raise Refusal("unsupported resource kind")
        record = {"token": secrets.token_hex(16), "kind": kind, "name": name,
                  "daemon": daemon, "state": "registered", "observers": []}
        if files:
            record["files"] = [str(Path(path).resolve()) for path in files]
        with self.locked():
            current = self.load_gate()
            if not current or current.get("token") != token or current["stage"] != "running" or INTERRUPTED:
                raise Refusal("owner stopped before resource registration")
            state = read_json(self.owner_dir(token) / "state.json")
            if len(state["resources"]) >= 256:
                raise Refusal("resource registry capacity reached")
            if any(row["name"] == name and row["kind"] == kind for row in state["resources"]):
                raise Refusal("resource identity is already registered")
            state["resources"].append(record)
            self.save_state(token, state)
        return token, record

    def bind_container(self, token, resource_token, container_id):
        record = self.resource(token, resource_token)
        if record["kind"] != "container":
            raise Refusal("container binding requires a container resource")
        container = self.inspect_container(record["name"], record["daemon"])
        if not container or container["Id"] != container_id:
            raise Refusal("container binding identity changed")
        self.check_container_owner(token, record, container)
        record["id"] = container_id
        self.update_resource(token, record)

    @staticmethod
    def check_container_owner(token, record, container):
        if container.get("Config", {}).get("Labels", {}).get(LABEL) != token:
            raise Refusal("container owner label does not match")
        if record.get("id") and record["id"] != container["Id"]:
            raise Refusal("immutable container identity changed")

    def resource_terminal(self, token, record):
        daemon = record["daemon"]
        if self.daemon() != daemon:
            raise Refusal("Docker daemon/context identity changed")
        if record["kind"] == "container":
            container = self.inspect_container(record["name"], daemon)
            if container:
                self.check_container_owner(token, record, container)
                if container["State"].get("Running") or container["State"].get("Restarting"):
                    return False
        elif record["kind"] == "compose":
            ids = self.docker(["ps", "-aq", "--filter", "label=com.docker.compose.project=" + record["name"]], daemon).splitlines()
            if ids:
                for container in json.loads(self.docker(["inspect", *ids], daemon)):
                    if container["State"].get("Running") or container["State"].get("Restarting"):
                        return False
        elif record["kind"] == "buildkit":
            if not record.get("nodes"):
                # Bootstrap can crash before IDs were bound; native builder discovery
                # is confined to the reserved unique name, never the default builder.
                self.builder_nodes(record)
            for node in record.get("nodes", []):
                container = self.inspect_container(node["name"], daemon)
                if container:
                    if container["Id"] != node["id"]:
                        raise Refusal("builder node was replaced")
                    if container["State"].get("Running") or container["State"].get("Restarting"):
                        return False
        for observer in record["observers"]:
            receipt = self.owner_dir(token) / observer["receipt"]
            if not receipt.exists() or not read_json(receipt).get("terminal"):
                return False
            if not scope_absent(observer["scope"], self.boot):
                return False
        return True

    def complete_resource(self, token, resource_token):
        record = self.resource(token, resource_token)
        if record["state"] != "complete":
            record["state"] = "closing"
            self.update_resource(token, record)
        if not self.resource_terminal(token, record):
            raise Refusal("resource is live or its terminal response is unavailable")
        record["state"] = "complete"
        self.update_resource(token, record)

    def resource_cleanup(self, token, resource_token):
        previous_deadline = self.deadline
        operation_deadline = time.monotonic() + 30
        self.deadline = min(previous_deadline, operation_deadline) if previous_deadline is not None else operation_deadline
        try:
            self._resource_cleanup(token, resource_token)
        except Deadline as error:
            raise Refusal("resource cleanup deadline expired; terminal state remains unknown") from error
        finally:
            self.deadline = previous_deadline

    def _resource_cleanup(self, token, resource_token):
        record = self.resource(token, resource_token)
        if record["state"] == "complete":
            return
        record["state"] = "closing"
        self.update_resource(token, record)
        initially_pending = any(not (self.owner_dir(token) / item["receipt"]).exists()
                                or not scope_absent(item["scope"], self.boot)
                                for item in record["observers"])
        self.native_cleanup(token, record)
        repeated = False
        # Native stop may make progress while the protected CLI is receiving its
        # terminal response. A pending create/up can also finish after the first
        # stop readback. One repeat is permitted only after that fresh response
        # establishes progress, and shares the same 30-second operation budget.
        while True:
            record = self.resource(token, resource_token)
            if self.resource_terminal(token, record):
                record["state"] = "complete"
                self.update_resource(token, record)
                return
            if time.monotonic() >= self.deadline:
                raise Refusal("resource terminal response is unavailable")
            pending = False
            for observer in record["observers"]:
                receipt = self.owner_dir(token) / observer["receipt"]
                if process_identity(observer["scope"]["pid"]) != observer["scope"]["identity"] and not receipt.exists():
                    raise Refusal("external terminal observer lost; provider terminal proof required")
                pending = pending or not receipt.exists() or not scope_absent(observer["scope"], self.boot)
            if initially_pending and not pending and not repeated:
                self.native_cleanup(token, record)
                repeated = True
            time.sleep(POLL)

    def native_cleanup(self, token, record):
        daemon = record["daemon"]
        if self.daemon() != daemon:
            raise Refusal("Docker daemon/context identity changed")
        if record["kind"] == "container":
            container = self.inspect_container(record["name"], daemon)
            if container:
                self.check_container_owner(token, record, container)
                self.docker(["rm", "-f", container["Id"]], daemon)
        elif record["kind"] == "compose":
            args = ["compose", "-p", record["name"]]
            for path in record["files"]:
                args.extend(["-f", path])
            self.docker([*args, "down", "-v", "--remove-orphans"], daemon)
        elif record["kind"] == "buildkit":
            if not record.get("nodes"):
                self.builder_nodes(record)
                self.update_resource(token, record)
            for node in record.get("nodes", []):
                container = self.inspect_container(node["name"], daemon)
                if container:
                    if container["Id"] != node["id"]:
                        raise Refusal("builder node was replaced")
                    if container["State"].get("Running") or container["State"].get("Restarting"):
                        self.docker(["stop", "--time", "10", node["id"]], daemon)

    def builder_nodes(self, record):
        builders = self.builder_inventory(record["daemon"])
        matching = [item for item in builders if item["Name"] == record["name"]]
        if not matching:
            prefix = "^/buildx_buildkit_" + re.escape(record["name"])
            if self.docker(["ps", "-aq", "--filter", "name=" + prefix], record["daemon"]):
                raise Refusal("reserved builder has orphan nodes without native metadata")
            record["nodes"] = []
            return
        if len(matching) != 1:
            raise Refusal("builder identity is ambiguous")
        info = matching[0]
        if info.get("Name") != record["name"] or info.get("Driver") != "docker-container":
            raise Refusal("ownership-unavailable: builder must use docker-container")
        nodes = []
        for node in info.get("Nodes", []):
            self.check_builder_endpoint(node, record["daemon"])
            name = "buildx_buildkit_" + node["Name"]
            container = self.inspect_container(name, record["daemon"])
            if not container:
                raise Refusal("builder node identity unavailable")
            nodes.append({"name": name, "id": container["Id"]})
        if not nodes:
            raise Refusal("builder has no owned nodes")
        record["nodes"] = nodes

    def check_builder_endpoint(self, node, daemon):
        endpoint = node.get("Endpoint")
        if not endpoint:
            raise Refusal("builder node endpoint unavailable")
        if endpoint != daemon["context"]:
            option = "--host" if "://" in endpoint else "--context"
            identity = self.docker([option, endpoint, "info", "--format", "{{.ID}}"], daemon)
            if identity != daemon["id"]:
                raise Refusal("ownership-unavailable: builder node belongs to another daemon")

    def builder_inventory(self, daemon):
        # Buildx v0.33 inspect is human-only. ls --format json is its supported
        # structured builder/node interface (commands/ls.go, Builder.MarshalJSON).
        output = self.docker(["buildx", "ls", "--format", "json"], daemon)
        return [json.loads(line) for line in output.splitlines() if line]

    def builder_prepare(self, name=None):
        gate = self.authenticate()
        adopted = name is not None
        if adopted and os.environ.get("VALIDATION_BUILDER_EXCLUSIVE") != "1":
            raise Refusal("ownership-unavailable: builder is not task-exclusive")
        name = name or ("validation-" + gate["token"][:12] + "-" + secrets.token_hex(4))
        daemon = self.daemon()
        builders = self.builder_inventory(daemon)
        present = any(item["Name"] == name for item in builders)
        if not adopted and present:
            raise Refusal("reserved builder name already exists")
        if adopted and not present:
            raise Refusal("owned setup builder is unavailable")
        if adopted:
            matching = [item for item in builders if item["Name"] == name]
            if len(matching) != 1 or matching[0].get("Driver") != "docker-container" or not matching[0].get("Nodes"):
                raise Refusal("ownership-unavailable: adoption requires an exclusive docker-container builder")
            for node in matching[0]["Nodes"]:
                self.check_builder_endpoint(node, daemon)
        # A collision is refused before claiming custody; failed registration
        # must never let guardian recovery stop somebody else's existing builder.
        token, record = self.add_resource("buildkit", name, adopted=adopted)
        if record["daemon"] != daemon:
            raise Refusal("Docker daemon changed during builder reservation")
        # Persist the reservation before native create/bootstrap can leave nodes.
        if not adopted:
            self.docker(["buildx", "create", "--name", name, "--driver", "docker-container"], record["daemon"])
        self.docker(["buildx", "inspect", name, "--bootstrap"], record["daemon"])
        self.builder_nodes(record)
        if not record["nodes"]:
            raise Refusal("builder bootstrap did not establish node identities")
        record["state"] = "ready"
        self.update_resource(token, record)
        return record["token"]

    def resource_run(self, resource_token, command):
        gate = self.authenticate()
        token = gate["token"]
        record = self.resource(token, resource_token)
        if record["state"] in {"closing", "complete"}:
            raise Refusal("closing resource cannot accept more effects")
        if Path(command[0]).name != "docker":
            raise Refusal("resource observer requires the native Docker CLI")
        if record["kind"] == "buildkit":
            if command[1:3] != ["buildx", "build"] or "--builder" not in command:
                raise Refusal("builder observer requires an explicit buildx build")
            index = command.index("--builder")
            if index + 1 >= len(command) or command[index + 1] != record["name"] or not record.get("nodes"):
                raise Refusal("build is not bound to the registered builder")
        elif record["kind"] == "container":
            if command[1:2] in (["run"], ["create"]):
                if "--name" not in command or command[command.index("--name") + 1] != record["name"]:
                    raise Refusal("container observer requires its registered name")
                if LABEL + "=" + token not in command:
                    raise Refusal("container observer requires its registered owner label")
            elif command[1:2] == ["start"]:
                if command[-1] not in {record["name"], record.get("id")}:
                    raise Refusal("container start is not bound to its registered identity")
                container = self.inspect_container(record["name"], record["daemon"])
                if not container:
                    raise Refusal("registered container is absent before start")
                self.check_container_owner(token, record, container)
            else:
                raise Refusal("container observer requires run, create or start")
        elif record["kind"] == "compose":
            if command[1:2] != ["compose"] or "-p" not in command or command[command.index("-p") + 1] != record["name"] or "up" not in command:
                raise Refusal("Compose observer requires its registered project and up command")
            files = [str(Path(command[index + 1]).resolve()) for index, arg in enumerate(command[:-1]) if arg == "-f"]
            if files != record["files"]:
                raise Refusal("Compose observer files differ from registration")
        else:
            raise Refusal("resource does not support a terminal CLI observer")
        if self.daemon() != record["daemon"]:
            raise Refusal("Docker daemon/context identity changed")
        operation = secrets.token_hex(16)
        owner = self.owner_dir(token)
        receipt = operation + ".terminal.json"
        environment = dict(os.environ)
        for key in ("VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_HELD"):
            environment.pop(key, None)
        scope, barrier = prepare_scope(command, environment, owner / receipt,
                                       owner / (operation + ".lease"), self.boot)
        launched = False
        try:
            with self.locked():
                current = self.load_gate()
                if not current or current.get("token") != token or current["stage"] != "running" or INTERRUPTED:
                    raise Refusal("cancelled before external launch barrier")
                state = read_json(owner / "state.json")
                for item in state["resources"]:
                    if item["token"] == resource_token:
                        if item["state"] in {"closing", "complete"}:
                            raise Refusal("resource closed before native operation publication")
                        if any(not scope_absent(observer["scope"], self.boot)
                               or not (owner / observer["receipt"]).exists()
                               for observer in item["observers"]):
                            raise Refusal("resource still has an unfinished native operation")
                        if len(item["observers"]) >= 256:
                            raise Refusal("observer registry capacity reached")
                        item["observers"].append({"scope": scope, "receipt": receipt,
                                                  "launch_may_have_occurred": True})
                        item["state"] = "running"
                        break
                with launch_signals_blocked():
                    if INTERRUPTED:
                        raise Refusal("cancelled before external launch barrier")
                    self.save_state(token, state)
                    os.write(barrier, b"L")
                    launched = True
            # The registered observer survives ordinary caller cancellation. Do
            # not signal it or reinterpret a lost response as solve completion.
            while True:
                if INTERRUPTED:
                    return 128 + INTERRUPTED
                if barrier is not None and (owner / receipt).exists() and session_members(scope["sid"]) == [scope["pid"]]:
                    os.close(barrier)
                    barrier = None
                pid, _ = os.waitpid(scope["pid"], os.WNOHANG)
                if pid:
                    if not (owner / receipt).exists():
                        raise Refusal("external terminal observer exited without a receipt")
                    return read_json(owner / receipt)["exit"]
                time.sleep(POLL)
        finally:
            if barrier is not None:
                os.close(barrier)
            if not launched:
                os.waitpid(scope["pid"], 0)
                atomic_json(owner / receipt, {"exit": 128 + INTERRUPTED if INTERRUPTED else 1,
                                              "terminal": True, "launched": False})

    def run(self, command):
        ticket, ticket_fd = self.register_ticket(command)
        token = ticket["token"]
        owner = self.owner_dir(token)
        lease = None
        barrier = None
        scope = None
        published = False
        started = self.started
        last_diagnostic = None
        reconciled_tokens = set()
        try:
            while True:
                self.check_deadline()
                with self.locked(interruptible=True):
                    previous = self.load_gate()
                if previous and previous.get("protocol") == PROTOCOL and previous["token"] not in reconciled_tokens and previous["identity_confidence"] == "lease-unheld":
                    try:
                        if scope_absent(previous["scope"], self.boot):
                            # One bounded native-resource attempt per abandoned
                            # generation. A still-live ordinary scope is observed
                            # until terminal and consumes no recovery attempt.
                            reconciled_tokens.add(previous["token"])
                            self.reconcile()
                        elif previous["stage"] != "quarantined":
                            self.quarantine(previous["token"], "ordinary session still exists; await its actual termination")
                    except (Refusal, subprocess.TimeoutExpired, OSError):
                        pass  # Unknown owner remains excluded and visible.
                with self.locked(interruptible=True):
                    live = self.live_tickets()
                    gate = self.load_gate()
                    first = bool(live and live[0]["token"] == token)
                    diagnostic = (gate.get("token", "legacy") if gate else None,
                                  live[0]["token"] if live else None)
                    if diagnostic != last_diagnostic:
                        print(f"validation wait sequence={ticket['sequence']} token={token[:12]} "
                              f"position={next((i + 1 for i, row in enumerate(live) if row['token'] == token), 0)} "
                              f"elapsed={time.monotonic() - started:.3f}s candidate={ticket['candidate']} "
                              f"command={ticket['command']} owner={diagnostic[0]} "
                              f"identity={gate['identity_confidence'] if gate else 'unowned'} "
                              f"mode={'legacy' if gate and gate.get('protocol') == 'legacy' else 'new'}", file=sys.stderr)
                        last_diagnostic = diagnostic
                    if first and gate is None:
                        private_dir(owner)
                        lease = safe_open(owner / "lease", os.O_RDWR | os.O_CREAT | os.O_EXCL)
                        fcntl.flock(lease, fcntl.LOCK_EX)
                        environment = dict(os.environ)
                        environment.update(VALIDATION_LOCK_TOKEN=token, VALIDATION_LOCK_DOMAIN=str(self.gate),
                                           VALIDATION_LOCK_HELD="1")
                        # An explicit test-domain override is consumed at this root;
                        # descendants authenticate the inherited domain normally.
                        environment.pop("VALIDATION_LOCK_DIR", None)
                        scope, barrier = prepare_scope(command, environment, owner / "command.terminal.json",
                                                       owner / "command.lease", self.boot)
                        state = {"token": token, "stage": "running", "quarantine": None, "resources": []}
                        self.save_state(token, state)
                        gate_record = {"protocol": PROTOCOL, "token": token, "domain": str(self.gate),
                                       "candidate": ticket["candidate"], "command": ticket["command"],
                                       "guardian": {"pid": os.getpid(), "identity": process_identity(os.getpid())},
                                       "scope": scope, "stage": "launch-intent", "launch_may_have_occurred": True}
                        atomic_json(owner / "gate", gate_record)
                        self.check_deadline()
                        try:
                            os.link(owner / "gate", self.gate, follow_symlinks=False)
                        except FileExistsError:
                            os.close(barrier)
                            barrier = None
                            os.waitpid(scope["pid"], 0)
                            # A legacy actor won the atomic path race. This attempt
                            # never launched, so retry preparation under a fresh owner
                            # record while retaining FIFO registration.
                            (owner / "gate").unlink()
                            (owner / "command.lease").unlink()
                            os.close(lease)
                            lease = None
                            (owner / "lease").unlink()
                            scope = None
                            continue
                        published = True
                        sync_dir(self.gate.parent)
                        self.remove_ticket(token)
                        with launch_signals_blocked():
                            self.check_deadline()
                            os.write(barrier, b"L")
                        break
                time.sleep(POLL)
            # The deadline was admission-only. Native cleanup uses its own bounded
            # operation timeout, never the elapsed waiting budget after launch.
            self.deadline = None
            interrupted_at = None
            escalated = False
            command_result = None
            reaped = False
            while True:
                if INTERRUPTED and interrupted_at is None:
                    interrupted_at = time.monotonic()
                    with self.locked():
                        state = read_json(owner / "state.json")
                        state["stage"] = "cancelling"
                        self.save_state(token, state)
                    if barrier is not None:
                        signal_scope(scope, INTERRUPTED, self.boot)
                if not reaped:
                    pid, _ = os.waitpid(scope["pid"], os.WNOHANG)
                    reaped = bool(pid)
                if (owner / "command.terminal.json").exists():
                    command_result = read_json(owner / "command.terminal.json")["exit"]
                if barrier is not None and command_result is not None and session_members(scope["sid"]) == [scope["pid"]]:
                    # No work remains capable of forking. Retire signalling
                    # before permitting the anchor itself to exit.
                    os.close(barrier)
                    barrier = None
                absent = scope_absent(scope, self.boot)
                if interrupted_at is not None and barrier is not None and not absent and time.monotonic() - interrupted_at >= 10 and not escalated:
                    signal_scope(scope, signal.SIGKILL, self.boot)
                    escalated = True
                    os.close(barrier)
                    barrier = None
                if absent:
                    state = read_json(owner / "state.json")
                    for record in state["resources"]:
                        if record["state"] != "complete":
                            try:
                                self.resource_cleanup(token, record["token"])
                            except (Refusal, subprocess.TimeoutExpired, OSError) as error:
                                self.quarantine(token, "resource " + record["token"] + " terminal readback unavailable")
                                print(f"validation quarantined: {error}", file=sys.stderr)
                                return 128 + INTERRUPTED if INTERRUPTED else 1
                    if self.release(token, guardian=True):
                        return 128 + INTERRUPTED if INTERRUPTED else (command_result if command_result is not None else 1)
                    self.quarantine(token, "protected observer session or terminal receipt remains unresolved")
                    return 128 + INTERRUPTED if INTERRUPTED else 1
                if interrupted_at is not None and time.monotonic() - interrupted_at >= 15:
                    self.quarantine(token, "ordinary session termination is unconfirmed")
                    return 128 + INTERRUPTED
                time.sleep(POLL)
        except (Deadline, InterruptedError):
            if barrier is not None:
                os.close(barrier)
                barrier = None
            if scope is not None and not published:
                os.waitpid(scope["pid"], 0)
            if published:
                self.deadline = None
                # The launch barrier is still closed on this path. Reconcile only
                # after the sentinel observes EOF and actually exits.
                if scope:
                    os.waitpid(scope["pid"], 0)
                self.release(token, guardian=True)
            if INTERRUPTED:
                return 128 + INTERRUPTED
            print(f"validation lock timed out: sequence={ticket['sequence']} token={token[:12]} "
                  f"elapsed={time.monotonic() - started:.3f}s candidate={ticket['candidate']} "
                  f"command={ticket['command']} last_owner={last_diagnostic[0] if last_diagnostic else 'unobserved'}", file=sys.stderr)
            return 75
        except (Refusal, OSError, subprocess.TimeoutExpired):
            if published:
                self.deadline = None
                # Failure to persist the quarantine itself does not remove the
                # immutable gate; the next reader still sees the held generation.
                with contextlib.suppress(OSError, Refusal):
                    self.quarantine(token, "guardian could not establish terminal process/resource evidence")
            raise
        finally:
            if barrier is not None:
                os.close(barrier)
            self.deadline = None
            with self.locked():
                self.remove_ticket(token)
            if lease is not None:
                os.close(lease)
            os.close(ticket_fd)


def domain():
    explicit = os.environ.get("VALIDATION_LOCK_DIR")
    if explicit:
        return str(Path(explicit).absolute()), False
    inherited = os.environ.get("VALIDATION_LOCK_DOMAIN")
    if inherited:
        return inherited, True
    common = command_output(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"])
    return str(Path(common) / "codex" / "validation.lock"), False


def timeout_seconds():
    try:
        value = float(os.environ.get("VALIDATION_LOCK_TIMEOUT_SECONDS", "900"))
    except ValueError as error:
        raise ValueError("timeout must be finite and nonnegative") from error
    if not math.isfinite(value) or value < 0:
        raise ValueError("timeout must be finite and nonnegative")
    return value


def main(args):
    if args == ["--self-test"]:
        environment = dict(os.environ)
        for key in ("VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_DIR"):
            environment.pop(key, None)
        return subprocess.call([sys.executable, str(SELF.parent.parent / "tests" / "validation-lock-test.py")], env=environment)
    if not args:
        raise ValueError("usage: validation-lock.sh -- command [args...] | --status | --reconcile")
    fixed_lengths = {"--status": 1, "--reconcile": 1, "--assert-held": 1,
                     "--resource-bind": 3, "--resource-cleanup": 2,
                     "--resource-complete": 2, "--builder-name": 2}
    if args[0] in fixed_lengths and len(args) != fixed_lengths[args[0]]:
        raise ValueError("invalid arguments for " + args[0])
    if args[0] == "--" and len(args) < 2:
        raise ValueError("a command is required after --")
    if args[0] not in {*fixed_lengths, "--", "--resource-register", "--resource-run", "--builder-prepare", "--container-run"}:
        raise ValueError("unknown validation-lock operation")
    waiting = timeout_seconds()
    gate, inherited = domain()
    queue = Queue(gate, time.monotonic() + waiting if args[0] in {"--", "--reconcile"} else None)
    if args[0] == "--" and len(args) > 1:
        if inherited:
            queue.authenticate()
            # Nested wrappers remain in the same native session and never own release.
            child = subprocess.Popen(args[1:])
            result = child.wait()
            return 128 + INTERRUPTED if INTERRUPTED else (128 - result if result < 0 else result)
        return queue.run(args[1:])
    if args == ["--status"]:
        print(json.dumps(queue.status(), sort_keys=True))
        return 0
    if args == ["--reconcile"]:
        return 0 if queue.reconcile() else 1
    if args == ["--assert-held"]:
        queue.authenticate(allow_cancel=True)
        return 0
    if args[0] == "--builder-prepare" and len(args) <= 2:
        print(queue.builder_prepare(args[1] if len(args) == 2 else None))
        return 0
    if args[0] == "--container-run" and len(args) > 4 and args[1:4] == ["--", "docker", "run"]:
        active = queue.authenticate()
        name = "validation-" + active["token"][:12] + "-" + secrets.token_hex(4)
        token, resource = queue.add_resource("container", name)
        command = ["docker", "run", "--name", name, "--label", LABEL + "=" + token, *args[4:]]
        result = queue.resource_run(resource["token"], command)
        queue.resource_cleanup(token, resource["token"])
        return result
    active = queue.authenticate(allow_cancel=args[0] in {"--resource-cleanup", "--resource-complete"})
    token = active["token"]
    if args[0] == "--resource-register" and len(args) >= 3:
        kind, name = args[1:3]
        if kind not in {"compose", "container"}:
            raise ValueError("resource registration supports compose or container; use --builder-prepare for builds")
        files = []
        rest = args[3:]
        while rest:
            if len(rest) < 2 or rest[0] != "--file":
                raise ValueError("resource files use --file PATH")
            files.append(rest[1])
            rest = rest[2:]
        _, record = queue.add_resource(kind, name, files)
        print(record["token"])
    elif args[0] == "--resource-bind" and len(args) == 3:
        queue.bind_container(token, args[1], args[2])
    elif args[0] == "--resource-complete" and len(args) == 2:
        queue.complete_resource(token, args[1])
    elif args[0] == "--resource-cleanup" and len(args) == 2:
        queue.resource_cleanup(token, args[1])
    elif args[0] == "--builder-name" and len(args) == 2:
        record = queue.resource(token, args[1])
        if record["kind"] != "buildkit":
            raise Refusal("resource is not a builder")
        print(record["name"])
    elif args[0] == "--resource-run" and len(args) >= 5 and args[2] == "--":
        return queue.resource_run(args[1], args[3:])
    else:
        raise ValueError("unknown validation-lock operation")
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    try:
        code = main(sys.argv[1:])
    except ValueError as error:
        print(f"validation lock usage: {error}", file=sys.stderr)
        code = 2
    except Deadline:
        print("validation lock timed out before registration or reconciliation", file=sys.stderr)
        code = 75
    except InterruptedError:
        code = 128 + INTERRUPTED
    except (Refusal, OSError, subprocess.TimeoutExpired) as error:
        print(f"validation lock: {error}", file=sys.stderr)
        code = 128 + INTERRUPTED if INTERRUPTED else 1
    sys.exit(code)
