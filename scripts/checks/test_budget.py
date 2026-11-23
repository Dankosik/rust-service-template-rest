#!/usr/bin/env python3
"""Advisory PR test-line growth budget check (non-blocking visibility ratchet)."""

from __future__ import annotations

import argparse
import os
import re
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from scripts import lib

DEFAULT_BUDGET = 400
_HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


@dataclass(frozen=True)
class BudgetReport:
    added: int
    removed: int
    budget: int
    by_file: dict[str, tuple[int, int]]

    @property
    def net(self) -> int:
        return self.added - self.removed

    @property
    def over_budget(self) -> bool:
        return self.net > self.budget


def is_dedicated_test_path(rel_path: str) -> bool:
    p = Path(rel_path)
    parts = p.parts
    if not parts or p.suffix != ".rs":
        return False
    if parts[0] == "test":
        return True
    if len(parts) >= 2 and parts[0] == "crates" and parts[1].endswith("-test-support"):
        return True
    if "tests" in parts:
        return True
    return p.name in {"tests.rs", "test_support.rs"} or p.name.endswith(("_tests.rs", "_test.rs"))


def _is_code_line(text: str) -> bool:
    stripped = text.strip()
    return bool(stripped) and not stripped.startswith("//")


def _inline_cfg_test_line(lines: Sequence[str]) -> int | None:
    for idx, line in enumerate(lines, start=1):
        if line.strip() == "#[cfg(test)]":
            return idx
    return None


def _file_lines_at(repo: Path, ref: str | None, rel_path: str) -> list[str]:
    if ref is None:
        full = repo / rel_path
        return full.read_text(encoding="utf-8", errors="replace").splitlines() if full.is_file() else []
    res = lib.run(["git", "-C", os.fspath(repo), "show", f"{ref}:{rel_path}"], cwd=repo, capture=True, check=False)
    return res.stdout.splitlines() if res.returncode == 0 else []


def _resolve_base(repo: Path, base: str) -> str:
    res = lib.run(["git", "-C", os.fspath(repo), "merge-base", base, "HEAD"], cwd=repo, capture=True, check=False)
    merged = res.stdout.strip()
    return merged if res.returncode == 0 and merged else base


def measure_test_delta(repo: Path, base: str, *, working_tree: bool = False, budget: int = DEFAULT_BUDGET) -> BudgetReport:
    effective_base = _resolve_base(repo, base)
    diff_args = ["diff", "--unified=0", "--no-renames", effective_base]
    if not working_tree:
        diff_args.append("HEAD")
    diff_args.extend(["--", "crates", "test"])
    diff_text: str = lib.git(*diff_args, cwd=repo)

    added_total = 0
    removed_total = 0
    by_file: dict[str, list[int]] = {}
    cur_path: str | None = None
    pending_old: str | None = None
    cur_dedicated = False
    old_cfg_line: int | None = None
    new_cfg_line: int | None = None
    old_ln = 0
    new_ln = 0

    for line in diff_text.splitlines():
        if line.startswith("diff --git "):
            cur_path = None
            pending_old = None
            continue
        if line.startswith("--- "):
            raw = line[4:]
            pending_old = None if raw == "/dev/null" else (raw[2:] if raw.startswith("a/") else raw)
            continue
        if line.startswith("+++ "):
            raw = line[4:]
            new_path = None if raw == "/dev/null" else (raw[2:] if raw.startswith("b/") else raw)
            rel = new_path or pending_old
            if not rel or not rel.endswith(".rs"):
                cur_path = None
                continue
            cur_path = rel
            cur_dedicated = is_dedicated_test_path(rel)
            if cur_dedicated:
                old_cfg_line = 1 if pending_old else None
                new_cfg_line = 1 if new_path else None
            else:
                old_cfg_line = _inline_cfg_test_line(_file_lines_at(repo, effective_base, rel)) if pending_old else None
                new_cfg_line = _inline_cfg_test_line(_file_lines_at(repo, None if working_tree else "HEAD", rel)) if new_path else None
            continue
        if cur_path is None:
            continue
        if m := _HUNK_RE.match(line):
            old_ln = int(m.group(1))
            new_ln = int(m.group(3))
            continue
        if line.startswith("+") and not line.startswith("+++"):
            content = line[1:]
            if _is_code_line(content) and (cur_dedicated or (new_cfg_line is not None and new_ln >= new_cfg_line)):
                added_total += 1
                by_file.setdefault(cur_path, [0, 0])[0] += 1
            new_ln += 1
        elif line.startswith("-") and not line.startswith("---"):
            content = line[1:]
            if _is_code_line(content) and (cur_dedicated or (old_cfg_line is not None and old_ln >= old_cfg_line)):
                removed_total += 1
                by_file.setdefault(cur_path, [0, 0])[1] += 1
            old_ln += 1

    if working_tree:
        untracked = lib.git("ls-files", "--others", "--exclude-standard", "-z", "--", "crates", "test", cwd=repo, binary=True)
        for raw_path in untracked.split(b"\0"):
            if not raw_path:
                continue
            rel = raw_path.decode("utf-8")
            if not rel.endswith(".rs"):
                continue
            lines = _file_lines_at(repo, None, rel)
            start = 1 if is_dedicated_test_path(rel) else _inline_cfg_test_line(lines)
            if start is None:
                continue
            count = sum(1 for ln in lines[start - 1 :] if _is_code_line(ln))
            if count:
                added_total += count
                by_file.setdefault(rel, [0, 0])[0] += count

    return BudgetReport(
        added=added_total,
        removed=removed_total,
        budget=budget,
        by_file={k: (v[0], v[1]) for k, v in sorted(by_file.items())},
    )


def _write_summary(report: BudgetReport) -> None:
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path:
        return
    status = "Advisory: exceeds budget" if report.over_budget else "Within budget"
    lines = [
        "### Test-line budget",
        f"- **Status:** {status} (`+{report.added}` / `-{report.removed}`, net `{report.net:+d}` vs budget `+{report.budget}`)",
    ]
    if report.over_budget:
        lines.append(
            "- Non-blocking reminder: confirm every added test protects an independent behavior contract "
            "or consolidate overlapping coverage (see `.agents/skills/rust-testing/SKILL.md`)."
        )
    if report.by_file:
        top = sorted(report.by_file.items(), key=lambda kv: -(kv[1][0] - kv[1][1]))[:10]
        lines.extend(["", "| Test file | + | - | Net |", "| --- | ---: | ---: | ---: |"])
        for path, (a, r) in top:
            lines.append(f"| `{path}` | +{a} | -{r} | {a - r:+d} |")
    with Path(summary_path).open("a", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n\n")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=lib.ROOT)
    parser.add_argument("--base", required=True)
    parser.add_argument("--working-tree", action="store_true")
    parser.add_argument("--budget", type=int, default=int(os.environ.get("TEST_LINE_BUDGET", str(DEFAULT_BUDGET))))
    parser.add_argument("--summary", action="store_true")
    args = parser.parse_args(argv)

    def _dispatch() -> None:
        report = measure_test_delta(args.repo.resolve(), args.base, working_tree=args.working_tree, budget=args.budget)
        print(f"test-budget: +{report.added} -{report.removed} (net {report.net:+d}, advisory budget +{report.budget})")
        if args.summary:
            _write_summary(report)
        if report.over_budget and os.environ.get("GITHUB_ACTIONS") == "true":
            print(
                f"::warning title=Test-line budget advisory::PR adds {report.net:+d} net test lines "
                f"(advisory threshold +{report.budget}). Verify each added test owns an independent contract "
                "or consolidate overlapping tests."
            )

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
