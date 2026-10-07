#!/usr/bin/env python3
"""Blackbox queue/lifetime contracts; no Cargo, Docker, or shared lock access."""

import json
import fcntl
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / "scripts/ci/validation-lock.sh"
LOCK_ENV = ("VALIDATION_LOCK_DIR", "VALIDATION_LOCK_DOMAIN",
            "VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_CHILD")

# Real children announce readiness after launch and have a safety deadline even
# when an assertion fails. Release files are test controls, never queue receipts.
HOLD = """
import json, os, pathlib, signal, sys, time
base = pathlib.Path(sys.argv[1])
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
base.with_suffix('.tmp').write_text(json.dumps({'pid': os.getpid(), 'sid': os.getsid(0)}))
base.with_suffix('.tmp').replace(base.with_suffix('.ready'))
deadline = time.monotonic() + 20
while not base.with_suffix('.release').exists() and time.monotonic() < deadline:
    time.sleep(.02)
"""
MARK = "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('launched')"

# Fault injection lives only in the temporary Python import path. It interrupts
# the actual native gate publication, never manufactures queue/owner state.
LINK_INTERRUPTION = """
import os, signal
native_link = os.link
def interrupted_link(source, destination, *args, **kwargs):
    if os.path.realpath(destination) != os.path.realpath(os.environ['TEST_LINK_GATE']):
        return native_link(source, destination, *args, **kwargs)
    mode = os.environ['TEST_LINK_INTERRUPTION']
    if mode != 'kill-before-link':
        native_link(source, destination, *args, **kwargs)
    with open(os.environ['TEST_LINK_REACHED'], 'w') as reached:
        reached.write(mode)
    os.kill(os.getpid(), signal.SIGTERM if mode == 'term-after-link' else signal.SIGKILL)
os.link = interrupted_link
"""

# Test control stays in the real root command session. Files carry requests to
# this fixture, never forged queue state or process-completion receipts.
CONTROLLER = """
import json, os, pathlib, subprocess, sys, time
entry, directory = sys.argv[1:]
base = pathlib.Path(directory)
(base / 'controller.env').write_text(json.dumps({key: value for key, value in os.environ.items()
                                               if key.startswith('VALIDATION_LOCK_')}))
(base / 'controller.ready').touch()
children = {}
deadline = time.monotonic() + 75
while not (base / 'controller.release').exists() and time.monotonic() < deadline:
    for request in sorted(base.glob('request-*.json')):
        reply = request.with_suffix('.reply')
        if reply.exists():
            continue
        value = json.loads(request.read_text())
        if value.get('background'):
            output = request.with_suffix('.stdout').open('w')
            errors = request.with_suffix('.stderr').open('w')
            process = subprocess.Popen(['bash', entry, *value['args']], stdout=output, stderr=errors)
            output.close()
            errors.close()
            children[request.stem] = process
            response = {'pid': process.pid, 'name': request.stem}
        else:
            process = subprocess.run(['bash', entry, *value['args']], text=True, capture_output=True)
            response = {'code': process.returncode, 'stdout': process.stdout, 'stderr': process.stderr}
        pending = reply.with_suffix('.pending')
        pending.write_text(json.dumps(response))
        pending.replace(reply)
    for name, process in children.items():
        code = process.poll()
        if code is not None and not (base / (name + '.done')).exists():
            (base / (name + '.done')).write_text(str(code))
    time.sleep(.015)
"""

# Exercise process loss at native boundaries without production fault switches.
# Each hook announces its PID before waiting, and has its own safety deadline.
CHILD_INTERRUPTION = """
import json, os, pathlib, signal, sys, time
base = pathlib.Path(os.environ['TEST_CHILD_HOOK_DIR'])
mode = os.environ['TEST_CHILD_HOOK']
helper = '--child-run' in sys.argv
def pause(label):
    reached = base / (label + '.reached')
    temporary = base / (label + '.' + str(os.getpid()) + '.tmp')
    temporary.write_text(str(os.getpid()))
    temporary.replace(reached)
    deadline = time.monotonic() + 25
    while not (base / (label + '.release')).exists() and time.monotonic() < deadline:
        time.sleep(.01)
native_fork = os.fork
def fork():
    pid = native_fork()
    if pid == 0 and helper and mode == 'fork-before-receipt':
        pause('fork')
    return pid
os.fork = fork
native_replace = os.replace
def replace(source, destination, *args, **kwargs):
    destination = str(destination)
    if helper and mode == 'ready-before-publication' and destination.endswith('/state.json'):
        value = json.loads(pathlib.Path(source).read_text())
        if any(child.get('prepared') and (child.get('helper') or {}).get('pid') == os.getpid()
               for child in value.get('children', [])):
            pause('publication')
    native_replace(source, destination, *args, **kwargs)
    if helper and destination.endswith('.child.prepared.json') and mode in ('receipt-before-ready', 'bad-receipt'):
        if mode == 'bad-receipt':
            value = json.loads(pathlib.Path(destination).read_text())
            value['nonce'] = '0' * 32
            pathlib.Path(destination).write_text(json.dumps(value))
        pause('receipt')
os.replace = replace
native_write = os.write
def write(fd, data):
    if data.startswith(b'QSC1 ') and (base / 'signal.target').exists():
        with (base / 'signals').open('a') as output:
            output.write(str(os.getpid()) + ':' + data.decode())
        if mode == 'signal-pin' and not (base / 'signal.reached').exists():
            pause('signal')
    if data == b'L' and (base / 'launch.arm').exists() and mode in ('guardian-before-L', 'guardian-after-L'):
        if mode == 'guardian-after-L':
            native_write(fd, data)
        (base / 'launch.reached').write_text(str(os.getpid()))
        os.kill(os.getpid(), signal.SIGKILL)
    return native_write(fd, data)
os.write = write
native_killpg = os.killpg
def killpg(group, signum):
    if signum:
        raise AssertionError('numeric ordinary signal fallback')
    return native_killpg(group, signum)
os.killpg = killpg
native_waitpid = os.waitpid
def waitpid(pid, options):
    if helper and mode == 'helper-wait' and options == os.WNOHANG and not (base / 'wait.reached').exists():
        owner = pathlib.Path(os.environ['VALIDATION_LOCK_DOMAIN'] + '.queue') / 'owners' / os.environ['VALIDATION_LOCK_TOKEN']
        state = json.loads((owner / 'state.json').read_text())
        if any(child.get('prepared') and (child.get('scope') or {}).get('pid') == pid
               and (child.get('helper') or {}).get('pid') == os.getpid()
               for child in state.get('children', [])):
            pause('wait')
    return native_waitpid(pid, options)
os.waitpid = waitpid
"""

TERM_COUNT = HOLD.replace("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))", """
def term(*_):
    with base.with_suffix('.signals').open('a') as output:
        output.write('TERM\\n')
signal.signal(signal.SIGTERM, term)
""").replace("+ 20", "+ 45")

# Faults alter the real channel/kernel-call boundary. They never create a queue
# receipt or grant authority to a helper, foreign process, or successor.
RELAY_INTERRUPTION = """
import errno, json, os, pathlib, signal, time
base = pathlib.Path(os.environ['TEST_RELAY_DIR'])
mode = os.environ['TEST_RELAY_MODE']
def target():
    path = base / 'target.sid'
    return path.exists() and os.getsid(0) == int(path.read_text())
def pause(name):
    (base / (name + '.reached')).write_text(str(os.getpid()))
    deadline = time.monotonic() + 30
    while not (base / (name + '.release')).exists() and time.monotonic() < deadline:
        time.sleep(.01)
native_getpgid = os.getpgid
def getpgid(pid):
    group = native_getpgid(pid)
    if mode == 'foreign-hint' and target() and pid != os.getpid():
        foreign = int((base / 'foreign.sid').read_text())
        (base / 'hint.reached').write_text(json.dumps({'member': pid, 'hint': foreign}))
        return foreign
    return group
os.getpgid = getpgid
native_setpgid = os.setpgid
def setpgid(pid, group):
    try:
        native_setpgid(pid, group)
    except OSError:
        if target():
            (base / 'join.refused').write_text(str(group))
        raise
    if mode == 'joined-relay' and target() and group != os.getsid(0):
        (base / 'joined.group').write_text(str(native_getpgid(0)))
        pause('relay')
os.setpgid = setpgid
native_fork = os.fork
def fork():
    if mode == 'fork-failure' and target():
        raise OSError(errno.EAGAIN, 'fixture relay fork unavailable')
    return native_fork()
os.fork = fork
native_kill = os.kill
def kill(pid, signum):
    if target() and signum:
        if pid != 0:
            raise AssertionError('numeric real signal was used')
        with (base / 'deliveries').open('a') as output:
            output.write(str(os.getpid()) + ':' + str(native_getpgid(0)) + ':' + str(signum) + '\\n')
    return native_kill(pid, signum)
os.kill = kill
native_killpg = os.killpg
def killpg(group, signum):
    if signum:
        raise AssertionError('numeric real signal was used')
    return native_killpg(group, signum)
os.killpg = killpg
native_read = os.read
def read(fd, count):
    data = native_read(fd, count)
    if mode == 'full-eof' and target() and data.startswith(b'QSC1 '):
        pause('frame')
    return data
os.read = read
native_write = os.write
def write(fd, data):
    if not data.startswith(b'QSC1 '):
        return native_write(fd, data)
    with (base / 'grants').open('a') as output:
        output.write(data.decode())
    if mode == 'partial-eof':
        native_write(fd, data[:8])
        native_kill(os.getpid(), signal.SIGKILL)
    if mode == 'expired':
        fields = data.decode().split()
        fields[-1] = str(time.monotonic_ns() - 1)
        native_write(fd, (' '.join(fields) + '\\n').encode())
        return len(data)
    result = native_write(fd, data)
    if mode == 'full-eof':
        native_kill(os.getpid(), signal.SIGKILL)
    if mode == 'unknown-write':
        raise OSError(errno.EIO, 'fixture write acknowledgement lost')
    if mode == 'duplicate':
        deadline = time.monotonic() + 5
        while not (base / 'deliveries').exists() and time.monotonic() < deadline:
            time.sleep(.01)
        native_write(fd, data)
    return result
os.write = write
"""

