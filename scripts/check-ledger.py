#!/usr/bin/env python3
"""Static structural verifier and execution-wave planner for Task Ledger V1."""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.dont_write_bytecode = True

VALID_STATUSES = frozenset({"draft", "ready", "blocked", "done"})
VALID_SHAPES = frozenset(
    {"prefactor", "shared-contract", "integration-first", "expand", "migrate", "contract"}
)
REQUIRED_PACKET_SECTIONS = (
    "Outcome",
    "Depends on",
    "Sources",
    "Provides",
    "Boundary",
    "Mutable owners",
    "Exclusive locks",
    "Final validation",
    "Reopen if",
)
RUNTIME_HEADER_PREFIXES = ("Base:", "Commit mode:")
RUNTIME_TASK_PREFIXES = ("Execution:", "Resume:", "Implemented:", "Accepted:", "Blocked:")

TASK_ID_PATTERN = r"T\d+[A-Za-z0-9_-]*"
TASK_LINE_RE = re.compile(rf"^- \[( |x)\] ({TASK_ID_PATTERN}):\s*(.+?)\s*$")
TRAILING_TAG_RE = re.compile(r"^(.*?)\s+\[([^\]]+)\]$")
PACKET_TITLE_RE = re.compile(rf"^#\s+({TASK_ID_PATTERN})\s+(?:\u2014|--|-)\s+(.+?)\s*$")
SECTION_RE = re.compile(
    r"^(Outcome|Depends on|Sources|Provides|Boundary|Mutable owners|Exclusive locks|Final validation|Reopen if):(.*)$"
)
MD_LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]+)\)")
TASK_DEP_RE = re.compile(rf"^`?({TASK_ID_PATTERN})`?(?:\s+\[(acceptance|effect:[A-Za-z0-9_:-]+)\])?$")
EXTERNAL_DEP_RE = re.compile(r"^(.+?)\s+\[(effect:[A-Za-z0-9_:-]+)\]$")
PACKET_TASK_DEP_RE = re.compile(
    rf"^`?({TASK_ID_PATTERN})`?(?:\s+\[(acceptance|effect:[A-Za-z0-9_:-]+)\])?(?:\s+(?:\u2014|--|-)\s+.*)?$"
)
PACKET_EXTERNAL_DEP_RE = re.compile(
    r"^(.+?)\s+\[(effect:[A-Za-z0-9_:-]+)\](?:\s+(?:\u2014|--|-)\s+.*)?$"
)
INTENT_I_HEADING_RE = re.compile(r"^###\s+(I\d+)\b", re.MULTILINE)
INTENT_ANCHOR_REF_RE = re.compile(r"intent\.md#(i\d+)\b", re.IGNORECASE)
SPEC_R_HEADING_RE = re.compile(r"^###\s+(R\d+)\b", re.MULTILINE)
SPEC_G_BULLET_RE = re.compile(r"^-\s+\*\*(G\d+)\b", re.MULTILINE)
DESIGN_D_HEADING_RE = re.compile(r"^###\s+(D\d+)\b", re.MULTILINE)
DESIGN_S_HEADING_RE = re.compile(r"^###\s+(S\d+)\b", re.MULTILINE)
ANCHOR_EXPLICIT_RANGE_RE = re.compile(r"\b([RGDS])(\d+)\s*(?:\u2013|-)\s*\1(\d+)\b")
ANCHOR_TIGHT_RANGE_RE = re.compile(r"\b([RGDS])(\d+)(?:\u2013|-)(\d+)\b(?!-)")
ANCHOR_TOKEN_RE = re.compile(r"\b([RGDS])(\d+)\b")
ANCHOR_FRAG_RE = re.compile(r"#([rgds])(\d+)\b", re.IGNORECASE)
BARE_SPEC_TOKEN_RE = re.compile(r"(?:^|[\s(`])(?:[^\s(`#]*?/)?spec\.md(?=$|[\s)`.,;])")
BARE_DESIGN_TOKEN_RE = re.compile(r"(?:^|[\s(`])(?:[^\s(`#]*?/)?design\.md(?=$|[\s)`.,;])")


@dataclass(frozen=True)
class Dependency:
    kind: str  # "task" or "external"
    target: str
    stage: str  # "coding", "acceptance", or "effect:<action>"

    def format_token(self) -> str:
        if self.stage == "coding":
            return self.target
        return f"{self.target} [{self.stage}]"


