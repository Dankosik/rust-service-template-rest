#!/usr/bin/env python3
"""The validation lock's OS custody, not a resource cleaner or job runner.

The permanent guard serializes admission and metadata transitions. A surviving
admission file fences an interrupted owner even after its kernel locks vanish.
Only supervised completion or token-specific owner reconciliation can establish
terminal custody; PID disappearance never grants admission.
"""

import contextlib
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import secrets
import signal
import stat
import subprocess
import sys
import tempfile
import time


INCOMPLETE = 74
TIMEOUT = 75
PROTOCOL = 2
IDENTITY_KEYS = ("VALIDATION_LOCK_PATH", "VALIDATION_LOCK_TOKEN")
TOKEN = re.compile(r"[0-9a-f]{32}\Z")
SAFE = re.compile(r"[A-Za-z0-9_.:/@+\-]{1,160}\Z")
OWNERS = {
    "compose-postgres": {"compose-absent"},
    "integration-messaging": {"compose-absent"},
    "integration-cache": {"compose-absent"},
    "integration-object-storage": {"compose-absent"},
    "integration-oauth": {"container-absent"},
    "migration-container": {"container-absent"},
    "runtime-image-build": {"build-completed"},
    "runtime-image-check": {"container-absent"},
    "runtime-progress-build": {"build-completed"},
    "runtime-progress-workload": {"workload-absent"},
    "make-shellcheck": {"container-absent"},
    "make-docs-check": {"container-absent"},
    "make-container-security": {"container-absent"},
    "make-container-sbom": {"container-absent"},
    "make-dockerfile-check": {"build-completed"},
}
cancelled = 0


class Refusal(Exception):
    def __init__(self, message, code=2):
        super().__init__(message)
        self.code = code


def notice(message):
    print(f"validation lock: {message}", file=sys.stderr, flush=True)


def safe(value):
    if not isinstance(value, str) or not SAFE.fullmatch(value):
        raise Refusal("invalid bounded owner scope or evidence reference")
    return value


def display(value):
    return re.sub(r"[^A-Za-z0-9_./:@+ =\-]", "?", str(value))[:200]


def interrupted(signum, _frame):
    global cancelled
    cancelled = signum


def wait_budget():
    try:
        value = float(os.environ.get("VALIDATION_LOCK_TIMEOUT_SECONDS", "900"))
    except ValueError as error:
        raise Refusal("VALIDATION_LOCK_TIMEOUT_SECONDS must be a finite nonnegative number") from error
    if not math.isfinite(value) or value < 0:
        raise Refusal("VALIDATION_LOCK_TIMEOUT_SECONDS must be a finite nonnegative number")
    return value


def inherited_path():
    path, token = (os.environ.get(key, "") for key in IDENTITY_KEYS)
    if not path and not token:
        return None
    if not os.path.isabs(path) or not TOKEN.fullmatch(token):
        raise Refusal("invalid inherited generation identity")
    return Path(path)


def git(*arguments):
    try:
        return subprocess.check_output(["git", *arguments], stderr=subprocess.DEVNULL)
    except (OSError, subprocess.CalledProcessError) as error:
        raise Refusal("cannot establish repository/candidate identity") from error


def origin():
    root = Path(os.fsdecode(git("rev-parse", "--show-toplevel").strip()))
    common = Path(os.fsdecode(git("rev-parse", "--git-common-dir").strip()))
    if not common.is_absolute():
        common = Path.cwd() / common
    path = Path(os.environ.get("VALIDATION_LOCK_DIR", str(common / "codex/validation.lock")))
    return root, path.absolute()


