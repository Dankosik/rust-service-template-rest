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
import select
import signal
import stat
import struct
import subprocess
import sys
import time

PROTOCOL = 2
CHILD_PROTOCOL = 3
CHILD_CAPABILITY = "ordinary-child-v1"
LABEL = "dev.rust-service.validation-owner"
POLL = 0.05
WAIT_REPORT_SECONDS = 10
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


def command_output(args, timeout=5, admission=None):
    with admission if admission is not None else contextlib.nullcontext():
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    with process:
        try:
            output, _ = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
            raise
    if process.returncode:
        raise Refusal("ownership-unavailable: native readback failed: " + Path(args[0]).name)
    return output.strip()


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


def session_members(sid, timeout=5):
    """A successful complete native snapshot is required to assert absence."""
    output = command_output(["ps", "-axo", "pid=,stat="], timeout=timeout)
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


def scope_absent(scope, current_boot, timeout=5):
    if scope["boot"] != current_boot:
        return True
    if session_members(scope["sid"], timeout):
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


def signal_scope(scope, writer, signum, current_boot, sequence, deadline_ns):
    """Grant a bounded request through the already-owned channel, never a PID."""
    if scope["boot"] != current_boot or process_identity(scope["pid"]) != scope["identity"]:
        raise Refusal("scope anchor unavailable; termination is unknown")
    if time.monotonic_ns() >= deadline_ns:
        raise Refusal("ordinary cancellation grant deadline expired")
    frame = f"QSC1 {sequence} {signum} {deadline_ns}\n".encode("ascii")
    if len(frame) > min(128, os.fpathconf(writer, "PC_PIPE_BUF")):
        raise Refusal("ordinary cancellation frame exceeds atomic channel capacity")
    os.set_blocking(writer, False)
    # A complete write is irrevocable, even if the issuer dies before recording
    # its return. An ambiguous failure is never retried on this channel.
    if os.write(writer, frame) != len(frame):
        raise Refusal("ordinary cancellation grant is unconfirmed")


def ordinary_scope_loop(command, control, completion, initial_result=None):
    """Deliver already-granted cancellation from inside the owned session."""
    os.set_blocking(control, False)
    sid = os.getsid(0)
    pending = []
    active = None
    relay = None
    buffer = b""
    eof = False
    last_sequence = 0
    stop_deadline = None
    error = None
    result = initial_result
    events = [{"command_finished_ns": time.monotonic_ns()}] if initial_result is not None else []
    saved = None

    def fail(reason):
        nonlocal error
        if error is None:
            error = reason
            events.append({"error": reason, "at_ns": time.monotonic_ns()})

    def publish():
        nonlocal saved
        receipt = {"exit": 128 - result if result is not None and result < 0 else result,
                   "terminal": result is not None and result >= 0,
                   "stop_deadline_ns": stop_deadline,
                   "control_error": error, "control_events": events}
        encoded = json.dumps(receipt, sort_keys=True)
        if encoded != saved:
            atomic_json(completion, receipt)
            saved = encoded

    while True:
        if result is None and command is not None:
            result = command.poll()
            if result is not None:
                events.append({"command_finished_ns": time.monotonic_ns()})
        if not eof:
            try:
                data = os.read(control, 4096)
            except BlockingIOError:
                data = None
            if data == b"":
                eof = True
            elif data:
                buffer += data
            while b"\n" in buffer:
                frame, buffer = buffer.split(b"\n", 1)
                match = re.fullmatch(rb"QSC1 ([12]) ([0-9]{1,2}) ([0-9]{1,19})", frame)
                if len(frame) + 1 > 128 or match is None:
                    fail("malformed ordinary cancellation frame")
                    continue
                sequence, signum, deadline = map(int, match.groups())
                if (sequence != last_sequence + 1 or deadline <= 0 or deadline > (1 << 63) - 1
                        or (sequence == 1 and deadline - time.monotonic_ns() > 15_000_000_000)
                        or (sequence == 1 and signum not in {signal.SIGINT, signal.SIGTERM})
                        or (sequence == 2 and (signum != signal.SIGKILL or deadline != stop_deadline))):
                    fail("duplicate or invalid ordinary cancellation grant")
                    continue
                if sequence == 1:
                    stop_deadline = deadline
                last_sequence = sequence
                if error is None:
                    pending.append((sequence, signum, deadline))
            if len(buffer) > 127 or (eof and buffer):
                fail("partial ordinary cancellation frame")
                buffer = b""
            # Complete frames read together with EOF remain granted; EOF only
            # closes the issuer. No metadata or reopened descriptor is consulted.
        if relay is not None:
            waited, status = os.waitpid(relay, os.WNOHANG)
            if waited:
                allowed_kill = active["signal"] == signal.SIGKILL and os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGKILL
                if not allowed_kill and (not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0):
                    fail("ordinary cancellation relay failed or join was refused")
                events.append({"sequence": active["sequence"], "relay_finished_ns": time.monotonic_ns()})
                relay = None
        # The tail belongs to the complete scope, not just its current delivery
        # queue. EOF after a completed TERM cannot erase a surviving command's
        # overrun or turn its later exit zero into compliant cancellation.
        if stop_deadline is not None and time.monotonic_ns() >= stop_deadline:
            fail("ordinary cancellation scope exceeded its fixed deadline")
        if active is None and pending and error is None:
            sequence, signum, deadline = pending.pop(0)
            if time.monotonic_ns() >= deadline:
                fail("ordinary cancellation grant expired before admission")
            else:
                try:
                    members = session_members(sid, min(5, (deadline - time.monotonic_ns()) / 1e9))
                    hints = set()
                    for pid in members:
                        with contextlib.suppress(ProcessLookupError):
                            hints.add(os.getpgid(pid))
                    hints.discard(sid)
                    active = {"sequence": sequence, "signal": signum, "deadline": deadline,
                              "hints": sorted(hints), "own_group": True}
                except (Refusal, OSError, subprocess.TimeoutExpired) as failure:
                    fail("ordinary cancellation observation failed: " + str(failure))
        if active is not None and relay is None and error is None:
            if time.monotonic_ns() >= active["deadline"]:
                fail("ordinary cancellation grant expired before admission")
            elif active["hints"]:
                hint = active["hints"].pop(0)
                # This fork is irreversible admission. A paused relay may finish
                # late; its session remains owned and prevents final retirement.
                try:
                    relay = os.fork()
                except OSError:
                    fail("ordinary cancellation relay could not be created")
                else:
                    if relay == 0:
                        try:
                            close_except({0, 1, 2})
                            signal.signal(signal.SIGINT, lambda *_: None)
                            signal.signal(signal.SIGTERM, lambda *_: None)
                            # Hints can be stale or foreign. The kernel join,
                            # followed by own-group delivery, binds the target.
                            os.setpgid(0, hint)
                            os.kill(0, active["signal"])
                            os._exit(0)
                        except BaseException:
                            os._exit(1)
            elif active["own_group"]:
                active["own_group"] = False
                events.append({"sequence": active["sequence"], "own_group_admitted_ns": time.monotonic_ns()})
                # Persist the admission before the last, self-terminating KILL.
                publish()
                try:
                    os.kill(0, active["signal"])
                except OSError as failure:
                    fail("ordinary self-group delivery failed: " + str(failure))
                if time.monotonic_ns() >= active["deadline"]:
                    fail("ordinary cancellation delivery completed after its deadline")
                active = None
        if result is not None or error is not None:
            publish()
        if result is not None and relay is None and (error is not None or (active is None and not pending)):
            if eof:
                observation_budget = 5
                if stop_deadline is not None and error is None:
                    observation_budget = min(5, max(1e-9, (stop_deadline - time.monotonic_ns()) / 1e9))
                try:
                    remaining = session_members(sid, observation_budget)
                except (Refusal, OSError, subprocess.TimeoutExpired):
                    fail("ordinary completion observation is unavailable")
                    publish()
                else:
                    if stop_deadline is not None and time.monotonic_ns() >= stop_deadline:
                        fail("ordinary cancellation scope exceeded its fixed deadline")
                    if not any(member != os.getpid() for member in remaining):
                        events.append({"sentinel_exiting_ns": time.monotonic_ns()})
                        publish()
                        return 1 if error is not None else 0
        if eof:
            time.sleep(POLL)
        else:
            select.select([control], [], [], POLL)


