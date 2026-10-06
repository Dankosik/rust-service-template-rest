#!/usr/bin/env python3
"""Blackbox queue/lifetime contracts; no Cargo, Docker, or shared lock access."""

import json
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