def candidate(root):
    head = git("rev-parse", "HEAD").strip().decode("ascii")
    digest = hashlib.sha256(head.encode())
    files = git("-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    for name in sorted(set(files.split(b"\0")) - {b""}):
        path = root / os.fsdecode(name)
        digest.update(name + b"\0")
        try:
            info = path.lstat()
        except FileNotFoundError:
            digest.update(b"deleted\0")
            continue
        digest.update(str(stat.S_IMODE(info.st_mode)).encode() + b"\0")
        if stat.S_ISLNK(info.st_mode):
            digest.update(os.fsencode(os.readlink(path)))
        elif stat.S_ISREG(info.st_mode):
            with path.open("rb") as source:
                for block in iter(lambda: source.read(65536), b""):
                    digest.update(block)
        else:
            digest.update(b"non-file")
        digest.update(b"\0")
    return f"{head}:{digest.hexdigest()}"


def kind(command):
    # Keep diagnostics useful without copying arguments, wrapper options or env.
    name = Path(command[0]).name
    if name in {"bash", "sh", "python3"} and len(command) > 1:
        script = Path(command[1]).name
        if re.fullmatch(r"[a-z][a-z0-9-]*\.(sh|py)", script):
            return script
    return name if re.fullmatch(r"[A-Za-z0-9_.+\-]{1,48}", name) else "command"


class Lock:
    def __init__(self, path):
        self.path = path
        self.guard = Path(f"{path}.guard")

    @contextlib.contextmanager
    def held(self, deadline=None, cancellable=False):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        descriptor = os.open(self.guard, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                raise Refusal("guard is not a regular file")
            if deadline is None:
                deadline = time.monotonic() + wait_budget()
            announced = False
            last_notice = 0.0
            while True:
                if cancellable and cancelled:
                    raise Refusal("queued admission cancelled; payload not started", 128 + cancelled)
                if announced and time.monotonic() >= deadline:
                    raise Refusal("metadata guard wait timed out; payload not started", TIMEOUT)
                try:
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    now = time.monotonic()
                    if not announced or now - last_notice >= 10:
                        notice("waiting for generation metadata guard")
                        announced, last_notice = True, now
                    if now >= deadline:
                        raise Refusal("metadata guard wait timed out; payload not started", TIMEOUT)
                    time.sleep(min(0.05, max(0, deadline - now)))
            yield
        finally:
            os.close(descriptor)

    def state_path(self, token):
        if not isinstance(token, str) or not TOKEN.fullmatch(token):
            raise Refusal("unknown generation token", INCOMPLETE)
        return Path(f"{self.path}.{token}.json")

    def load(self):
        """Read under the guard; no PID-based guesses and no legacy mutation."""
        try:
            info = self.path.lstat()
        except FileNotFoundError:
            return None
        if stat.S_ISDIR(info.st_mode):
            raise Refusal("legacy_protocol_unreconciled", INCOMPLETE)
        if not stat.S_ISREG(info.st_mode):
            raise Refusal("unknown admission file type", INCOMPLETE)
        try:
            admission = self.read_json(self.path, 4096)
            token = admission["token"]
            if admission["protocol"] != PROTOCOL:
                raise ValueError("protocol")
            record = self.read_json(self.state_path(token), 2 * 1024 * 1024)
            if (record["protocol"] != PROTOCOL or record["token"] != token
                    or record["admission"] != [info.st_dev, info.st_ino]):
                raise ValueError("identity")
            if record["phase"] not in {"preparing", "active", "interrupted", "completed"}:
                raise ValueError("phase")
            if not isinstance(record["tickets"], dict) or len(record["tickets"]) > 4096:
                raise ValueError("tickets")
            for nonce, ticket in record["tickets"].items():
                if (not TOKEN.fullmatch(nonce) or ticket["owner"] not in OWNERS
                        or not SAFE.fullmatch(ticket["scope"])
                        or ticket["state"] not in {"pending", "terminal"}):
                    raise ValueError("ticket identity")
                if ticket["state"] == "terminal" and (
                        ticket["evidence"] not in OWNERS[ticket["owner"]]
                        or not SAFE.fullmatch(ticket["reference"])):
                    raise ValueError("terminal evidence")
            if record["phase"] in {"active", "interrupted"} and (
                    not isinstance(record["pgid"], int) or record["pgid"] <= 1
                    or record["sid"] != record["pgid"]):
                raise ValueError("group identity")
            return record
        except (KeyError, TypeError, ValueError, OSError) as error:
            raise Refusal("generation metadata unknown; owner reconciliation required", INCOMPLETE) from error

    @staticmethod
    def read_json(path, limit):
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as source:
            if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                raise ValueError("not a regular file")
            data = source.read(limit + 1)
        if len(data) > limit:
            raise ValueError("metadata too large")
        return json.loads(data)

    def save(self, record):
        path = self.state_path(record["token"])
        descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(descriptor, "w") as output:
                json.dump(record, output, sort_keys=True)
                output.write("\n")
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def matching(self, token):
        record = self.load()
        if record is None or record["token"] != token:
            raise Refusal("generation changed; refusing stale owner operation", INCOMPLETE)
        return record

    def retire(self, record):
        current = self.matching(record["token"])
        if current["phase"] != "completed" or current.get("command_terminal") is not True:
            raise Refusal("command custody is not terminal", INCOMPLETE)
        if pending(current) or current.get("group_terminal") is not True:
            raise Refusal("owned work is not terminal", INCOMPLETE)
        # matching() rechecks both the token and inode under the same guard.
        os.unlink(self.path)


def pending(record):
    return {nonce: ticket for nonce, ticket in record["tickets"].items() if ticket["state"] != "terminal"}


def verify_inherited(lock):
    try:
        record = lock.matching(os.environ["VALIDATION_LOCK_TOKEN"])
    except Refusal as error:
        raise Refusal(str(error)) from error
    if (record["phase"] not in {"active", "interrupted"}
            or record.get("pgid") != os.getpgrp() or record.get("sid") != os.getsid(0)):
        raise Refusal("inherited generation does not own this session/group")
    return record


def describe(record):
    return (f"generation={record['token']} phase={record['phase']} "
            f"owner={record.get('supervisor', 'unknown')} "
            f"checkout={display(record.get('checkout', 'unknown'))} "
            f"candidate={display(record.get('candidate', 'unknown'))} "
            f"kind={display(record.get('kind', 'unknown'))}")


def report_pending(record):
    tickets = pending(record)
    for nonce, ticket in list(tickets.items())[:16]:
        notice(f"pending ticket={nonce} owner={display(ticket['owner'])} "
               f"scope={display(ticket['scope'])}; named owner must establish terminal completion")
    if len(tickets) > 16:
        notice(f"{len(tickets) - 16} further pending tickets retained in generation={record['token']} metadata")


def group_exists(pgid):
    if not isinstance(pgid, int) or pgid <= 1:
        raise Refusal("process group custody is unknown", INCOMPLETE)
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def acquire(lock, root, command):
    started = time.monotonic()
    deadline = started + wait_budget()
    fingerprint = os.environ.get("VALIDATION_LOCK_CANDIDATE") if kind(command) == "verify.sh" else None
    if fingerprint is not None:
        if not re.fullmatch(r"verify:[0-9a-f]{64}:[0-9a-f]{64}", fingerprint):
            raise Refusal("invalid verifier candidate/plan identity")
    else:
        fingerprint = candidate(root)
    last_owner = None
    last_notice = started
    while True:
        if cancelled:
            raise Refusal("queued admission cancelled; payload not started", 128 + cancelled)
        with lock.held(deadline, cancellable=True):
            try:
                record = lock.load()
                if record is not None and record["phase"] == "completed":
                    lock.retire(record)
                    record = None
                summary = describe(record) if record else None
            except Refusal as error:
                summary = str(error)
                record = "unknown"
            if last_owner is not None and time.monotonic() >= deadline:
                raise Refusal("admission wait timed out; payload not started", TIMEOUT)
            if record is None and not cancelled:
                token = secrets.token_hex(16)
                try:
                    descriptor = os.open(lock.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                except FileExistsError:
                    continue  # A legacy mkdir may win between load and create.
                with os.fdopen(descriptor, "w") as admission:
                    info = os.fstat(admission.fileno())
                    json.dump({"protocol": PROTOCOL, "token": token}, admission)
                    admission.write("\n")
                    admission.flush()
                    os.fsync(admission.fileno())
                record = {
                    "protocol": PROTOCOL, "token": token, "phase": "preparing",
                    "admission": [info.st_dev, info.st_ino], "supervisor": os.getpid(),
                    "checkout": str(root), "candidate": fingerprint, "kind": kind(command),
                    "started_monotonic": time.monotonic(), "tickets": {},
                }
                lock.save(record)
                return record
        now = time.monotonic()
        if summary != last_owner or now - last_notice >= 10:
            notice(f"waiting {now - started:.1f}s: {summary}")
            last_owner, last_notice = summary, now
        if now >= deadline:
            raise Refusal(f"timed out after {now - started:.1f}s; payload not started; {summary}", TIMEOUT)
        time.sleep(min(0.1, max(0, deadline - now)))


def gate(arguments):
    if len(arguments) < 3 or arguments[1] != "--":
        raise Refusal("invalid internal gate arguments")
    descriptor = int(arguments[0])
    permission = b""
    while len(permission) < 3:
        block = os.read(descriptor, 3 - len(permission))
        if not block:
            break
        permission += block
    os.close(descriptor)
    if permission != b"GO\n":
        return 125
    path = inherited_path()
    if path is None:
        raise Refusal("gate has no generation")
    lock = Lock(path)
    with lock.held():
        verify_inherited(lock)
    os.execvpe(arguments[2], arguments[2:], os.environ)


def supervise(lock, record, command):
    child = None
    read_fd = write_fd = None
    token = record["token"]
    try:
        with lock.held():
            record = lock.matching(token)
            if cancelled:
                record.update(phase="completed", command_terminal=True, group_terminal=True,
                              result=128 + cancelled, disposition="cancelled-before-admission")
                lock.save(record)
                lock.retire(record)
                return 128 + cancelled
            read_fd, write_fd = os.pipe()
            environment = dict(os.environ)
            environment.pop("VALIDATION_LOCK_HELD", None)
            environment.update(VALIDATION_LOCK_PATH=str(lock.path), VALIDATION_LOCK_DIR=str(lock.path),
                               VALIDATION_LOCK_TOKEN=token)
            child = subprocess.Popen(
                [sys.executable, str(Path(__file__).resolve()), "--gate", str(read_fd), "--", *command],
                start_new_session=True, pass_fds=(read_fd,), env=environment,
            )
            os.close(read_fd)
            read_fd = None
            record.update(phase="active", pgid=child.pid, sid=child.pid)
            lock.save(record)
            # Active/group identity is durable before the one permission write.
            if not cancelled:
                os.write(write_fd, b"GO\n")
            os.close(write_fd)
            write_fd = None
        forwarded = 0
        last_notice = time.monotonic()
        while True:
            # Never signal after poll()/wait() has reaped the owned direct child.
            if cancelled and cancelled != forwarded and child.returncode is None:
                try:
                    os.killpg(child.pid, cancelled)
                except ProcessLookupError:
                    pass
                forwarded = cancelled
                with lock.held():
                    current = lock.matching(token)
                    current.update(phase="interrupted", disposition="cancellation-pending",
                                   cancellation_signal=cancelled)
                    lock.save(current)
                notice(f"cancellation pending for generation={token}; waiting for owned command")
            result = child.poll()
            if result is not None:
                break
            if cancelled and time.monotonic() - last_notice >= 10:
                notice(f"cancellation pending for generation={token}; owned command still running")
                last_notice = time.monotonic()
            time.sleep(0.05)
        result = result if result >= 0 else 128 - result
        if cancelled:
            result = 128 + cancelled
        with lock.held():
            record = lock.matching(token)
            record.update(command_terminal=True, result=result)
            record["group_terminal"] = not group_exists(child.pid)
            if not record["group_terminal"] or pending(record):
                record.update(phase="interrupted", disposition="custody-incomplete")
                lock.save(record)
                notice(f"custody incomplete; retained {describe(record)}")
                if not record["group_terminal"]:
                    notice("direct child reaped but group survives; no post-reap group signal is authorized")
                report_pending(record)
                return INCOMPLETE
            record.update(phase="completed", disposition="owned-work-terminal")
            lock.save(record)
            lock.retire(record)
        return result
    except (OSError, Refusal) as error:
        # Closing GO first makes every pre-admission failure refuse execution.
        # The admission record is deliberately retained, including unknown I/O.
        notice(f"custody interrupted for generation={token}: {display(error)}")
        return INCOMPLETE
    finally:
        for descriptor in (read_fd, write_fd):
            if descriptor is not None:
                os.close(descriptor)
        # An error must not detach the owned direct child. No signal after reap.
        if child is not None and child.returncode is None:
            forwarded = 0
            while True:
                if cancelled and cancelled != forwarded:
                    try:
                        os.killpg(child.pid, cancelled)
                    except ProcessLookupError:
                        pass
                    forwarded = cancelled
                if child.poll() is not None:
                    break
                time.sleep(0.05)


def ticket_command(arguments):
    operation = arguments[0]
    required = 3 if operation == "--ticket-begin" else 4
    if len(arguments) not in ({required} if operation == "--ticket-begin" else {required, required + 1}):
        raise Refusal("usage: --ticket-begin OWNER SCOPE | --ticket-complete OWNER NONCE EVIDENCE [REFERENCE]")
    owner = arguments[1]
    if owner not in OWNERS:
        raise Refusal("unknown external-work owner")
    path = inherited_path()
    if operation == "--ticket-begin":
        safe(arguments[2])
    else:
        if arguments[3] not in OWNERS[owner]:
            raise Refusal("invalid terminal evidence class for owner")
        safe(arguments[4] if len(arguments) == 5 else arguments[3])
    if path is None:
        if operation == "--ticket-begin":
            print("standalone")
        elif arguments[2] != "standalone":
            raise Refusal("ticket completion lost inherited custody", INCOMPLETE)
        return 0
    lock = Lock(path)
    with lock.held():
        record = verify_inherited(lock)
        if operation == "--ticket-begin":
            if len(record["tickets"]) >= 4096:
                raise Refusal("generation ticket capacity exhausted", INCOMPLETE)
            nonce = secrets.token_hex(16)
            record["tickets"][nonce] = {"owner": owner, "scope": arguments[2], "state": "pending"}
            lock.save(record)
            print(nonce)
        else:
            ticket = record["tickets"].get(arguments[2])
            if ticket is None or ticket["owner"] != owner:
                raise Refusal("ticket does not belong to this generation/owner", INCOMPLETE)
            if ticket["state"] != "terminal":
                ticket.update(state="terminal", evidence=arguments[3],
                              reference=arguments[4] if len(arguments) == 5 else arguments[3])
                lock.save(record)
    return 0


def reconcile(arguments):
    if len(arguments) != 3 or arguments[1] != "--evidence":
        raise Refusal("usage: --reconcile TOKEN --evidence OWNER_TERMINAL_JSON")
    token, _, source = arguments
    _, path = origin()
    lock = Lock(path)
    evidence = Lock.read_json(Path(source), 1024 * 1024)
    if (evidence.get("token") != token or evidence.get("decision") != "owner-confirmed-terminal"
            or evidence.get("command") != "joined" or evidence.get("process_group") != "joined"):
        raise Refusal("recovery requires a token-specific terminal decision by the named work owner", INCOMPLETE)
    safe(evidence.get("reference"))
    with lock.held():
        record = lock.matching(token)
        if record.get("pgid") is not None and group_exists(record["pgid"]):
            raise Refusal("owned group still exists; terminal decision is not established", INCOMPLETE)
        acknowledgements = evidence.get("tickets", {})
        if set(acknowledgements) != set(pending(record)):
            raise Refusal("recovery must account for every pending ticket, and only this generation", INCOMPLETE)
        for nonce, ticket in pending(record).items():
            acknowledgement = acknowledgements[nonce]
            if (acknowledgement.get("owner") != ticket["owner"]
                    or acknowledgement.get("scope") != ticket["scope"]
                    or acknowledgement.get("evidence") not in OWNERS[ticket["owner"]]):
                raise Refusal("recovery evidence does not match the pending owner/scope", INCOMPLETE)
            safe(acknowledgement.get("reference"))
        for nonce, acknowledgement in acknowledgements.items():
            record["tickets"][nonce].update(state="terminal", evidence=acknowledgement["evidence"],
                                             reference=acknowledgement["reference"])
        record.update(phase="completed", command_terminal=True, group_terminal=True,
                      disposition="owner-reconciled", recovery_reference=evidence["reference"])
        lock.save(record)
        lock.retire(record)
    notice(f"reconciled generation={token}; retained terminal evidence")
    return 0


def self_test():
    """Existing lock self-test, exercising only isolated, fixture-owned paths."""
    entrypoint = str(Path(__file__).with_suffix(".sh").resolve())
    environment = dict(os.environ)
    for key in (*IDENTITY_KEYS, "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_DIR", "VALIDATION_LOCK_CANDIDATE"):
        environment.pop(key, None)
    environment["VALIDATION_LOCK_TIMEOUT_SECONDS"] = "5"
    active = []
    releases = []

    def expect(value, message):
        if not value:
            raise Refusal(f"self-test: {message}", 1)

    def wait_for(predicate, message):
        deadline = time.monotonic() + 10
        while not predicate():
            if time.monotonic() >= deadline:
                raise Refusal(f"self-test: timed out awaiting {message}", 1)
            time.sleep(0.02)

    def run(path, arguments, expected=0, extra=None):
        env = dict(environment, VALIDATION_LOCK_DIR=str(path))
        env.update(extra or {})
        result = subprocess.run(["bash", entrypoint, *arguments], env=env,
                                capture_output=True, text=True, timeout=15)
        expect(result.returncode == expected,
               f"expected exit {expected}, got {result.returncode}: {result.stderr}")
        return result

    hold_source = (
        "import os,sys,time\nfrom pathlib import Path\n"
        "ready,release,done=map(Path,sys.argv[1:])\n"
        "ready.write_text(os.environ['VALIDATION_LOCK_TOKEN'])\n"
        "deadline=time.monotonic()+15\n"
        "while not release.exists() and time.monotonic()<deadline: time.sleep(.02)\n"
        "done.write_text('terminal')\n"
    )

    with tempfile.TemporaryDirectory(prefix="validation-lock-self-test-") as directory:
        fixture = Path(directory)

        def hold(path, label, source=hold_source):
            ready, release, done = (fixture / f"{label}.{suffix}" for suffix in ("ready", "release", "done"))
            releases.append(release)
            process = subprocess.Popen(
                ["bash", entrypoint, "--", sys.executable, "-c", source,
                 str(ready), str(release), str(done)],
                env=dict(environment, VALIDATION_LOCK_DIR=str(path)),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            active.append(process)
            wait_for(ready.exists, f"{label} payload admission")
            return process, ready, release, done

        def finish(process, expected=0):
            _, error = process.communicate(timeout=15)
            expect(process.returncode == expected,
                   f"fixture exited {process.returncode}, expected {expected}: {error}")

        try:
            # Serialize real payloads, preserve the guard inode, and allow old
            # mkdir admission only after the current owner has finished.
            path = fixture / "serial.lock"
            first, ready, release, _ = hold(path, "first")
            guard_inode = Path(f"{path}.guard").stat().st_ino
            expect(path.is_file(), "current admission is not a regular file")
            try:
                path.mkdir()
            except FileExistsError:
                pass
            else:
                raise Refusal("self-test: legacy mkdir overlapped current admission", 1)
            payload = fixture / "queued-ran"
            touch = ["--", sys.executable, "-c", "from pathlib import Path; import sys; Path(sys.argv[1]).touch()", str(payload)]
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1", "VALIDATION_LOCK_HELD": "1"})
            expect(not payload.exists(), "timed-out/boolean-forged payload ran")

            # A real token copied to another process group is not inheritance.
            run(path, touch, 2, {"VALIDATION_LOCK_PATH": str(path), "VALIDATION_LOCK_TOKEN": ready.read_text()})
            expect(not payload.exists(), "copied generation token granted unrelated admission")

            log = fixture / "cancelled-waiter.log"
            with log.open("w") as error:
                waiter = subprocess.Popen(["bash", entrypoint, *touch],
                                          env=dict(environment, VALIDATION_LOCK_DIR=str(path)), stderr=error)
                active.append(waiter)
                wait_for(lambda: "waiting" in log.read_text(), "waiter contention diagnostic")
                waiter.send_signal(signal.SIGTERM)
                expect(waiter.wait(timeout=10) == 143, "queued cancellation changed its exit disposition")
            release.touch()
            finish(first)
            expect(not payload.exists(), "cancelled waiter ran after owner release")
            expect(not path.exists(), "normal release retained admission")
            path.mkdir()
            (path / "owner").write_text("pid=999999999\ncommand=must-not-appear-in-diagnostics\n")
            legacy = run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            expect("legacy_protocol_unreconciled" in legacy.stderr, "legacy custody was guessed")
            expect("must-not-appear" not in legacy.stderr, "legacy argv leaked into diagnostics")
            expect((path / "owner").is_file(), "legacy owner was reclaimed")
            (path / "owner").unlink()
            path.rmdir()
            run(path, touch)
            expect(payload.exists(), "normal successor did not execute")
            expect(Path(f"{path}.guard").stat().st_ino == guard_inode, "permanent guard inode changed")

            # Cancellation retains the real owned child until it finishes,
            # even when that child deliberately delays handling SIGTERM.
            path = fixture / "cancelling.lock"
            handled = fixture / "term-observed"
            cancellable_source = (
                "import signal\nfrom pathlib import Path\n"
                f"signal.signal(signal.SIGTERM, lambda *_: Path({str(handled)!r}).touch())\n"
                + hold_source
            )
            owner, _, release, _ = hold(path, "cancelling", cancellable_source)
            owner.send_signal(signal.SIGTERM)
            wait_for(handled.exists, "owned child cancellation")
            expect(owner.poll() is None and path.is_file(), "cancellation prematurely released live work")
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            release.touch()
            finish(owner, 143)
            expect(not path.exists(), "completed cancellation retained admission")

            # Both queued current clients must execute serially after a release.
            path = fixture / "contenders.lock"
            owner, _, release, _ = hold(path, "contenders")
            mutex = fixture / "payload.mutex"
            contender_source = (
                "import os,sys,time\nfrom pathlib import Path\n"
                "path=Path(sys.argv[1]); fd=os.open(path,os.O_CREAT|os.O_EXCL|os.O_WRONLY)\n"
                "time.sleep(.1); os.close(fd); path.unlink()\n"
            )
            contenders = [subprocess.Popen(
                ["bash", entrypoint, "--", sys.executable, "-c", contender_source, str(mutex)],
                env=dict(environment, VALIDATION_LOCK_DIR=str(path)),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            ) for _ in range(2)]
            active.extend(contenders)
            release.touch()
            finish(owner)
            for contender in contenders:
                finish(contender)

            # Nested execution carries the originating path across Git roots;
            # a contradictory legacy override cannot select a second lock.
            nested = (
                "import os,subprocess,sys\n"
                "env=dict(os.environ,VALIDATION_LOCK_DIR=sys.argv[3])\n"
                "result=subprocess.run(['bash',sys.argv[1],'--','sh','-c','printf nested > \"$1\"','_',sys.argv[2]],"
                "cwd=sys.argv[4],env=env)\n"
                "raise SystemExit(result.returncode)\n"
            )
            nested_marker = fixture / "nested-ran"
            other_path = fixture / "must-not-lock"
            run(fixture / "nested.lock", ["--", sys.executable, "-c", nested,
                                          entrypoint, str(nested_marker), str(other_path), str(fixture)])
            expect(nested_marker.read_text() == "nested" and not other_path.exists(), "nested origin was not retained")

            # The pre-exec gate cannot run a command on supervisor EOF.
            read_fd, write_fd = os.pipe()
            os.close(write_fd)
            gate_marker = fixture / "gate-ran"
            try:
                result = subprocess.run(
                    [sys.executable, str(Path(__file__).resolve()), "--gate", str(read_fd), "--",
                     "sh", "-c", 'touch "$1"', "_", str(gate_marker)],
                    pass_fds=(read_fd,), env=environment, capture_output=True, timeout=10,
                )
            finally:
                os.close(read_fd)
            expect(result.returncode == 125 and not gate_marker.exists(), "gate EOF admitted payload")

            # Reaping a shell is not evidence that its background work ended.
            path = fixture / "surviving-group.lock"
            ready, release, done = (fixture / f"survivor.{suffix}" for suffix in ("ready", "release", "done"))
            releases.append(release)
            escaping = (
                "import subprocess,sys,time\nfrom pathlib import Path\n"
                "subprocess.Popen([sys.executable,'-c',sys.argv[1],*sys.argv[2:]],"
                "stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n"
                "deadline=time.monotonic()+10\n"
                "while not Path(sys.argv[2]).exists() and time.monotonic()<deadline: time.sleep(.02)\n"
            )
            run(path, ["--", sys.executable, "-c", escaping, hold_source, str(ready), str(release), str(done)], INCOMPLETE)
            expect(path.exists() and not done.exists(), "surviving group was treated as completed")
            release.touch()
            wait_for(done.exists, "surviving fixture completion")

            # Corrupt/missing metadata cannot become permission to reclaim.
            path = fixture / "unknown.lock"
            path.write_text('{"protocol":2,"token":"' + "0" * 32 + '"}\n')
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            expect(path.exists(), "unknown generation metadata was reclaimed")

            # A successful shell with pending external work is incomplete.
            path = fixture / "ticket.lock"
            ticket_file = fixture / "ticket.nonce"
            run(path, ["--", "sh", "-c", 'bash "$1" --ticket-begin make-docs-check fixture-container >"$2"',
                       "_", entrypoint, str(ticket_file)], INCOMPLETE)
            token = json.loads(path.read_text())["token"]
            nonce = ticket_file.read_text().strip()
            expect(path.is_file(), "pending ticket released admission")
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            evidence = {
                "token": token, "decision": "owner-confirmed-terminal", "command": "joined",
                "process_group": "joined", "reference": "self-test-no-external-effect", "tickets": {},
            }
            recovery = fixture / "terminal.json"
            recovery.write_text(json.dumps(evidence))
            run(path, ["--reconcile", token, "--evidence", str(recovery)], INCOMPLETE)
            evidence["tickets"][nonce] = {"owner": "make-docs-check", "scope": "fixture-container",
                                          "evidence": "container-absent", "reference": "no-external-effect"}
            recovery.write_text(json.dumps(evidence))
            run(path, ["--reconcile", token, "--evidence", str(recovery)])
            successor, _, successor_release, _ = hold(path, "successor")
            successor_bytes = path.read_bytes()
            run(path, ["--reconcile", token, "--evidence", str(recovery)], INCOMPLETE)
            expect(path.read_bytes() == successor_bytes, "stale recovery retired a successor")
            successor_release.touch()
            finish(successor)

            # Terminal acknowledgement is idempotent and preserves a failed
            # test's status; the wrong owner cannot close the same ticket.
            complete = (
                'ticket=$(bash "$1" --ticket-begin make-docs-check fixture-container) || exit; '
                'if bash "$1" --ticket-complete make-shellcheck "$ticket" container-absent; then exit 99; fi; '
                'bash "$1" --ticket-complete make-docs-check "$ticket" container-absent || exit; '
                'bash "$1" --ticket-complete make-docs-check "$ticket" container-absent || exit; exit 7'
            )
            run(fixture / "completed-ticket.lock", ["--", "sh", "-c", complete, "_", entrypoint], 7)
            expect(not (fixture / "completed-ticket.lock").exists(), "known failed command retained terminal custody")

            # At the existing Compose owner, a failed native submission with
            # a responsive but empty daemon is still ambiguous. The successful
            # startup control proves ordinary later test failure can release.
            root = Path(__file__).resolve().parents[2]
            if (root / "scripts/lib/compose-postgres.sh").is_file():
                compose_fixture = r'''
ROOT_DIR=$1
fixture_up_exit=$2
source "${ROOT_DIR}/scripts/lib/compose-postgres.sh"
docker() {
    local argument
    for argument in "$@"; do
        case "${argument}" in
            up) return "${fixture_up_exit}" ;;
            port) printf '127.0.0.1:15432\n'; return 0 ;;
        esac
    done
    return 0
}
trap compose_postgres_exit EXIT
compose_postgres_up fixture-compose
exit 7
'''
                uncertain = fixture / "compose-unknown.lock"
                run(uncertain, ["--", "bash", "-eu", "-c", compose_fixture, "_", str(root), "17"], INCOMPLETE)
                expect(uncertain.is_file(), "empty readback erased unknown Compose submission")
                known = fixture / "compose-terminal.lock"
                run(known, ["--", "bash", "-eu", "-c", compose_fixture, "_", str(root), "0"], 7)
                expect(not known.exists(), "known Compose cleanup obscured an ordinary test failure")

            # Supervisor death cannot release its live child, nor does later
            # child/PID absence become authority to reclaim that generation.
            path = fixture / "killed.lock"
            killed, _, release, done = hold(path, "killed")
            original = path.read_bytes()
            killed.kill()
            # Its child still owns the inherited output pipes; waiting for EOF
            # here would accidentally wait for that child before the assertion.
            expect(killed.wait(timeout=10) == -signal.SIGKILL, "fixture supervisor was not killed")
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            expect(path.read_bytes() == original, "dead supervisor was reclaimed")
            release.touch()
            wait_for(done.exists, "orphaned fixture's owned completion")
            finish(killed, -signal.SIGKILL)
            run(path, touch, TIMEOUT, {"VALIDATION_LOCK_TIMEOUT_SECONDS": "0.1"})
            expect(path.read_bytes() == original, "PID absence was used as terminal evidence")
        finally:
            for release in releases:
                release.touch()
            for process in active:
                if process.poll() is None:
                    # These are unreaped fixture supervisors, never guessed PIDs.
                    process.send_signal(signal.SIGTERM)
                    process.wait(timeout=20)
    print("validation lock self-test: pass")
    return 0


def main(arguments):
    if not arguments:
        raise Refusal("usage: validation-lock.sh -- command [args...]")
    if arguments[0] == "--gate":
        return gate(arguments[1:])
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    if arguments[0] in {"--ticket-begin", "--ticket-complete"}:
        return ticket_command(arguments)
    if arguments[0] == "--reconcile":
        return reconcile(arguments[1:])
    if arguments == ["--self-test"]:
        return self_test()
    path = inherited_path()
    if arguments in (["--check-custody"], ["--assert-complete"]):
        if path is None:
            if arguments == ["--check-custody"]:
                return 1
            raise Refusal("no inherited generation to assert")
        lock = Lock(path)
        with lock.held():
            record = verify_inherited(lock)
            if arguments == ["--assert-complete"] and pending(record):
                report_pending(record)
                return INCOMPLETE
        return 0
    if arguments[0] != "--" or len(arguments) < 2:
        raise Refusal("usage: validation-lock.sh -- command [args...]")
    command = arguments[1:]
    if path is not None:
        lock = Lock(path)
        with lock.held():
            verify_inherited(lock)
        if cancelled:
            return 128 + cancelled
        os.execvpe(command[0], command, os.environ)
    root, path = origin()
    lock = Lock(path)
    record = acquire(lock, root, command)
    return supervise(lock, record, command)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Refusal as failure:
        notice(str(failure))
        sys.exit(failure.code)
    except (OSError, ValueError, KeyError, TypeError) as failure:
        notice(f"refused: {display(failure)}")
        sys.exit(2)