@dataclass
class LedgerTask:
    task_id: str
    title: str
    checked: bool
    shape: str | None
    depends_on: list[Dependency] = field(default_factory=list)
    depends_on_present: bool = False
    packet_rel: str | None = None
    has_runtime_lines: bool = False
    order: int = 0


@dataclass
class PacketData:
    task_id: str | None
    shape: str | None
    sections: dict[str, list[str]]
    depends_on: list[Dependency]
    mutable_owners: list[str]
    exclusive_locks: list[str]
    reopen_if: list[str]
    raw_text: str


@dataclass
class WaveSummary:
    index: int
    tasks: list[str]
    serializations: list[tuple[str, str, str]]


@dataclass
class CheckReport:
    ledger_path: Path
    status: str | None
    task_count: int
    upstream_statuses: dict[str, str]
    integrated_tasks: list[str]
    waves: list[WaveSummary]
    reopen_risks: list[tuple[str, str]]
    anchor_counts: dict[str, tuple[int, int, list[str]]]
    errors: list[str]
    warnings: list[str]


def _is_none_value(value: str) -> bool:
    cleaned = value.strip().strip("`").rstrip(".").strip().lower()
    return cleaned == "none"


def _is_leaf_none(value: str) -> bool:
    cleaned = value.strip().strip("`").rstrip(".").strip().lower()
    return cleaned in {"none", "none (leaf)"} or cleaned.startswith("none (")


def _unwrap_packet_path(raw: str) -> str:
    cleaned = raw.strip().strip("`")
    match = MD_LINK_RE.fullmatch(cleaned)
    if match:
        return match.group(2).strip()
    return cleaned


def _resolve_task_dir(target: Path) -> tuple[Path, Path]:
    if target.is_file():
        return target.parent, target
    return target, target / "tasks.md"


def _parse_front_status(path: Path) -> str | None:
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            break
        match = re.match(r"^status:\s*(\S+)", line.strip())
        if match:
            return match.group(1).lower()
    return None


def _parse_ledger_deps(raw: str, task_id: str, errors: list[str]) -> list[Dependency]:
    cleaned = raw.strip()
    if not cleaned or _is_none_value(cleaned):
        return []
    deps: list[Dependency] = []
    for part in (item.strip() for item in cleaned.split(",")):
        if not part:
            continue
        match = TASK_DEP_RE.match(part)
        if match:
            deps.append(Dependency("task", match.group(1), match.group(2) or "coding"))
            continue
        ext_match = EXTERNAL_DEP_RE.match(part)
        if ext_match:
            gate = ext_match.group(1).strip().strip("`").lower()
            deps.append(Dependency("external", gate, ext_match.group(2)))
            continue
        errors.append(f"tasks.md {task_id}: invalid Depends on entry {part!r}")
    return deps


