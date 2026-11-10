#!/usr/bin/env python3
"""Blackbox queue/lifetime contracts; no Cargo, Docker, or shared lock access."""

import json
import hashlib
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/lib"))
from template_state import load_lock

ENTRY = ROOT / "scripts/ci/validation-lock.sh"
LOCK_ENV = ("VALIDATION_LOCK_DIR", "VALIDATION_LOCK_DOMAIN",
            "VALIDATION_LOCK_TOKEN", "VALIDATION_LOCK_HELD")

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
(base / 'controller.ready').touch()
deadline = time.monotonic() + 75
while not (base / 'controller.release').exists() and time.monotonic() < deadline:
    for request in sorted(base.glob('request-*.json')):
        reply = request.with_suffix('.reply')
        if reply.exists():
            continue
        process = subprocess.run(['bash', entry, *json.loads(request.read_text())], text=True, capture_output=True)
        pending = reply.with_suffix('.pending')
        pending.write_text(json.dumps({'code': process.returncode, 'stdout': process.stdout,
                                       'stderr': process.stderr}))
        pending.replace(reply)
    time.sleep(.015)
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
if os.environ.get('TEST_DOCKER_SIGNAL_MASK') == '1':
    with (root / 'signal-masks.jsonl').open('a') as output:
        output.write(json.dumps({'args': args, 'mask': sorted(int(item) for item in signal.pthread_sigmask(signal.SIG_BLOCK, []))}) + '\\n')
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
elif args[:2] == ['volume', 'ls'] and any(arg.startswith('name=') for arg in args):
    pattern = next(arg for arg in args if arg.startswith('name='))[5:]
    for volume in sorted(root.glob('volume-*')):
        if re.search(pattern, volume.name[7:]):
            print(volume.name[7:])
elif args[:2] in (['network', 'ls'], ['volume', 'ls']):
    pass
elif args and args[0] == 'compose':
    project = args[args.index('-p') + 1]
    files = [args[index + 1] for index, value in enumerate(args[:-1]) if value == '-f']
    if 'config' in args:
        print(json.dumps({'name': project, 'services': {'fixture': {'image': 'fixture'}}}))
    elif 'up' in args:
        state = {'Id': 'c' * 64, 'Name': '/' + project + '-nats1-1',
                 'Config': {'Labels': {'com.docker.compose.project': project,
                                      'com.docker.compose.service': 'nats1',
                                      'recovery.generation': project.removeprefix('messaging-recovery-')}},
                 'Image': 'fixture-image', 'RestartCount': 0,
                 'State': {'Running': True, 'Restarting': False, 'Status': 'running', 'StartedAt': 'fixture-start'}}
        save(statefile, state)
        (root / ('volume-' + project)).touch()
        if os.environ.get('TEST_COMPOSE_LOST_RESPONSE') == '1':
            sys.exit(1)
    elif 'run' in args:
        save(root / 'compose-run.json', {'project': project, 'files': files,
                                       'name': args[args.index('--name') + 1]})
        print('native configuration checked')
    elif 'stop' in args:
        if state is not None and state['Config']['Labels'].get('com.docker.compose.project') == project:
            state['State'].update(Running=False, Status='exited')
            save(statefile, state)
        (root / 'compose-stop').touch()
    elif 'down' in args:
        if state is not None and state['Config']['Labels'].get('com.docker.compose.project') != project:
            sys.exit(1)
        statefile.unlink(missing_ok=True)
        if '-v' in args:
            (root / ('volume-' + project)).unlink(missing_ok=True)
        (root / 'compose-down').touch()
    else:
        sys.exit(2)
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
        (root / ('volume' + state['Name'].replace('/', '-') + '_state')).touch()
    print('Name: ' + builder['Name'] + '\\nDriver: ' + builder['Driver'])
elif args[:2] == ['buildx', 'rm']:
    if builder is None or args[2:] != [builder['Name']]:
        sys.exit(1)
    for node in builder['Nodes']:
        if state is not None and state['Name'] == '/buildx_buildkit_' + node['Name']:
            statefile.unlink()
        (root / ('volume-buildx_buildkit_' + node['Name'] + '_state')).unlink(missing_ok=True)
    builderfile.unlink()
    with (root / 'builders-removed').open('a') as removed:
        removed.write(builder['Name'] + '\\n')