# This fixture implements native container state independently of the queue.
# It never writes queue files, owner records, or completion receipts.
DOCKER = """#!/usr/bin/env python3
import json, os, pathlib, re, signal, sys, time
root = pathlib.Path(os.environ['TEST_DOCKER_STATE'])
if (root / 'unavailable').exists():
    sys.exit(1)
args = sys.argv[1:]
def save(path, value):
    staged = path.with_name(path.name + '.' + str(os.getpid()) + '.tmp')
    staged.write_text(json.dumps(value))
    staged.replace(path)
statefile = root / 'container.json'
state = json.loads(statefile.read_text()) if statefile.exists() else None
builderfile = root / 'builder.json'
builder = json.loads(builderfile.read_text()) if builderfile.exists() else None
if args[:2] == ['context', 'show']:
    print('test-context')
elif args and args[0] == 'info':
    print('test-daemon-id')
elif args[:2] == ['buildx', 'ls']:
    output_format = args[args.index('--format') + 1] if '--format' in args else None
    if output_format not in (None, 'json', '{{.Name}}'):
        sys.exit(2)
    if builder is not None:
        print(json.dumps(builder) if output_format == 'json' else builder['Name'])
elif args[:2] == ['buildx', 'create']:
    if builder is not None:
        sys.exit(1)
    name = args[args.index('--name') + 1]
    driver = args[args.index('--driver') + 1]
    builder = {'Name': name, 'Driver': driver, 'Current': False,
               'Nodes': [{'Name': name + '0', 'Endpoint': 'test-context'}]}
    save(builderfile, builder)
    print(name)
elif args[:2] == ['buildx', 'inspect']:
    if '--format' in args or any(arg.startswith('--format=') for arg in args):
        sys.exit(2)
    if builder is None or args[2] != builder['Name']:
        sys.exit(1)
    if '--bootstrap' in args and state is None:
        state = {'Id': 'b' * 64, 'Name': '/buildx_buildkit_' + builder['Nodes'][0]['Name'],
                 'Config': {'Labels': {}},
                 'State': {'Running': True, 'Restarting': False, 'Status': 'running'}}
        save(statefile, state)
    print('Name: ' + builder['Name'] + '\\nDriver: ' + builder['Driver'])
elif args[:2] == ['buildx', 'build']:
    if (builder is None or args[args.index('--builder') + 1] != builder['Name']
            or state is None or not state['State']['Running']):
        sys.exit(1)
    def interrupted(signum, _frame):
        (root / 'observer.signal').write_text(str(signum))
        sys.exit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    save(root / 'observer.ready', {'pid': os.getpid(), 'sid': os.getsid(0)})
    deadline = time.monotonic() + 35
    # Model a CLI awaiting Engine import completion after its BuildKit node stops.
    while not (root / 'observer.release').exists() and time.monotonic() < deadline:
        if (root / 'observer.transport-loss').exists():
            sys.exit(1)
        time.sleep(.02)
    if not (root / 'observer.release').exists():
        sys.exit(124)
    (root / 'native-terminal').touch()
elif args and args[0] == 'run':
    name = args[args.index('--name') + 1]
    label = args[args.index('--label') + 1].split('=', 1)
    if state is not None:
        sys.exit(1)
    if os.environ.get('TEST_DOCKER_DELAY_CREATE') == '1':
        def interrupted_create(signum, _frame):
            (root / 'create.signal').write_text(str(signum))
            sys.exit(128 + signum)
        signal.signal(signal.SIGTERM, interrupted_create)
        signal.signal(signal.SIGINT, interrupted_create)
        save(root / 'create.ready', {'pid': os.getpid(), 'sid': os.getsid(0)})
        deadline = time.monotonic() + 35
        while not (root / 'create.release').exists() and time.monotonic() < deadline:
            time.sleep(.02)
        if not (root / 'create.release').exists():
            sys.exit(124)
    state = {'Id': 'a' * 64, 'Name': '/' + name,
             'Config': {'Labels': {label[0]: label[1]}},
             'State': {'Running': True, 'Restarting': False, 'Status': 'running'}}
    save(statefile, state)
    if os.environ.get('TEST_DOCKER_CREATE_RESPONSE_LOSS') == '1':
        sys.exit(1)
    if '--cidfile' in args:
        pathlib.Path(args[args.index('--cidfile') + 1]).write_text(state['Id'])
    if os.environ.get('TEST_DOCKER_DELAY_CREATE') == '1':
        (root / 'create.published').touch()
    if 'TEST_DOCKER_APP_EXIT' in os.environ:
        status = int(os.environ['TEST_DOCKER_APP_EXIT'])
        state['State'].update(Running=False, Status='exited', ExitCode=status)
        save(statefile, state)
        if '--rm' in args:
            statefile.unlink()
        sys.exit(status)
    print(state['Id'])
elif args and args[0] == 'ps':
    selected = state is not None
    if selected:
        for index, arg in enumerate(args):
            if arg != '--filter':
                continue
            field, value = args[index + 1].split('=', 1)
            if field == 'name':
                selected = selected and re.search(value, state['Name']) is not None
            elif field == 'label':
                key, _, expected = value.partition('=')
                labels = state['Config']['Labels']
                selected = selected and key in labels and (not expected or labels[key] == expected)
            else:
                sys.exit(2)
        include_stopped = '--all' in args or any(arg.startswith('-') and not arg.startswith('--')
                                                and 'a' in arg for arg in args)
        selected = selected and (include_stopped or state['State']['Running'])
    if selected:
        print(state['Id'])
    elif state is None and (root / 'create.ready').exists():
        (root / 'create.absence-read').touch()
elif args and args[0] == 'inspect':
    if state is None:
        sys.exit(1)
    print(json.dumps([state]))
elif args and args[0] in ('stop', 'kill'):
    if state is None or args[-1] not in (state['Id'], state['Name'][1:]):
        sys.exit(1)
    state['State'].update(Running=False, Restarting=False, Status='exited')
    save(statefile, state)
    print(state['Id'])
elif args and args[0] == 'wait':
    if state is None or state['State']['Running']:
        sys.exit(1)
    print(0)
elif args and args[0] == 'rm':
    if state is not None and state['State']['Running'] and not any(arg in args for arg in ('-f', '--force')):
        sys.exit(1)
    if state is not None:
        with (root / 'removed').open('a') as removed:
            removed.write(state['Id'] + '\\n')
    statefile.unlink(missing_ok=True)
else:
    sys.exit(2)
"""


class ValidationLockTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="validation-queue-test-")
        self.base = Path(self.tmp.name)
        self.gate = self.base / "validation.lock"
        self.env = {k: v for k, v in os.environ.items() if k not in LOCK_ENV}
        self.env.update(VALIDATION_LOCK_DIR=str(self.gate),
                        VALIDATION_LOCK_TIMEOUT_SECONDS="8")
        self.processes = []
        self.logs = []
        self.releases = []

    def tearDown(self):
        for release in self.releases:
            release.touch()
        for proc in self.processes:
            if proc.poll() is None:
                proc.terminate()
        for proc in self.processes:
            try:
                proc.wait(timeout=18)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)
        for log in self.logs:
            log.close()
        self.tmp.cleanup()

    def launch(self, *args, env=None):
        log = tempfile.TemporaryFile(mode="w+")
        self.logs.append(log)
        proc = subprocess.Popen(["bash", str(ENTRY), *args], cwd=ROOT,
                                env=self.env if env is None else env,
                                stdout=log, stderr=log, start_new_session=True)
        self.processes.append(proc)
        return proc

    def run_cli(self, *args, env=None, timeout=10):
        return subprocess.run(["bash", str(ENTRY), *args], cwd=ROOT,
                              env=self.env if env is None else env,
                              text=True, capture_output=True, timeout=timeout)

    def eventually(self, predicate, message, timeout=6):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = predicate()
            if result:
                return result
            time.sleep(.025)
        self.fail(message)

    def status(self):
        result = self.run_cli("--status")
        self.assertEqual(result.returncode, 0, result.stderr)
        state = json.loads(result.stdout)
        self.assertIn("gate", state)
        self.assertIn("tickets", state)
        return state

    def hold(self, name):
        base = self.base / name
        self.releases.append(base.with_suffix(".release"))
        proc = self.launch("--", sys.executable, "-c", HOLD, str(base))
        self.eventually(base.with_suffix(".ready").exists, name + " did not start")
        return proc, base

    def release(self, base):
        base.with_suffix(".release").touch()

    def marker_command(self, name):
        return ("--", sys.executable, "-c", MARK, str(self.base / name))

    def process_terminal(self, pid):
        result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                                text=True, capture_output=True, timeout=3)
        if result.returncode == 1 and not result.stdout.strip():
            return True
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip().startswith("Z")

    def docker_fixture(self):
        fixture = self.base / "bin"
        fixture.mkdir()
        docker = fixture / "docker"
        docker.write_text(DOCKER)
        docker.chmod(0o700)
        native = self.base / "native-state"
        native.mkdir()
        env = {**self.env, "PATH": str(fixture) + os.pathsep + self.env["PATH"],
               "TEST_DOCKER_STATE": str(native)}
        return native, env

    def controller(self, env=None, v3=True):
        self.request_number = 0
        self.releases.append(self.base / "controller.release")
        prefix = ("--with-child-scopes", "--") if v3 else ("--",)
        owner = self.launch(*prefix, sys.executable, "-c", CONTROLLER,
                            str(ENTRY), str(self.base), env=env)
        self.eventually((self.base / "controller.ready").exists, "root controller did not start")
        return owner

    def control(self, *args, background=False, timeout=8):
        self.request_number += 1
        request = self.base / (f"request-{self.request_number:04d}.json")
        pending = request.with_suffix(".pending")
        pending.write_text(json.dumps({"args": list(args), "background": background}))
        pending.replace(request)
        reply = request.with_suffix(".reply")
        self.eventually(reply.exists, "controller did not answer " + args[0], timeout=timeout)
        return json.loads(reply.read_text())

    def reserve_child(self, seconds=40):
        response = self.control("--child-reserve", "--cancel-at-monotonic-ns",
                                str(time.monotonic_ns() + int(seconds * 1e9)))
        self.assertEqual(response["code"], 0, response)
        return response["stdout"].strip()

    def child_state(self, handle):
        response = self.control("--child-status", handle)
        self.assertEqual(response["code"], 0, response)
        return json.loads(response["stdout"])

    def child_hold(self, name, seconds=40, command=HOLD):
        base = self.base / name
        self.releases.append(base.with_suffix(".release"))
        handle = self.reserve_child(seconds)
        helper = self.control("--child-run", handle, "--", sys.executable, "-c",
                              command, str(base), background=True)
        self.eventually(base.with_suffix(".ready").exists, "ordinary child did not start: " + name)
        return handle, helper, base

    def child_stopped(self, handle, timeout=8):
        return self.eventually(lambda: value if (value := self.child_state(handle))["ordinary_stop"] else None,
                               "ordinary child absence was not established", timeout=timeout)

    def finish_controller(self, owner, expected=0):
        (self.base / "controller.release").touch()
        self.assertEqual(owner.wait(timeout=18), expected)

    def hook_environment(self, mode, env=None):
        directory = self.base / ("hook-" + mode)
        directory.mkdir()
        (directory / "sitecustomize.py").write_text(CHILD_INTERRUPTION)
        for label in ("fork", "receipt", "publication", "signal", "wait"):
            self.releases.append(directory / (label + ".release"))
        result = dict(self.env if env is None else env)
        result.update(TEST_CHILD_HOOK=mode, TEST_CHILD_HOOK_DIR=str(directory),
                      PYTHONPATH=str(directory) + (os.pathsep + result["PYTHONPATH"] if result.get("PYTHONPATH") else ""))
        return directory, result

    def old_helper(self):
        commit = "2f0e2638245bdced61429e7f388d0da812c75c13"
        source = subprocess.run(["git", "show", commit + ":scripts/ci/validation-lock.py"],
                                cwd=ROOT, text=True, capture_output=True, timeout=5)
        if source.returncode:
            self.skipTest("exact old2f helper unavailable in this shallow checkout; run historical compatibility with full history")
        path = self.base / "actual-old2f.py"
        path.write_text(source.stdout)
        return path

    def relay_environment(self, mode):
        directory = self.base / ("relay-" + mode)
        directory.mkdir()
        (directory / "sitecustomize.py").write_text(RELAY_INTERRUPTION)
        self.releases.extend(directory / (name + ".release") for name in ("relay", "frame"))
        env = {**self.env, "TEST_RELAY_MODE": mode, "TEST_RELAY_DIR": str(directory),
               "PYTHONPATH": str(directory) + (os.pathsep + self.env["PYTHONPATH"] if self.env.get("PYTHONPATH") else "")}
        return directory, env

    def recover_gate(self):
        self.run_cli("--reconcile")
        return self.status()["gate"] is None

    def test_receipt_completion_requires_owned_children_and_resources_to_finish(self):
        self.assertNotEqual(self.run_cli("--assert-complete").returncode, 0)
        _native, env = self.docker_fixture()
        owner = self.controller(env=env)
        self.assertEqual(self.control("--assert-complete")["code"], 0)
        handle = self.reserve_child()
        self.assertNotEqual(self.control("--assert-complete")["code"], 0)
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        self.child_stopped(handle)
        self.assertEqual(self.control("--assert-complete")["code"], 0)

        handle, _helper, child = self.child_hold("receipt-child")
        self.assertNotEqual(self.control("--assert-complete")["code"], 0)
        self.release(child)
        self.child_stopped(handle)
        self.assertEqual(self.control("--assert-complete")["code"], 0)

        token = self.status()["gate"]["token"]
        registered = self.control("--resource-register", "container", "receipt-" + token[:12])
        self.assertEqual(registered["code"], 0, registered)
        resource = registered["stdout"].strip()
        self.assertNotEqual(self.control("--assert-complete")["code"], 0)
        cleaned = self.control("--resource-cleanup", resource)
        self.assertEqual(cleaned["code"], 0, cleaned)
        self.assertEqual(self.control("--assert-complete")["code"], 0)
        # An admitted child can check its work without requiring its own
        # still-running caller session to disappear first.
        handle = self.reserve_child()
        checked = self.control("--child-run", handle, "--", "bash", str(ENTRY), "--assert-complete")
        self.assertEqual(checked["code"], 0, checked)
        self.child_stopped(handle)
        # Completion admission is an observation, not release of the live root.
        self.assertIsNotNone(self.status()["gate"])
        self.finish_controller(owner)
        self.assertIsNone(self.status()["gate"])

    def test_reused_member_hint_cannot_signal_foreign_kernel_session_in_v2_or_v3(self):
        root_base = self.base
        for child_mode in (False, True):
            with self.subTest(child_v3=child_mode):
                self.base = root_base / ("v3" if child_mode else "v2")
                self.base.mkdir()
                self.gate = self.base / "validation.lock"
                self.env = {**self.env, "VALIDATION_LOCK_DIR": str(self.gate)}
                hook, env = self.relay_environment("foreign-hint")
                foreign_base = self.base / "foreign"
                self.releases.append(foreign_base.with_suffix(".release"))
                foreign = subprocess.Popen([sys.executable, "-c", HOLD, str(foreign_base)], start_new_session=True)
                self.processes.append(foreign)
                self.eventually(foreign_base.with_suffix(".ready").exists, "owned foreign-session fixture did not start")
                foreign_sid = json.loads(foreign_base.with_suffix(".ready").read_text())["sid"]
                (hook / "foreign.sid").write_text(str(foreign_sid))
                if child_mode:
                    owner = self.controller(env=env)
                    handle, _helper, base = self.child_hold("work", command=TERM_COUNT)
                    scope = self.child_state(handle)["scope"]
                else:
                    base = self.base / "work"
                    self.releases.append(base.with_suffix(".release"))
                    owner = self.launch("--", sys.executable, "-c", TERM_COUNT, str(base), env=env)
                    self.eventually(base.with_suffix(".ready").exists, "v2 command did not start")
                    scope = self.status()["gate"]["scope"]
                (hook / "target.sid").write_text(str(scope["sid"]))
                if child_mode:
                    self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
                    self.eventually(lambda: self.child_state(handle)["stage"] == "unknown", "foreign join did not preserve unknown")
                    self.assertEqual(self.control("--assert-held")["code"], 0)
                else:
                    owner.send_signal(signal.SIGTERM)
                    self.assertNotEqual(owner.wait(timeout=8), 0)
                self.assertEqual((hook / "join.refused").read_text(), str(foreign_sid))
                injected = json.loads((hook / "hint.reached").read_text())
                self.assertEqual(injected["hint"], foreign_sid)
                self.assertIsNone(foreign.poll(), "reused member hint signalled the foreign session")
                self.assertFalse((hook / "deliveries").exists())
                self.assertIsNotNone(self.status()["gate"])
                self.release(base)
                if child_mode:
                    self.finish_controller(owner, expected=1)
                self.eventually(self.recover_gate, "observed terminal scopes did not reconcile")
                self.release(foreign_base)
                self.assertEqual(foreign.wait(timeout=5), 0)

    def test_full_grant_survives_issuer_eof_but_partial_eof_never_delivers(self):
        root_base = self.base
        for mode in ("full-eof", "partial-eof"):
            with self.subTest(frame=mode):
                self.base = root_base / mode
                self.base.mkdir()
                self.gate = self.base / "validation.lock"
                self.env = {**self.env, "VALIDATION_LOCK_DIR": str(self.gate)}
                hook, env = self.relay_environment(mode)
                owner = self.controller(env=env)
                handle, _helper, base = self.child_hold("buffered", command=TERM_COUNT)
                child = self.child_state(handle)
                receipt = self.gate.with_name(self.gate.name + ".queue") / "owners" / child["root"] / child["receipt"]
                (hook / "target.sid").write_text(str(child["scope"]["sid"]))
                self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
                self.assertEqual(owner.wait(timeout=5), -signal.SIGKILL)
                if mode == "full-eof":
                    self.eventually((hook / "frame.reached").exists, "sentinel did not hold the complete buffered grant")
                    self.assertFalse(base.with_suffix(".signals").exists())
                    (hook / "frame.release").touch()
                    self.eventually(base.with_suffix(".signals").exists, "complete grant was discarded after issuer EOF")
                    self.assertEqual(base.with_suffix(".signals").read_text(), "TERM\n")
                    # Delivery is finished, but the TERM-ignoring command is
                    # still live when the shared tail expires after issuer loss.
                    self.eventually(lambda: receipt.exists() and json.loads(receipt.read_text()).get("control_error"),
                                    "completed TERM erased the surviving scope's deadline", timeout=17)
                    overrun = json.loads(receipt.read_text())
                    self.assertIsNone(overrun["exit"])
                    self.assertTrue(any(event.get("at_ns", 0) >= overrun["stop_deadline_ns"]
                                        for event in overrun["control_events"] if "error" in event))
                else:
                    self.eventually(lambda: receipt.exists() and json.loads(receipt.read_text()).get("control_error"),
                                    "partial EOF did not record control uncertainty")
                    self.assertFalse(base.with_suffix(".signals").exists())
                    self.assertFalse((hook / "deliveries").exists())
                (self.base / "controller.release").touch()
                self.assertEqual(self.run_cli("--reconcile").returncode, 1)
                self.assertIsNotNone(self.status()["gate"])
                self.release(base)
                self.eventually(self.recover_gate, "final whole-scope absence was not recognized")
                if mode == "full-eof":
                    final = json.loads(receipt.read_text())
                    self.assertEqual(final["exit"], 0, "native command exit was overwritten by cancellation accounting")
                    self.assertTrue(final["control_error"], "later exit zero erased the overrun")
                    self.assertTrue(any(event.get("command_finished_ns", 0) >= final["stop_deadline_ns"]
                                        for event in final["control_events"]))

    def test_duplicate_expired_unknown_write_and_relay_failure_never_regrant(self):
        root_base = self.base
        for mode in ("duplicate", "expired", "unknown-write", "fork-failure"):
            with self.subTest(failure=mode):
                self.base = root_base / mode
                self.base.mkdir()
                self.gate = self.base / "validation.lock"
                self.env = {**self.env, "VALIDATION_LOCK_DIR": str(self.gate)}
                hook, env = self.relay_environment(mode)
                owner = self.controller(env=env)
                # A separate group is needed to reach an actual relay fork.
                child_program = TERM_COUNT
                extra_base = self.base / "extra"
                if mode == "fork-failure":
                    self.releases.append(extra_base.with_suffix(".release"))
                    child_program = "import subprocess,os,sys; subprocess.Popen([sys.executable,'-c',sys.argv[2],sys.argv[3]],preexec_fn=os.setpgrp)\n" + TERM_COUNT
                    handle = self.reserve_child()
                    base = self.base / "work"
                    self.releases.append(base.with_suffix(".release"))
                    self.control("--child-run", handle, "--", sys.executable, "-c", child_program,
                                 str(base), HOLD, str(extra_base), background=True)
                    self.eventually(extra_base.with_suffix(".ready").exists, "additional group did not start")
                else:
                    handle, _helper, base = self.child_hold("work", command=child_program)
                scope = self.child_state(handle)["scope"]
                (hook / "target.sid").write_text(str(scope["sid"]))
                self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
                self.eventually(lambda: self.child_state(handle)["stage"] == "unknown", "control failure did not retain unknown")
                grants = (hook / "grants").read_text()
                self.assertEqual(len(grants.splitlines()), 1)
                self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
                self.assertFalse(self.child_state(handle)["ordinary_stop"])
                self.assertEqual((hook / "grants").read_text(), grants, "ambiguous/retired grant was replayed")
                if mode in {"duplicate", "unknown-write"}:
                    self.eventually(base.with_suffix(".signals").exists, "first granted request never arrived")
                    self.assertEqual(base.with_suffix(".signals").read_text(), "TERM\n")
                else:
                    self.assertFalse(base.with_suffix(".signals").exists())
                self.release(base)
                if mode == "fork-failure":
                    self.release(extra_base)
                # Unknown writes may resolve by fresh positive scope absence;
                # recorded protocol/relay failures keep the run failed.
                if mode == "unknown-write":
                    self.child_stopped(handle)
                    self.finish_controller(owner)
                else:
                    self.finish_controller(owner, expected=1)
                    self.eventually(self.recover_gate, "failed scope could not reconcile after actual absence")

    def test_joined_relay_outlives_issuer_retirement_until_positive_scope_absence(self):
        hook, env = self.relay_environment("joined-relay")
        base = self.base / "root-work"
        group_base = self.base / "group-work"
        self.releases.extend((base.with_suffix(".release"), group_base.with_suffix(".release")))
        program = "import subprocess,os,sys; subprocess.Popen([sys.executable,'-c',sys.argv[2],sys.argv[3]],preexec_fn=os.setpgrp)\n" + TERM_COUNT
        owner = self.launch("--", sys.executable, "-c", program, str(base), HOLD, str(group_base), env=env)
        self.eventually(group_base.with_suffix(".ready").exists, "owned additional group did not start")
        scope = self.status()["gate"]["scope"]
        (hook / "target.sid").write_text(str(scope["sid"]))
        owner.send_signal(signal.SIGTERM)
        self.eventually((hook / "relay.reached").exists, "same-session relay did not join")
        relay_pid = int((hook / "relay.reached").read_text())
        group = json.loads(group_base.with_suffix(".ready").read_text())
        self.assertEqual((hook / "joined.group").read_text(), str(group["pid"]))
        self.release(group_base)
        self.release(base)
        self.eventually(lambda: self.process_terminal(group["pid"]), "original group member did not disappear")
        # Root's 10+5 budget expires while an already-admitted relay is paused.
        # Its closed writer cannot revoke this performer or prove final absence.
        self.assertEqual(owner.wait(timeout=17), 128 + signal.SIGTERM)
        self.assertFalse(self.process_terminal(relay_pid))
        self.assertEqual(self.run_cli("--reconcile").returncode, 1)
        self.assertIsNotNone(self.status()["gate"])
        self.assertFalse((hook / "deliveries").exists())
        (hook / "relay.release").touch()
        self.eventually(self.recover_gate, "late relay completion did not permit fresh whole-scope absence")
        deliveries = (hook / "deliveries").read_text().splitlines()
        self.assertEqual(deliveries, [f"{relay_pid}:{group['pid']}:{signal.SIGTERM}"])
        self.assertTrue(self.process_terminal(relay_pid))

    def test_v2_refuses_all_child_operations_before_effect(self):
        owner = self.controller(v3=False)
        initial = self.status()["gate"]
        self.assertEqual(initial["protocol"], 2)
        fake = ".".join([initial["token"], "0" * 32, *map(str, initial["inode"])])
        operations = [
            ("--child-reserve", "--cancel-at-monotonic-ns", str(time.monotonic_ns() + 10**10)),
            ("--child-run", fake, "--", sys.executable, "-c", MARK, str(self.base / "forbidden")),
            ("--child-cancel", fake), ("--child-status", fake),
            ("--with-child-scopes", "--", sys.executable, "-c", MARK, str(self.base / "forbidden")),
        ]
        for operation in operations:
            response = self.control(*operation)
            self.assertEqual(response["code"], 1, response)
        current = self.status()["gate"]
        self.assertEqual(current["inode"], initial["inode"])
        self.assertNotIn("children", current)
        self.assertFalse((self.base / "forbidden").exists())
        self.finish_controller(owner)

    def test_reserved_cancel_is_idempotent_and_never_launches(self):
        owner = self.controller()
        handle = self.reserve_child()
        first = self.control("--child-cancel", handle)
        second = self.control("--child-cancel", handle)
        self.assertEqual(first["code"], 0, first)
        self.assertEqual(second["code"], 0, second)
        self.assertEqual(json.loads(first["stdout"])["cancel_at_ns"], json.loads(second["stdout"])["cancel_at_ns"])
        result = self.control("--child-run", handle, "--", sys.executable, "-c", MARK,
                              str(self.base / "forbidden"))
        self.assertEqual(result["code"], 1, result)
        state = self.child_stopped(handle)
        self.assertFalse(state["launch_may_have_occurred"])
        self.assertTrue(state["no_command_effect"])
        self.assertIsNone(state["command_exit"])
        self.assertFalse((self.base / "forbidden").exists())
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.finish_controller(owner)

    def test_child_authentication_and_cancel_cover_descendants_without_parent_or_sibling(self):
        owner = self.controller()
        sibling, _sibling_helper, sibling_base = self.child_hold("sibling")
        handle = self.reserve_child()
        forbidden = str(self.base / "forbidden")
        script = """
import json, os, pathlib, subprocess, sys, time
entry, sibling, marker, ready = sys.argv[1:]
for args in (['--child-cancel', sibling], ['--child-reserve', '--cancel-at-monotonic-ns', str(time.monotonic_ns()+10**10)],
             ['--with-child-scopes', '--', sys.executable, '-c', 'pass']):
    assert subprocess.run(['bash', entry, *args], capture_output=True).returncode == 1
assert subprocess.run(['bash', entry, '--', sys.executable, '-c', 'pass']).returncode == 0
pipeline = subprocess.Popen(['bash', '-c', 'sleep 20 | cat'], preexec_fn=os.setpgrp)
pathlib.Path(ready).write_text(json.dumps({'pid': pipeline.pid, 'sid': os.getsid(0)}))
"""
        helper = self.control("--child-run", handle, "--", sys.executable, "-c", script,
                              str(ENTRY), sibling, forbidden, str(self.base / "descendant.ready"), background=True)
        self.eventually((self.base / "descendant.ready").exists, "child pipeline was not admitted")
        descendant = json.loads((self.base / "descendant.ready").read_text())
        inherited = {**self.env, **json.loads((self.base / "controller.env").read_text())}
        inherited.pop("VALIDATION_LOCK_DIR", None)
        self.assertEqual(self.run_cli("--child-cancel", handle, env=inherited).returncode, 1)
        self.assertEqual(self.control("--child-status", handle + "stale")["code"], 1)
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        stopped = self.child_stopped(handle)
        self.assertEqual(stopped["command_exit"], 0)
        self.assertTrue(self.process_terminal(descendant["pid"]))
        self.assertFalse(self.child_state(sibling)["ordinary_stop"])
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.assertFalse((self.base / "forbidden").exists())
        self.eventually((self.base / (helper["name"] + ".done")).exists, "child helper did not finish")
        self.release(sibling_base)
        self.child_stopped(sibling)
        self.finish_controller(owner)

    def test_cutoff_escalates_term_ignoring_work_inside_one_tail(self):
        owner = self.controller()
        resistant = HOLD.replace("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))",
                                 "signal.signal(signal.SIGTERM, lambda *_: None)")
        extra = self.base / "resistant-group"
        self.releases.append(extra.with_suffix(".release"))
        program = ("import os,subprocess,sys; subprocess.Popen([sys.executable,'-c',"
                   + repr(resistant) + "," + repr(str(extra)) + "],preexec_fn=os.setpgrp)\n" + resistant)
        handle, _helper, _base = self.child_hold("resistant", seconds=2, command=program)
        self.eventually(extra.with_suffix(".ready").exists, "TERM-ignoring additional group did not start")
        member = json.loads(extra.with_suffix(".ready").read_text())
        cutoff = self.child_state(handle)["cutoff_ns"]
        state = self.child_stopped(handle, timeout=17)
        self.assertTrue(state["retired"])
        self.assertEqual(state["cancel_at_ns"], cutoff)
        self.assertLess(time.monotonic_ns() - cutoff, 16_000_000_000)
        self.assertTrue(self.process_terminal(member["pid"]), "KILL relay did not terminate the additional group")
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.finish_controller(owner)

    def test_normal_child_preserves_streams_exit_and_one_use(self):
        owner = self.controller()
        handle = self.reserve_child()
        result = self.control("--child-run", handle, "--", sys.executable, "-c",
                              "import sys; print('native-out'); print('native-err',file=sys.stderr); raise SystemExit(23)")
        self.assertEqual(result["code"], 23, result)
        self.assertEqual(result["stdout"], "native-out\n")
        self.assertEqual(result["stderr"], "native-err\n")
        state = self.child_stopped(handle)
        self.assertEqual(state["command_exit"], 23)
        self.assertIsNone(state["cancel_at_ns"])
        self.assertEqual(self.control("--child-run", handle, "--", sys.executable, "-c", "pass")["code"], 1)
        self.finish_controller(owner)

    def test_prepublication_helper_loss_requires_a_valid_identity_receipt(self):
        root_base = self.base
        for mode, can_observe in (("fork-before-receipt", False), ("receipt-before-ready", True),
                                  ("ready-before-publication", True), ("bad-receipt", False)):
            with self.subTest(boundary=mode):
                self.base = root_base / mode
                self.base.mkdir()
                self.gate = self.base / "validation.lock"
                self.env = {**self.env, "VALIDATION_LOCK_DIR": str(self.gate)}
                hook, env = self.hook_environment(mode)
                owner = self.controller(env=env)
                handle = self.reserve_child()
                helper = self.control("--child-run", handle, "--", sys.executable, "-c", MARK,
                                      str(self.base / "forbidden"), background=True)
                label = {"fork-before-receipt": "fork", "receipt-before-ready": "receipt",
                         "ready-before-publication": "publication", "bad-receipt": "receipt"}[mode]
                self.eventually((hook / (label + ".reached")).exists, "native fault boundary was not reached")
                paused = int((hook / (label + ".reached")).read_text())
                os.kill(helper["pid"], signal.SIGKILL)
                state = self.eventually(lambda: value if (value := self.child_state(handle))["retired"] else None,
                                        "helper loss did not retire guardian capability")
                self.assertTrue(state["no_command_effect"])
                if mode != "ready-before-publication":
                    self.assertFalse(state["ordinary_stop"], "a paused or unidentified process is not absent")
                self.assertFalse((self.base / "forbidden").exists())
                if mode == "fork-before-receipt":
                    os.kill(paused, signal.SIGKILL)
                else:
                    (hook / (label + ".release")).touch()
                self.eventually(lambda: self.process_terminal(paused), "faulted process did not terminate")
                if can_observe:
                    stopped = self.child_stopped(handle)
                    self.assertFalse(stopped["wait_completed"], "lost helper cannot supply a wait result")
                    self.assertEqual(stopped["stop_evidence"], "recovery-kernel-absence")
                    self.finish_controller(owner)
                else:
                    state = self.child_state(handle)
                    self.assertFalse(state["ordinary_stop"])
                    self.assertEqual(state["stage"], "unknown")
                    self.assertEqual(self.control("--assert-held")["code"], 0)
                    self.finish_controller(owner, expected=1)
                    self.assertEqual(self.run_cli("--reconcile").returncode, 1)
                    result = self.run_cli(*self.marker_command("next"),
                                          env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".1"})
                    self.assertEqual(result.returncode, 75, result.stderr)
                self.assertFalse((self.base / "forbidden").exists())

    def test_guardian_pin_survives_helper_death_and_concurrent_command_exit(self):
        hook, env = self.hook_environment("signal-pin")
        owner = self.controller(env=env)
        handle, helper, base = self.child_hold("pin")
        child = self.child_state(handle)
        (hook / "signal.target").write_text(str(child["scope"]["sid"]))
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        self.eventually((hook / "signal.reached").exists, "guardian did not reach checked signal")
        os.kill(helper["pid"], signal.SIGKILL)
        self.release(base)
        scope = child["scope"]
        owner_dir = self.gate.with_name(self.gate.name + ".queue") / "owners" / child["root"]
        self.eventually((owner_dir / child["receipt"]).exists, "ordinary command did not complete during guardian suspension")
        self.assertFalse(self.process_terminal(scope["pid"]), "helper death retired the live guardian pin")
        self.assertEqual(os.getsid(scope["pid"]), scope["sid"])
        (hook / "signal.release").touch()
        stopped = self.child_stopped(handle)
        self.assertFalse(stopped["wait_completed"])
        self.assertEqual(stopped["command_exit"], 0)
        before = (hook / "signals").read_text()
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        self.assertTrue(self.child_state(handle)["retired"])
        self.assertEqual((hook / "signals").read_text(), before, "retired child was signalled again")
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.finish_controller(owner)

    def test_guardian_death_before_or_after_launch_never_restores_signal_authority(self):
        root_base = self.base
        for mode in ("guardian-before-L", "guardian-after-L"):
            with self.subTest(boundary=mode):
                self.base = root_base / mode
                self.base.mkdir()
                self.gate = self.base / "validation.lock"
                self.env = {**self.env, "VALIDATION_LOCK_DIR": str(self.gate)}
                hook, env = self.hook_environment(mode)
                owner = self.controller(env=env)
                handle = self.reserve_child()
                initial = self.status()["gate"]
                (hook / "launch.arm").touch()
                base = self.base / "orphan"
                self.releases.append(base.with_suffix(".release"))
                self.control("--child-run", handle, "--", sys.executable, "-c", HOLD, str(base), background=True)
                self.eventually((hook / "launch.reached").exists, "guardian launch boundary not reached")
                self.assertEqual(owner.wait(timeout=5), -signal.SIGKILL)
                if mode == "guardian-after-L":
                    self.eventually(base.with_suffix(".ready").exists, "authorized child did not start")
                else:
                    self.assertFalse(base.with_suffix(".ready").exists())
                current = self.status()["gate"]
                self.assertEqual(current["inode"], initial["inode"])
                self.assertEqual(current["token"], initial["token"])
                self.assertTrue(current["children"][0]["launch_may_have_occurred"])
                self.assertEqual(self.control("--child-cancel", handle)["code"], 1)
                result = self.run_cli(*self.marker_command("next"),
                                      env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
                self.assertEqual(result.returncode, 75, result.stderr)
                self.assertFalse((self.base / "next").exists())
                self.release(base)
                (self.base / "controller.release").touch()
                def recovered():
                    self.run_cli("--reconcile")
                    return self.status()["gate"] is None
                self.eventually(recovered, "actual orphan absence did not permit observation-only recovery")
                self.assertFalse((hook / "signals").exists())

    def test_only_guardian_holds_fifo_writer_while_helper_is_suspended(self):
        hook, env = self.hook_environment("helper-wait")
        owner = self.controller(env=env)
        handle, _helper, base = self.child_hold("only-writer")
        self.eventually((hook / "wait.reached").exists, "helper did not reach direct-child wait")
        child = self.child_state(handle)
        scope = child["scope"]
        owner.kill()
        owner.wait(timeout=5)
        self.release(base)
        self.eventually(lambda: self.process_terminal(scope["pid"]),
                        "suspended helper or executable inherited the guardian FIFO writer")
        self.assertFalse((hook / "wait.release").exists())
        (hook / "wait.release").touch()
        (self.base / "controller.release").touch()
        def recovered():
            self.run_cli("--reconcile")
            return self.status()["gate"] is None
        self.eventually(recovered, "unwaited child did not reconcile after helper resumed")

    def test_actual_old2f_refuses_live_and_abandoned_v3_without_generation_change(self):
        old = self.old_helper()
        owner = self.controller()
        handle, _helper, base = self.child_hold("old-reader-orphan")
        initial = self.status()["gate"]
        inherited = {**self.env, **json.loads((self.base / "controller.env").read_text())}
        inherited.pop("VALIDATION_LOCK_DIR", None)
        for abandoned in (False, True):
            with self.subTest(guardian_dead=abandoned):
                if abandoned:
                    owner.kill()
                    owner.wait(timeout=5)
                for args in (("--status",), ("--reconcile",),
                             ("--", sys.executable, "-c", MARK, str(self.base / "forbidden")),
                             ("--assert-held",)):
                    result = subprocess.run([sys.executable, str(old), *args], text=True, capture_output=True,
                                            env=inherited if args[0] == "--assert-held" else self.env, timeout=6)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("gate protocol/domain unavailable", result.stderr)
                current = self.status()["gate"]
                self.assertEqual((current["token"], current["inode"], current["protocol"]),
                                 (initial["token"], initial["inode"], 3))
                self.assertFalse((self.base / "forbidden").exists())
                self.assertFalse(self.process_terminal(json.loads(base.with_suffix(".ready").read_text())["pid"]))
        self.release(base)
        (self.base / "controller.release").touch()
        def recovered():
            self.run_cli("--reconcile")
            return self.status()["gate"] is None
        self.eventually(recovered, "retained v3 helper could not recover the v3 generation")

    def test_lost_anchor_never_signals_remaining_session_or_restores_capability(self):
        hook, env = self.hook_environment("signal-log")
        owner = self.controller(env=env)
        handle, _helper, base = self.child_hold("lost-anchor")
        child = self.child_state(handle)
        (hook / "signal.target").write_text(str(child["scope"]["sid"]))
        os.kill(child["scope"]["pid"], signal.SIGKILL)
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        self.eventually(lambda: self.child_state(handle)["stage"] == "unknown", "lost anchor was not quarantined")
        self.assertFalse(self.child_state(handle)["ordinary_stop"])
        ordinary = json.loads(base.with_suffix(".ready").read_text())
        self.assertFalse(self.process_terminal(ordinary["pid"]))
        self.assertFalse((hook / "signals").exists(), "unanchored numeric session was signalled")
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.release(base)
        self.child_stopped(handle)
        self.assertFalse((hook / "signals").exists())
        self.finish_controller(owner)

    def test_replayed_child_identity_cannot_target_another_live_scope(self):
        hook, env = self.hook_environment("signal-log")
        owner = self.controller(env=env)
        target, _target_helper, target_base = self.child_hold("record-target")
        sibling, _sibling_helper, sibling_base = self.child_hold("record-sibling")
        target_record = self.child_state(target)
        sibling_record = self.child_state(sibling)
        directory = self.gate.with_name(self.gate.name + ".queue")
        statefile = directory / "owners" / target_record["root"] / "state.json"
        (hook / "signal.target").write_text(str(sibling_record["scope"]["sid"]))
        # Replay a real but foreign live identity under the original nonce. The
        # independently published sentinel receipt must reject the contradiction.
        with (directory / "mutex").open("r+") as mutex:
            fcntl.flock(mutex, fcntl.LOCK_EX)
            state = json.loads(statefile.read_text())
            for item in state["children"]:
                if item["nonce"] == target_record["nonce"]:
                    item["scope"] = sibling_record["scope"]
            pending = statefile.with_suffix(".test-pending")
            pending.write_text(json.dumps(state))
            pending.replace(statefile)
        self.assertEqual(self.control("--child-cancel", target)["code"], 0)
        self.eventually(lambda: self.child_state(target)["stage"] == "unknown", "foreign identity was not refused")
        self.assertFalse((hook / "signals").exists(), "replayed identity authorized a sibling signal")
        self.assertFalse(self.child_state(target)["ordinary_stop"])
        self.release(target_base)
        self.release(sibling_base)
        self.child_stopped(sibling)
        self.assertFalse(self.child_state(target)["ordinary_stop"], "foreign absence was substituted for original scope proof")
        self.finish_controller(owner, expected=1)
        self.assertEqual(self.run_cli("--reconcile").returncode, 1)

    def test_unknown_gate_capability_refuses_without_mutable_downgrade(self):
        owner = self.controller()
        initial = self.status()["gate"]
        original = self.gate.read_bytes()
        owner.send_signal(signal.SIGSTOP)
        try:
            for field, value in (("protocol", 999), ("capability", "unknown-child-capability")):
                gate = json.loads(original)
                gate[field] = value
                self.gate.write_text(json.dumps(gate))
                result = self.run_cli("--status")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(self.run_cli("--reconcile").returncode, 1)
                current = self.gate.stat()
                self.assertEqual([current.st_dev, current.st_ino], initial["inode"])
                self.gate.write_bytes(original)
        finally:
            self.gate.write_bytes(original)
            owner.send_signal(signal.SIGCONT)
        self.assertEqual(self.status()["gate"]["protocol"], 3)
        self.finish_controller(owner)

    def test_child_cancellation_preserves_delayed_native_create_and_parent_cleanup(self):
        native, env = self.docker_fixture()
        env["TEST_DOCKER_DELAY_CREATE"] = "1"
        self.releases.append(native / "create.release")
        owner = self.controller(env=env)
        handle = self.reserve_child()
        self.control("--child-run", handle, "--", "bash", str(ENTRY), "--container-run", "--",
                     "docker", "run", "-d", "fixture", background=True)
        self.eventually((native / "create.ready").exists, "child native create did not begin")
        resource = self.status()["gate"]["resources"][0]
        self.assertEqual(resource["ordinary_scope"], handle.split(".")[1])
        observed = json.loads((native / "create.ready").read_text())
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        cleanup = self.control("--resource-cleanup", resource["token"], background=True)
        self.eventually((native / "create.absence-read").exists, "parent cleanup did not observe pre-create absence")
        self.child_stopped(handle, timeout=17)
        self.assertFalse(self.process_terminal(observed["pid"]))
        self.assertFalse((native / "create.signal").exists())
        self.assertTrue(self.child_state(handle)["unresolved_resources"])
        self.assertEqual(self.control("--assert-held")["code"], 0)
        (native / "create.release").touch()
        done = self.base / (cleanup["name"] + ".done")
        self.eventually(done.exists, "parent cleanup did not receive native terminal progress", timeout=10)
        self.assertEqual(done.read_text(), "0")
        self.assertFalse((native / "container.json").exists())
        self.assertEqual(self.child_state(handle)["unresolved_resources"], [])
        self.finish_controller(owner)

    def test_lost_child_native_observer_holds_root_after_ordinary_stop(self):
        native, env = self.docker_fixture()
        self.releases.append(native / "observer.release")
        owner = self.controller(env=env)
        handle = self.reserve_child()
        script = """
import subprocess, sys
entry = sys.argv[1]
resource = subprocess.check_output(['bash', entry, '--builder-prepare'], text=True).strip()
name = subprocess.check_output(['bash', entry, '--builder-name', resource], text=True).strip()
raise SystemExit(subprocess.call(['bash', entry, '--resource-run', resource, '--',
                                'docker', 'buildx', 'build', '--builder', name, '--load', '.']))
"""
        self.control("--child-run", handle, "--", sys.executable, "-c", script, str(ENTRY), background=True)
        self.eventually((native / "observer.ready").exists, "child BuildKit observer did not launch")
        resource = self.status()["gate"]["resources"][0]
        observer = resource["observers"][0]["scope"]
        self.assertEqual(self.control("--child-cancel", handle)["code"], 0)
        self.child_stopped(handle, timeout=17)
        self.assertFalse((native / "observer.signal").exists())
        os.kill(observer["pid"], signal.SIGKILL)
        (native / "observer.release").touch()
        self.eventually((native / "native-terminal").exists, "orphan native CLI did not finish")
        cleanup = self.control("--resource-cleanup", resource["token"], timeout=12)
        self.assertEqual(cleanup["code"], 1, cleanup)
        self.assertTrue(self.child_state(handle)["unresolved_resources"])
        self.assertEqual(self.control("--assert-held")["code"], 0)
        self.finish_controller(owner, expected=1)
        self.assertEqual(self.run_cli("--reconcile", env=env).returncode, 1)
        result = self.run_cli(*self.marker_command("next"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "next").exists())

    def start_build_observer(self, resist_cancellation=False, check_only=False):
        native, env = self.docker_fixture()
        self.releases.append(native / "observer.release")
        self.releases.append(native / "ordinary.release")
        script = """
import os, pathlib, signal, subprocess, sys, time
env = os.environ.copy()
env.pop('VALIDATION_LOCK_DIR', None)
entry, native, resistant, operation_mode = sys.argv[1:]
resource = subprocess.check_output(['bash', entry, '--builder-prepare'], env=env, text=True).strip()
name = subprocess.check_output(['bash', entry, '--builder-name', resource], env=env, text=True).strip()
build_arguments = ['--check', '-f', 'build/docker/Dockerfile', '.'] if operation_mode == '--check' else ['--load', '.']
operation = subprocess.Popen(['bash', entry, '--resource-run', resource, '--',
    'docker', 'buildx', 'build', '--builder', name, *build_arguments], env=env)
if resistant == 'yes':
    signal.signal(signal.SIGTERM, lambda *_: None)
    deadline = time.monotonic() + 30
    while not (pathlib.Path(native) / 'ordinary.release').exists() and time.monotonic() < deadline:
        time.sleep(.02)
raise SystemExit(operation.wait())
"""
        owner = self.launch("--", sys.executable, "-c", script, str(ENTRY), str(native),
                            "yes" if resist_cancellation else "no",
                            "--check" if check_only else "--load", env=env)
        self.eventually((native / "observer.ready").exists, "native observer did not launch")
        state = self.status()
        resource = state["gate"]["resources"][0]
        observer = resource["observers"][0]["scope"]
        observed = json.loads((native / "observer.ready").read_text())
        self.assertEqual(observer["sid"], observed["sid"])
        self.assertNotEqual(observer["sid"], state["gate"]["scope"]["sid"])
        return owner, native, env, observer

    def test_fifo_registration_and_dead_waiter_do_not_allow_overtaking(self):
        owner, base = self.hold("owner")
        order = self.base / "order"
        command = "import pathlib,sys; p=pathlib.Path(sys.argv[1]); " \
                  "f=p.open('a'); f.write(sys.argv[2]); f.close()"
        dead = self.launch("--", sys.executable, "-c", command, str(order), "dead")
        self.eventually(lambda: len(self.status()["tickets"]) == 1, "dead ticket missing")
        followers = []
        for count, name in enumerate(("first", "second"), start=2):
            followers.append(self.launch("--", sys.executable, "-c", command,
                                         str(order), name + "\n"))
            state = self.eventually(lambda: (s if len(s["tickets"]) == count else None)
                                    if (s := self.status()) else None,
                                    "follower registration missing")
            sequences = [ticket["sequence"] for ticket in state["tickets"]]
            self.assertEqual(sequences, sorted(set(sequences)))
        dead.kill()
        dead.wait(timeout=3)
        self.assertFalse(order.exists())
        self.release(base)
        self.assertEqual(owner.wait(timeout=8), 0)
        for follower in followers:
            self.assertEqual(follower.wait(timeout=8), 0)
        self.assertEqual(order.read_text(), "first\nsecond\n")

    def test_timeout_and_invalid_timeout_never_launch(self):
        _, base = self.hold("busy")
        for value, expected in (("0", 75), (".08", 75), ("-1", 2),
                                ("nan", 2), ("inf", 2), ("no", 2)):
            with self.subTest(timeout=value):
                result = self.run_cli(*self.marker_command("forbidden"),
                                      env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": value})
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertFalse((self.base / "forbidden").exists())
        self.release(base)

    def test_waiter_cancellation_departs_without_launch(self):
        owner, base = self.hold("busy")
        waiter = self.launch(*self.marker_command("forbidden"))
        self.eventually(lambda: self.status()["tickets"], "waiter did not register")
        waiter.send_signal(signal.SIGTERM)
        self.assertEqual(waiter.wait(timeout=5), 128 + signal.SIGTERM)
        self.release(base)
        self.assertEqual(owner.wait(timeout=8), 0)
        self.assertFalse((self.base / "forbidden").exists())
        self.assertEqual(self.status()["tickets"], [])

    def test_publication_crash_and_prelaunch_signal_never_execute_command(self):
        instrumentation = self.base / "instrumentation"
        instrumentation.mkdir()
        (instrumentation / "sitecustomize.py").write_text(LINK_INTERRUPTION)
        pythonpath = str(instrumentation)
        if self.env.get("PYTHONPATH"):
            pythonpath += os.pathsep + self.env["PYTHONPATH"]
        for mode in ("kill-before-link", "kill-after-link", "term-after-link"):
            with self.subTest(interruption=mode):
                reached = self.base / (mode + ".reached")
                marker = mode + ".forbidden"
                owner = self.launch(*self.marker_command(marker), env={
                    **self.env, "PYTHONPATH": pythonpath,
                    "TEST_LINK_GATE": str(self.gate),
                    "TEST_LINK_INTERRUPTION": mode, "TEST_LINK_REACHED": str(reached),
                })
                expected = 128 + signal.SIGTERM if mode == "term-after-link" else -signal.SIGKILL
                self.assertEqual(owner.wait(timeout=8), expected)
                self.assertEqual(reached.read_text(), mode, "publication hook was not reached")
                self.assertFalse((self.base / marker).exists())
                if mode == "kill-after-link":
                    self.assertTrue(self.gate.is_file(), "crash lost the published exclusion gate")

                def reconciled():
                    self.run_cli("--reconcile")
                    state = self.status()
                    return state["gate"] is None and not state["tickets"]

                self.eventually(reconciled, "unlaunched predecessor did not reconcile")
                follower = mode + ".follower"
                result = self.run_cli(*self.marker_command(follower))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((self.base / follower).exists())
                self.assertFalse((self.base / marker).exists(), "prelaunch interruption allowed an effect")

    def test_child_status_and_requested_interruption_are_preserved(self):
        result = self.run_cli("--", sys.executable, "-c", "raise SystemExit(23)")
        self.assertEqual(result.returncode, 23, result.stderr)
        owner, _ = self.hold("signal")
        owner.send_signal(signal.SIGTERM)
        self.assertEqual(owner.wait(timeout=16), 128 + signal.SIGTERM)
        self.assertIsNone(self.status()["gate"])

    def test_live_nested_calls_authenticate_across_changed_cwd(self):
        # Projection children deliberately have another cwd/Git common directory.
        script = """
import os, pathlib, subprocess, sys
env = os.environ.copy()
env.pop('VALIDATION_LOCK_DIR', None)
entry, cwd, marker = sys.argv[1:]
assert subprocess.run(['bash', entry, '--assert-held'], env=env, cwd=cwd).returncode == 0
result = subprocess.run(['bash', entry, '--', sys.executable, '-c',
    'import pathlib,sys; pathlib.Path(sys.argv[1]).touch()', marker], env=env, cwd=cwd)
raise SystemExit(result.returncode)
"""
        result = self.run_cli("--", sys.executable, "-c", script, str(ENTRY),
                              str(self.base), str(self.base / "nested"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.base / "nested").exists())
        self.assertIsNone(self.status()["gate"])

    def test_boolean_foreign_session_and_stale_token_cannot_bypass(self):
        base = self.base / "credentials"
        self.releases.append(base.with_suffix(".release"))
        save = "import os,pathlib,json; pathlib.Path(os.sys.argv[1]+'.env').write_text(" \
               "json.dumps({k:v for k,v in os.environ.items() if k.startswith('VALIDATION_LOCK_')}));\n"
        owner = self.launch("--", sys.executable, "-c", save + HOLD, str(base))
        self.eventually(base.with_suffix(".ready").exists, "owner did not start")
        inherited = json.loads(base.with_suffix(".env").read_text())
        foreign = {**self.env, **inherited, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".08"}
        foreign.pop("VALIDATION_LOCK_DIR", None)
        self.assertNotEqual(self.run_cli("--assert-held", env=foreign).returncode, 0)
        for env in (foreign, {**self.env, "VALIDATION_LOCK_HELD": "1",
                              "VALIDATION_LOCK_TIMEOUT_SECONDS": ".08"}):
            result = self.run_cli(*self.marker_command("forbidden"), env=env)
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertFalse((self.base / "forbidden").exists())
        self.release(base)
        self.assertEqual(owner.wait(timeout=8), 0)
        self.assertNotEqual(self.run_cli("--assert-held", env=foreign).returncode, 0)

    def test_explicit_domain_ignores_inherited_owner(self):
        other_gate = self.base / "other.lock"
        script = """
import os, subprocess, sys
env = dict(os.environ, VALIDATION_LOCK_DIR=sys.argv[2])
raise SystemExit(subprocess.run(['bash', sys.argv[1], '--', sys.executable, '-c',
    'import pathlib,sys; assert pathlib.Path(sys.argv[1]).is_file(); pathlib.Path(sys.argv[2]).touch()',
    sys.argv[2], sys.argv[3]], env=env).returncode)
"""
        result = self.run_cli("--", sys.executable, "-c", script, str(ENTRY),
                              str(other_gate), str(self.base / "isolated"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.base / "isolated").exists())
        self.assertFalse(other_gate.exists())

    def test_live_legacy_directory_blocks_and_diagnostics_hide_owner_contents(self):
        self.gate.mkdir()
        secret = "legacy-secret-do-not-log"
        (self.gate / "owner").write_text(f"pid={os.getpid()}\ncommand={secret}\n")
        result = self.run_cli(*self.marker_command("forbidden"),
                              env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".08"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertNotIn(secret, result.stdout + result.stderr)
        self.assertNotIn(secret, json.dumps(self.status()))
        self.assertFalse((self.base / "forbidden").exists())
        self.assertTrue(self.gate.is_dir())

        # PID-only legacy metadata cannot prove a dead owner's descendants gone.
        previous = subprocess.Popen([sys.executable, "-c", "pass"])
        previous.wait(timeout=3)
        (self.gate / "owner").write_text(f"pid={previous.pid}\n")
        result = self.run_cli(*self.marker_command("forbidden"),
                              env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".08"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "forbidden").exists())
        self.assertTrue(self.gate.is_dir())

    def test_old_client_cleanup_cannot_remove_new_regular_gate(self):
        _, base = self.hold("new")
        self.assertTrue(self.gate.is_file())
        # The actual legacy removal operations, independently of new internals.
        with self.assertRaises(OSError):
            (self.gate / "owner").unlink()
        with self.assertRaises(OSError):
            self.gate.rmdir()
        with self.assertRaises(FileExistsError):
            self.gate.mkdir()
        self.assertIsNotNone(self.status()["gate"])
        self.release(base)

    def test_direct_child_exit_does_not_release_surviving_descendant(self):
        base = self.base / "descendant"
        self.releases.append(base.with_suffix(".release"))
        script = "import subprocess,sys; subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2]])"
        owner = self.launch("--", sys.executable, "-c", script, HOLD, str(base))
        self.eventually(base.with_suffix(".ready").exists, "descendant did not start")
        waiter = self.launch(*self.marker_command("next"))
        self.eventually(lambda: self.status()["tickets"], "waiter did not register")
        self.assertIsNone(owner.poll(), "direct-child exit prematurely returned")
        self.assertFalse((self.base / "next").exists())
        self.release(base)
        self.assertEqual(owner.wait(timeout=8), 0)
        self.assertEqual(waiter.wait(timeout=8), 0)
        self.assertTrue((self.base / "next").exists())

    def test_guardian_death_quarantines_live_child_then_reconciles(self):
        owner, base = self.hold("orphan")
        token = self.status()["gate"]["token"]
        owner.kill()
        owner.wait(timeout=3)
        self.run_cli("--reconcile")
        state = self.status()
        self.assertEqual(state["gate"]["token"], token)
        self.assertTrue(state["gate"]["quarantine"])
        result = self.run_cli(*self.marker_command("next"),
                              env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "next").exists())
        self.release(base)
        def reconciled():
            self.run_cli("--reconcile")
            return self.status()["gate"] is None
        self.eventually(reconciled, "terminal orphan scope did not reconcile")
        self.assertEqual(self.run_cli(*self.marker_command("next")).returncode, 0)

    def test_daemon_outage_holds_resource_until_restored_terminal_readback(self):
        native, env = self.docker_fixture()
        base = self.base / "container-owner"
        self.releases.append(base.with_suffix(".release"))
        script = """
import os, subprocess, sys
env = os.environ.copy()
env.pop('VALIDATION_LOCK_DIR', None)
name = 'fixture-' + env['VALIDATION_LOCK_TOKEN'][:12]
resource = subprocess.check_output(['bash', sys.argv[2], '--resource-register', 'container', name],
                                   env=env, text=True).strip()
subprocess.run(['docker', 'run', '-d', '--name', name, '--label',
    'dev.rust-service.validation-owner=' + env['VALIDATION_LOCK_TOKEN'], 'fixture'], check=True)
""" + HOLD
        owner = self.launch("--", sys.executable, "-c", script, str(base), str(ENTRY), env=env)
        self.eventually(base.with_suffix(".ready").exists, "registered container did not launch")
        self.assertTrue(json.loads((native / "container.json").read_text())["State"]["Running"])
        unavailable = native / "unavailable"
        unavailable.touch()
        owner.kill()
        owner.wait(timeout=3)
        self.release(base)
        pid = json.loads(base.with_suffix(".ready").read_text())["pid"]
        def child_absent():
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            return False
        self.eventually(child_absent, "ordinary child did not exit before daemon reconciliation")
        self.run_cli("--reconcile", env=env)
        state = self.status()
        self.assertIsNotNone(state["gate"])
        self.assertTrue(state["gate"]["quarantine"])
        self.assertTrue(state["gate"]["resources"])
        result = self.run_cli(*self.marker_command("next"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "next").exists())
        unavailable.unlink()
        def reconciled():
            self.run_cli("--reconcile", env=env)
            return self.status()["gate"] is None
        self.eventually(reconciled, "restored daemon did not allow terminal reconciliation")
        statefile = native / "container.json"
        if statefile.exists():
            self.assertFalse(json.loads(statefile.read_text())["State"]["Running"])
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_cancellation_stops_owned_builder_but_preserves_terminal_observer(self):
        owner, native, env, observer = self.start_build_observer(resist_cancellation=True)
        owner.send_signal(signal.SIGTERM)
        # Ordinary command deliberately survives TERM, exercising the ten-second
        # escalation. Its separately owned native observer must survive that too.
        def node_stopped():
            return not json.loads((native / "container.json").read_text())["State"]["Running"]
        self.eventually(node_stopped, "cancellation did not stop owned builder", timeout=18)
        observed = json.loads((native / "observer.ready").read_text())
        self.assertEqual(os.getsid(observed["pid"]), observer["sid"])
        self.assertFalse((native / "observer.signal").exists())
        self.assertFalse((native / "native-terminal").exists())
        self.assertIsNotNone(self.status()["gate"])
        result = self.run_cli(*self.marker_command("next"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "next").exists())
        (native / "observer.release").touch()
        self.eventually((native / "native-terminal").exists, "native terminal response missing")
        self.assertEqual(owner.wait(timeout=8), 128 + signal.SIGTERM)
        self.eventually(lambda: self.process_terminal(observed["pid"])
                        and self.process_terminal(observer["pid"]),
                        "native CLI or terminal observer still alive")
        def reconciled():
            self.run_cli("--reconcile", env=env)
            return self.status()["gate"] is None
        self.eventually(reconciled, "terminal observer did not release exclusion")
        self.assertFalse((native / "observer.signal").exists())
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_delayed_container_create_stays_owned_through_cancel_and_late_cleanup(self):
        native, fixture_env = self.docker_fixture()
        env = {**fixture_env, "TEST_DOCKER_DELAY_CREATE": "1"}
        self.releases.append(native / "create.release")
        owner = self.launch("--", "bash", str(ENTRY), "--container-run", "--",
                            "docker", "run", "-d", "fixture", env=env)
        self.eventually((native / "create.ready").exists, "native container creation did not begin")
        observed = json.loads((native / "create.ready").read_text())
        state = self.status()
        token = state["gate"]["token"]
        observer = state["gate"]["resources"][0]["observers"][0]["scope"]
        self.assertEqual(observer["sid"], observed["sid"])
        self.assertNotEqual(observer["sid"], state["gate"]["scope"]["sid"])
        self.assertFalse((native / "container.json").exists())

        owner.send_signal(signal.SIGTERM)
        self.eventually((native / "create.absence-read").exists,
                        "cancellation did not read native absence before create completion")
        self.assertEqual(self.status()["gate"]["token"], token)
        self.assertEqual(os.getsid(observed["pid"]), observer["sid"])
        self.assertFalse((native / "create.signal").exists())
        result = self.run_cli(*self.marker_command("next"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "next").exists())
        self.assertFalse((native / "container.json").exists())

        (native / "create.release").touch()
        self.eventually((native / "create.published").exists, "native create did not publish its late effect")
        self.assertEqual(owner.wait(timeout=8), 128 + signal.SIGTERM)
        self.assertIsNone(self.status()["gate"])
        self.assertFalse((native / "container.json").exists(), "late-created container survived cleanup")
        self.assertEqual((native / "removed").read_text().splitlines(), ["a" * 64])
        self.assertFalse((native / "create.signal").exists())
        self.eventually(lambda: self.process_terminal(observed["pid"])
                        and self.process_terminal(observer["pid"]),
                        "container observer still alive after exclusion released")
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_lost_observer_retains_hold_even_after_native_cli_finishes(self):
        owner, native, env, observer = self.start_build_observer()
        observed = json.loads((native / "observer.ready").read_text())
        token = self.status()["gate"]["token"]
        # Target the recorded observer identity while its real CLI is blocked;
        # killing it loses the sole response custodian without stopping import.
        os.kill(observer["pid"], signal.SIGKILL)
        self.assertNotEqual(owner.wait(timeout=8), 0)
        (native / "observer.release").touch()
        self.eventually((native / "native-terminal").exists, "native CLI did not finish")
        self.eventually(lambda: self.process_terminal(observed["pid"])
                        and self.process_terminal(observer["pid"]),
                        "lost observer or native CLI still alive")
        self.run_cli("--reconcile", env=env)
        state = self.status()
        self.assertEqual(state["gate"]["token"], token)
        self.assertTrue(state["gate"]["quarantine"])
        self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        result = self.run_cli(*self.marker_command("forbidden"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "forbidden").exists())

    def assert_lost_native_response_holds(self, signum):
        owner, native, env, _observer = self.start_build_observer()
        observed = json.loads((native / "observer.ready").read_text())
        token = self.status()["gate"]["token"]
        # The response custodian survives, but a killed buildx process cannot
        # acknowledge whether its Engine import has actually finished.
        if signum is None:
            (native / "observer.transport-loss").touch()
        else:
            os.kill(observed["pid"], signum)
        self.assertNotEqual(owner.wait(timeout=8), 0)
        self.assertFalse((native / "native-terminal").exists())
        self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        self.assertIsNotNone(self.status()["gate"], "killed CLI released unknown external work")
        self.run_cli("--reconcile", env=env)
        state = self.status()
        self.assertEqual(state["gate"]["token"], token)
        self.assertTrue(state["gate"]["quarantine"])
        result = self.run_cli(*self.marker_command("forbidden"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "forbidden").exists())

    def test_killed_native_cli_does_not_supply_a_terminal_response(self):
        self.assert_lost_native_response_holds(signal.SIGKILL)

    def test_handled_native_cli_cancellation_does_not_supply_a_terminal_response(self):
        self.assert_lost_native_response_holds(signal.SIGTERM)

    def test_native_cli_transport_error_does_not_supply_a_terminal_response(self):
        self.assert_lost_native_response_holds(None)

    def test_failed_build_check_is_terminal_after_its_owned_nodes_stop(self):
        owner, native, env, _observer = self.start_build_observer(check_only=True)
        (native / "observer.transport-loss").touch()
        self.assertEqual(owner.wait(timeout=8), 1)
        self.assertFalse((native / "native-terminal").exists())
        self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        self.assertIsNone(self.status()["gate"])
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_container_application_failure_preserves_exit_status_and_releases(self):
        _native, fixture_env = self.docker_fixture()
        env = {**fixture_env, "TEST_DOCKER_APP_EXIT": "23"}
        result = self.run_cli("--", "bash", str(ENTRY), "--container-run", "--",
                              "docker", "run", "--rm", "fixture", env=env)
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertIsNone(self.status()["gate"])
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_lost_create_response_uses_registered_name_and_label_for_terminal_proof(self):
        native, fixture_env = self.docker_fixture()
        env = {**fixture_env, "TEST_DOCKER_CREATE_RESPONSE_LOSS": "1"}
        result = self.run_cli("--", "bash", str(ENTRY), "--container-run", "--",
                              "docker", "run", "-d", "fixture", env=env)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse((native / "container.json").exists())
        self.assertEqual((native / "removed").read_text().splitlines(), ["a" * 64])
        self.assertIsNone(self.status()["gate"], "exact owned ID removal did not release custody")
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_status_generation_is_coherent_and_never_publishes_argv(self):
        secret = "argument-secret-should-never-appear"
        base = self.base / "safe-status"
        self.releases.append(base.with_suffix(".release"))
        owner = self.launch("--", sys.executable, "-c", HOLD, str(base), secret)
        self.eventually(base.with_suffix(".ready").exists, "owner did not start")
        first = self.status()["gate"]["token"]
        first_sid = json.loads(base.with_suffix(".ready").read_text())["sid"]
        next_base = self.base / "next-status"
        self.releases.append(next_base.with_suffix(".release"))
        waiter = self.launch("--", sys.executable, "-c", HOLD, str(next_base), secret)
        self.eventually(lambda: self.status()["tickets"], "waiter did not register")
        snapshots = []
        for _ in range(4):
            state = self.status()
            self.assertEqual(state["gate"]["token"], first)
            self.assertNotIn(secret, json.dumps(state))
            snapshots.append(state)
        self.release(base)
        # Inspect through the generation transition, then bind each observed
        # token to the independently announced kernel session of that owner.
        for _ in range(6):
            snapshots.append(self.status())
        self.eventually(next_base.with_suffix(".ready").exists, "second owner did not launch")
        second_state = self.status()
        snapshots.append(second_state)
        second = second_state["gate"]["token"]
        self.assertNotEqual(first, second)
        expected = {first: first_sid,
                    second: json.loads(next_base.with_suffix(".ready").read_text())["sid"]}
        for state in snapshots:
            self.assertNotIn(secret, json.dumps(state))
            if state["gate"] is not None:
                gate = state["gate"]
                self.assertEqual(gate["scope"]["sid"], expected[gate["token"]])
        self.release(next_base)
        self.assertEqual(owner.wait(timeout=8), 0)
        self.assertEqual(waiter.wait(timeout=8), 0)
        self.assertIsNone(self.status()["gate"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