def _parse_tasks_md(
    ledger_path: Path, errors: list[str]
) -> tuple[dict[str, str], list[LedgerTask], str]:
    raw_text = ledger_path.read_text(encoding="utf-8")
    lines = raw_text.splitlines()
    header: dict[str, str] = {}
    tasks: list[LedgerTask] = []

    tasks_heading_idx: int | None = None
    for idx, line in enumerate(lines):
        if line.strip() == "## Tasks":
            tasks_heading_idx = idx
            break

    if tasks_heading_idx is None:
        errors.append("tasks.md: missing '## Tasks' heading")
        return header, tasks, raw_text

    current_header_key: str | None = None
    has_runtime_header = False
    for line in lines[:tasks_heading_idx]:
        stripped = line.strip()
        if not stripped:
            current_header_key = None
            continue
        if stripped.startswith("# Goal:"):
            header["Goal"] = stripped.removeprefix("# Goal:").strip()
            current_header_key = "Goal"
        elif any(stripped.startswith(prefix) for prefix in RUNTIME_HEADER_PREFIXES):
            has_runtime_header = True
            current_header_key = None
        else:
            key_match = re.match(
                r"^(status|Blocked stop|Completion|Global constraints):(.*)$", stripped
            )
            if key_match:
                key = key_match.group(1)
                header[key] = key_match.group(2).strip()
                current_header_key = key
            elif current_header_key in {"Completion", "Global constraints", "Blocked stop"}:
                existing = header.get(current_header_key, "")
                header[current_header_key] = f"{existing}\n{stripped}".strip()

    if not header.get("Goal"):
        errors.append("tasks.md: missing non-empty '# Goal:' header")

    status = header.get("status", "").lower()
    if status not in VALID_STATUSES:
        errors.append(
            f"tasks.md: invalid or missing status {header.get('status')!r} "
            f"(expected one of {sorted(VALID_STATUSES)})"
        )
    else:
        header["status"] = status

    if status == "blocked" and not header.get("Blocked stop"):
        errors.append("tasks.md: status is 'blocked' but 'Blocked stop:' is missing or empty")
    if status and status != "blocked" and "Blocked stop" in header:
        errors.append(f"tasks.md: 'Blocked stop:' must be omitted when status is {status!r}")

    if not header.get("Completion"):
        errors.append("tasks.md: missing non-empty 'Completion:' header")

    current_task: LedgerTask | None = None
    for line in lines[tasks_heading_idx + 1 :]:
        if line.startswith("## "):
            break
        task_match = TASK_LINE_RE.match(line)
        if task_match:
            checked = task_match.group(1) == "x"
            task_id = task_match.group(2)
            raw_title = task_match.group(3)
            shape: str | None = None
            tag_match = TRAILING_TAG_RE.match(raw_title)
            if tag_match:
                tag = tag_match.group(2).strip()
                if tag in VALID_SHAPES:
                    shape = tag
                    raw_title = tag_match.group(1).strip()
                else:
                    errors.append(
                        f"tasks.md {task_id}: unknown shape tag [{tag}] "
                        f"(expected one of {sorted(VALID_SHAPES)})"
                    )
            current_task = LedgerTask(
                task_id=task_id,
                title=raw_title,
                checked=checked,
                shape=shape,
                order=len(tasks),
            )
            tasks.append(current_task)
            continue

        stripped = line.strip()
        if not stripped or current_task is None:
            continue
        if stripped.startswith("- Depends on:"):
            current_task.depends_on_present = True
            current_task.depends_on = _parse_ledger_deps(
                stripped.removeprefix("- Depends on:"), current_task.task_id, errors
            )
        elif stripped.startswith("- Packet:"):
            current_task.packet_rel = _unwrap_packet_path(stripped.removeprefix("- Packet:"))
        elif any(
            stripped.startswith((prefix, f"- {prefix}"))
            for prefix in RUNTIME_TASK_PREFIXES
        ):
            current_task.has_runtime_lines = True

    if not tasks:
        errors.append("tasks.md: no tasks found under '## Tasks'")

    any_checked = any(t.checked for t in tasks)
    if status == "draft" and not any_checked:
        if has_runtime_header or any(t.has_runtime_lines for t in tasks):
            errors.append(
                "tasks.md: fresh draft ledger must omit Orchestrator runtime fields "
                "(Base, Commit mode, Execution, Resume, receipts)"
            )

    seen_ids: set[str] = set()
    for task in tasks:
        if task.task_id in seen_ids:
            errors.append(f"tasks.md: duplicate task ID {task.task_id}")
        seen_ids.add(task.task_id)
        if not task.depends_on_present:
            errors.append(f"tasks.md {task.task_id}: missing '- Depends on:' field")
        if not task.packet_rel:
            errors.append(f"tasks.md {task.task_id}: missing '- Packet:' field")

    return header, tasks, raw_text


def _strip_bullet_items(lines: list[str]) -> list[str]:
    items: list[str] = []
    for raw in lines:
        stripped = raw.strip()
        if not stripped:
            continue
        if stripped.startswith("- "):
            items.append(stripped[2:].strip())
        elif items:
            items[-1] = f"{items[-1]} {stripped}"
        else:
            items.append(stripped)
    return items


def _is_canonical_lock_path(token: str) -> bool:
    if not token or any(ch.isspace() for ch in token):
        return False
    if token.startswith(("/", "./")):
        return False
    if ".." in Path(token).parts:
        return False
    if "*" in token or "?" in token or "#" in token or ":" in token:
        return False
    return True


