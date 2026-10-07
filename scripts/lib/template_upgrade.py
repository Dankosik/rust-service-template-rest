#!/usr/bin/env python3
"""Complete rendered baseline custody and isolated, native Git upgrades.

No command updates the original consumer. Acceptance adds a control record and
a baseline parent to an already committed and independently reviewed candidate.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any

import template_state as state

CONTROL = "template.upgrade.json"
REF = "refs/heads/template-upgrade"
OID = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
RECIPE_PATHS = (
    "scripts/init-module.sh", "scripts/lib/template_init.py",
    "scripts/lib/template_state.py", "scripts/lib/template_profiles.json",
    "rust-toolchain.toml", "Cargo.lock",
)


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def shape(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise state.Refusal(f"{label} has an unsupported shape")
    return value


def object_id(value: Any) -> str:
    if not isinstance(value, str) or not OID.fullmatch(value):
        raise state.Refusal("an exact full Git object ID is required")
    return value


def sha256(value: Any) -> str:
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise state.Refusal("invalid SHA-256 identity")
    return value


def run(repo: Path, args: list[str], *, data: bytes | None = None,
        allowed: tuple[int, ...] = (0,), env: dict[str, str] | None = None) -> subprocess.CompletedProcess[bytes]:
    command = ["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
               "-c", "core.attributesFile=/dev/null", "-c", "core.autocrlf=false",
               "-c", "commit.gpgSign=false", "-c", "gc.auto=0", "-C", str(repo), *args]
    result = subprocess.run(command, input=data, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env=env, check=False)
    if result.returncode not in allowed:
        raise state.Refusal(f"Git {args[0]} failed (exit {result.returncode}): "
                            + result.stderr.decode(errors="replace")[:2048])
    return result


def git(repo: Path, args: list[str], **kwargs: Any) -> bytes:
    return run(repo, args, **kwargs).stdout


def rev(repo: Path, value: str = "HEAD") -> str:
    return object_id(git(repo, ["rev-parse", "--verify", value]).decode().strip())


def blob(repo: Path, commit: str, path: str) -> bytes | None:
    entries = git(repo, ["ls-tree", "-z", commit, "--", path])
    if not entries:
        return None
    metadata, found = entries.rstrip(b"\0").split(b"\t", 1)
    mode, kind, oid = metadata.decode().split()
    if found.decode() != path or kind != "blob" or mode not in {"100644", "100755"}:
        raise state.Refusal(f"{path} must be a regular committed file")
    return git(repo, ["cat-file", "blob", oid])


def lock_at(repo: Path, commit: str) -> tuple[dict[str, Any], bytes]:
    raw = blob(repo, commit, state.LOCK_NAME)
    if raw is None:
        raise state.Refusal("consumer requires a committed, complete template.lock")
    lock = state.validate_lock(state.parse_json_bytes(raw, state.LOCK_NAME))
    if lock["state"] != "complete":
        raise state.Refusal("template.lock is incomplete; recover initialization first")
    return lock, raw


def choices(lock: dict[str, Any]) -> dict[str, Any]:
    return {"identity": lock["identity"], "profiles": lock["profiles"]}


def entries(repo: Path, tree: str) -> list[dict[str, str]]:
    result = []
    for raw in git(repo, ["ls-tree", "-r", "-z", tree]).split(b"\0"):
        if not raw:
            continue
        mode, kind, oid, path = state._parse_tree_record(raw)
        if mode == "120000" and state._generated_skill_link(path, git(repo, ["cat-file", kind, oid])) is None:
            raise state.Refusal(f"unsupported Git link: {path}")
        result.append({"path": path, "mode": mode, "object": oid})
    folded = [item["path"].casefold() for item in result]
    if len(folded) != len(set(folded)):
        raise state.Refusal("tree has case-folding path aliases")
    return result


def commit(repo: Path, tree: str, parents: list[str], message: str) -> str:
    args = ["commit-tree", tree]
    for parent in parents:
        args.extend(["-p", parent])
    return object_id(git(repo, args, data=(message + "\n").encode()).decode().strip())


def import_commit(source: Path, target: Path, revision: str) -> None:
    # Native pack transport copies reachable history without alternates, local
    # clone hardlinks, fetch, hooks, or source configuration in the destination.
    with tempfile.TemporaryFile() as packed:
        command = ["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                   "-C", str(source), "pack-objects", "--stdout", "--revs"]
        result = subprocess.run(command, input=(revision + "\n").encode(), stdout=packed,
                                stderr=subprocess.PIPE, check=False)
        if result.returncode:
            raise state.Refusal("cannot copy complete local Git history; obtain missing objects first")
        packed.seek(0)
        result = subprocess.run(["git", "-C", str(target), "index-pack", "--stdin"],
                                stdin=packed, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
        if result.returncode:
            raise state.Refusal("cannot import copied Git history")
    if rev(target, revision + "^{commit}") != revision:
        raise state.Refusal("copied revision identity differs")


def init_repo(path: Path, source: Path) -> None:
    path.mkdir()
    algorithm = git(source, ["rev-parse", "--show-object-format"]).decode().strip()
    git(path, ["init", "-q", "--template=", "--initial-branch=template-upgrade", f"--object-format={algorithm}"])
    git(path, ["config", "user.name", "Template upgrade"])
    git(path, ["config", "user.email", "template-upgrade@example.invalid"])
    git(path, ["config", "core.hooksPath", "/dev/null"])
    git(path, ["config", "core.autocrlf", "false"])


def index_tree(repo: Path, tree: str, replacements: dict[str, bytes | None]) -> str:
    with tempfile.TemporaryDirectory(prefix="upgrade-index-") as temporary:
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(temporary) / "index"))
        git(repo, ["read-tree", tree], env=env)
        for path, raw in replacements.items():
            if raw is None:
                git(repo, ["update-index", "--force-remove", "--", path], env=env)
            else:
                oid = git(repo, ["hash-object", "-w", "--stdin", "--no-filters"], data=raw).decode().strip()
                git(repo, ["update-index", "--add", "--cacheinfo", f"100644,{oid},{path}"], env=env)
        return rev_output(git(repo, ["write-tree"], env=env))


def rev_output(raw: bytes) -> str:
    return object_id(raw.decode().strip())


def content_tree(repo: Path, revision: str) -> str:
    return index_tree(repo, rev(repo, revision + "^{tree}"), {CONTROL: None})


def render(source: Path, revision: str, lock: dict[str, Any], target: Path,
           baseline_parent: str | None, cache: Path | None) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="template-upgrade-render-") as temporary:
        stage = Path(temporary) / "source"
        init_repo(stage, source)
        import_commit(source, stage, revision)
        git(stage, ["update-ref", REF, revision])
        # The shared snapshot admission preserves executable bits and only the
        # already-supported generated skill links, before any code executes.
        snapshot = Path(temporary) / "snapshot"
        state.snapshot_tree(stage, snapshot, revision)
        for child in snapshot.iterdir():
            shutil.move(str(child), stage / child.name)
        git(stage, ["read-tree", revision])
        inputs = {}
        for path in RECIPE_PATHS:
            if blob(stage, revision, path) is None:
                raise state.Refusal(f"historical public initializer input is missing: {path}")
            inputs[path] = rev(stage, f"{revision}:{path}")
        recipe = {"source_tree": rev(stage, revision + "^{tree}"), "inputs": inputs}
        env = dict(os.environ)
        if cache is not None:
            env["CARGO_TARGET_DIR"] = str(cache)
        # Environment input names are the initializer's public interface and
        # allow old revisions to ignore later selectors normalized to none.
        env.update({key.upper(): value for group in choices(lock).values() for key, value in group.items()})
        outcome = subprocess.run(["bash", str(stage / "scripts/init-module.sh"), "--repo", str(stage)],
                                 cwd=stage, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
        if outcome.returncode:
            raise state.Refusal("complete public initialization failed; make the historical pinned "
                                "toolchain and locked offline dependencies available:\n"
                                + outcome.stderr.decode(errors="replace")[-8192:])
        generated_lock = state.load_lock(stage, required=True)
        if (generated_lock is None or generated_lock["state"] != "complete"
                or choices(generated_lock) != choices(lock)
                or generated_lock["source"]["checkout_revision"] != revision):
            raise state.Refusal("rendered lock contradicts requested source/identity/profile choices")
        if (stage / CONTROL).exists():
            raise state.Refusal("a template render may not contain accepted upgrade custody")
        # No checkout filters or line-ending normalization may redefine the
        # actual public initializer output bytes.
        git(stage, ["read-tree", "--empty"])
        lines = []
        for root, directories, files in os.walk(stage, followlinks=False):
            directories[:] = sorted(name for name in directories if name != ".git")
            names = sorted(files + [name for name in directories if (Path(root) / name).is_symlink()])
            directories[:] = [name for name in directories if not (Path(root) / name).is_symlink()]
            for name in names:
                file = Path(root) / name
                relative = file.relative_to(stage).as_posix()
                state.safe_relative(relative)
                if file.is_symlink():
                    raw = os.readlink(file).encode()
                    if state._generated_skill_link(relative, raw) is None:
                        raise state.Refusal("initializer produced an unsupported link")
                    mode = "120000"
                elif file.is_file():
                    raw = file.read_bytes()
                    mode = "100755" if file.stat().st_mode & 0o111 else "100644"
                else:
                    raise state.Refusal("initializer produced a non-file")
                oid = rev_output(git(stage, ["hash-object", "-w", "--stdin", "--no-filters"], data=raw))
                lines.append(f"{mode} {oid}\t{relative}".encode() + b"\0")
        git(stage, ["update-index", "-z", "--index-info"], data=b"".join(lines))
        tree = rev_output(git(stage, ["write-tree"]))
        entries(stage, tree)
        rendered = commit(stage, tree, [], "Complete public template render")
        import_commit(stage, target, rendered)
        baseline = commit(target, tree, [baseline_parent] if baseline_parent else [], "Rendered template baseline")
        manifest = {"source_revision": revision, "choices": choices(lock), "recipe": recipe,
                    "tree": tree, "files": entries(target, tree)}
        return {"commit": baseline, "tree": tree, "manifest_sha256": digest(canonical(manifest)), "recipe": recipe}


def verify_baseline(repo: Path, record: dict[str, Any]) -> None:
    baseline = shape(record["baseline"], {"commit", "tree", "manifest_sha256", "recipe"}, "baseline")
    object_id(baseline["commit"])
    object_id(baseline["tree"])
    sha256(baseline["manifest_sha256"])
    recipe = shape(baseline["recipe"], {"source_tree", "inputs"}, "render recipe")
    object_id(recipe["source_tree"])
    shape(recipe["inputs"], set(RECIPE_PATHS), "render inputs")
    for oid in recipe["inputs"].values():
        object_id(oid)
    if rev(repo, baseline["commit"] + "^{tree}") != baseline["tree"]:
        raise state.Refusal("baseline commit/tree mismatch")
    manifest = {"source_revision": record["source"]["revision"], "choices": record["choices"],
                "recipe": recipe, "tree": baseline["tree"], "files": entries(repo, baseline["tree"])}
    if digest(canonical(manifest)) != baseline["manifest_sha256"]:
        raise state.Refusal("render manifest identity mismatch")
    baseline_lock, _ = lock_at(repo, baseline["commit"])
    if choices(baseline_lock) != record["choices"] or baseline_lock["source"]["checkout_revision"] != record["source"]["revision"]:
        raise state.Refusal("baseline lock contradicts accepted recipe")


def accepted(repo: Path, head: str, lock: dict[str, Any], raw_lock: bytes) -> dict[str, Any] | None:
    raw = blob(repo, head, CONTROL)
    if raw is None:
        return None
    record = shape(state.parse_json_bytes(raw, CONTROL), {
        "schema_version", "source", "baseline", "choices", "initial_lock_sha256",
        "origin", "content_tree", "evidence", "previous_baseline",
    }, CONTROL)
    if type(record["schema_version"]) is not int or record["schema_version"] != 1:
        raise state.Refusal("unsupported upgrade schema")
    source = shape(record["source"], {"repository", "revision"}, "accepted source")
    if source["repository"] != state.TEMPLATE_REPOSITORY:
        raise state.Refusal("unsupported accepted source identity")
    object_id(source["revision"])
    if record["choices"] != choices(lock) or record["initial_lock_sha256"] != digest(raw_lock):
        raise state.Refusal("accepted custody contradicts original initialization lock")
    origin = shape(record["origin"], {"kind", "initial_commit"}, "baseline origin")
    if origin["kind"] not in {"captured", "reconstructed"}:
        raise state.Refusal("unsupported baseline origin")
    if origin["initial_commit"] is not None:
        object_id(origin["initial_commit"])
    elif origin["kind"] == "captured":
        raise state.Refusal("captured baseline requires its initialization commit")
    object_id(record["content_tree"])
    if record["previous_baseline"] is not None:
        object_id(record["previous_baseline"])
    verify_baseline(repo, record)
    # Follow the consumer's first-parent line to the commit which introduced
    # these exact bytes; a copied JSON file is not acceptance.
    sealing = None
    for line in git(repo, ["rev-list", "--first-parent", head]).decode().splitlines():
        if blob(repo, line, CONTROL) != raw:
            break
        sealing = line
    if sealing is None:
        raise state.Refusal("accepted metadata has no reachable sealing commit")
    parents = git(repo, ["rev-list", "--parents", "-n", "1", sealing]).decode().split()[1:]
    if len(parents) != 2 or parents[1] != record["baseline"]["commit"]:
        raise state.Refusal("accepted baseline is missing its required second-parent custody; obtain full history")
    if content_tree(repo, sealing) != record["content_tree"] or content_tree(repo, parents[0]) != record["content_tree"]:
        raise state.Refusal("acceptance changed content outside its control record")
    if record["previous_baseline"] is not None:
        baseline_parents = git(repo, ["rev-list", "--parents", "-n", "1", parents[1]]).decode().split()[1:]
        if baseline_parents != [record["previous_baseline"]]:
            raise state.Refusal("rendered baseline parent chain differs")
    evidence = shape(record["evidence"], {"review", "validation"}, "accepted evidence")
    for kind, item in evidence.items():
        shape(item, {"sha256", "locator", "record"}, "evidence locator")
        if digest(canonical(item["record"])) != sha256(item["sha256"]) or item["locator"] != "sha256:" + item["sha256"]:
            raise state.Refusal("accepted evidence digest differs")
        proof_record = shape(item["record"], {"schema_version", "kind", "verdict", "consumer_commit",
                             "baseline_commit", "target_revision", "content_tree", "summary", "checks",
                             "migration_dispositions"}, "accepted proof")
        if (type(proof_record["schema_version"]) is not int or proof_record["schema_version"] != 1
                or proof_record["verdict"] != "pass" or proof_record["kind"] != kind
                or proof_record["content_tree"] != record["content_tree"]
                or proof_record["target_revision"] != source["revision"]
                or proof_record["baseline_commit"] != record["previous_baseline"]):
            raise state.Refusal("accepted evidence identifies other inputs/content")
        original = object_id(proof_record["consumer_commit"])
        if run(repo, ["merge-base", "--is-ancestor", original, parents[0]], allowed=(0, 1)).returncode:
            raise state.Refusal("accepted evidence consumer is outside resolved ancestry")
        required = {"resolutions", "baseline_adoption"} if kind == "review" else {"generated", "locked_graph", "migration_history", "compatibility"}
        shape(proof_record["checks"], required, "accepted checks")
        if not isinstance(proof_record["migration_dispositions"], dict):
            raise state.Refusal("accepted migration dispositions must be an object")
        if any(not isinstance(text, str) or not text.strip() for text in
               [proof_record["summary"], *proof_record["checks"].values(), *proof_record["migration_dispositions"].values()]):
            raise state.Refusal("accepted evidence contains an empty result")
    if evidence["review"]["record"]["consumer_commit"] != evidence["validation"]["record"]["consumer_commit"]:
        raise state.Refusal("accepted review and validation name different original consumers")
    return record


def attempt_path(repo: Path) -> Path:
    common = Path(git(repo, ["rev-parse", "--path-format=absolute", "--git-common-dir"]).decode().strip())
    matches = list((common / "codex/template-upgrade").glob("*/attempt.json"))
    if len(matches) != 1:
        raise state.Refusal("no unique complete upgrade attempt; retain this directory and prepare a fresh destination")
    return matches[0]


def load_attempt(repo: Path) -> tuple[Path, dict[str, Any]]:
    path = attempt_path(repo)
    state._regular_file(path, "attempt record")
    record = state.parse_json_bytes(path.read_bytes(), "attempt record")
    if not isinstance(record, dict) or record.get("schema_version") != 1:
        raise state.Refusal("unsupported attempt record")
    return path, record


def save_attempt(path: Path, record: dict[str, Any]) -> None:
    state.atomic_write(path, canonical(record), 0o600)


def migration_issues(repo: Path, consumer: str, old: str, new: str) -> list[str]:
    def migrations(revision: str) -> dict[str, dict[str, str]]:
        return {item["path"]: item for item in entries(repo, revision)
                if re.fullmatch(r"migrations/[0-9]+_.+\.sql", item["path"])}
    current, before, after = migrations(consumer), migrations(old), migrations(new)
    maximum = max((int(Path(path).name.split("_", 1)[0]) for path in current), default=-1)
    issues = []
    for path in sorted(before.keys() | after.keys()):
        if before.get(path) == after.get(path):
            continue
        if path in current and after.get(path) != current[path]:
            issues.append("applied-checksum:" + path)
        elif path in after and path not in before and int(Path(path).name.split("_", 1)[0]) <= maximum:
            issues.append("version-order:" + path)
    return issues


def assert_append_only(repo: Path, base: str, resolved: str) -> None:
    old = {entry["path"]: entry for entry in entries(repo, base)
           if re.fullmatch(r"migrations/[0-9]+_.+\.sql", entry["path"])}
    new = {entry["path"]: entry for entry in entries(repo, resolved)
           if re.fullmatch(r"migrations/[0-9]+_.+\.sql", entry["path"])}
    maximum = max((int(Path(path).name.split("_", 1)[0]) for path in old), default=-1)
    versions: set[int] = set()
    for path, entry in new.items():
        version = int(Path(path).name.split("_", 1)[0])
        if version in versions:
            raise state.Refusal("colliding migration versions require forward reconciliation")
        versions.add(version)
        if path not in old and version <= maximum:
            raise state.Refusal("imported migration is below the consumer maximum; supply a reviewed forward migration")
    if any(new.get(path) != entry for path, entry in old.items()):
        raise state.Refusal("consumer migration bytes/modes were changed or removed; preserve applied history")


def prepare(args: argparse.Namespace) -> dict[str, Any]:
    consumer = state.git_root(args.consumer)
    source = state.git_root(args.source)
    head = rev(consumer, "HEAD^{commit}")
    lock, raw_lock = lock_at(consumer, head)
    current = accepted(consumer, head, lock, raw_lock)
    revision = object_id(args.revision)
    if rev(source, revision + "^{commit}") != revision:
        raise state.Refusal("source revision is not available locally")
    if args.command == "adopt":
        if current is not None:
            raise state.Refusal("consumer already has accepted custody; use prepare")
        if revision != lock["source"]["checkout_revision"]:
            raise state.Refusal("adoption must reconstruct the lock's exact historical source")
        if not args.initial_commit and not args.reconstruct:
            raise state.Refusal("adopt requires --initial-commit or explicit --reconstruct review")
    elif current is None:
        raise state.Refusal("adopt the historical baseline before preparing an upgrade")
    elif current["source"]["revision"] == revision:
        return {"state": "no-op", "consumer_commit": head, "baseline": current["baseline"]["commit"], "target_revision": revision}
    old = current["baseline"]["commit"] if current else None
    fingerprint = digest(canonical({"consumer_commit": head, "baseline": old, "revision": revision,
                                    "choices": choices(lock), "operation": args.command}))
    if args.destination.is_symlink():
        raise state.Refusal("candidate destination may not be a symlink")
    destination = args.destination.resolve()
    if destination.is_symlink() or destination == consumer or destination.is_relative_to(consumer):
        raise state.Refusal("candidate must be a separate repository outside the original consumer")
    if destination.exists():
        _, previous = load_attempt(destination)
        if previous.get("fingerprint") != fingerprint:
            raise state.Refusal("destination belongs to another attempt; select a fresh destination")
        return status(destination)
    cache = args.cargo_target
    if cache is not None and not cache.is_absolute():
        raise state.Refusal("--cargo-target must be an explicitly shared absolute cache path")
    if cache is not None:
        cache = cache.resolve()
        if cache.is_relative_to(consumer) or cache.is_relative_to(destination):
            raise state.Refusal("shared Cargo target must be outside the original consumer and candidate")
    init_repo(destination, consumer)
    import_commit(consumer, destination, head)
    git(destination, ["update-ref", REF, head])
    # Refuse unsafe paths before rendering or materializing consumer content.
    entries(destination, head)
    git(destination, ["reset", "--hard", "-q", head])
    attempt_dir = destination / ".git/codex/template-upgrade" / fingerprint
    attempt_dir.mkdir(parents=True)
    path = attempt_dir / "attempt.json"
    attempt = {"schema_version": 1, "fingerprint": fingerprint, "operation": args.command,
               "state": "incomplete", "consumer": str(consumer), "consumer_commit": head,
               "original_status": git(consumer, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]).decode(),
               "original_branch": git(consumer, ["symbolic-ref", "-q", "HEAD"], allowed=(0, 1)).decode().strip(),
               "source": str(source), "target_revision": revision, "choices": choices(lock),
               "initial_lock_sha256": digest(raw_lock), "old_baseline": old}
    save_attempt(path, attempt)
    baseline = render(source, revision, lock, destination, old, cache)
    origin = current["origin"] if current else {"kind": "reconstructed", "initial_commit": None}
    if args.command == "adopt" and args.initial_commit:
        initial = object_id(args.initial_commit)
        if run(destination, ["merge-base", "--is-ancestor", initial, head], allowed=(0, 1)).returncode:
            raise state.Refusal("initialization commit must be reachable from consumer history")
        if rev(destination, initial + "^{tree}") != baseline["tree"]:
            raise state.Refusal("initialization commit differs from the complete historical render")
        origin = {"kind": "captured", "initial_commit": initial}
    attempt.update({"baseline": baseline, "origin": origin, "merge_exit": 0, "conflicts": [],
                    "migration_issues": [], "control_dispositions": ["preserve consumer template.lock", "preserve consumer template.upgrade.json"]})
    if old is not None:
        # Exclude bookkeeping before merging, so a lock/control conflict cannot
        # obscure native runtime conflicts or choose update policy.
        preserved = {name: blob(destination, head, name) for name in (state.LOCK_NAME, CONTROL)}
        merge_target = index_tree(destination, baseline["tree"], preserved)
        outcome = run(destination, ["merge-tree", "--write-tree", "-z", "--messages", f"--merge-base={old}", head, merge_target], allowed=(0, 1))
        sections = outcome.stdout.split(b"\0")
        merged_tree = object_id(sections.pop(0).decode())
        conflicts = []
        while sections and sections[0]:
            item = sections.pop(0)
            metadata, raw_path = item.split(b"\t", 1)
            mode, oid, stage_number = metadata.decode().split()
            name = raw_path.decode()
            state.safe_relative(name)
            conflicts.append({"mode": mode, "object": object_id(oid), "stage": int(stage_number), "path": name})
        entries(destination, merged_tree)
        git(destination, ["read-tree", "--reset", "-u", merged_tree])
        if conflicts:
            removal = b"".join(f"0 {'0' * len(head)}\t{name}".encode() + b"\0" for name in sorted({item["path"] for item in conflicts}))
            stages = b"".join(f"{item['mode']} {item['object']} {item['stage']}\t{item['path']}".encode() + b"\0" for item in conflicts)
            git(destination, ["update-index", "-z", "--index-info"], data=removal + stages)
        (attempt_dir / "merge-output").write_bytes(outcome.stdout)
        attempt.update({"merge_exit": outcome.returncode, "conflicts": conflicts,
                        "merge_tree": merged_tree, "migration_issues": migration_issues(destination, head, old, baseline["commit"])})
    attempt["state"] = "conflicted" if attempt["merge_exit"] else "prepared"
    (attempt_dir / "consumer.diff").write_bytes(git(destination, [
        "diff", "--binary", "--no-ext-diff", "--no-textconv", head,
    ]))
    if args.command == "adopt":
        (attempt_dir / "baseline-to-consumer.diff").write_bytes(git(destination, [
            "diff", "--binary", "--no-ext-diff", "--no-textconv", baseline["commit"], head,
        ]))
    save_attempt(path, attempt)
    return status(destination)


def status(repo: Path) -> dict[str, Any]:
    common = Path(git(repo, ["rev-parse", "--path-format=absolute", "--git-common-dir"]).decode().strip())
    if not (common / "codex/template-upgrade").exists():
        return {"state": "no-attempt", "candidate": str(repo)}
    path, attempt = load_attempt(repo)
    head = rev(repo)
    result = {"state": attempt["state"], "candidate": str(repo), "attempt": str(path),
              "consumer_commit": attempt["consumer_commit"], "candidate_commit": head,
              "baseline": attempt.get("baseline"), "target_revision": attempt["target_revision"],
              "conflicts": git(repo, ["ls-files", "--unmerged"]).decode(),
              "merge_conflicts": attempt.get("conflicts", []), "merge_exit": attempt.get("merge_exit"),
              "migration_issues": attempt.get("migration_issues", []),
              "complete_diff_command": ["git", "-C", str(repo), "diff", "--binary", "--no-ext-diff", "--no-textconv", attempt["consumer_commit"]],
              "prepared_diff": str(path.parent / "consumer.diff"),
              "delta": git(repo, ["diff", "--stat", attempt["consumer_commit"]]).decode()}
    if attempt["operation"] == "adopt" and "baseline" in attempt:
        result["adoption_diff"] = str(path.parent / "baseline-to-consumer.diff")
    if attempt["state"] in {"accepted", "sealing"}:
        expected = attempt.get("seal_commit")
        if expected != head:
            raise state.Refusal("interrupted acceptance or candidate moved; inspect attempt and candidate ref")
        lock, raw = lock_at(repo, head)
        accepted(repo, head, lock, raw)
        result["state"] = attempt["state"]
    elif attempt["state"] not in {"incomplete", "aborted"}:
        if result["conflicts"] or (attempt["merge_exit"] and head == attempt["consumer_commit"]):
            result["state"] = "conflicted"
        elif not git(repo, ["status", "--porcelain=v1", "--untracked-files=all"]):
            result["state"] = "resolved-awaiting-proof"
            result["content_tree"] = content_tree(repo, head)
    return result


def proof(path: Path, kind: str, attempt: dict[str, Any], tree: str) -> dict[str, Any]:
    state._regular_file(path, kind + " evidence")
    value = shape(state.parse_json_bytes(path.read_bytes(), kind + " evidence"), {
        "schema_version", "kind", "verdict", "consumer_commit", "baseline_commit",
        "target_revision", "content_tree", "summary", "checks", "migration_dispositions",
    }, kind + " evidence")
    expected = {"schema_version": 1, "kind": kind, "verdict": "pass",
                "consumer_commit": attempt["consumer_commit"], "baseline_commit": attempt["old_baseline"],
                "target_revision": attempt["target_revision"], "content_tree": tree}
    if type(value["schema_version"]) is not int or any(value[key] != item for key, item in expected.items()):
        raise state.Refusal(f"{kind} evidence does not admit these exact inputs and resolved content")
    required = {"resolutions", "baseline_adoption"} if kind == "review" else {"generated", "locked_graph", "migration_history", "compatibility"}
    shape(value["checks"], required, kind + " checks")
    descriptions = [value["summary"], *value["checks"].values()]
    dispositions = shape(value["migration_dispositions"], set(attempt["migration_issues"]), "migration dispositions")
    if any(not isinstance(item, str) or not item.strip() for item in [*descriptions, *dispositions.values()]):
        raise state.Refusal("evidence requires concrete review/check results and every migration disposition")
    hashed = digest(canonical(value))
    return {"locator": "sha256:" + hashed, "sha256": hashed, "record": value}


def accept(args: argparse.Namespace) -> dict[str, Any]:
    repo = state.git_root(args.candidate)
    path, attempt = load_attempt(repo)
    if attempt["state"] == "accepted":
        return status(repo)
    if attempt["state"] == "sealing":
        finish_sealing(repo, path, attempt)
        return status(repo)
    if attempt["state"] not in {"prepared", "conflicted"}:
        raise state.Refusal("attempt is incomplete or aborted")
    if rev(Path(attempt["consumer"])) != attempt["consumer_commit"]:
        raise state.Refusal("original committed HEAD changed; prepare a new attempt")
    resolved = rev(repo)
    if git(repo, ["symbolic-ref", "HEAD"]).decode().strip() != REF:
        raise state.Refusal("candidate branch changed")
    if git(repo, ["status", "--porcelain=v1", "--untracked-files=all"]):
        raise state.Refusal("commit all resolutions and remove unresolved stages before accepting")
    if attempt["merge_exit"] and resolved == attempt["consumer_commit"]:
        raise state.Refusal("native conflicts require an explicit resolution commit, including directory conflicts")
    if run(repo, ["merge-base", "--is-ancestor", attempt["consumer_commit"], resolved], allowed=(0, 1)).returncode:
        raise state.Refusal("resolved candidate no longer descends from the original consumer")
    lock, raw_lock = lock_at(repo, resolved)
    if digest(raw_lock) != attempt["initial_lock_sha256"] or choices(lock) != attempt["choices"]:
        raise state.Refusal("resolution changed the initialization record")
    if blob(repo, resolved, CONTROL) != blob(repo, attempt["consumer_commit"], CONTROL):
        raise state.Refusal("resolution changed accepted custody before acceptance")
    tree = content_tree(repo, resolved)
    if attempt["operation"] == "adopt" and tree != content_tree(repo, attempt["consumer_commit"]):
        raise state.Refusal("adoption may not change consumer files")
    assert_append_only(repo, attempt["consumer_commit"], resolved)
    evidence = {kind: proof(getattr(args, kind), kind, attempt, tree) for kind in ("review", "validation")}
    record = {"schema_version": 1, "source": {"repository": state.TEMPLATE_REPOSITORY, "revision": attempt["target_revision"]},
              "baseline": attempt["baseline"], "choices": attempt["choices"],
              "initial_lock_sha256": attempt["initial_lock_sha256"], "origin": attempt["origin"],
              "content_tree": tree, "evidence": evidence, "previous_baseline": attempt["old_baseline"]}
    verify_baseline(repo, record)
    sealed_tree = index_tree(repo, rev(repo, resolved + "^{tree}"), {CONTROL: canonical(record)})
    seal = commit(repo, sealed_tree, [resolved, attempt["baseline"]["commit"]], "Accept reviewed rendered template baseline")
    attempt.update({"state": "sealing", "resolved_commit": resolved, "seal_commit": seal})
    save_attempt(path, attempt)
    finish_sealing(repo, path, attempt)
    return status(repo)


def finish_sealing(repo: Path, path: Path, attempt: dict[str, Any]) -> None:
    """Reconcile either side of the one compare-and-swap acceptance update."""
    resolved, seal = attempt["resolved_commit"], attempt["seal_commit"]
    head = rev(repo)
    if head not in {resolved, seal} or git(repo, ["symbolic-ref", "HEAD"]).decode().strip() != REF:
        raise state.Refusal("candidate moved during acceptance; inspect its recorded seal")
    lock, raw_lock = lock_at(repo, seal)
    accepted(repo, seal, lock, raw_lock)
    # An interrupted metadata write can be completed, but edits after the
    # interruption must never be overwritten or staged by the recovery path.
    if (git(repo, ["diff", "--no-ext-diff", "--no-textconv", resolved, "--", ".", f":(exclude){CONTROL}"])
            or git(repo, ["diff", "--cached", "--no-ext-diff", "--no-textconv", resolved, "--", ".", f":(exclude){CONTROL}"])):
        raise state.Refusal("candidate content changed while sealing; preserve edits and recover the recorded candidate")
    current = repo / CONTROL
    before, after = blob(repo, resolved, CONTROL), blob(repo, seal, CONTROL)
    if current.is_symlink() or (current.exists() and not current.is_file()):
        raise state.Refusal("control path changed while sealing")
    present = current.read_bytes() if current.exists() else None
    if present not in (before, after):
        raise state.Refusal("control record changed while sealing")
    if head == resolved:
        if rev(Path(attempt["consumer"])) != attempt["consumer_commit"]:
            raise state.Refusal("original HEAD changed before acceptance")
        git(repo, ["update-ref", REF, seal, resolved])
    assert after is not None
    state.atomic_write(current, after, 0o644)
    # Update this one index entry only; it is the sole permitted sealing delta.
    control_oid = rev(repo, seal + ":" + CONTROL)
    git(repo, ["update-index", "--add", "--cacheinfo", f"100644,{control_oid},{CONTROL}"])
    attempt["state"] = "accepted"
    save_attempt(path, attempt)


def abort(repo: Path) -> dict[str, Any]:
    path, attempt = load_attempt(repo)
    if attempt["state"] in {"accepted", "sealing"}:
        raise state.Refusal("accepted or sealing custody cannot be aborted; inspect status")
    attempt["state"] = "aborted"
    save_attempt(path, attempt)
    return status(repo)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    for name in ("adopt", "prepare"):
        command = commands.add_parser(name)
        command.add_argument("--consumer", type=Path, required=True)
        command.add_argument("--source", type=Path, required=True, help="explicitly trusted local template repository; its selected code will execute")
        command.add_argument("--revision", required=True, help="exact full source commit ID")
        command.add_argument("--destination", type=Path, required=True, help="new isolated candidate directory")
        command.add_argument("--cargo-target", type=Path)
        if name == "adopt":
            origin = command.add_mutually_exclusive_group(required=True)
            origin.add_argument("--initial-commit")
            origin.add_argument("--reconstruct", action="store_true")
    for name in ("status", "accept", "abort"):
        command = commands.add_parser(name)
        command.add_argument("--candidate", type=Path, required=True)
        if name == "accept":
            command.add_argument("--review", type=Path, required=True)
            command.add_argument("--validation", type=Path, required=True)
    return result


def main() -> int:
    args = parser().parse_args()
    # Trust applies to selected generation code, not ambient publication tokens,
    # Git hooks/config, Cargo credentials/config, Python import hooks or providers.
    original = dict(os.environ)
    with tempfile.TemporaryDirectory(prefix="template-upgrade-env-") as temporary:
        home = Path(temporary)
        cargo = home / "cargo"
        cargo.mkdir()
        cached = Path(original.get("CARGO_HOME", str(Path.home() / ".cargo")))
        for name in ("registry", "git"):
            if (cached / name).is_dir():
                (cargo / name).symlink_to((cached / name).resolve(), target_is_directory=True)
        clean = {"PATH": original.get("PATH", os.defpath), "HOME": str(home),
                 "CARGO_HOME": str(cargo), "RUSTUP_HOME": original.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
                 "RUSTUP_AUTO_INSTALL": "0", "CARGO_NET_OFFLINE": "true", "PYTHONDONTWRITEBYTECODE": "1",
                 "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_ATTR_NOSYSTEM": "1",
                 "GIT_TERMINAL_PROMPT": "0", "GIT_OPTIONAL_LOCKS": "0", "GIT_NO_LAZY_FETCH": "1",
                 "GIT_NO_REPLACE_OBJECTS": "1", "GIT_GRAFT_FILE": os.devnull, "LC_ALL": "C", "TZ": "UTC",
                 "GIT_AUTHOR_NAME": "Template upgrade", "GIT_AUTHOR_EMAIL": "template-upgrade@example.invalid",
                 "GIT_COMMITTER_NAME": "Template upgrade", "GIT_COMMITTER_EMAIL": "template-upgrade@example.invalid"}
        if original.get("CARGO_PROFILE_DEV_DEBUG") == "line-tables-only":
            clean["CARGO_PROFILE_DEV_DEBUG"] = "line-tables-only"
        os.environ.clear()
        os.environ.update(clean)
        try:
            if args.command in {"adopt", "prepare"}:
                result = prepare(args)
            elif args.command == "accept":
                result = accept(args)
            elif args.command == "abort":
                result = abort(state.git_root(args.candidate))
            else:
                result = status(state.git_root(args.candidate))
            print(json.dumps(result, indent=2, ensure_ascii=False))
            return 0
        except (state.Refusal, state.ToolFailure, OSError, UnicodeError, ValueError, KeyError, TypeError) as error:
            print(f"template upgrade: {error}", file=sys.stderr)
            return 2
        finally:
            os.environ.clear()
            os.environ.update(original)


if __name__ == "__main__":
    raise SystemExit(main())