def close_except(retained):
    last = 3
    for fd in sorted(fd for fd in retained if fd >= 3):
        os.closerange(last, fd)
        last = fd + 1
    os.closerange(last, os.sysconf("SC_OPEN_MAX"))


def prepare_scope(command, environment, completion, lease_path, current_boot,
                  native_operation=False, barrier_read=None, prepared_receipt=None):
    """Prepare a native session with no command effect until the launch byte."""
    barrier_write = None
    if barrier_read is None:
        barrier_read, barrier_write = os.pipe()
    ready_read, ready_write = os.pipe()
    lease = safe_open(lease_path, os.O_RDWR | os.O_CREAT | os.O_EXCL)
    fcntl.flock(lease, fcntl.LOCK_EX)
    pid = os.fork()
    if pid == 0:
        try:
            close_except({0, 1, 2, barrier_read, ready_write, lease})
            os.set_blocking(barrier_read, True)
            os.setsid()
            signal.signal(signal.SIGINT, lambda *_: None)
            signal.signal(signal.SIGTERM, lambda *_: None)
            ready = {"pid": os.getpid(), "sid": os.getsid(0),
                     "identity": process_identity(os.getpid()), "boot": current_boot}
            if prepared_receipt is not None:
                path, fields = prepared_receipt
                if path.exists():
                    raise Refusal("ordinary child identity receipt already exists")
                atomic_json(path, {**fields, "scope": ready})
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
            except FileNotFoundError:
                result = 127
                terminal = True  # No executable, so no operation was launched.
            except OSError:
                result = 126
                terminal = True
            else:
                if not native_operation:
                    os._exit(ordinary_scope_loop(child, barrier_read, completion))
                result = child.wait()
                # Native CLI failures include handled cancellation and transport
                # loss. Their exit code alone cannot acknowledge daemon work.
                terminal = result == 0 if native_operation else result >= 0
                result = 128 - result if result < 0 else result
            if not native_operation:
                os._exit(ordinary_scope_loop(None, barrier_read, completion, result))
            atomic_json(completion, {"exit": result, "terminal": terminal})
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
        if barrier_write is not None:
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