def _parse_packet(
    packet_path: Path, expected_task_id: str, errors: list[str]
) -> PacketData:
    raw_text = packet_path.read_text(encoding="utf-8")
    lines = raw_text.splitlines()
    rel_label = packet_path.name

    packet_task_id: str | None = None
    shape: str | None = None
    sections: dict[str, list[str]] = {}
    current_section: str | None = None
    in_code_fence = False

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("```"):
            in_code_fence = not in_code_fence
            if current_section is not None:
                sections[current_section].append(line)
            continue

        if not in_code_fence and packet_task_id is None and stripped.startswith("# "):
            title_match = PACKET_TITLE_RE.match(stripped)
            if title_match:
                packet_task_id = title_match.group(1)
            else:
                errors.append(f"{rel_label}: heading must match '# T<ID> \u2014 <outcome>'")
            continue

        if not in_code_fence and re.match(r"^#{2,6}\s+", line):
            current_section = None
            continue

        if not in_code_fence and current_section is None and stripped.startswith("Shape:"):
            raw_shape = stripped.removeprefix("Shape:").strip().strip("`")
            if raw_shape.startswith("[") and raw_shape.endswith("]"):
                raw_shape = raw_shape[1:-1].strip()
            if raw_shape in VALID_SHAPES:
                shape = raw_shape
            else:
                errors.append(
                    f"{rel_label}: invalid Shape {raw_shape!r} "
                    f"(expected one of {sorted(VALID_SHAPES)}; omit for vertical slice)"
                )
            continue

        if not in_code_fence and not line.startswith((" ", "\t")):
            sec_match = SECTION_RE.match(line)
            if sec_match:
                current_section = sec_match.group(1)
                sections.setdefault(current_section, [])
                inline = sec_match.group(2).strip()
                if inline:
                    sections[current_section].append(inline)
                continue

        if current_section is not None:
            sections[current_section].append(line)

    if packet_task_id is None:
        errors.append(f"{rel_label}: missing '# {expected_task_id} \u2014 ...' title")
    elif packet_task_id != expected_task_id:
        errors.append(
            f"{rel_label}: packet title ID {packet_task_id} does not match tasks.md ID {expected_task_id}"
        )

    for sec in REQUIRED_PACKET_SECTIONS:
        if sec not in sections or not any(s.strip() for s in sections[sec]):
            errors.append(f"{rel_label}: missing or empty '{sec}:' section")

    boundary_items = _strip_bullet_items(sections.get("Boundary", []))
    in_scope = [i for i in boundary_items if i.startswith("In scope:") and i.removeprefix("In scope:").strip()]
    out_scope = [i for i in boundary_items if i.startswith("Out of scope:") and i.removeprefix("Out of scope:").strip()]
    if "Boundary" in sections:
        if not in_scope:
            errors.append(f"{rel_label}: Boundary missing non-empty '- In scope:'")
        if not out_scope:
            errors.append(f"{rel_label}: Boundary missing non-empty '- Out of scope:'")

    fv_items = _strip_bullet_items(sections.get("Final validation", []))
    obs_claim = [i for i in fv_items if i.startswith("Observable claim:") and i.removeprefix("Observable claim:").strip()]
    add_proof = [i for i in fv_items if i.startswith("Additional proof:") and i.removeprefix("Additional proof:").strip()]
    if "Final validation" in sections:
        if not obs_claim:
            errors.append(f"{rel_label}: Final validation missing non-empty '- Observable claim:'")
        if not add_proof:
            errors.append(f"{rel_label}: Final validation missing non-empty '- Additional proof:'")

    packet_deps: list[Dependency] = []
    for item in _strip_bullet_items(sections.get("Depends on", [])):
        if _is_none_value(item):
            continue
        match = PACKET_TASK_DEP_RE.match(item)
        if match:
            packet_deps.append(Dependency("task", match.group(1), match.group(2) or "coding"))
            continue
        ext_match = PACKET_EXTERNAL_DEP_RE.match(item)
        if ext_match:
            gate = ext_match.group(1).strip().strip("`").lower()
            packet_deps.append(Dependency("external", gate, ext_match.group(2)))
            continue
        errors.append(f"{rel_label}: invalid Depends on entry {item!r}")

    owners = [
        item for item in _strip_bullet_items(sections.get("Mutable owners", []))
        if not _is_none_value(item)
    ]
    if "Mutable owners" in sections and not owners:
        errors.append(f"{rel_label}: Mutable owners must name at least one semantic owner")

    locks: list[str] = []
    for item in _strip_bullet_items(sections.get("Exclusive locks", [])):
        if _is_none_value(item):
            continue
        cleaned = item.strip().strip("`")
        if not _is_canonical_lock_path(cleaned):
            errors.append(
                f"{rel_label}: Exclusive lock {item!r} must be a canonical repo-relative path "
                "(no whitespace, leading slash, '..', glob, or line number)"
            )
        else:
            locks.append(cleaned)

    reopen_if = [
        item for item in _strip_bullet_items(sections.get("Reopen if", []))
        if not _is_none_value(item)
    ]

    return PacketData(
        task_id=packet_task_id,
        shape=shape,
        sections=sections,
        depends_on=packet_deps,
        mutable_owners=owners,
        exclusive_locks=locks,
        reopen_if=reopen_if,
        raw_text=raw_text,
    )


