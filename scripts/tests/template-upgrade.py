#!/usr/bin/env python3
"""Small real-Git lifecycle fixtures; full Rust render proof belongs to A/B use.

The fixture's public initializer deliberately has revision-specific generated
output. No private renderer or updater seam is substituted. These tests protect
Git custody/conflicts and input refusal; they do not claim Rust generation proof.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts/template-upgrade.sh"
ENV = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
           GIT_AUTHOR_NAME="fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
           GIT_COMMITTER_NAME="fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid",
           PYTHONDONTWRITEBYTECODE="1")
IDENTITY = {"service_name": "upgrade-fixture", "repository": "https://github.com/example/upgrade-fixture",
            "description": "Upgrade fixture", "codeowner": "@example"}

# This is an intentionally tiny public initializer for the Git protocol tests.
# Full initialization (Cargo, pinned rustfmt and OpenAPI) is exercised separately
# with actual template revisions, never inferred from this fixture.
INITIALIZER = '''#!/usr/bin/env python3
import json, os, pathlib, subprocess
root = pathlib.Path.cwd()
revision = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
identity = {key: os.environ[key.upper()] for key in ("service_name", "repository", "description", "codeowner")}
(root / "generated.txt").write_text("public initializer " + (root / "generation.txt").read_text())
lock = {"schema_version": 1, "state": "complete", "identity": identity,
        "profiles": {"database": "none", "agent_harness": "core"},
        "source": {"repository": "https://github.com/Dankosik/rust-service-template-rest",
                   "checkout_revision": revision, "provenance": "local-checkout"}}
(root / "template.lock").write_text(json.dumps(lock, sort_keys=True) + "\\n")
'''


def command(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, cwd=repo, env=env or ENV, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, check=False)
    if result.returncode:
        raise AssertionError(f"{args}: {result.stderr}")
    return result.stdout.strip()


def git(repo: Path, *args: str) -> str:
    return command(repo, "git", "-c", "core.hooksPath=/dev/null", *args)


def commit(repo: Path, message: str) -> str:
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", message)
    return git(repo, "rev-parse", "HEAD")


class UpgradeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="upgrade-fixture-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        git(self.source, "init", "-q", "--template=", "-b", "main")
        files = {"scripts/init-module.sh": '#!/usr/bin/env bash\nset -eu\npython3 scripts/lib/template_init.py\n',
                 "scripts/lib/template_init.py": INITIALIZER,
                 "scripts/lib/template_state.py": "# historical state owner\n",
                 "scripts/lib/template_profiles.json": '{"fixture":1}\n',
                 "rust-toolchain.toml": '[toolchain]\nchannel = "fixture"\n',
                 "Cargo.lock": '# fixture locked graph\n', "runtime.txt": "upstream A\n",
                 "generation.txt": "A\n", "retired.txt": "consumer may delete me\n", ".gitignore": "ignored.txt\n"}
        for path, data in files.items():
            file = self.source / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text(data)
        (self.source / "scripts/init-module.sh").chmod(0o755)
        self.a = commit(self.source, "source A")
        self.consumer = self.root / "consumer"
        command(self.root, "git", "clone", "--no-local", "-q", str(self.source), str(self.consumer))
        env = dict(ENV, **{key.upper(): value for key, value in IDENTITY.items()})
        command(self.consumer, "bash", "scripts/init-module.sh", env=env)
        self.initial = commit(self.consumer, "fully initialized A")
        (self.consumer / "business.txt").write_text("business survives\n")
        self.c = commit(self.consumer, "consumer business")

    def upgrade(self, *args: str, success: bool = True) -> dict:
        result = subprocess.run(["bash", str(TOOL), *map(str, args)], cwd=self.root,
                                env=ENV, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
        if not success:
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            return {"error": result.stderr}
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def proof(self, candidate: Path, **overrides: str) -> tuple[Path, Path]:
        result = self.upgrade("status", "--candidate", candidate)
        attempt = json.loads(Path(result["attempt"]).read_text())
        paths = []
        for kind in ("review", "validation"):
            checks = {"resolutions": "Fixture outcome inspected", "baseline_adoption": "Exact initial tree compared"} if kind == "review" else {
                "generated": "Fixture generated output inspected", "locked_graph": "Fixture lock unchanged",
                "migration_history": "Fixture append-only admission", "compatibility": "Fixture text contract checked"}
            value = {"schema_version": 1, "kind": kind, "verdict": "pass", "consumer_commit": attempt["consumer_commit"],
                     "baseline_commit": attempt["old_baseline"], "target_revision": attempt["target_revision"],
                     "content_tree": result["content_tree"], "summary": "Synthetic native-Git protocol evidence",
                     "checks": checks, "migration_dispositions": {issue: "Forward reconciliation reviewed" for issue in attempt["migration_issues"]}}
            value.update(overrides)
            path = self.root / f"{candidate.name}-{kind}.json"
            path.write_text(json.dumps(value))
            paths.append(path)
        return paths[0], paths[1]

    def seal(self, candidate: Path) -> dict:
        review, validation = self.proof(candidate)
        return self.upgrade("accept", "--candidate", candidate, "--review", review, "--validation", validation)

    def adopt(self) -> Path:
        candidate = self.root / "adoption"
        self.upgrade("adopt", "--consumer", self.consumer, "--source", self.source,
                     "--revision", self.a, "--destination", candidate, "--initial-commit", self.initial)
        self.seal(candidate)
        return candidate

    def next_source(self, letter: str) -> str:
        (self.source / "runtime.txt").write_text(f"upstream {letter}\n")
        (self.source / "generation.txt").write_text(letter + "\n")
        (self.source / "scripts/lib/template_init.py").write_text(
            INITIALIZER.replace("public initializer ", "new public initializer ")
        )
        return commit(self.source, "source " + letter)

    def test_adoption_native_conflicts_clone_custody_and_second_upgrade(self) -> None:
        # The original is deliberately dirty in all three ownership classes.
        (self.consumer / "business.txt").write_text("unstaged local work\n")
        (self.consumer / "untracked.txt").write_text("untracked local work\n")
        (self.consumer / "ignored.txt").write_text("ignored local work\n")
        original_status = git(self.consumer, "status", "--porcelain=v1", "--ignored")
        # The source checkout is already at B; adoption must still execute A's
        # historical helper and reproduce A's earlier committed full output.
        b = self.next_source("B")
        candidate = self.adopt()
        self.assertEqual(git(self.consumer, "rev-parse", "HEAD"), self.c)
        self.assertEqual(git(self.consumer, "status", "--porcelain=v1", "--ignored"), original_status)
        self.assertEqual((self.consumer / "business.txt").read_text(), "unstaged local work\n")
        self.assertEqual((self.consumer / "ignored.txt").read_text(), "ignored local work\n")
        self.assertEqual((candidate / "business.txt").read_text(), "business survives\n")
        original_lock = (self.consumer / "template.lock").read_bytes()
        # A normal full clone carries B0 solely through accepted parent edges.
        clone = self.root / "clone"
        command(self.root, "git", "clone", "--no-local", "-q", str(candidate), str(clone))
        baseline = json.loads((clone / "template.upgrade.json").read_text())["baseline"]["commit"]
        git(clone, "merge-base", "--is-ancestor", baseline, "HEAD")
        (clone / "runtime.txt").write_text("consumer customization\n")
        (clone / "retired.txt").unlink()
        commit(clone, "consumer customizations")
        upgrade = self.root / "upgrade-B"
        prepared = self.upgrade("prepare", "--consumer", clone, "--source", self.source,
                                "--revision", b, "--destination", upgrade)
        self.assertEqual(prepared["state"], "conflicted")
        self.assertIn("runtime.txt", prepared["conflicts"])
        self.assertEqual((upgrade / "generated.txt").read_text(), "new public initializer B\n")
        self.assertFalse((upgrade / "retired.txt").exists())
        self.assertEqual((upgrade / "template.lock").read_bytes(), original_lock)
        self.assertEqual(json.loads((upgrade / "template.upgrade.json").read_text())["source"]["revision"], self.a)
        (upgrade / "runtime.txt").write_text("resolved consumer and B\n")
        commit(upgrade, "resolve B explicitly")
        repeated = self.upgrade("prepare", "--consumer", clone, "--source", self.source,
                                "--revision", b, "--destination", upgrade)
        self.assertEqual(repeated["state"], "resolved-awaiting-proof")
        self.assertEqual((upgrade / "runtime.txt").read_text(), "resolved consumer and B\n")
        review, validation = self.proof(upgrade, content_tree="0" * 40)
        refusal = self.upgrade("accept", "--candidate", upgrade, "--review", review, "--validation", validation, success=False)
        self.assertIn("evidence does not admit", refusal["error"])
        self.seal(upgrade)
        self.assertEqual((upgrade / "template.lock").read_bytes(), original_lock)
        noop = self.upgrade("prepare", "--consumer", upgrade, "--source", self.source,
                           "--revision", b, "--destination", self.root / "unused")
        self.assertEqual(noop["state"], "no-op")
        self.assertFalse((self.root / "unused").exists())
        # New consumer work after B does not redefine B's pristine baseline.
        second = self.root / "second-clone"
        command(self.root, "git", "clone", "--no-local", "-q", str(upgrade), str(second))
        (second / "later-business.txt").write_text("after B\n")
        commit(second, "business after B")
        (self.source / "extra.txt").write_text("source C addition\n")
        c = commit(self.source, "source C")
        third = self.root / "upgrade-C"
        self.upgrade("prepare", "--consumer", second, "--source", self.source, "--revision", c, "--destination", third)
        commit(third, "adopt C content")
        self.seal(third)
        self.assertEqual((third / "later-business.txt").read_text(), "after B\n")
        self.assertEqual((third / "runtime.txt").read_text(), "resolved consumer and B\n")
        self.assertEqual((third / "extra.txt").read_text(), "source C addition\n")

    def test_refusals_do_not_create_custody_and_abort_retains_work(self) -> None:
        b = self.next_source("B")
        refusal = self.upgrade("prepare", "--consumer", self.consumer, "--source", self.source,
                               "--revision", b, "--destination", self.root / "before-adoption", success=False)
        self.assertIn("adopt the historical baseline", refusal["error"])
        wrong = self.upgrade("adopt", "--consumer", self.consumer, "--source", self.source, "--revision", b,
                             "--destination", self.root / "wrong-source", "--reconstruct", success=False)
        self.assertIn("exact historical source", wrong["error"])
        candidate = self.root / "reconstructed"
        self.upgrade("adopt", "--consumer", self.consumer, "--source", self.source, "--revision", self.a,
                     "--destination", candidate, "--reconstruct")
        (candidate / "operator-notes.txt").write_text("keep evidence\n")
        self.upgrade("abort", "--candidate", candidate)
        self.assertEqual((candidate / "operator-notes.txt").read_text(), "keep evidence\n")
        self.assertFalse((self.consumer / "template.upgrade.json").exists())
        self.assertEqual(self.upgrade("status", "--candidate", candidate)["state"], "aborted")

    def test_migration_collision_and_copied_control_cannot_be_accepted(self) -> None:
        candidate = self.adopt()
        consumer = self.root / "migration-consumer"
        command(self.root, "git", "clone", "--no-local", "-q", str(candidate), str(consumer))
        (consumer / "migrations").mkdir()
        (consumer / "migrations/20_business.sql").write_text("SELECT 20;\n")
        commit(consumer, "admitted migration 20")
        (self.source / "migrations").mkdir()
        (self.source / "migrations/10_template.sql").write_text("SELECT 10;\n")
        b = commit(self.source, "template migration 10")
        upgrade = self.root / "migration-upgrade"
        prepared = self.upgrade("prepare", "--consumer", consumer, "--source", self.source,
                                "--revision", b, "--destination", upgrade)
        self.assertEqual(prepared["migration_issues"], ["version-order:migrations/10_template.sql"])
        commit(upgrade, "merge is clean but migration invalid")
        review, validation = self.proof(upgrade)
        refusal = self.upgrade("accept", "--candidate", upgrade, "--review", review, "--validation", validation, success=False)
        self.assertIn("below the consumer maximum", refusal["error"])
        # Copying accepted JSON to a different commit never creates its parent edge.
        # Supply the real baseline objects without accepting them into ancestry,
        # so this refusal exercises custody rather than missing-object admission.
        baseline = json.loads((candidate / "template.upgrade.json").read_text())["baseline"]["commit"]
        git(self.consumer, "fetch", "--no-tags", str(candidate), baseline)
        (self.consumer / "template.upgrade.json").write_bytes((candidate / "template.upgrade.json").read_bytes())
        commit(self.consumer, "copy a control file without custody")
        refusal = self.upgrade("prepare", "--consumer", self.consumer, "--source", self.source, "--revision", b,
                               "--destination", self.root / "forged", success=False)
        self.assertIn("second-parent custody", refusal["error"])

    def test_seal_interruption_is_reconciled_without_a_second_commit(self) -> None:
        candidate = self.adopt()
        result = self.upgrade("status", "--candidate", candidate)
        seal = result["candidate_commit"]
        attempt_path = Path(result["attempt"])
        attempt = json.loads(attempt_path.read_text())
        # Recreate a crash after the ref CAS and before the sole metadata write.
        # The attempt/ref protocol is persisted public state, not a test hook.
        attempt["state"] = "sealing"
        attempt_path.write_text(json.dumps(attempt))
        (candidate / "template.upgrade.json").unlink()
        git(candidate, "read-tree", attempt["resolved_commit"])
        self.upgrade("accept", "--candidate", candidate, "--review", self.root / "not-read-review",
                     "--validation", self.root / "not-read-validation")
        self.assertEqual(git(candidate, "rev-parse", "HEAD"), seal)
        self.assertEqual(git(candidate, "status", "--porcelain=v1"), "")
        self.assertEqual(self.upgrade("status", "--candidate", candidate)["state"], "accepted")

    def test_reconstructed_adoption_requires_explicit_evidence_and_preserves_content(self) -> None:
        candidate = self.root / "legacy-adoption"
        self.upgrade("adopt", "--consumer", self.consumer, "--source", self.source, "--revision", self.a,
                     "--destination", candidate, "--reconstruct")
        self.assertFalse((candidate / "template.upgrade.json").exists())
        self.seal(candidate)
        record = json.loads((candidate / "template.upgrade.json").read_text())
        self.assertEqual(record["origin"], {"kind": "reconstructed", "initial_commit": None})
        self.assertEqual(git(candidate, "diff", "HEAD^1", "HEAD", "--name-only"), "template.upgrade.json")

    def test_ambient_credentials_and_git_hooks_are_not_inherited(self) -> None:
        # Observe the real public initializer environment and compare its tree
        # with an independently initialized consumer, without invoking Cargo.
        with (self.source / "scripts/lib/template_init.py").open("a") as script:
            script.write('''
for key in ("UPGRADE_FIXTURE_TOKEN", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_BUILD_RUSTC_WRAPPER"):
    assert key not in os.environ, key
(root / "render-environment.json").write_text(json.dumps({
    "debug": os.environ.get("CARGO_PROFILE_DEV_DEBUG"),
    "target": os.environ.get("CARGO_TARGET_DIR"),
}, sort_keys=True))
''')
        source = commit(self.source, "credential-denial fixture")
        for index, supplied in enumerate((None, "line-tables-only", "full")):
            with self.subTest(debug=supplied):
                clean = self.root / f"fresh-consumer-{index}"
                target = (self.root / f"target-{index}").resolve()
                command(self.root, "git", "clone", "--no-local", "-q", str(self.source), str(clean))
                env = {key: value for key, value in ENV.items()
                       if not key.startswith(("CARGO_", "RUSTFLAGS", "UPGRADE_FIXTURE_"))}
                env.update({key.upper(): value for key, value in IDENTITY.items()})
                env["CARGO_TARGET_DIR"] = str(target)
                if supplied == "line-tables-only":
                    env["CARGO_PROFILE_DEV_DEBUG"] = supplied
                command(clean, "bash", "scripts/init-module.sh", env=env)
                initial = commit(clean, "initialized credential-denial fixture")
                hook = clean / ".git/hooks/post-checkout"
                hook.write_text('#!/bin/sh\nexit 98\n')
                hook.chmod(0o755)
                candidate = self.root / f"scrubbed-{index}"
                env.update(UPGRADE_FIXTURE_TOKEN="synthetic-not-secret", RUSTFLAGS="--cfg forbidden",
                           CARGO_ENCODED_RUSTFLAGS="--cfg=forbidden", CARGO_PROFILE_RELEASE_DEBUG="full",
                           CARGO_BUILD_RUSTC_WRAPPER="forbidden", CARGO_TARGET_DIR="forbidden")
                if supplied is not None:
                    env["CARGO_PROFILE_DEV_DEBUG"] = supplied
                result = subprocess.run(["bash", str(TOOL), "adopt", "--consumer", str(clean), "--source", str(self.source),
                                         "--revision", source, "--initial-commit", initial, "--destination", str(candidate),
                                         "--cargo-target", str(target)], env=env,
                                        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse((candidate / ".git/hooks/post-checkout").exists())
                if supplied == "line-tables-only":
                    review, validation = self.proof(candidate)
                    receipt = json.loads(validation.read_text())
                    receipt["checks"]["generated"] = (
                        f"Fixture render: CARGO_PROFILE_DEV_DEBUG=line-tables-only; "
                        f"--cargo-target {target}; source {source}; initial tree "
                        + git(clean, "rev-parse", initial + "^{tree}")
                    )
                    validation.write_text(json.dumps(receipt))
                    self.upgrade("accept", "--candidate", candidate, "--review", review, "--validation", validation)
                    record = json.loads((candidate / "template.upgrade.json").read_text())
                    self.assertEqual(record["evidence"]["validation"]["record"], receipt)


if __name__ == "__main__":
    arguments = argparse.ArgumentParser()
    arguments.add_argument("--source", type=Path, default=ROOT)
    args, remaining = arguments.parse_known_args()
    TOOL = args.source.resolve() / "scripts/template-upgrade.sh"
    unittest.main(argv=[__file__, *remaining])