def open_fifo(path, flags, expected=None):
    before = Path(path).lstat()
    if not stat.S_ISFIFO(before.st_mode) or before.st_uid != os.getuid():
        raise Refusal("ordinary child pin is not a private FIFO")
    identity = [before.st_dev, before.st_ino]
    if expected is not None and identity != expected:
        raise Refusal("ordinary child FIFO generation changed")
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    after = os.fstat(fd)
    if (not stat.S_ISFIFO(after.st_mode) or after.st_uid != os.getuid()
            or [after.st_dev, after.st_ino] != identity):
        os.close(fd)
        raise Refusal("ordinary child FIFO changed during open")
    os.set_inheritable(fd, False)
    return fd, identity


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
    def __init__(self, gate, deadline=None, wait_budget=None):
        self.started = time.monotonic()
        supplied = Path(gate).absolute()
        self.gate = supplied.parent.resolve() / supplied.name
        self.directory = self.gate.with_name(self.gate.name + ".queue")
        self.deadline = deadline
        self.wait_budget = wait_budget
        self.boot = boot_id()
        self.observer_children = set()
        # Only the original guardian creates these capabilities. Neither a
        # helper nor recovery reconstructs one from the durable child record.
        self.child_pins = {}
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
        if gate.get("protocol") not in {PROTOCOL, CHILD_PROTOCOL} or gate.get("domain") != str(self.gate):
            raise Refusal("gate protocol/domain unavailable")
        if gate["protocol"] == CHILD_PROTOCOL and gate.get("capability") != CHILD_CAPABILITY:
            raise Refusal("gate child capability unavailable")
        owner = self.owner_dir(gate["token"])
        expected = (owner / "gate").stat(follow_symlinks=False)
        if (inode.st_dev, inode.st_ino) != (expected.st_dev, expected.st_ino):
            raise Refusal("gate generation does not match its owner")
        state = read_json(owner / "state.json")
        if state.get("token") != gate["token"]:
            raise Refusal("owner generation changed")
        # Mutable state may never override the admitted protocol or generation.
        return {**state, **{key: value for key, value in gate.items() if key != "stage"},
                "inode": [inode.st_dev, inode.st_ino],
                "identity_confidence": "lease-held" if lease_held(owner / "lease") else "lease-unheld"}

    def save_state(self, token, state):
        state["token"] = token
        atomic_json(self.owner_dir(token) / "state.json", state)

    def authenticate(self, allow_cancel=False):
        with self.locked():
            gate = self.load_gate()
            self.authenticate_gate(gate, allow_cancel)
            return gate

    def authenticate_gate(self, gate, allow_cancel=False):
        token = os.environ.get("VALIDATION_LOCK_TOKEN", "")
        if not gate or gate.get("token") != token:
            raise Refusal("ownership-unavailable: inherited owner is stale or absent")
        owner = self.owner_dir(token)
        if not lease_held(owner / "lease"):
            raise Refusal("ownership-unavailable: guardian lease is not live")
        if gate["guardian"]["identity"] != process_identity(gate["guardian"]["pid"]):
            raise Refusal("ownership-unavailable: guardian identity changed")
        scope = gate["scope"]
        child_nonce = os.environ.get("VALIDATION_LOCK_CHILD")
        if child_nonce:
            if gate["protocol"] != CHILD_PROTOCOL:
                raise Refusal("ownership-unavailable: child capability is absent")
            child = self.child_record(gate, child_nonce)
            scope = child.get("scope")
            if not scope or not child["launch_may_have_occurred"]:
                raise Refusal("ownership-unavailable: child has not launched")
            self.child_identity_receipt(token, child)
            if not allow_cancel and (child["cancel_at_ns"] is not None
                                     or time.monotonic_ns() >= child["cutoff_ns"]
                                     or child["retired"]):
                raise Refusal("ownership-unavailable: child no longer accepts effects")
            if process_identity(scope["pid"]) != scope["identity"] or not lease_held(owner / child["lease"]):
                raise Refusal("ownership-unavailable: child anchor is not pinned")
        if scope["boot"] != self.boot or os.getsid(0) != scope["sid"]:
            raise Refusal("ownership-unavailable: caller is outside the command session")
        if gate["protocol"] == CHILD_PROTOCOL and (process_identity(gate["scope"]["pid"]) != gate["scope"]["identity"]
                or not lease_held(owner / "command.lease")):
            raise Refusal("ownership-unavailable: root anchor is not pinned")
        if gate["stage"] != "running" and not (allow_cancel and gate["stage"] == "cancelling"):
            raise Refusal("ownership-unavailable: owner no longer accepts effects")

    def authenticate_parent(self, gate, allow_cancel=False):
        self.authenticate_gate(gate, allow_cancel)
        if gate["protocol"] != CHILD_PROTOCOL or gate.get("capability") != CHILD_CAPABILITY:
            raise Refusal("ordinary child scopes require an opt-in v3 root")
        if os.environ.get("VALIDATION_LOCK_CHILD"):
            raise Refusal("ordinary child scopes are not recursive")

    def child_record(self, gate, nonce):
        for child in gate.get("children", []):
            if child["nonce"] == nonce:
                if (child["generation"] != gate["inode"] or child["root"] != gate["token"]
                        or child["domain"] != str(self.gate) or child["parent"] != "root"
                        or child["capability"] != CHILD_CAPABILITY):
                    raise Refusal("child generation/capability changed")
                return child
        raise Refusal("unknown ordinary child handle")

    def child_handle_record(self, gate, handle):
        parts = handle.split(".")
        if len(parts) != 4 or parts[0] != gate["token"] or parts[2:] != [str(n) for n in gate["inode"]]:
            raise Refusal("stale or foreign ordinary child handle")
        return self.child_record(gate, parts[1])

    def child_reserve(self, cutoff):
        if not re.fullmatch(r"[0-9]{1,19}", cutoff) or not 0 < int(cutoff) <= (1 << 63) - 1:
            raise ValueError("child cutoff must be a positive monotonic nanosecond integer")
        with self.locked():
            gate = self.load_gate()
            self.authenticate_parent(gate)
            state = read_json(self.owner_dir(gate["token"]) / "state.json")
            if len(state["children"]) >= 256:
                raise Refusal("ordinary child registry capacity reached")
            nonce = secrets.token_hex(16)
            child = {"nonce": nonce, "root": gate["token"], "generation": gate["inode"],
                     "domain": str(self.gate), "capability": CHILD_CAPABILITY, "parent": "root",
                     "boot": self.boot, "cutoff_ns": int(cutoff), "cancel_at_ns": None,
                     "stage": "reserved", "launch_may_have_occurred": False,
                     "retired": False, "ordinary_stop": False, "wait_completed": False,
                     "command_exit": None, "scope": None, "helper": None,
                     "prepared": False, "preparation_may_have_occurred": False,
                     "lease": nonce + ".child.lease", "helper_lease": nonce + ".helper.lease",
                     "receipt": nonce + ".child.terminal.json",
                     "prepared_receipt": nonce + ".child.prepared.json", "fifo": None}
            state["children"].append(child)
            self.save_state(gate["token"], state)
            return ".".join([gate["token"], nonce, *map(str, gate["inode"])])

    @staticmethod
    def latch_child_cancel(child, when=None):
        when = time.monotonic_ns() if when is None else when
        previous = child["cancel_at_ns"]
        child["cancel_at_ns"] = when if previous is None else min(previous, when)

    def child_cancel(self, handle):
        with self.locked():
            gate = self.load_gate()
            self.authenticate_parent(gate, allow_cancel=True)
            child = self.child_handle_record(gate, handle)
            self.latch_child_cancel(child)
            state = read_json(self.owner_dir(gate["token"]) / "state.json")
            state["children"] = gate["children"]
            self.save_state(gate["token"], state)
            return {"accepted": True, "cancel_at_ns": child["cancel_at_ns"],
                    "ordinary_stop": child["ordinary_stop"]}

    def child_status(self, handle):
        with self.locked():
            gate = self.load_gate()
            self.authenticate_parent(gate, allow_cancel=True)
            child = dict(self.child_handle_record(gate, handle))
            unresolved = []
            for resource in gate["resources"]:
                if resource.get("ordinary_scope", "root") == child["nonce"]:
                    observers = [item["scope"] for item in resource["observers"]
                                 if not self.observer_absent(item)
                                 or not (self.owner_dir(gate["token"]) / item["receipt"]).exists()]
                    if resource["state"] != "complete" or observers:
                        unresolved.append({"token": resource["token"], "state": resource["state"],
                                           "observers": observers})
            child["unresolved_resources"] = unresolved
            child["no_command_effect"] = child["retired"] and not child["launch_may_have_occurred"]
            return child

    @staticmethod
    def child_identity_fields(child):
        return {"type": "ordinary-child-prepared", "version": 1,
                "root": child["root"], "generation": child["generation"],
                "nonce": child["nonce"], "boot": child["boot"],
                "lease": child["lease"], "fifo": child["fifo"]}

    def child_identity_receipt(self, token, child):
        try:
            value = read_json(self.owner_dir(token) / child["prepared_receipt"])
            if not isinstance(value, dict) or any(value.get(key) != expected for key, expected in self.child_identity_fields(child).items()):
                raise Refusal("ordinary child identity receipt generation is invalid")
            scope = value["scope"]
            if (not isinstance(scope, dict) or type(scope["pid"]) is not int or scope["pid"] <= 0
                    or scope["sid"] != scope["pid"] or scope["boot"] != child["boot"]
                    or not isinstance(scope["identity"], str) or not scope["identity"]):
                raise Refusal("ordinary child identity receipt is invalid")
            if child["scope"] is not None and child["scope"] != scope:
                raise Refusal("ordinary child identity receipt contradicts publication")
            return scope
        except (KeyError, TypeError, ValueError, FileNotFoundError) as error:
            raise Refusal("ordinary child prepared identity is unavailable") from error

    def helper_live(self, token, child):
        helper = child["helper"]
        return bool(helper and helper["boot"] == self.boot
                    and helper["identity"] == process_identity(helper["pid"])
                    and lease_held(self.owner_dir(token) / child["helper_lease"]))

    def child_run(self, handle, command):
        helper_lease = None
        scope = None
        token = None
        try:
            with self.locked():
                gate = self.load_gate()
                self.authenticate_parent(gate)
                child = self.child_handle_record(gate, handle)
                if child["stage"] != "reserved" or child["cancel_at_ns"] is not None or time.monotonic_ns() >= child["cutoff_ns"] or INTERRUPTED:
                    raise Refusal("ordinary child is used, cancelled or past its cutoff")
                token = gate["token"]
                owner = self.owner_dir(token)
                helper_lease = safe_open(owner / child["helper_lease"], os.O_RDWR | os.O_CREAT | os.O_EXCL)
                fcntl.flock(helper_lease, fcntl.LOCK_EX)
                child["helper"] = {"pid": os.getpid(), "identity": process_identity(os.getpid()), "boot": self.boot}
                child["stage"] = "preparing"
                state = read_json(owner / "state.json")
                state["children"] = gate["children"]
                self.save_state(token, state)
            while True:
                with self.locked():
                    gate = self.load_gate()
                    self.authenticate_parent(gate)
                    child = self.child_handle_record(gate, handle)
                    if child["retired"] or child["cancel_at_ns"] is not None or time.monotonic_ns() >= child["cutoff_ns"] or INTERRUPTED:
                        raise Refusal("ordinary child cancelled before preparation")
                    if child["fifo"]:
                        child["preparation_may_have_occurred"] = True
                        state = read_json(owner / "state.json")
                        state["children"] = gate["children"]
                        self.save_state(token, state)
                        reader, _ = open_fifo(owner / child["fifo"]["path"], os.O_RDONLY, child["fifo"]["inode"])
                        break
                time.sleep(POLL)
            environment = dict(os.environ, VALIDATION_LOCK_CHILD=child["nonce"])
            # Fork with no metadata mutex held: a child suspended before its
            # close_except must not inherit a lock that blocks the guardian.
            # Preparation grants no launch; publication rechecks admission.
            scope, _ = prepare_scope(command, environment, owner / child["receipt"],
                                     owner / child["lease"], self.boot, barrier_read=reader,
                                     prepared_receipt=(owner / child["prepared_receipt"],
                                                       self.child_identity_fields(child)))
            with self.locked():
                gate = self.load_gate()
                self.authenticate_parent(gate)
                child = self.child_handle_record(gate, handle)
                if (child["stage"] != "preparing" or child["retired"] or child["cancel_at_ns"] is not None
                        or time.monotonic_ns() >= child["cutoff_ns"] or INTERRUPTED):
                    raise Refusal("ordinary child cancelled before prepared publication")
                child["scope"] = scope
                child["prepared"] = True
                child["stage"] = "prepared"
                state = read_json(owner / "state.json")
                state["children"] = gate["children"]
                self.save_state(token, state)
            while True:
                pid, _ = os.waitpid(scope["pid"], os.WNOHANG)
                with self.locked():
                    gate = self.load_gate()
                    if not gate or gate.get("token") != token:
                        raise Refusal("ordinary child owner disappeared before wait completion")
                    child = self.child_handle_record(gate, handle)
                    if INTERRUPTED:
                        self.latch_child_cancel(child)
                    if pid:
                        child["wait_completed"] = True
                    state = read_json(owner / "state.json")
                    state["children"] = gate["children"]
                    self.save_state(token, state)
                    if pid:
                        receipt = owner / child["receipt"]
                        result = read_json(receipt)["exit"] if receipt.exists() else 1
                        if result is None:
                            result = 1
                        return 128 + INTERRUPTED if INTERRUPTED else (128 + signal.SIGTERM if child["cancel_at_ns"] is not None else result)
                    if not lease_held(owner / "lease") or child["stage"] == "unknown":
                        raise Refusal("ordinary child termination is unknown; custody retained")
                time.sleep(POLL)
        finally:
            if helper_lease is not None:
                os.close(helper_lease)

    def create_child_pin(self, token, child):
        path = self.owner_dir(token) / (child["nonce"] + ".child.fifo")
        os.mkfifo(path, 0o600)
        reader, inode = open_fifo(path, os.O_RDONLY)
        try:
            writer, _ = open_fifo(path, os.O_WRONLY, inode)
        finally:
            os.close(reader)
        self.child_pins[child["nonce"]] = {"fd": writer, "generation": child["generation"],
                                           "root": token, "scope": None}
        child["fifo"] = {"path": path.name, "inode": inode}

    def retire_child(self, token, state, child):
        child["retired"] = True
        try:
            self.save_state(token, state)
        finally:
            pin = self.child_pins.pop(child["nonce"], None)
            if pin is not None:
                os.close(pin["fd"])

    def signal_child(self, token, state, child, signum):
        pin = self.child_pins.get(child["nonce"])
        scope = child["scope"]
        if (not pin or child["retired"] or pin["root"] != token
                or pin["generation"] != child["generation"] or pin["scope"] != scope):
            raise Refusal("ordinary child signal capability is unavailable")
        if (scope["boot"] != self.boot or process_identity(scope["pid"]) != scope["identity"]
                or not lease_held(self.owner_dir(token) / child["lease"])):
            raise Refusal("ordinary child pinned identity is unavailable")
        sequence = pin.get("sequence", 0) + 1
        if ((sequence == 1 and signum not in {signal.SIGINT, signal.SIGTERM})
                or (sequence == 2 and signum != signal.SIGKILL) or sequence > 2):
            raise Refusal("ordinary child signal grant sequence is invalid")
        pin["sequence"] = sequence
        signal_scope(scope, pin["fd"], signum, self.boot, sequence,
                     child["cancel_at_ns"] + 15_000_000_000)
        if signum == signal.SIGKILL:
            # Close only the issuer. Buffered full grants and an already-forked
            # relay remain in-flight until positive whole-session absence.
            self.retire_child(token, state, child)

    @staticmethod
    def child_observation_budget(child):
        if child["cancel_at_ns"] is None:
            return 5
        remaining = (child["cancel_at_ns"] + 15_000_000_000 - time.monotonic_ns()) / 1e9
        if remaining <= 0:
            raise Refusal("ordinary child termination exceeded the shared 10+5-second tail")
        return min(5, remaining)

    def service_children(self, token, root_cancel_ns=None):
        with self.locked():
            gate = self.load_gate()
            if gate["protocol"] != CHILD_PROTOCOL:
                return True
            state = read_json(self.owner_dir(token) / "state.json")
            for child in state["children"]:
                if child["ordinary_stop"]:
                    continue
                now = time.monotonic_ns()
                if now >= child["cutoff_ns"]:
                    self.latch_child_cancel(child, child["cutoff_ns"])
                if root_cancel_ns is not None:
                    self.latch_child_cancel(child, root_cancel_ns)
                try:
                    self.advance_child(token, state, child)
                except (Refusal, OSError, subprocess.TimeoutExpired) as error:
                    child["stage"] = "unknown"
                    child["unknown"] = str(error)
                    self.retire_child(token, state, child)
            self.save_state(token, state)
            return all(child["ordinary_stop"] for child in state["children"])

    def advance_child(self, token, state, child):
        owner = self.owner_dir(token)
        live_helper = self.helper_live(token, child)
        if child["helper"] and not live_helper and not child["wait_completed"]:
            self.latch_child_cancel(child)
        cancelled = child["cancel_at_ns"] is not None
        if child["stage"] == "reserved":
            if cancelled:
                self.retire_child(token, state, child)
                child.update(stage="stopped", ordinary_stop=True)
            return
        if not child["fifo"] and not child["retired"]:
            if cancelled:
                self.retire_child(token, state, child)
            else:
                self.create_child_pin(token, child)
                self.save_state(token, state)
        scope = child["scope"]
        if scope is not None:
            self.child_identity_receipt(token, child)
        if scope is None:
            if cancelled and not child["retired"]:
                self.retire_child(token, state, child)
            # Missing prepared identity is never fabricated process absence.
            if child["retired"] and not live_helper and not child.get("preparation_may_have_occurred"):
                child.update(stage="stopped", ordinary_stop=True, stop_evidence="never-prepared")
                return
            if child["retired"] and not live_helper and child["preparation_may_have_occurred"]:
                scope = self.child_identity_receipt(token, child)
                child["scope"] = scope
            else:
                return
        pin = self.child_pins.get(child["nonce"])
        if pin is not None and pin["scope"] is None and child["prepared"]:
            pin["scope"] = scope
        if child["prepared"] and not child["launch_may_have_occurred"] and not child["retired"]:
            if cancelled or not live_helper or time.monotonic_ns() >= child["cutoff_ns"]:
                self.latch_child_cancel(child)
                self.retire_child(token, state, child)
            else:
                if (not pin or scope["boot"] != self.boot or process_identity(scope["pid"]) != scope["identity"]
                        or not lease_held(owner / child["lease"])):
                    raise Refusal("ordinary child launch anchor unavailable")
                with launch_signals_blocked():
                    if INTERRUPTED or not self.helper_live(token, child) or time.monotonic_ns() >= child["cutoff_ns"]:
                        self.latch_child_cancel(child)
                        self.retire_child(token, state, child)
                    else:
                        child.update(stage="running", launch_may_have_occurred=True)
                        self.save_state(token, state)
                        os.write(pin["fd"], b"L")
        receipt = owner / child["receipt"]
        if receipt.exists():
            terminal = read_json(receipt)
            child["command_exit"] = terminal["exit"]
            if terminal.get("control_error"):
                raise Refusal(terminal["control_error"])
        members = session_members(scope["sid"], self.child_observation_budget(child))
        if not child["retired"] and child["command_exit"] is not None and members == [scope["pid"]]:
            self.retire_child(token, state, child)
        if child["cancel_at_ns"] is not None and not child["retired"]:
            elapsed = (time.monotonic_ns() - child["cancel_at_ns"]) / 1e9
            if not child.get("term_sent"):
                child["term_sent"] = True
                self.save_state(token, state)
                self.signal_child(token, state, child, signal.SIGTERM)
            elif elapsed >= 10:
                self.signal_child(token, state, child, signal.SIGKILL)
        if child["retired"] and scope_absent(scope, self.boot, self.child_observation_budget(child)):
            child.update(stage="stopped", ordinary_stop=True,
                         stop_evidence="wait-and-kernel-absence" if child["wait_completed"] else "recovery-kernel-absence")

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
            try:
                checkout = command_output(["git", "rev-parse", "--show-toplevel"])
            except (Refusal, FileNotFoundError):
                checkout = "unavailable"
            ticket = {"sequence": sequence, "token": token, "command": safe_command(command),
                      "candidate": candidate, "checkout": checkout}
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

    def custody_complete(self, gate, caller_child=None):
        # One completion predicate serves in-session receipt admission and final
        # release. Only release also proves the caller's own session is absent.
        if any(not child["ordinary_stop"] and child["nonce"] != caller_child
               for child in gate.get("children", [])):
            return False
        for record in gate["resources"]:
            if record["state"] != "complete":
                return False
            if any(not self.observer_absent(observer) for observer in record.get("observers", [])):
                return False
        return True

    def assert_complete(self):
        with self.locked():
            gate = self.load_gate()
            self.authenticate_gate(gate)
            if not self.custody_complete(gate, os.environ.get("VALIDATION_LOCK_CHILD")):
                raise Refusal("validation custody has unfinished children or resources")

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
            if not self.custody_complete(gate):
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
        if gate["protocol"] == CHILD_PROTOCOL:
            with self.locked():
                state = read_json(self.owner_dir(token) / "state.json")
                for child in state["children"]:
                    # Guardian death destroys the sole pin and signal actor.
                    # Reconciliation never opens a FIFO or signals its SID.
                    child["retired"] = True
                    if child.get("preparation_may_have_occurred"):
                        try:
                            child["scope"] = self.child_identity_receipt(token, child)
                        except Refusal:
                            child["stage"] = "unknown"
                            continue
                    if child["scope"]:
                        child["ordinary_stop"] = scope_absent(child["scope"], self.boot)
                    elif not child.get("preparation_may_have_occurred"):
                        child["ordinary_stop"] = True
                    if child["ordinary_stop"]:
                        child["stage"] = "stopped"
                        child["stop_evidence"] = "recovery-kernel-absence" if child["scope"] else "never-prepared"
                    else:
                        child["stage"] = "unknown"
                self.save_state(token, state)
                children_absent = all(child["ordinary_stop"] for child in state["children"])
            if not children_absent:
                self.quarantine(token, "ordinary child session still exists or its absence is unknown")
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

    def authorize_resource(self, gate, resource, effects=False):
        caller = os.environ.get("VALIDATION_LOCK_CHILD", "root")
        origin = resource.get("ordinary_scope", "root")
        if caller != "root" and origin != caller:
            raise Refusal("resource belongs to a different ordinary scope")
        if effects and origin != "root":
            child = self.child_record(gate, origin)
            if child["retired"] or child["cancel_at_ns"] is not None or time.monotonic_ns() >= child["cutoff_ns"]:
                raise Refusal("resource origin no longer accepts effects")

    @contextlib.contextmanager
    def resource_admission(self, token, resource_token):
        with self.locked():
            gate = self.load_gate()
            self.authenticate_gate(gate)
            self.authorize_resource(gate, self.resource(token, resource_token), effects=True)
            with launch_signals_blocked():
                if INTERRUPTED:
                    raise Refusal("cancelled before native resource launch")
                yield

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

    def docker(self, args, daemon=None, admission=None):
        timeout = 30
        if self.deadline is not None:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                raise Deadline
            timeout = min(timeout, remaining)
        if daemon is not None and self.daemon() != daemon:
            raise Refusal("Docker daemon/context identity changed")
        return command_output(["docker", *args], timeout=timeout, admission=admission)

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
                  "daemon": daemon, "state": "registered", "observers": [],
                  "ordinary_scope": os.environ.get("VALIDATION_LOCK_CHILD", "root")}
        if files:
            record["files"] = [str(Path(path).resolve()) for path in files]
        with self.locked():
            current = self.load_gate()
            self.authenticate_gate(current)
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

    def observer_absent(self, observer):
        scope = observer["scope"]
        child = (scope["pid"], scope["identity"])
        if child in self.observer_children:
            try:
                reaped, _ = os.waitpid(scope["pid"], os.WNOHANG)
            except ChildProcessError:
                self.observer_children.discard(child)
            else:
                if not reaped:
                    return False
                self.observer_children.remove(child)
        # An interrupted container-run can itself own the protected observer.
        # Reap that child before probing its group; a Linux zombie otherwise
        # keeps cleanup waiting for the parent that is executing this cleanup.
        return scope_absent(scope, self.boot)

    def observer_container_id(self, token, observer):
        if observer.get("container_id"):
            return observer["container_id"]
        if not observer.get("cidfile"):
            return None
        try:
            fd = safe_open(self.owner_dir(token) / observer["cidfile"])
        except FileNotFoundError:
            return None
        with os.fdopen(fd, "r") as stream:
            value = stream.read(65).strip()
        # Docker writes --cidfile only after the Engine acknowledges create.
        # A partial write or pre-create file is not an immutable identity.
        return value if re.fullmatch(r"[a-f0-9]{64}", value) else None

    def observer_contained_by_resource(self, token, record, observer):
        if record["kind"] == "container":
            # There is one creation per resource; later starts bind the same
            # immutable ID. A discovered name+label can recover a lost ID reply.
            return bool(record.get("id"))
        return record["kind"] == "buildkit" and observer.get("check_only", False)

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
            native_response = receipt.exists() and read_json(receipt).get("terminal")
            if not native_response and not self.observer_contained_by_resource(token, record, observer):
                return False
            if not self.observer_absent(observer):
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
        initially_pending = any(not self.observer_absent(item)
                                or not (self.owner_dir(token) / item["receipt"]).exists()
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
            lost_response = False
            for observer in record["observers"]:
                receipt = self.owner_dir(token) / observer["receipt"]
                absent = self.observer_absent(observer)
                if receipt.exists() and not read_json(receipt).get("terminal"):
                    lost_response = True
                if process_identity(observer["scope"]["pid"]) != observer["scope"]["identity"] and not receipt.exists():
                    lost_response = True
                pending = pending or not receipt.exists() or not absent
            if initially_pending and not pending and not repeated:
                self.native_cleanup(token, record)
                repeated = True
                continue
            if lost_response:
                raise Refusal("external native CLI response lost; provider terminal proof required")
            time.sleep(POLL)

    def native_cleanup(self, token, record):
        daemon = record["daemon"]
        if self.daemon() != daemon:
            raise Refusal("Docker daemon/context identity changed")
        if record["kind"] == "container":
            for observer in record["observers"]:
                container_id = self.observer_container_id(token, observer)
                if container_id:
                    if record.get("id") and record["id"] != container_id:
                        raise Refusal("native container identity changed")
                    record["id"] = container_id
            container = self.inspect_container(record["name"], daemon)
            if container:
                self.check_container_owner(token, record, container)
                record["id"] = container["Id"]
                self.update_resource(token, record)
                self.docker(["rm", "-f", container["Id"]], daemon)
            elif record.get("id"):
                self.update_resource(token, record)
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
            self.docker(["buildx", "create", "--name", name, "--driver", "docker-container"], record["daemon"],
                        admission=self.resource_admission(token, record["token"]))
        self.docker(["buildx", "inspect", name, "--bootstrap"], record["daemon"],
                    admission=self.resource_admission(token, record["token"]))
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
        self.authorize_resource(gate, record, effects=True)
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
            # Only the registered project's native up or named disposable run
            # may create resources. Do not admit arbitrary Compose subcommands.
            index, project, files = 2, None, []
            while index + 1 < len(command) and command[index] in {"-p", "-f"}:
                option, value = command[index:index + 2]
                if option == "-p":
                    if project is not None:
                        raise Refusal("Compose observer project is ambiguous")
                    project = value
                else:
                    files.append(str(Path(value).resolve()))
                index += 2
            if command[1:2] != ["compose"] or project != record["name"]:
                raise Refusal("Compose observer requires its registered project")
            if files != record["files"]:
                raise Refusal("Compose observer files differ from registration")
            action = command[index] if index < len(command) else None
            if action == "run":
                arguments = command[index + 1:]
                offset, disposable, name = 0, False, None
                while offset < len(arguments) and arguments[offset].startswith("-"):
                    option = arguments[offset]
                    if option in {"--rm", "--no-deps", "-T"}:
                        disposable = disposable or option == "--rm"
                        offset += 1
                    elif option in {"--name", "--pull"} and offset + 1 < len(arguments):
                        value = arguments[offset + 1]
                        if option == "--name" and name is None:
                            name = value
                        elif option != "--pull" or value != "never":
                            raise Refusal("Compose run option is not an owned foreground operation")
                        offset += 2
                    else:
                        raise Refusal("Compose run option is not an owned foreground operation")
                if (not disposable or name is None or not name.startswith(record["name"] + "-")
                        or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,160}", name) is None
                        or offset >= len(arguments)):
                    raise Refusal("Compose run requires a disposable name within its registered project")
            elif action != "up":
                raise Refusal("Compose observer requires up or named run --rm")
        else:
            raise Refusal("resource does not support a terminal CLI observer")
        if self.daemon() != record["daemon"]:
            raise Refusal("Docker daemon/context identity changed")
        operation = secrets.token_hex(16)
        owner = self.owner_dir(token)
        receipt = operation + ".terminal.json"
        native_scope = {}
        if record["kind"] == "container":
            if command[1] in {"run", "create"}:
                if any(arg == "--cidfile" or arg.startswith("--cidfile=") for arg in command):
                    raise Refusal("container observer owns its native ID file")
                native_scope["cidfile"] = operation + ".cid"
                command = [*command[:2], "--cidfile", str(owner / native_scope["cidfile"]), *command[2:]]
            else:
                native_scope["container_id"] = container["Id"]
        elif record["kind"] == "buildkit":
            # This is the canonical Dockerfile-check form. Other builds may
            # export outside BuildKit; stopping nodes cannot prove exporter termination.
            native_scope["check_only"] = command[3:] == [
                "--builder", record["name"], "--check", "-f", "build/docker/Dockerfile", "."]
        environment = dict(os.environ)
        for key in ("VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_CHILD"):
            environment.pop(key, None)
        scope, barrier = prepare_scope(command, environment, owner / receipt,
                                       owner / (operation + ".lease"), self.boot, native_operation=True)
        observer_child = (scope["pid"], scope["identity"])
        self.observer_children.add(observer_child)
        launched = False
        try:
            with self.locked():
                current = self.load_gate()
                self.authenticate_gate(current)
                if not current or current.get("token") != token or current["stage"] != "running" or INTERRUPTED:
                    raise Refusal("cancelled before external launch barrier")
                state = read_json(owner / "state.json")
                for item in state["resources"]:
                    if item["token"] == resource_token:
                        self.authorize_resource(current, item, effects=True)
                        if item["state"] in {"closing", "complete"}:
                            raise Refusal("resource closed before native operation publication")
                        if native_scope.get("cidfile") and item["observers"]:
                            raise Refusal("container creation requires a fresh registered resource")
                        if any(not self.observer_absent(observer)
                               or not (owner / observer["receipt"]).exists()
                               for observer in item["observers"]):
                            raise Refusal("resource still has an unfinished native operation")
                        if len(item["observers"]) >= 256:
                            raise Refusal("observer registry capacity reached")
                        item["observers"].append({"scope": scope, "receipt": receipt,
                                                  "ordinary_scope": item.get("ordinary_scope", "root"),
                                                  "launch_may_have_occurred": True, **native_scope})
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
                    self.observer_children.remove(observer_child)
                    if not (owner / receipt).exists():
                        raise Refusal("external terminal observer exited without a receipt")
                    return read_json(owner / receipt)["exit"]
                time.sleep(POLL)
        finally:
            if barrier is not None:
                os.close(barrier)
            if not launched:
                os.waitpid(scope["pid"], 0)
                self.observer_children.remove(observer_child)
                atomic_json(owner / receipt, {"exit": 128 + INTERRUPTED if INTERRUPTED else 1,
                                              "terminal": True, "launched": False})

    def run(self, command, with_child_scopes=False):
        ticket, ticket_fd = self.register_ticket(command)
        token = ticket["token"]
        owner = self.owner_dir(token)
        lease = None
        barrier = None
        scope = None
        published = False
        started = self.started
        last_diagnostic = None
        next_diagnostic_at = started
        reconciled_tokens = set()
        try:
            while True:
                self.check_deadline()
                with self.locked(interruptible=True):
                    previous = self.load_gate()
                if previous and previous.get("protocol") in {PROTOCOL, CHILD_PROTOCOL} and previous["token"] not in reconciled_tokens and previous["identity_confidence"] == "lease-unheld":
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
                    now = time.monotonic()
                    if diagnostic != last_diagnostic or now >= next_diagnostic_at:
                        if gate:
                            owner_fields = (f"owner_pid={gate.get('guardian', {}).get('pid', 'unknown')} "
                                            f"owner_checkout={json.dumps(gate.get('checkout', 'unknown'))} "
                                            f"owner_command={json.dumps(gate.get('command', 'unknown'))}")
                        else:
                            owner_fields = "owner_pid=none owner_checkout=null owner_command=null"
                        print(f"validation wait sequence={ticket['sequence']} token={token[:12]} "
                              f"position={next((i + 1 for i, row in enumerate(live) if row['token'] == token), 0)} "
                              f"elapsed={now - started:.3f}s timeout={self.wait_budget:g}s candidate={ticket['candidate']} "
                              f"command={ticket['command']} owner={diagnostic[0]} {owner_fields} "
                              f"identity={gate['identity_confidence'] if gate else 'unowned'} "
                              f"mode={'legacy' if gate and gate.get('protocol') == 'legacy' else 'new'}", file=sys.stderr)
                        last_diagnostic = diagnostic
                        next_diagnostic_at = now + WAIT_REPORT_SECONDS
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
                        environment.pop("VALIDATION_LOCK_CHILD", None)
                        scope, barrier = prepare_scope(command, environment, owner / "command.terminal.json",
                                                       owner / "command.lease", self.boot)
                        state = {"token": token, "stage": "running", "quarantine": None, "resources": []}
                        if with_child_scopes:
                            state["children"] = []
                        self.save_state(token, state)
                        gate_record = {"protocol": CHILD_PROTOCOL if with_child_scopes else PROTOCOL,
                                       "token": token, "domain": str(self.gate),
                                       "candidate": ticket["candidate"], "command": ticket["command"],
                                       "checkout": ticket["checkout"],
                                       "guardian": {"pid": os.getpid(), "identity": process_identity(os.getpid())},
                                       "scope": scope, "stage": "launch-intent", "launch_may_have_occurred": True}
                        if with_child_scopes:
                            gate_record["capability"] = CHILD_CAPABILITY
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
                        print(f"validation acquired sequence={ticket['sequence']} token={token[:12]} "
                              f"elapsed={time.monotonic() - started:.3f}s timeout={self.wait_budget:g}s "
                              f"command={ticket['command']}", file=sys.stderr)
                        break
                time.sleep(POLL)
            # The deadline was admission-only. Native cleanup uses its own bounded
            # operation timeout, never the elapsed waiting budget after launch.
            self.deadline = None
            interrupted_at = None
            escalated = False
            command_result = None
            reaped = False
            root_closed_ns = None
            while True:
                if INTERRUPTED and interrupted_at is None:
                    interrupted_at = time.monotonic()
                    print(f"validation cancelled token={token[:12]} signal={INTERRUPTED} "
                          "phase=running custody=retained", file=sys.stderr)
                    with self.locked():
                        state = read_json(owner / "state.json")
                        state["stage"] = "cancelling"
                        self.save_state(token, state)
                    if barrier is not None:
                        signal_scope(scope, barrier, INTERRUPTED, self.boot, 1,
                                     int(interrupted_at * 1e9) + 15_000_000_000)
                if not reaped:
                    pid, _ = os.waitpid(scope["pid"], os.WNOHANG)
                    reaped = bool(pid)
                if (owner / "command.terminal.json").exists():
                    terminal = read_json(owner / "command.terminal.json")
                    command_result = terminal["exit"]
                    if terminal.get("control_error"):
                        raise Refusal(terminal["control_error"])
                if barrier is not None and command_result is not None and session_members(scope["sid"]) == [scope["pid"]]:
                    # No work remains capable of forking. Retire signalling
                    # before permitting the anchor itself to exit.
                    os.close(barrier)
                    barrier = None
                # Reap our sentinel before the kernel group probe. Darwin's
                # killpg(..., 0) returns EPERM for a group containing only its
                # unreaped zombie, which is not an observation failure.
                absent = reaped and scope_absent(scope, self.boot)
                if absent and root_closed_ns is None:
                    root_closed_ns = time.monotonic_ns()
                children_stopped = self.service_children(token, int(interrupted_at * 1e9) if interrupted_at is not None else root_closed_ns) if with_child_scopes else True
                if interrupted_at is not None and barrier is not None and not absent and time.monotonic() - interrupted_at >= 10 and not escalated:
                    signal_scope(scope, barrier, signal.SIGKILL, self.boot, 2,
                                 int(interrupted_at * 1e9) + 15_000_000_000)
                    escalated = True
                    os.close(barrier)
                    barrier = None
                if absent:
                    if not children_stopped:
                        state = read_json(owner / "state.json")
                        if any(not child["ordinary_stop"] and child["stage"] != "unknown" for child in state["children"]):
                            time.sleep(POLL)
                            continue
                        self.quarantine(token, "ordinary child termination is unconfirmed")
                        return 128 + INTERRUPTED if INTERRUPTED else 1
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
                print(f"validation cancelled token={token[:12]} signal={INTERRUPTED} "
                      f"phase=admission elapsed={time.monotonic() - started:.3f}s "
                      f"timeout={self.wait_budget:g}s command=not-started", file=sys.stderr)
                return 128 + INTERRUPTED
            print(f"validation lock timed out: sequence={ticket['sequence']} token={token[:12]} "
                  f"elapsed={time.monotonic() - started:.3f}s timeout={self.wait_budget:g}s candidate={ticket['candidate']} "
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
            # Clear capabilities before closing pins, including exceptional exits.
            pins, self.child_pins = self.child_pins, {}
            for pin in pins.values():
                os.close(pin["fd"])
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
        for key in ("VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_DIR", "VALIDATION_LOCK_CHILD"):
            environment.pop(key, None)
        return subprocess.call([sys.executable, str(SELF.parent.parent / "tests" / "validation-lock-test.py")], env=environment)
    if not args:
        raise ValueError("usage: validation-lock.sh -- command [args...] | --status | --reconcile")
    fixed_lengths = {"--status": 1, "--reconcile": 1, "--assert-held": 1, "--assert-complete": 1,
                     "--resource-bind": 3, "--resource-cleanup": 2,
                     "--resource-complete": 2, "--builder-name": 2,
                     "--child-cancel": 2, "--child-status": 2, "--child-reserve": 3}
    if args[0] in fixed_lengths and len(args) != fixed_lengths[args[0]]:
        raise ValueError("invalid arguments for " + args[0])
    if args[0] == "--" and len(args) < 2:
        raise ValueError("a command is required after --")
    if args[0] not in {*fixed_lengths, "--", "--with-child-scopes", "--child-run", "--resource-register", "--resource-run", "--builder-prepare", "--container-run"}:
        raise ValueError("unknown validation-lock operation")
    child_root = args[0] == "--with-child-scopes"
    if child_root:
        if len(args) < 3 or args[1] != "--":
            raise ValueError("--with-child-scopes requires -- COMMAND")
        args = args[1:]
    waiting = timeout_seconds()
    gate, inherited = domain()
    queue = Queue(gate, time.monotonic() + waiting if args[0] in {"--", "--reconcile"} else None, wait_budget=waiting)
    if args[0] == "--" and len(args) > 1:
        if inherited:
            if child_root:
                raise Refusal("child capability cannot upgrade an admitted root")
            # Nested wrappers remain in the same native session and never own release.
            with queue.locked():
                queue.authenticate_gate(queue.load_gate())
                with launch_signals_blocked():
                    if INTERRUPTED:
                        raise Refusal("cancelled before nested launch barrier")
                    child = subprocess.Popen(args[1:])
            result = child.wait()
            return 128 + INTERRUPTED if INTERRUPTED else (128 - result if result < 0 else result)
        return queue.run(args[1:], with_child_scopes=child_root)
    if args == ["--status"]:
        print(json.dumps(queue.status(), sort_keys=True))
        return 0
    if args == ["--reconcile"]:
        return 0 if queue.reconcile() else 1
    if args == ["--assert-complete"]:
        queue.assert_complete()
        return 0
    if args == ["--assert-held"]:
        queue.authenticate(allow_cancel=True)
        return 0
    if args[0] == "--child-reserve":
        if args[1] != "--cancel-at-monotonic-ns":
            raise ValueError("--child-reserve requires --cancel-at-monotonic-ns N")
        print(queue.child_reserve(args[2]))
        return 0
    if args[0] == "--child-cancel":
        print(json.dumps(queue.child_cancel(args[1]), sort_keys=True))
        return 0
    if args[0] == "--child-status":
        print(json.dumps(queue.child_status(args[1]), sort_keys=True))
        return 0
    if args[0] == "--child-run":
        if len(args) < 4 or args[2] != "--":
            raise ValueError("--child-run requires HANDLE -- COMMAND")
        return queue.child_run(args[1], args[3:])
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
    if args[0] in {"--resource-bind", "--resource-complete", "--resource-cleanup", "--builder-name", "--resource-run"}:
        queue.authorize_resource(active, queue.resource(token, args[1]), effects=args[0] in {"--resource-bind", "--resource-run"})
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
        print(f"validation lock timed out before registration or reconciliation timeout={timeout_seconds():g}s", file=sys.stderr)
        code = 75
    except InterruptedError:
        print(f"validation cancelled signal={INTERRUPTED} phase=before-registration command=not-started", file=sys.stderr)
        code = 128 + INTERRUPTED
    except (Refusal, OSError, subprocess.TimeoutExpired) as error:
        print(f"validation lock: {error}", file=sys.stderr)
        code = 128 + INTERRUPTED if INTERRUPTED else 1
    sys.exit(code)