def _normalize_owner(owner: str) -> tuple[str, str | None]:
    cleaned = owner.replace("`", "").strip().lower()
    parts = re.split(r"\s*(?:\u2014|--|::|:|\s-\s)\s*", cleaned, maxsplit=1)
    crate = re.sub(r"^crates/", "", parts[0].strip())
    sub = parts[1].strip() if len(parts) > 1 and parts[1].strip() else None
    return crate, sub


def _owners_overlap(a: list[str], b: list[str]) -> str | None:
    norm_a = [_normalize_owner(o) for o in a]
    norm_b = [_normalize_owner(o) for o in b]
    for crate_a, sub_a in norm_a:
        for crate_b, sub_b in norm_b:
            if crate_a != crate_b:
                continue
            if sub_a is None or sub_b is None or sub_a == sub_b:
                label = crate_a if sub_a is None or sub_b is None else f"{crate_a} \u2014 {sub_a}"
                return f"Mutable owner: {label}"
    return None


def _locks_overlap(a: list[str], b: list[str]) -> str | None:
    for lock_a in a:
        parts_a = [p for p in lock_a.strip("/").split("/") if p]
        for lock_b in b:
            parts_b = [p for p in lock_b.strip("/").split("/") if p]
            if not parts_a or not parts_b:
                continue
            if parts_a == parts_b:
                return f"Exclusive lock: {lock_a}"
            if len(parts_a) < len(parts_b) and parts_b[: len(parts_a)] == parts_a:
                return f"Exclusive lock: {lock_a} overlaps {lock_b}"
            if len(parts_b) < len(parts_a) and parts_a[: len(parts_b)] == parts_b:
                return f"Exclusive lock: {lock_b} overlaps {lock_a}"
    return None


def _detect_cycle(tasks: list[LedgerTask], task_ids: set[str]) -> list[str] | None:
    adj: dict[str, list[str]] = {t.task_id: [] for t in tasks}
    for task in tasks:
        for dep in task.depends_on:
            if dep.kind == "task" and dep.target in task_ids:
                adj[task.task_id].append(dep.target)

    state: dict[str, int] = {}
    stack: list[str] = []

    def dfs(node: str) -> list[str] | None:
        state[node] = 1
        stack.append(node)
        for nxt in adj[node]:
            st = state.get(nxt, 0)
            if st == 1:
                idx = stack.index(nxt)
                return [*stack[idx:], nxt]
            if st == 0:
                found = dfs(nxt)
                if found is not None:
                    return found
        stack.pop()
        state[node] = 2
        return None

    for task in tasks:
        if state.get(task.task_id, 0) == 0:
            cycle = dfs(task.task_id)
            if cycle is not None:
                return cycle
    return None


def _simulate_waves(
    tasks: list[LedgerTask], packets: dict[str, PacketData]
) -> tuple[list[str], list[WaveSummary]]:
    integrated = [t.task_id for t in tasks if t.checked]
    completed: set[str] = set(integrated)
    remaining = [t.task_id for t in tasks if not t.checked]
    task_map = {t.task_id: t for t in tasks}
    waves: list[WaveSummary] = []

    while remaining:
        frontier = [
            tid
            for tid in remaining
            if all(
                dep.target in completed
                for dep in task_map[tid].depends_on
                if dep.kind == "task" and dep.stage == "coding"
            )
        ]
        if not frontier:
            break

        wave_tasks: list[str] = []
        serializations: list[tuple[str, str, str]] = []
        for tid in frontier:
            pkt = packets.get(tid)
            conflict: tuple[str, str] | None = None
            if pkt is not None:
                for active_id in wave_tasks:
                    active_pkt = packets.get(active_id)
                    if active_pkt is None:
                        continue
                    reason = _locks_overlap(pkt.exclusive_locks, active_pkt.exclusive_locks)
                    if reason is None:
                        reason = _owners_overlap(pkt.mutable_owners, active_pkt.mutable_owners)
                    if reason is not None:
                        conflict = (active_id, reason)
                        break
            if conflict is None:
                wave_tasks.append(tid)
            else:
                serializations.append((tid, conflict[0], conflict[1]))

        waves.append(WaveSummary(index=len(waves) + 1, tasks=wave_tasks, serializations=serializations))
        completed.update(wave_tasks)
        remaining = [tid for tid in remaining if tid not in completed]

    return integrated, waves