elif args[:3] == ['buildx', 'history', 'logs']:
    history = json.loads((root / 'history.json').read_text())
    if args[-1] != history['record']['Ref'] or args[args.index('--builder') + 1] != builder['Name']:
        sys.exit(1)
    sys.stderr.write((root / 'progress.jsonl').read_text())
elif args and args[0] == 'exec':
    if state is None or args[1] != state['Id'] or not state['State']['Running']:
        sys.exit(1)
    if 'histories' in args:
        print((root / 'history.json').read_text())
    elif 'get' in args:
        sys.stdout.buffer.write((root / 'error.pb').read_bytes())
    else:
        sys.exit(2)
elif args and args[0] == 'start':
    if state is None or args[-1] != state['Id']:
        sys.exit(1)
    state['State'].update(Running=True, Status='running')
    save(statefile, state)
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

    def controller(self, env=None, retained_compose=False):
        self.request_number = 0
        self.releases.append(self.base / "controller.release")
        prefix = ("--with-retained-compose", "--") if retained_compose else ("--",)
        owner = self.launch(*prefix, sys.executable, "-c", CONTROLLER,
                            str(ENTRY), str(self.base), env=env)
        self.eventually((self.base / "controller.ready").exists, "root controller did not start")
        return owner

    def control(self, *args, timeout=8):
        self.request_number += 1
        request = self.base / (f"request-{self.request_number:04d}.json")
        pending = request.with_suffix(".pending")
        pending.write_text(json.dumps(list(args)))
        pending.replace(request)
        reply = request.with_suffix(".reply")
        self.eventually(reply.exists, "controller did not answer " + args[0], timeout=timeout)
        return json.loads(reply.read_text())

    def finish_controller(self, owner, expected=0):
        (self.base / "controller.release").touch()
        self.assertEqual(owner.wait(timeout=18), expected)

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

    def test_consumer_carrier_retains_failed_projects_and_removes_successful_volumes(self):
        carrier_source = ROOT / "scripts/ci/consumer-lifecycle-check.sh"
        if not carrier_source.is_file():
            lock = load_lock(ROOT)
            if lock is not None:
                self.assertEqual(lock["state"], "complete", "derived service initialization is incomplete")
                self.skipTest("source-template consumer carrier is not retained in this derived service")
        source = carrier_source.read_text()
        native, environment = self.docker_fixture()
        capture = self.base / "capture-timeout"
        capture.mkdir()
        # Observe the actual native subprocess budget without waiting through
        # a simulated 45-second shutdown or replacing queue deadline logic.
        (capture / "sitecustomize.py").write_text("""
import json, os, pathlib, subprocess
original = subprocess.Popen.communicate
def communicate(self, *args, **kwargs):
    if isinstance(self.args, list) and self.args[:2] == ['docker', 'compose'] and 'stop' in self.args:
        record = {'argv': self.args, 'timeout': kwargs.get('timeout')}
        with (pathlib.Path(os.environ['TEST_DOCKER_STATE']) / 'stop-timeouts.jsonl').open('a') as output:
            output.write(json.dumps(record) + '\\n')
    return original(self, *args, **kwargs)
subprocess.Popen.communicate = communicate
""")
        environment["PYTHONPATH"] = str(capture) + (os.pathsep + environment["PYTHONPATH"] if environment.get("PYTHONPATH") else "")
        checkout = self.base / "checkout"
        for relative in ("scripts/ci/validation-lock.sh", "scripts/ci/validation-lock.py",
                         "scripts/lib/compose-postgres.sh"):
            destination = checkout / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)
        (checkout / "env").mkdir()
        (checkout / "env/docker-compose.yml").write_text("services: {}\n")
        carrier = checkout / "scripts/ci/consumer-lifecycle-check.sh"
        # Execute the real admission and Compose carrier functions, replacing
        # only the expensive historical build/archive scenario dispatch.
        functions, _ = source.rsplit('case "${1:-run}" in', 1)
        carrier.write_text(functions + r'''
LIFECYCLE_EVIDENCE=$2
mkdir -p "$LIFECYCLE_EVIDENCE"
printf 'services: {}\n' >"$LIFECYCLE_EVIDENCE/compose.yml"
LIFECYCLE_SOURCE_PROJECT="consumer-source-${VALIDATION_LOCK_TOKEN:0:12}"
LIFECYCLE_RESTORE_PROJECT="consumer-restore-${VALIDATION_LOCK_TOKEN:0:12}"
register_projects
compose "$LIFECYCLE_SOURCE_PROJECT" up -d --wait postgres nats
if [[ $3 == killed ]]; then kill -KILL "$$"; fi
if [[ $3 == passed ]]; then
    bash scripts/ci/validation-lock.sh --resource-cleanup "$LIFECYCLE_SOURCE_RESOURCE" --remove-volumes
    bash scripts/ci/validation-lock.sh --resource-cleanup "$LIFECYCLE_RESTORE_RESOURCE" --remove-volumes
fi
''')
        for mode, expected in (("killed", 137), ("passed", 0), ("lost", 1)):
            with self.subTest(mode=mode):
                for path in native.iterdir():
                    path.unlink()
                gate = self.base / (mode + ".lock")
                env = {**environment, "VALIDATION_LOCK_DIR": str(gate)}
                if mode == "lost":
                    env["TEST_COMPOSE_LOST_RESPONSE"] = "1"
                result = subprocess.run(["bash", str(carrier), "run", str(self.base / (mode + "-evidence")), mode],
                                        cwd=checkout, env=env, capture_output=True, text=True, timeout=40)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(gate.exists(), mode == "lost", "unknown native response must retain custody")
                if mode == "passed":
                    self.assertFalse((native / "container.json").exists())
                    self.assertEqual(list(native.glob("volume-*")), [])
                else:
                    self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
                    self.assertEqual(len(list(native.glob("volume-*"))), 1)
                    self.assertTrue((native / "compose-stop").exists())
                    self.assertFalse((native / "compose-down").exists())
                    observations = [json.loads(line) for line in (native / "stop-timeouts.jsonl").read_text().splitlines()]
                    self.assertTrue(observations)
                    for observation in observations:
                        self.assertEqual(observation["argv"][-3:], ["stop", "--timeout", "45"])
                        self.assertGreater(observation["timeout"], 45)
                        self.assertLessEqual(observation["timeout"], 60)
                self.assertTrue((self.base / (mode + "-evidence") / "compose.yml").exists())

    def test_retained_compose_capability_refuses_old_helpers_and_wrong_root_upgrade(self):
        marker = self.base / "unexpected"
        refused = self.run_cli("--", "bash", str(ENTRY), "--with-retained-compose", "--",
                               sys.executable, "-c", MARK, str(marker))
        self.assertNotEqual(refused.returncode, 0, refused.stderr)
        self.assertFalse(marker.exists())
        old_source = subprocess.run(["git", "show", "519d66ac9ab614cd79e572abf8bfc393bcc312e2"],
                                    cwd=ROOT, capture_output=True, text=True, timeout=5)
        if old_source.returncode:
            self.skipTest("pre-retention queue blob absent in shallow checkout; compatibility requires history")
        old = self.base / "old-helper.py"
        old.write_text(old_source.stdout)
        result = self.run_cli("--with-retained-compose", "--", sys.executable, str(old), "--assert-held")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("capability", result.stderr)
        self.assertIsNone(self.status()["gate"])

    def test_compose_run_admits_only_named_disposable_registered_project(self):
        native, env = self.docker_fixture()
        owner = self.controller(env=env)
        token = self.status()["gate"]["token"]
        project = "compose-" + token[:12]
        config = self.base / "compose.json"
        config.write_text('{"services":{"fixture":{"image":"fixture"}}}')
        registration = self.control("--resource-register", "compose", project, "--file", str(config))
        self.assertEqual(registration["code"], 0, registration)
        resource = registration["stdout"].strip()
        command = ["docker", "compose", "-p", project, "-f", str(config)]
        for tail in (["run", "--name", project + "-probe", "fixture"],
                     ["run", "--rm", "--name", "foreign-probe", "fixture"],
                     ["run", "--rm", "--detach", "--name", project + "-probe", "fixture"],
                     ["exec", "fixture", "run", "--rm", "--name", project + "-probe"]):
            with self.subTest(tail=tail):
                response = self.control("--resource-run", resource, "--", *command, *tail)
                self.assertNotEqual(response["code"], 0, response)
                self.assertFalse((native / "compose-run.json").exists())
        response = self.control("--resource-run", resource, "--", *command,
                                "run", "--rm", "--no-deps", "-T", "--pull", "never",
                                "--name", project + "-probe", "fixture")
        self.assertEqual(response["code"], 0, response)
        self.assertEqual(response["stdout"].strip(), "native configuration checked")
        self.assertEqual(json.loads((native / "compose-run.json").read_text())["project"], project)
        self.assertEqual(self.control("--resource-cleanup", resource)["code"], 0)
        self.assertTrue((native / "compose-down").exists())
        self.finish_controller(owner)
        self.assertIsNone(self.status()["gate"])

    def test_receipt_completion_requires_owned_resources_to_finish(self):
        self.assertNotEqual(self.run_cli("--assert-complete").returncode, 0)
        _native, env = self.docker_fixture()
        owner = self.controller(env=env)
        self.assertEqual(self.control("--assert-complete")["code"], 0)
        token = self.status()["gate"]["token"]
        registered = self.control("--resource-register", "container", "receipt-" + token[:12])
        self.assertEqual(registered["code"], 0, registered)
        resource = registered["stdout"].strip()
        self.assertNotEqual(self.control("--assert-complete")["code"], 0)
        cleaned = self.control("--resource-cleanup", resource)
        self.assertEqual(cleaned["code"], 0, cleaned)
        self.assertEqual(self.control("--assert-complete")["code"], 0)
        # Completion admission is an observation, not release of the live root.
        self.assertIsNotNone(self.status()["gate"])
        self.finish_controller(owner)
        self.assertIsNone(self.status()["gate"])

    def test_reused_member_hint_cannot_signal_foreign_kernel_session(self):
        hook, env = self.relay_environment("foreign-hint")
        foreign_base = self.base / "foreign"
        self.releases.append(foreign_base.with_suffix(".release"))
        foreign = subprocess.Popen([sys.executable, "-c", HOLD, str(foreign_base)], start_new_session=True)
        self.processes.append(foreign)
        self.eventually(foreign_base.with_suffix(".ready").exists, "owned foreign-session fixture did not start")
        foreign_sid = json.loads(foreign_base.with_suffix(".ready").read_text())["sid"]
        (hook / "foreign.sid").write_text(str(foreign_sid))
        base = self.base / "work"
        self.releases.append(base.with_suffix(".release"))
        owner = self.launch("--", sys.executable, "-c", TERM_COUNT, str(base), env=env)
        self.eventually(base.with_suffix(".ready").exists, "command did not start")
        (hook / "target.sid").write_text(str(self.status()["gate"]["scope"]["sid"]))
        owner.send_signal(signal.SIGTERM)
        self.assertNotEqual(owner.wait(timeout=8), 0)
        self.assertEqual((hook / "join.refused").read_text(), str(foreign_sid))
        injected = json.loads((hook / "hint.reached").read_text())
        self.assertEqual(injected["hint"], foreign_sid)
        self.assertIsNone(foreign.poll(), "reused member hint signalled the foreign session")
        self.assertFalse((hook / "deliveries").exists())
        self.assertIsNotNone(self.status()["gate"])
        self.release(base)
        self.eventually(self.recover_gate, "observed terminal scope did not reconcile")
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
                base = self.base / "buffered"
                self.releases.append(base.with_suffix(".release"))
                owner = self.launch("--", sys.executable, "-c", TERM_COUNT, str(base), env=env)
                self.eventually(base.with_suffix(".ready").exists, "command did not start")
                gate = self.status()["gate"]
                receipt = (self.gate.with_name(self.gate.name + ".queue") / "owners" / gate["token"]
                           / "command.terminal.json")
                (hook / "target.sid").write_text(str(gate["scope"]["sid"]))
                # The guardian issues the grant and dies at the write boundary.
                owner.send_signal(signal.SIGTERM)
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
                base = self.base / "work"
                extra_base = self.base / "extra"
                self.releases.extend((base.with_suffix(".release"), extra_base.with_suffix(".release")))
                if mode == "fork-failure":
                    # A separate group is needed to reach an actual relay fork.
                    program = "import subprocess,os,sys; subprocess.Popen([sys.executable,'-c',sys.argv[2],sys.argv[3]],preexec_fn=os.setpgrp)\n" + TERM_COUNT
                    owner = self.launch("--", sys.executable, "-c", program, str(base), HOLD, str(extra_base), env=env)
                    self.eventually(extra_base.with_suffix(".ready").exists, "additional group did not start")
                else:
                    owner = self.launch("--", sys.executable, "-c", TERM_COUNT, str(base), env=env)
                self.eventually(base.with_suffix(".ready").exists, "command did not start")
                (hook / "target.sid").write_text(str(self.status()["gate"]["scope"]["sid"]))
                owner.send_signal(signal.SIGTERM)
                # One refused, ambiguous or failed grant ends the guardian
                # without a second frame, including the KILL escalation.
                self.assertEqual(owner.wait(timeout=10), 128 + signal.SIGTERM)
                grants = (hook / "grants").read_text()
                self.assertEqual(len(grants.splitlines()), 1)
                if mode in {"duplicate", "unknown-write"}:
                    self.eventually(base.with_suffix(".signals").exists, "first granted request never arrived")
                    self.assertEqual(base.with_suffix(".signals").read_text(), "TERM\n")
                else:
                    self.assertFalse(base.with_suffix(".signals").exists())
                self.assertEqual(self.run_cli("--reconcile").returncode, 1)
                self.assertIsNotNone(self.status()["gate"])
                self.release(base)
                self.release(extra_base)
                self.eventually(self.recover_gate, "failed scope could not reconcile after actual absence")
                self.assertEqual((hook / "grants").read_text(), grants, "ambiguous/retired grant was replayed")

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

    def test_unknown_gate_capability_refuses_without_mutable_downgrade(self):
        owner = self.controller(retained_compose=True)
        initial = self.status()["gate"]
        original = self.gate.read_bytes()
        owner.send_signal(signal.SIGSTOP)
        try:
            for field, value in (("protocol", 999), ("capability", "unknown-capability"),
                                 ("capability", "ordinary-child-v1")):
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

    def test_stable_owner_wait_reports_periodic_safe_progress_then_acquisition(self):
        secret = "private-argument-and-environment-must-not-be-logged"
        base = self.base / "progress-owner"
        self.releases.append(base.with_suffix(".release"))
        environment = {**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": "30", "PRIVATE_INPUT": secret}
        owner = self.launch("--", sys.executable, "-c", HOLD, str(base), secret, env=environment)
        self.eventually(base.with_suffix(".ready").exists, "progress owner did not start")
        active = self.status()["gate"]
        marker = self.base / "after-progress"
        waiter = self.launch("--", sys.executable, "-c", MARK, str(marker), secret, env=environment)
        log = self.logs[-1]

        def output():
            # Read without moving the offset shared with the actual writer.
            return os.pread(log.fileno(), 65536, 0).decode()

        def waits():
            return [line for line in output().splitlines() if line.startswith("validation wait ")]

        first = self.eventually(waits, "initial wait progress was not emitted", timeout=3)
        repeated = self.eventually(lambda: rows if len(rows := waits()) >= 2 else None,
                                   "stable owner had no bounded periodic wait progress", timeout=12)
        self.assertFalse(marker.exists(), "wait progress launched the waiting command")
        self.assertEqual(self.status()["gate"]["token"], active["token"])
        self.assertEqual(len(repeated), 2, "wait progress flooded without a state change")
        for line in (first[0], repeated[1]):
            self.assertIn("timeout=30s", line)
            self.assertIn(f"owner_pid={owner.pid}", line)
            self.assertIn("owner_checkout=" + json.dumps(str(ROOT)), line)
            self.assertIn("owner_command=" + json.dumps(active["command"]), line)
            self.assertIn("identity=lease-held", line)
            self.assertNotIn(secret, line)
        first_elapsed = float(first[0].split("elapsed=", 1)[1].split("s", 1)[0])
        next_elapsed = float(repeated[1].split("elapsed=", 1)[1].split("s", 1)[0])
        self.assertGreaterEqual(next_elapsed - first_elapsed, 9.5)
        self.assertLessEqual(next_elapsed - first_elapsed, 12)
        self.release(base)
        self.assertEqual(owner.wait(timeout=8), 0)
        self.assertEqual(waiter.wait(timeout=8), 0)
        self.assertTrue(marker.exists())
        self.assertIn("validation acquired ", output())
        self.assertNotIn(secret, output())
        self.assertIsNone(self.status()["gate"])

    def test_timeout_and_invalid_timeout_never_launch(self):
        _, base = self.hold("busy")
        for value, expected in (("0", 75), (".08", 75), ("-1", 2),
                                ("nan", 2), ("inf", 2), ("no", 2)):
            with self.subTest(timeout=value):
                result = self.run_cli(*self.marker_command("forbidden"),
                                      env={**self.env, "VALIDATION_LOCK_TIMEOUT_SECONDS": value})
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertFalse((self.base / "forbidden").exists())
                if expected == 75:
                    self.assertIn("validation lock timed out", result.stderr)
                    self.assertIn(f"timeout={float(value):g}s", result.stderr)
                    self.assertNotIn("validation acquired ", result.stderr)
        self.release(base)

    def test_waiter_cancellation_departs_without_launch(self):
        owner, base = self.hold("busy")
        waiter = self.launch(*self.marker_command("forbidden"))
        log = self.logs[-1]
        self.eventually(lambda: self.status()["tickets"], "waiter did not register")
        waiter.send_signal(signal.SIGTERM)
        self.assertEqual(waiter.wait(timeout=5), 128 + signal.SIGTERM)
        output = os.pread(log.fileno(), 65536, 0).decode()
        self.assertIn("validation cancelled ", output)
        self.assertIn("phase=admission", output)
        self.assertNotIn("validation acquired ", output)
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

    def test_nested_executable_receives_term_and_preserves_callers_mask(self):
        script = r'''
import json, os, pathlib, signal, subprocess, sys, time
entry, ready = sys.argv[1:]
signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1})
code = """
import json, os, pathlib, signal, sys, time
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
pathlib.Path(sys.argv[1]).write_text(json.dumps({'pid': os.getpid(), 'mask': sorted(int(item) for item in signal.pthread_sigmask(signal.SIG_BLOCK, []))}))
time.sleep(15)
raise SystemExit(31)
"""
child = subprocess.Popen(['bash', entry, '--', sys.executable, '-c', code, ready])
deadline = time.monotonic() + 5
while not pathlib.Path(ready).exists() and time.monotonic() < deadline:
    time.sleep(.01)
state = json.loads(pathlib.Path(ready).read_text())
os.kill(state['pid'], signal.SIGTERM)
try:
    result = child.wait(timeout=3)
except subprocess.TimeoutExpired:
    os.kill(state['pid'], signal.SIGKILL)
    child.wait(timeout=5)
    raise AssertionError('nested executable did not receive SIGTERM; mask=' + str(state['mask']))
assert result == 0, result
assert state['mask'] == [int(signal.SIGUSR1)], state
assert signal.pthread_sigmask(signal.SIG_BLOCK, []) == {signal.SIGUSR1}
'''
        result = self.run_cli("--", sys.executable, "-c", script, str(ENTRY),
                              str(self.base / "nested-signals.json"), timeout=12)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(self.status()["gate"])

    def test_admitted_native_cli_preserves_callers_mask_without_guard_signals(self):
        native, environment = self.docker_fixture()
        environment["TEST_DOCKER_SIGNAL_MASK"] = "1"
        script = """
import signal, subprocess, sys
signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1})
resource = subprocess.check_output(['bash', sys.argv[1], '--builder-prepare'], text=True).strip()
subprocess.run(['bash', sys.argv[1], '--resource-cleanup', resource], check=True)
subprocess.run(['bash', sys.argv[1], '--resource-complete', resource], check=True)
assert signal.pthread_sigmask(signal.SIG_BLOCK, []) == {signal.SIGUSR1}
"""
        result = self.run_cli("--", sys.executable, "-c", script, str(ENTRY), env=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in (native / "signal-masks.jsonl").read_text().splitlines()]
        self.assertTrue(any(row["args"][:2] == ["buildx", "create"] for row in records))
        self.assertTrue(any("--bootstrap" in row["args"] for row in records))
        self.assertTrue(any(row["args"] == ["context", "show"] for row in records))
        for row in records:
            with self.subTest(command=row["args"]):
                self.assertEqual(row["mask"], [int(signal.SIGUSR1)])
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
        self.assert_builder_removed(native)
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

    def build_failure_history(self, native, candidate, mode):
        # Encode the native Status/Any schema independently of the queue parser.
        def varint(value):
            result = bytearray()
            while value > 127:
                result.append((value & 127) | 128)
                value >>= 7
            return bytes(result) + bytes([value])

        def field(key, value):
            return varint(key * 8 + 2) + varint(len(value)) + value

        def detail(kind, value):
            return field(3, field(1, ("github.com/moby/buildkit/" + kind + "+json").encode())
                         + field(2, json.dumps(value).encode()))

        digest = "sha256:" + "d" * 64
        code, message = (1, "context canceled") if mode == "cancel" else (2, "command failed")
        # A failed RUN on v0.33.1 carries one stack per wrap site: the runc
        # exit error and the ExecOp wrap both pass through ExecOp.Exec.
        execution = ["github.com/moby/buildkit/solver/llbsolver/ops.(*ExecOp).Exec",
                     "github.com/moby/buildkit/solver.(*sharedOp).Exec.func2"]
        frame_lists = [["github.com/moby/buildkit/executor/runcexecutor.exitError",
                        "github.com/moby/buildkit/executor/runcexecutor.(*runcExecutor).Run",
                        *execution, "runtime.goexit"],
                       ["github.com/moby/buildkit/solver.(*edge).execOp", "runtime.goexit"],
                       execution]
        if mode == "export":
            frame_lists = [["github.com/moby/buildkit/exporter/containerimage.(*imageExporterInstance).Export"]]
        stacks = [{"version": "unknown" if mode == "version" else "v0.33.1",
                   "revision": "8c91502cf280bd70a0c50912ce251c46a8881d9f",
                   "cmdline": ["/usr/bin/buildkitd", "--allow-insecure-entitlement=network.host"],
                   "frames": [{"Name": name} for name in frames]} for frames in frame_lists]
        exit_messages = {"exit-code-duplicate": [{"Code": 1}, {"Code": 1}], "exit-code-zero": [{"Code": 0}],
                         "exit-code-text": [{"Code": "1"}]}.get(mode, [{"Code": 1}])
        blob = (b"\x08" + varint(code) + field(2, message.encode())
                + b"".join(detail("stack.Stack", stack) for stack in stacks)
                + detail("errdefs.Source", {"info": {"filename": "Dockerfile"}})
                + detail("errdefs.Vertex", {"digest": digest})
                + b"".join(detail("gatewayapi.ExitMessage", body) for body in exit_messages))
        if mode == "malformed":
            blob = blob[:-1]
        history = {"Ref": "testref", "FrontendAttrs": {"vcs:revision": candidate},
                   "CreatedAt": {"seconds": 1792545594, "nanos": 0},
                   "CompletedAt": {"seconds": 1792545653, "nanos": 0},
                   "error": {"code": code, "message": message},
                   "externalError": {"media_type": "application/vnd.googeapis.google.rpc.status+proto",
                                     "digest": "sha256:" + hashlib.sha256(blob).hexdigest(), "size": len(blob)},
                   "numTotalSteps": 1, "numCompletedSteps": 1}
        progress = {"vertexes": [{"digest": digest, "name": "RUN false", "error": message,
                                  "started": "2026-10-21T01:20:52Z", "completed": "2026-10-21T01:20:52.9Z"}]}
        if mode == "candidate":
            history["FrontendAttrs"]["vcs:revision"] = "0" * 40
        elif mode == "incomplete":
            history.pop("CompletedAt")
        elif mode == "digest":
            history["externalError"]["digest"] = "sha256:" + "0" * 64
        elif mode == "export-progress":
            progress["vertexes"][0]["name"] = "exporting to docker image format"
        elif mode == "wrong-vertex":
            progress["vertexes"][0]["digest"] = "sha256:" + "e" * 64
        (native / "error.pb").write_bytes(blob)
        (native / "history.json").write_text(json.dumps({"type": 1, "record": history}))
        (native / "progress.jsonl").write_text(json.dumps(progress) + "\n")

    def test_completed_native_exec_failure_releases_without_rewriting_exit(self):
        owner, native, env, _observer = self.start_build_observer()
        (native / "observer.transport-loss").touch()
        self.assertEqual(owner.wait(timeout=8), 1)
        gate = self.status()["gate"]
        resource = gate["resources"][0]
        receipt = self.base / "validation.lock.queue" / "owners" / gate["token"] / resource["observers"][0]["receipt"]
        before = receipt.read_bytes()
        self.build_failure_history(native, gate["candidate"], "exec")
        result = self.run_cli("--reconcile-buildkit", resource["token"], env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt.read_bytes(), before)
        self.assertEqual(json.loads(before), {"exit": 1, "terminal": False})
        self.assertIsNone(self.status()["gate"])
        self.assert_builder_removed(native)
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def test_unproven_buildkit_failure_preserves_quarantine(self):
        owner, native, env, _observer = self.start_build_observer()
        (native / "observer.transport-loss").touch()
        self.assertEqual(owner.wait(timeout=8), 1)
        gate = self.status()["gate"]
        resource = gate["resources"][0]
        for mode in ("export", "export-progress", "cancel", "candidate", "incomplete", "version",
                     "digest", "malformed", "wrong-vertex", "exit-code-duplicate", "exit-code-zero",
                     "exit-code-text"):
            with self.subTest(mode=mode):
                self.build_failure_history(native, gate["candidate"], mode)
                result = self.run_cli("--reconcile-buildkit", resource["token"], env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.status()["gate"]["token"], gate["token"])
                self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        # A replacement with the same name is never restarted or accepted.
        state = json.loads((native / "container.json").read_text())
        state["Id"] = "c" * 64
        (native / "container.json").write_text(json.dumps(state))
        self.build_failure_history(native, gate["candidate"], "exec")
        result = self.run_cli("--reconcile-buildkit", resource["token"], env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        result = self.run_cli(*self.marker_command("forbidden"),
                              env={**env, "VALIDATION_LOCK_TIMEOUT_SECONDS": ".15"})
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertFalse((self.base / "forbidden").exists())

    def test_failed_build_check_is_terminal_after_its_owned_nodes_stop(self):
        owner, native, env, _observer = self.start_build_observer(check_only=True)
        (native / "observer.transport-loss").touch()
        self.assertEqual(owner.wait(timeout=8), 1)
        self.assertFalse((native / "native-terminal").exists())
        self.assert_builder_removed(native)
        self.assertIsNone(self.status()["gate"])
        self.assertEqual(self.run_cli(*self.marker_command("next"), env=env).returncode, 0)

    def assert_builder_removed(self, native):
        self.assertTrue((native / "builders-removed").read_text().startswith("validation-"))
        self.assertFalse((native / "builder.json").exists())
        self.assertFalse((native / "container.json").exists())
        self.assertEqual(list(native.glob("volume-*_state")), [])

    def run_builder_gate(self, env, name=None, cleanup=True):
        script = """
import subprocess, sys
entry, name, cleanup = sys.argv[1:]
resource = subprocess.check_output(['bash', entry, '--builder-prepare', *([name] if name else [])], text=True).strip()
builder = subprocess.check_output(['bash', entry, '--builder-name', resource], text=True).strip()
status = subprocess.call(['bash', entry, '--resource-run', resource, '--',
                          'docker', 'buildx', 'build', '--builder', builder, '--load', '.'])
if cleanup == 'yes':
    subprocess.check_call(['bash', entry, '--resource-cleanup', resource])
    subprocess.check_call(['bash', entry, '--resource-complete', resource])
raise SystemExit(status)
"""
        return self.run_cli("--", sys.executable, "-c", script, str(ENTRY), name or "",
                            "yes" if cleanup else "no", env=env, timeout=20)

    def test_created_builder_and_its_state_are_removed_after_a_build(self):
        for cleanup in (True, False):
            with self.subTest(caller_cleanup=cleanup):
                native, env = self.docker_fixture()
                (native / "observer.release").touch()
                result = self.run_builder_gate(env, cleanup=cleanup)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((native / "native-terminal").exists())
                self.assert_builder_removed(native)
                self.assertIsNone(self.status()["gate"])
                shutil.rmtree(self.base / "bin")
                shutil.rmtree(native)

    def test_adopted_exclusive_builder_is_retained(self):
        native, env = self.docker_fixture()
        (native / "observer.release").touch()
        subprocess.run(["docker", "buildx", "create", "--name", "task-builder", "--driver", "docker-container"],
                       env=env, check=True, capture_output=True)
        result = self.run_builder_gate(env, name="task-builder")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("not task-exclusive", result.stderr)
        result = self.run_builder_gate({**env, "VALIDATION_BUILDER_EXCLUSIVE": "1"}, name="task-builder")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((native / "builders-removed").exists())
        self.assertEqual(json.loads((native / "builder.json").read_text())["Name"], "task-builder")
        self.assertFalse(json.loads((native / "container.json").read_text())["State"]["Running"])
        self.assertEqual(len(list(native.glob("volume-*_state"))), 1)
        self.assertIsNone(self.status()["gate"])

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
