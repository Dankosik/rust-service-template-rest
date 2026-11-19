#!/usr/bin/env python3
"""Compact unit tests for repository scripts, CI helpers, and template initializer."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]


def _load(rel_path: str, module_name: str) -> Any:
    spec = importlib.util.spec_from_file_location(module_name, ROOT / rel_path)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = mod
    spec.loader.exec_module(mod)
    return mod


ARCH = _load("scripts/check-architecture.py", "_test_arch")
IDEMP = _load("scripts/check-openapi-idempotency.py", "_test_idemp")
MIG = _load("scripts/migration-check.py", "_test_mig")
DETECT = _load(".github/actions/detect-changes/detect.py", "_test_detect")
PUB = _load(".github/actions/publish-image/publish.py", "_test_pub")
INIT = _load("template/init.py", "_test_init")
HARNESS = _load("scripts/harness-sync.py", "_test_harness")


class ArchitectureTests(unittest.TestCase):
    def test_rejects_forbidden_edge_and_cross_package_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/b/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text('#[path = "../../b/src/lib.rs"] mod leak;\n', encoding="utf-8")
            (root / "crates/b/src/lib.rs").write_text("", encoding="utf-8")
            policy = {
                "roles": {"leaf": {"rule": "leaf-only"}, "root": {"rule": "root", "allow_roles": ["leaf"]}},
                "members": {"crates/a/Cargo.toml": {"role": "leaf"}, "crates/b/Cargo.toml": {"role": "root"}},
            }
            meta = {
                "workspace_members": ["a", "b"],
                "packages": [
                    {
                        "id": "a",
                        "name": "a",
                        "manifest_path": str(root / "crates/a/Cargo.toml"),
                        "dependencies": [{"name": "b", "kind": None, "optional": False, "path": str(root / "crates/b")}],
                    },
                    {"id": "b", "name": "b", "manifest_path": str(root / "crates/b/Cargo.toml"), "dependencies": []},
                ],
            }
            issues = ARCH.check(meta, policy, root)
            self.assertTrue(any("forbids crates/b/Cargo.toml" in i for i in issues))
            self.assertTrue(any("#[path] reaches outside its package" in i for i in issues))


class OpenApiIdempotencyTests(unittest.TestCase):
    @staticmethod
    def _doc(paths: dict[str, Any]) -> dict[str, Any]:
        return {"openapi": "3.1.0", "paths": paths}

    @staticmethod
    def _idempotent_op() -> dict[str, Any]:
        return {
            "post": {
                "parameters": [{"name": "Idempotency-Key", "in": "header", "required": True}],
                "responses": {"200": {"headers": {"Idempotent-Replayed": {}}}},
            }
        }

    def test_detects_contract_removal_and_precedence_shadow(self) -> None:
        base = self._doc({"/v1/orders/{id}": self._idempotent_op()})
        removed = self._doc({"/v1/orders/{id}": {"post": {"responses": {"200": {}}}}})
        self.assertTrue(IDEMP.check_compatibility(base, removed))
        shadowed = self._doc({"/v1/orders/{id}": self._idempotent_op(), "/v1/orders/special": {"post": {"responses": {"200": {}}}}})
        self.assertTrue(IDEMP.check_compatibility(base, shadowed))
        self.assertEqual(IDEMP.check_compatibility(base, base), [])


class MigrationCheckTests(unittest.TestCase):
    @staticmethod
    def _git(repo: Path, *args: str) -> None:
        subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)

    def test_append_only_monotonic_enforcement(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self._git(repo, "init", "-q", "-b", "main")
            self._git(repo, "config", "user.email", "test@example.invalid")
            self._git(repo, "config", "user.name", "Test")
            (repo / "migrations").mkdir()
            (repo / "migrations/20260101000001_init.sql").write_text("SELECT 1;\n", encoding="utf-8")
            self._git(repo, "add", ".")
            self._git(repo, "commit", "-qm", "base")

            (repo / "migrations/20260102000001_next.sql").write_text("SELECT 2;\n", encoding="utf-8")
            added, _ = MIG.check_history(repo, "migrations", None)
            self.assertEqual(added, ["migrations/20260102000001_next.sql"])

            (repo / "migrations/20260100000001_old.sql").write_text("SELECT 0;\n", encoding="utf-8")
            with self.assertRaises(MIG.MigrationCheckError):
                MIG.check_history(repo, "migrations", None)
            (repo / "migrations/20260100000001_old.sql").unlink()

            (repo / "migrations/20260101000001_init.sql").write_text("SELECT 99;\n", encoding="utf-8")
            with self.assertRaises(MIG.MigrationCheckError):
                MIG.check_history(repo, "migrations", None)


class DetectChangesTests(unittest.TestCase):
    def test_classifies_surfaces_and_pruned_profiles(self) -> None:
        res = DETECT.classify(ROOT, ["crates/service/src/main.rs"])
        self.assertTrue(res["rust"])
        self.assertTrue(res["openapi"])
        self.assertFalse(res["grpc"])
        self.assertFalse(res["migrations"])

        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp)
            all_empty = DETECT.classify(empty, None)
            self.assertFalse(all_empty["grpc"])
            self.assertFalse(all_empty["migrations"])
            self.assertFalse(all_empty["template"])
            self.assertTrue(all_empty["rust"])


class PublishMetadataTests(unittest.TestCase):
    def test_main_and_release_metadata(self) -> None:
        sha = "a" * 40
        main_meta = PUB.compute_metadata(
            mode="main", candidate_sha=sha, repository="Org/Svc", default_branch="main",
            ref_name="main", ref="refs/heads/main", run_id="10", run_attempt="1", crate_version="1.2.3",
        )
        self.assertEqual(main_meta["image_repo"], "ghcr.io/org/svc")
        self.assertEqual(main_meta["promote_tags"], f"sha-{sha[:12]} main")

        rel_meta = PUB.compute_metadata(
            mode="release", candidate_sha=sha, repository="Org/Svc", default_branch="main",
            ref_name="v1.2.3", ref="refs/tags/v1.2.3", run_id="11", run_attempt="1", crate_version="1.2.3",
        )
        self.assertEqual(rel_meta["promote_tags"], "v1.2.3 latest")

        with self.assertRaises(PUB.PublishError):
            PUB.compute_metadata(
                mode="release", candidate_sha=sha, repository="Org/Svc", default_branch="main",
                ref_name="v1.2.4", ref="refs/tags/v1.2.4", run_id="11", run_attempt="1", crate_version="1.2.3",
            )


class TemplateInitValidationTests(unittest.TestCase):
    def test_rejects_invalid_profiles_and_reserved_names(self) -> None:
        base = {"service_name": "orders-api", "repository": "https://github.com/org/orders", "description": "Orders", "codeowner": "@org/team"}
        self.assertEqual(INIT.validate_inputs(INIT.InitInputs(**base)).service_name, "orders-api")
        with self.assertRaises(INIT.InitError):
            INIT.validate_inputs(INIT.InitInputs(**{**base, "service_name": "async"}))
        with self.assertRaises(INIT.InitError):
            INIT.validate_inputs(INIT.InitInputs(**{**base, "jobs": "postgres"}))
        with self.assertRaises(INIT.InitError):
            INIT.validate_inputs(INIT.InitInputs(**{**base, "service_auth": "jwt"}))
        resolved = INIT.validate_inputs(INIT.InitInputs(**{**base, "outbound_auth": "oauth2-client-credentials"}))
        self.assertEqual(resolved.outbound_http, "bounded")


class HarnessWorkspaceCratesTests(unittest.TestCase):
    def test_workspace_crates_reference_matches_cargo_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            ref = repo / ".agents/skills/rust-dependencies/references/workspace-crates.md"
            ref.parent.mkdir(parents=True)
            (repo / "Cargo.toml").write_text(
                '[workspace.dependencies]\nmediatype = { version = "0.23.0", default-features = false }\n',
                encoding="utf-8",
            )
            ref.write_text(
                "# Workspace utility crates\n\n"
                "- **MIME parsing** — `mediatype`: guard empty unquoted parameters.\n"
                "- **RAII cleanup** — `std::mem::DropGuard`: Rust 1.100 std.\n",
                encoding="utf-8",
            )
            self.assertEqual(HARNESS._check_workspace_crates_reference(repo, source_template=True), [])

            ref.write_text(
                "# Workspace utility crates\n\n"
                "- **Retired crate** — `mime`: removed from workspace.\n",
                encoding="utf-8",
            )
            stale = HARNESS._check_workspace_crates_reference(repo, source_template=True)
            self.assertTrue(any("crate `mime` is not declared" in item for item in stale), stale)
            self.assertEqual(HARNESS._check_workspace_crates_reference(repo, source_template=False), [])

            ref.write_text("# Workspace utility crates\n\nNo bullets here.\n", encoding="utf-8")
            empty = HARNESS._check_workspace_crates_reference(repo, source_template=True)
            self.assertTrue(any("contains no crate entries" in item for item in empty), empty)

            ref.write_text("# Workspace utility crates\n\n- malformed bullet\n", encoding="utf-8")
            malformed = HARNESS._check_workspace_crates_reference(repo, source_template=False)
            self.assertTrue(any("bullet must match" in item for item in malformed), malformed)


class WorktreeSyncTests(unittest.TestCase):
    SCRIPT = ROOT / "scripts/worktree-sync.py"

    @staticmethod
    def _run(cmd: list[str], cwd: Path, *, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=check)

    def _setup_repo(self, tmp: Path) -> tuple[Path, Path, Path, Path]:
        origin = tmp / "origin.git"
        primary = tmp / "repo"
        pusher = tmp / "pusher"
        self._run(["git", "init", "--bare", "-q", "-b", "main", str(origin)], tmp)
        self._run(["git", "clone", "-q", str(origin), str(primary)], tmp)
        for repo in (primary,):
            self._run(["git", "config", "user.email", "test@example.invalid"], repo)
            self._run(["git", "config", "user.name", "Test"], repo)
        (primary / "crates/foo").mkdir(parents=True)
        (primary / "crates/bar").mkdir(parents=True)
        (primary / "crates/grpc-contracts").mkdir(parents=True)
        (primary / "quality").mkdir(parents=True)
        (primary / "crates/foo/a.rs").write_text("fn a() {}\n", encoding="utf-8")
        (primary / "crates/foo/old.rs").write_text("fn old() {}\n", encoding="utf-8")
        (primary / "crates/bar/b.rs").write_text("fn b() {}\n", encoding="utf-8")
        (primary / "crates/grpc-contracts/lib.rs").write_text("pub struct Rpc;\n", encoding="utf-8")
        (primary / "quality/architecture.json").write_text(
            '{"version":1,"members":{"crates/grpc-contracts/Cargo.toml":{"role":"contract"}}}\n',
            encoding="utf-8",
        )
        self._run(["git", "add", "."], primary)
        self._run(["git", "commit", "-qm", "init"], primary)
        self._run(["git", "push", "-q", "-u", "origin", "main"], primary)
        self._run(["git", "clone", "-q", str(origin), str(pusher)], tmp)
        self._run(["git", "config", "user.email", "test@example.invalid"], pusher)
        self._run(["git", "config", "user.name", "Test"], pusher)
        wt = tmp / "repo.feat-one"
        self._run(["git", "worktree", "add", "-q", "--no-track", "-b", "feat/one", str(wt), "origin/main"], primary)
        return origin, primary, pusher, wt

    def test_sync_main_edge_cases(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            _, primary, pusher, wt = self._setup_repo(Path(raw))

            (pusher / "crates/bar/b.rs").write_text("fn b() { /* v2 */ }\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "advance main"], pusher)
            self._run(["git", "push", "-q", "origin", "main"], pusher)

            # Dirty checked-out main refuses even when status.showUntrackedFiles=no
            self._run(["git", "config", "status.showUntrackedFiles", "no"], primary)
            (primary / "untracked.txt").write_text("wip\n", encoding="utf-8")
            dirty = self._run([sys.executable, str(self.SCRIPT), "sync-main"], wt, check=False)
            self.assertEqual(dirty.returncode, 1)
            self.assertIn("uncommitted changes", dirty.stderr)
            (primary / "untracked.txt").unlink()
            self._run(["git", "config", "--unset", "status.showUntrackedFiles"], primary)

            # Clean checked-out main fast-forwards
            ok = self._run([sys.executable, str(self.SCRIPT), "sync-main"], wt)
            self.assertIn("fast-forwarded checked-out main", ok.stdout)

            # Strictly-ahead checked-out main must fail closed (not report fast-forward)
            (primary / "local_only.txt").write_text("ahead\n", encoding="utf-8")
            self._run(["git", "add", "local_only.txt"], primary)
            self._run(["git", "commit", "-qm", "local ahead"], primary)
            ahead = self._run([sys.executable, str(self.SCRIPT), "sync-main"], wt, check=False)
            self.assertEqual(ahead.returncode, 1)
            self.assertIn("refusing non-fast-forward update", ahead.stderr)
            self._run(["git", "reset", "--hard", "origin/main"], primary)

            # Non-checked-out main fast-forwards and single-branch clone updates refs/remotes/origin/main
            self._run(["git", "checkout", "-q", "--detach"], primary)
            (pusher / "crates/bar/b.rs").write_text("fn b() { /* v3 */ }\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "advance main v3"], pusher)
            self._run(["git", "push", "-q", "origin", "main"], pusher)
            self._run(["git", "config", "remote.origin.fetch", "+refs/heads/feat/one:refs/remotes/origin/feat/one"], primary)
            non_checked = self._run([sys.executable, str(self.SCRIPT), "sync-main"], wt)
            self.assertIn("fast-forwarded local main", non_checked.stdout)

            # Unborn branch checked out in a worktree must fail closed
            unborn = Path(raw) / "repo.orphan"
            self._run(["git", "worktree", "add", "-q", "--orphan", "-b", "release", str(unborn)], primary)
            self._run(["git", "push", "-q", "origin", "HEAD:refs/heads/release"], pusher)
            refused = self._run([sys.executable, str(self.SCRIPT), "sync-main", "--branch", "release"], wt, check=False)
            self.assertEqual(refused.returncode, 1)
            self.assertIn("unborn branch", refused.stderr)

    def test_drift_overlap_renames_staged_and_architecture_policy(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            _, primary, pusher, wt = self._setup_repo(Path(raw))

            # Up-to-date from subdirectory
            up = self._run([sys.executable, str(self.SCRIPT), "drift", "--no-fetch"], wt / "crates/foo")
            self.assertIn("status: up-to-date", up.stdout)

            # Upstream edits shared contract crate from quality/architecture.json -> shared-or-same-crate
            (pusher / "crates/grpc-contracts/lib.rs").write_text("pub struct RpcV2;\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "contract bump"], pusher)
            self._run(["git", "push", "-q", "origin", "main"], pusher)
            shared = self._run([sys.executable, str(self.SCRIPT), "drift"], wt)
            self.assertIn("status: shared-or-same-crate", shared.stdout)
            self.assertIn("crates/grpc-contracts/lib.rs", shared.stdout)

            # Upstream renames crates/foo/old.rs -> crates/foo/renamed.rs; branch stages edit to old.rs then reverts working tree
            self._run(["git", "mv", "crates/foo/old.rs", "crates/foo/renamed.rs"], pusher)
            self._run(["git", "commit", "-qm", "rename old"], pusher)
            self._run(["git", "push", "-q", "origin", "main"], pusher)
            (wt / "crates/foo/old.rs").write_text("fn old() { /* staged */ }\n", encoding="utf-8")
            self._run(["git", "add", "crates/foo/old.rs"], wt)
            (wt / "crates/foo/old.rs").write_text("fn old() {}\n", encoding="utf-8")
            overlap = self._run([sys.executable, str(self.SCRIPT), "drift"], wt / "crates/foo")
            self.assertIn("status: direct-overlap", overlap.stdout)
            self.assertIn("crates/foo/old.rs", overlap.stdout)

            # Diverged local main should not recommend sync-main
            (primary / "diverge.txt").write_text("local\n", encoding="utf-8")
            self._run(["git", "add", "diverge.txt"], primary)
            self._run(["git", "commit", "-qm", "diverge local main"], primary)
            diverged = self._run([sys.executable, str(self.SCRIPT), "drift", "--no-fetch"], wt)
            self.assertIn("local_main: diverged from origin/main", diverged.stdout)
            self.assertNotIn("sync-main", diverged.stdout)

    def test_retire_guards_regular_and_squash_merges(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            raw_path = Path(raw)
            origin, primary, pusher, wt = self._setup_repo(raw_path)

            # Refuses default branch, case-variant ref on case-insensitive FS, and --branch override targeting main
            refuse_main = self._run([sys.executable, str(self.SCRIPT), "retire", "main", "--no-fetch"], primary, check=False)
            self.assertEqual(refuse_main.returncode, 1)
            self.assertIn("protected branch", refuse_main.stderr)

            refuse_case = self._run([sys.executable, str(self.SCRIPT), "retire", "MAIN", "--no-fetch"], primary, check=False)
            self.assertEqual(refuse_case.returncode, 1)
            self.assertIn("does not exist", refuse_case.stderr)
            self.assertEqual(
                self._run(["git", "show-ref", "--verify", "--quiet", "refs/heads/main"], primary, check=False).returncode, 0
            )

            self._run(["git", "push", "-q", "origin", "main:refs/heads/integration"], primary)
            self._run(["git", "fetch", "-q", "origin", "+refs/heads/integration:refs/remotes/origin/integration"], primary)
            self._run(["git", "checkout", "-q", "--detach"], primary)
            refuse_main_override = self._run(
                [sys.executable, str(self.SCRIPT), "retire", "main", "--branch", "integration", "--no-fetch"],
                primary, check=False,
            )
            self.assertEqual(refuse_main_override.returncode, 1)
            self.assertIn("protected branch", refuse_main_override.stderr)
            self._run(["git", "checkout", "-q", "main"], primary)

            # Refuses primary worktree on a non-default branch
            self._run(["git", "checkout", "-q", "-b", "feat/primary"], primary)
            refuse_primary = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/primary", "--no-fetch"], wt, check=False)
            self.assertEqual(refuse_primary.returncode, 1)
            self.assertIn("primary worktree", refuse_primary.stderr)
            self._run(["git", "checkout", "-q", "main"], primary)
            self._run(["git", "branch", "-q", "-d", "feat/primary"], primary)

            # Refuses symbolic branch refs (never dereferences into main)
            self._run(["git", "symbolic-ref", "refs/heads/sym-main", "refs/heads/main"], primary)
            refuse_sym = self._run([sys.executable, str(self.SCRIPT), "retire", "sym-main", "--no-fetch"], primary, check=False)
            self.assertEqual(refuse_sym.returncode, 1)
            self.assertIn("symbolic ref", refuse_sym.stderr)
            self._run(["git", "symbolic-ref", "-d", "refs/heads/sym-main"], primary)

            # Refuses unmerged branch without PR proof, CWD inside worktree, dirty (incl. showUntrackedFiles=no), locked, in-progress merge, nested & duplicate worktrees
            (wt / "crates/foo/a.rs").write_text("fn a() { /* feat */ }\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "feat commit"], wt)
            unmerged = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], primary, check=False)
            self.assertEqual(unmerged.returncode, 1)
            self.assertIn("not an ancestor", unmerged.stderr)

            inside = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], wt, check=False)
            self.assertEqual(inside.returncode, 1)
            self.assertIn("inside target worktree", inside.stderr)

            self._run(["git", "config", "status.showUntrackedFiles", "no"], wt)
            (wt / "untracked.tmp").write_text("wip\n", encoding="utf-8")
            dirty = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], primary, check=False)
            self.assertEqual(dirty.returncode, 1)
            self.assertIn("uncommitted or untracked", dirty.stderr)
            (wt / "untracked.tmp").unlink()
            self._run(["git", "config", "--unset", "status.showUntrackedFiles"], wt)

            self._run(["git", "worktree", "lock", str(wt)], primary)
            locked = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], primary, check=False)
            self.assertEqual(locked.returncode, 1)
            self.assertIn("locked", locked.stderr)
            self._run(["git", "worktree", "unlock", str(wt)], primary)

            # Duplicate checkout of the same branch refuses
            wt_dup = raw_path / "repo.feat-dup"
            self._run(["git", "worktree", "add", "-q", "--force", str(wt_dup), "feat/one"], primary)
            dup = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], primary, check=False)
            self.assertEqual(dup.returncode, 1)
            self.assertIn("multiple worktrees", dup.stderr)
            self._run(["git", "worktree", "remove", str(wt_dup)], primary)

            # Nested worktree inside target refuses
            wt_nested = wt / ".nested-wt"
            self._run(["git", "worktree", "add", "-q", "--detach", str(wt_nested), "origin/main"], primary)
            nested = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/one", "--no-fetch"], primary, check=False)
            self.assertEqual(nested.returncode, 1)
            self.assertIn("nested worktree", nested.stderr)
            self._run(["git", "worktree", "remove", str(wt_nested)], primary)

            # Squash-merge verified via gh stub: validates PR number, mergeCommit in origin/main, exact OID, and prunes deleted remote ref
            self._run(["git", "push", "-q", "-u", "origin", "feat/one"], wt)
            self._run(["git", "branch", "-q", "-D", "feat/one"], origin)
            (pusher / "crates/foo/a.rs").write_text("fn a() { /* squashed */ }\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "squash feat/one (#99)"], pusher)
            self._run(["git", "push", "-q", "origin", "main"], pusher)
            squash_oid = self._run(["git", "rev-parse", "HEAD"], pusher).stdout.strip()
            feat_oid = self._run(["git", "rev-parse", "refs/heads/feat/one"], primary).stdout.strip()

            fake_bin = raw_path / "bin"
            fake_bin.mkdir()
            gh_stub = fake_bin / "gh"
            env = {**os.environ, "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}"}

            # Rejects non-numeric/flag PR values before calling gh
            bad_pr_arg = subprocess.run(
                [sys.executable, str(self.SCRIPT), "retire", "feat/one", "--pr=--version"],
                cwd=primary, env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(bad_pr_arg.returncode, 1)
            self.assertIn("invalid PR number", bad_pr_arg.stderr)

            # Rejects PR whose mergeCommit is not in origin/main
            gh_stub.write_text(
                f'#!/usr/bin/env python3\nimport json, sys\nassert sys.argv[-2:] == ["--", "99"], sys.argv\n'
                f'print(json.dumps({{"state":"MERGED","headRefName":"feat/one","baseRefName":"main","headRefOid":"{feat_oid}","mergeCommit":{{"oid":"{"a"*40}"}}}}))\n',
                encoding="utf-8",
            )
            gh_stub.chmod(0o755)
            unlanded = subprocess.run(
                [sys.executable, str(self.SCRIPT), "retire", "feat/one", "--pr", "99"],
                cwd=primary, env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(unlanded.returncode, 1)
            self.assertIn("is not in origin/main", unlanded.stderr)

            # Rejects mismatched headRefOid
            gh_stub.write_text(
                f'#!/usr/bin/env python3\nimport json, sys\nassert sys.argv[-2:] == ["--", "99"], sys.argv\n'
                f'print(json.dumps({{"state":"MERGED","headRefName":"feat/one","baseRefName":"main","headRefOid":"{"0"*40}","mergeCommit":{{"oid":"{squash_oid}"}}}}))\n',
                encoding="utf-8",
            )
            bad_oid = subprocess.run(
                [sys.executable, str(self.SCRIPT), "retire", "feat/one", "--pr", "99"],
                cwd=primary, env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(bad_oid.returncode, 1)
            self.assertIn("does not match local feat/one", bad_oid.stderr)

            # Succeeds when state, branch, base, headRefOid, and mergeCommit.oid all match; also removes branch config
            gh_stub.write_text(
                f'#!/usr/bin/env python3\nimport json, sys\nassert sys.argv[-2:] == ["--", "99"], sys.argv\n'
                f'print(json.dumps({{"state":"MERGED","headRefName":"feat/one","baseRefName":"main","headRefOid":"{feat_oid}","mergeCommit":{{"oid":"{squash_oid}"}}}}))\n',
                encoding="utf-8",
            )
            ok_squash = subprocess.run(
                [sys.executable, str(self.SCRIPT), "retire", "feat/one", "--pr", "99"],
                cwd=primary, env=env, capture_output=True, text=True, check=True,
            )
            self.assertIn("removed worktree:", ok_squash.stdout)
            self.assertIn("deleted local branch: feat/one", ok_squash.stdout)
            self.assertIn("pruned stale remote-tracking ref: origin/feat/one", ok_squash.stdout)
            self.assertFalse(wt.exists())
            self.assertNotEqual(
                self._run(["git", "config", "--get", "branch.feat/one.remote"], primary, check=False).returncode, 0
            )

            # Regular ancestry merge retires cleanly without --pr and retains remote-tracking ref if still on remote
            wt2 = raw_path / "repo.feat-two"
            self._run(["git", "worktree", "add", "-q", "--no-track", "-b", "feat/two", str(wt2), "origin/main"], primary)
            (wt2 / "crates/bar/b.rs").write_text("fn b() { /* two */ }\n", encoding="utf-8")
            self._run(["git", "commit", "-qam", "feat two"], wt2)
            self._run(["git", "push", "-q", "origin", "feat/two:feat/two", "feat/two:main"], wt2)
            ok_ancestor = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/two"], primary)
            self.assertIn("ancestor of origin/main", ok_ancestor.stdout)
            self.assertNotIn("pruned stale remote-tracking ref", ok_ancestor.stdout)
            self.assertFalse(wt2.exists())
            self.assertNotEqual(
                self._run(["git", "show-ref", "--verify", "--quiet", "refs/heads/feat/two"], primary, check=False).returncode, 0
            )

            # Branch-only retirement (no linked worktree) works cleanly
            self._run(["git", "branch", "feat/branch-only", "origin/main"], primary)
            ok_branch_only = self._run([sys.executable, str(self.SCRIPT), "retire", "feat/branch-only", "--no-fetch"], primary)
            self.assertIn("deleted local branch: feat/branch-only", ok_branch_only.stdout)
            self.assertNotIn("removed worktree:", ok_branch_only.stdout)


if __name__ == "__main__":
    unittest.main()