def _extract_referenced_anchors(text: str) -> set[str]:
    found: set[str] = set()
    for pattern in (ANCHOR_EXPLICIT_RANGE_RE, ANCHOR_TIGHT_RANGE_RE):
        for match in pattern.finditer(text):
            prefix = match.group(1).upper()
            start = int(match.group(2))
            end = int(match.group(3))
            if start <= end and (end - start) <= 50:
                for num in range(start, end + 1):
                    found.add(f"{prefix}{num}")
    for match in ANCHOR_TOKEN_RE.finditer(text):
        found.add(f"{match.group(1).upper()}{match.group(2)}")
    for match in ANCHOR_FRAG_RE.finditer(text):
        found.add(f"{match.group(1).upper()}{match.group(2)}")
    return found


def _has_bare_doc_reference(source_item: str, filename: str) -> bool:
    locator = re.split(r"\s+(?:\u2014|--|-)\s+", source_item, maxsplit=1)[0]
    token_re = BARE_SPEC_TOKEN_RE if filename == "spec.md" else BARE_DESIGN_TOKEN_RE
    for match in MD_LINK_RE.finditer(locator):
        href = match.group(2).strip()
        if "#" not in href and (href == filename or href.endswith(f"/{filename}")):
            return True
    plain = MD_LINK_RE.sub(" ", locator)
    return bool(token_re.search(plain))


def check_ledger(target: Path) -> CheckReport:
    task_dir, ledger_path = _resolve_task_dir(target)
    errors: list[str] = []
    warnings: list[str] = []
    upstream_statuses: dict[str, str] = {}
    anchor_counts: dict[str, tuple[int, int, list[str]]] = {}

    if not ledger_path.is_file():
        errors.append(f"ledger not found: {ledger_path}")
        return CheckReport(
            ledger_path=ledger_path,
            status=None,
            task_count=0,
            upstream_statuses=upstream_statuses,
            integrated_tasks=[],
            waves=[],
            reopen_risks=[],
            anchor_counts=anchor_counts,
            errors=errors,
            warnings=warnings,
        )

    header, tasks, ledger_raw = _parse_tasks_md(ledger_path, errors)
    status = header.get("status")

    for rel in ("intent.md", "spec.md", "design/design.md", "rollout.md"):
        up_path = task_dir / rel
        if up_path.is_file():
            up_status = _parse_front_status(up_path) or "missing"
            upstream_statuses[rel] = up_status
            if status in {"draft", "ready", "done"} and up_status != "ready":
                errors.append(f"upstream artifact {rel} has status {up_status!r} (must be 'ready')")

    if status == "ready" and (task_dir / "workflow-plan.md").exists():
        errors.append("workflow-plan.md must be deleted once tasks.md is set to status: ready")

    task_map = {t.task_id: t for t in tasks}
    task_ids = set(task_map)

    for task in tasks:
        for dep in task.depends_on:
            if dep.kind == "task":
                if dep.target == task.task_id:
                    errors.append(f"tasks.md {task.task_id}: task cannot depend on itself")
                elif dep.target not in task_ids:
                    errors.append(
                        f"tasks.md {task.task_id}: Depends on unknown task {dep.target}"
                    )

    cycle = _detect_cycle(tasks, task_ids)
    if cycle is not None:
        errors.append(f"tasks.md: cyclic dependency detected: {' -> '.join(cycle)}")
    else:
        for task in tasks:
            for dep in task.depends_on:
                if dep.kind == "task" and dep.target in task_map:
                    if task_map[dep.target].order > task.order:
                        errors.append(
                            f"tasks.md: non-topological order ({task.task_id} depends on "
                            f"{dep.target}, which appears later)"
                        )

    if tasks:
        ready_frontier = [
            t.task_id
            for t in tasks
            if not any(d.kind == "task" and d.stage == "coding" for d in t.depends_on)
        ]
        if not ready_frontier:
            errors.append("tasks.md: initial Ready Frontier is empty (every task has a coding blocker)")

    tasks_dir = task_dir / "tasks"
    referenced_packets: set[Path] = set()
    packets: dict[str, PacketData] = {}
    for task in tasks:
        if not task.packet_rel:
            continue
        packet_path = (task_dir / task.packet_rel).resolve()
        try:
            packet_path.relative_to(tasks_dir.resolve())
        except ValueError:
            errors.append(
                f"tasks.md {task.task_id}: Packet path {task.packet_rel!r} must live under tasks/"
            )
            continue
        if not packet_path.is_file():
            errors.append(f"tasks.md {task.task_id}: missing packet file {task.packet_rel}")
            continue
        referenced_packets.add(packet_path)
        pkt = _parse_packet(packet_path, task.task_id, errors)
        packets[task.task_id] = pkt

        if pkt.shape != task.shape:
            errors.append(
                f"{packet_path.name}: Shape {pkt.shape!r} does not match tasks.md shape tag {task.shape!r}"
            )
        if set(pkt.depends_on) != set(task.depends_on):
            ledger_fmt = sorted(d.format_token() for d in task.depends_on)
            pkt_fmt = sorted(d.format_token() for d in pkt.depends_on)
            errors.append(
                f"{packet_path.name}: Depends on {pkt_fmt} does not match tasks.md {ledger_fmt}"
            )

    if tasks_dir.is_dir():
        for existing in sorted(tasks_dir.glob("*.md")):
            if existing.resolve() not in referenced_packets:
                errors.append(f"orphan packet file not referenced in tasks.md: tasks/{existing.name}")

    producers: set[str] = set()
    for task in tasks:
        for dep in task.depends_on:
            if dep.kind == "task" and dep.stage in {"coding", "acceptance"}:
                producers.add(dep.target)

    for prod_id in sorted(producers):
        pkt = packets.get(prod_id)
        if pkt is None:
            continue
        provides_items = _strip_bullet_items(pkt.sections.get("Provides", []))
        if not provides_items or all(_is_leaf_none(item) for item in provides_items):
            errors.append(
                f"{prod_id}: downstream tasks depend on {prod_id}, so Provides cannot be 'none'"
            )

    intent_path = task_dir / "intent.md"
    spec_path = task_dir / "spec.md"
    design_path = task_dir / "design/design.md"
    intent_i: set[str] = set()
    spec_text = ""
    design_text = ""
    spec_r: list[str] = []
    spec_g: list[str] = []
    design_d: list[str] = []
    design_s: list[str] = []
    if intent_path.is_file():
        intent_text = intent_path.read_text(encoding="utf-8")
        intent_i = {m.upper() for m in INTENT_I_HEADING_RE.findall(intent_text)}
    if spec_path.is_file():
        spec_text = spec_path.read_text(encoding="utf-8")
        spec_r = sorted(set(SPEC_R_HEADING_RE.findall(spec_text)), key=lambda s: int(s[1:]))
        spec_g = sorted(set(SPEC_G_BULLET_RE.findall(spec_text)), key=lambda s: int(s[1:]))
    if design_path.is_file():
        design_text = design_path.read_text(encoding="utf-8")
        design_d = sorted(set(DESIGN_D_HEADING_RE.findall(design_text)), key=lambda s: int(s[1:]))
        design_s = sorted(set(DESIGN_S_HEADING_RE.findall(design_text)), key=lambda s: int(s[1:]))

    checked_intent_sources: list[tuple[str, str]] = [("tasks.md", ledger_raw)]
    if spec_text:
        checked_intent_sources.append(("spec.md", spec_text))
    if design_text:
        checked_intent_sources.append(("design/design.md", design_text))
    for tid, pkt in sorted(packets.items()):
        checked_intent_sources.append((tid, pkt.raw_text))
    for label, text in checked_intent_sources:
        seen_bad: set[str] = set()
        for match in INTENT_ANCHOR_REF_RE.finditer(text):
            ref_id = match.group(1).upper()
            if ref_id in seen_bad:
                continue
            if not intent_path.is_file():
                seen_bad.add(ref_id)
                errors.append(
                    f"{label}: references intent.md#{ref_id}, but intent.md is missing"
                )
            elif ref_id not in intent_i:
                seen_bad.add(ref_id)
                errors.append(
                    f"{label}: references unknown intent.md decision anchor {ref_id}"
                )

    if len(tasks) >= 2:
        for tid, pkt in packets.items():
            for source_item in _strip_bullet_items(pkt.sections.get("Sources", [])):
                if (spec_r or spec_g) and _has_bare_doc_reference(source_item, "spec.md"):
                    errors.append(
                        f"{tid}: Sources cites bare 'spec.md' in a multi-task ledger; "
                        "cite exact owned anchors (spec.md#r<n>-..., spec.md#preserved-guarantees)"
                    )
                if (design_d or design_s) and _has_bare_doc_reference(source_item, "design.md"):
                    errors.append(
                        f"{tid}: Sources cites bare 'design.md' in a multi-task ledger; "
                        "cite exact owned anchors (design/design.md#d<n>-..., design/design.md#s<n>-...)"
                    )

    combined_plan_text = "\n".join([ledger_raw, *(p.raw_text for p in packets.values())])
    referenced = _extract_referenced_anchors(combined_plan_text)
    if spec_r:
        uncovered_r = [r for r in spec_r if r not in referenced]
        anchor_counts["spec R"] = (len(spec_r) - len(uncovered_r), len(spec_r), uncovered_r)
        if uncovered_r:
            errors.append(
                f"uncovered spec.md behavior rules (no reference in ledger/packets): {', '.join(uncovered_r)}"
            )
    if spec_g:
        uncovered_g = [g for g in spec_g if g not in referenced]
        anchor_counts["spec G"] = (len(spec_g) - len(uncovered_g), len(spec_g), uncovered_g)
        if uncovered_g:
            warnings.append(
                f"unreferenced spec.md preserved guarantees (check pairing in Observable claim / Completion): {', '.join(uncovered_g)}"
            )
    if design_d:
        uncovered_d = [d for d in design_d if d not in referenced]
        anchor_counts["design D"] = (len(design_d) - len(uncovered_d), len(design_d), uncovered_d)
        if uncovered_d:
            errors.append(
                f"uncovered design/design.md decisions (no reference in ledger/packets): {', '.join(uncovered_d)}"
            )
    if design_s:
        uncovered_s = [s for s in design_s if s not in referenced]
        anchor_counts["design S"] = (len(design_s) - len(uncovered_s), len(design_s), uncovered_s)
        if uncovered_s:
            errors.append(
                f"uncovered design/design.md seams (no reference in ledger/packets): {', '.join(uncovered_s)}"
            )

    integrated_tasks: list[str] = []
    waves: list[WaveSummary] = []
    if cycle is None:
        integrated_tasks, waves = _simulate_waves(tasks, packets)

    reopen_risks: list[tuple[str, str]] = []
    for task in tasks:
        pkt = packets.get(task.task_id)
        if pkt is None:
            continue
        for risk in pkt.reopen_if:
            reopen_risks.append((task.task_id, risk))

    return CheckReport(
        ledger_path=ledger_path,
        status=status,
        task_count=len(tasks),
        upstream_statuses=upstream_statuses,
        integrated_tasks=integrated_tasks,
        waves=waves,
        reopen_risks=reopen_risks,
        anchor_counts=anchor_counts,
        errors=errors,
        warnings=warnings,
    )


def format_report(report: CheckReport) -> str:
    out: list[str] = [
        f"Ledger: {report.ledger_path} (status: {report.status or 'unknown'}, tasks: {report.task_count})"
    ]
    if report.upstream_statuses:
        up = ", ".join(f"{k}={v}" for k, v in report.upstream_statuses.items())
        out.append(f"Upstream: {up}")
    if report.anchor_counts:
        parts = []
        for label, (hit, total, missing) in report.anchor_counts.items():
            suffix = f" (missing: {', '.join(missing)})" if missing else ""
            parts.append(f"{label}={hit}/{total}{suffix}")
        out.append(f"Anchor coverage: {'; '.join(parts)}")
    if report.integrated_tasks:
        out.append(f"Integrated: {', '.join(report.integrated_tasks)}")
    if report.waves:
        out.append("Execution waves:")
        for wave in report.waves:
            tag = " (Ready Frontier)" if wave.index == 1 else ""
            mode = " [parallel]" if len(wave.tasks) > 1 else ""
            out.append(f"  Wave {wave.index}{tag}: {', '.join(wave.tasks)}{mode}")
            for deferred, blocker, reason in wave.serializations:
                out.append(f"    serialized: {deferred} deferred behind {blocker} ({reason})")
    if report.reopen_risks:
        out.append("Reopen if risks:")
        for tid, risk in report.reopen_risks:
            out.append(f"  - {tid}: {risk}")
    else:
        out.append("Reopen if risks: none")
    if report.warnings:
        out.append("Warnings:")
        for warn in report.warnings:
            out.append(f"  - {warn}")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Verify Task Ledger V1 mechanical invariants and compute execution waves."
    )
    parser.add_argument(
        "target",
        type=Path,
        help="Path to specs/<task> directory or specs/<task>/tasks.md",
    )
    args = parser.parse_args(argv)
    report = check_ledger(args.target)
    print(format_report(report))
    if report.errors:
        print("Errors:", file=sys.stderr)
        for err in report.errors:
            print(f"  - {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
