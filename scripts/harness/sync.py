#!/usr/bin/env python3
"""Synchronize and verify canonical .agents skills, roles, and harness carriers."""

from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import sys
import tomllib
from collections.abc import Sequence
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True

_SCRIPTS_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, os.fspath(_SCRIPTS_DIR / "template"))

from state import (  # noqa: E402
    ADAPTERS,
    HARNESS_CHOICES,
    Refusal,
    atomic_write,
    load_lock,
    parse_json_bytes,
    selected_adapters,
    selected_profile_map,
)

ALLOWED_ROLE_KEYS = (
    "name",
    "description",
    "class",
    "claude_model",
    "claude_effort",
    "qwen_model",
    "cursor_model",
    "grok_model",
    "grok_effort",
    "output_schema",
    "instructions",
)

OUTPUT_SCHEMA_LINES = {
    "lane-result-v1": (
        "Return fields: conclusion, evidence (primary locator and result), "
        "uncertainty_or_conflict, disposition (accept | reject | repair | "
        "carry_as_proof_or_risk | reopen), destination."
    ),
    "specialist-result-v1": (
        "Return Decision Result V1 (disposition: decision | constraint_only | "
        "proof_only | no_new_decision_required | blocked, decision_or_constraint, "
        "forced_consequences, proof_or_gap, blocker, "
        "strongest_rejected_alternative, rejection_reason, "
        "reopen_owner_or_condition), or Candidate Proposal V1 when dispatched "
        "as a generative candidate lane (pressure, proposal, caller_sketch, "
        "hidden_complexity, dependencies_and_cost, thin_leverage_and_risks, "
        "blocker_or_gap, anchors)."
    ),
    "review-result-v1": "Return `docs/spec-first-workflow/interfaces/review-result-v1.md`.",
    "delegated-result-v1": (
        "Return fields: result, changed_paths, proof (checks actually run "
        "with their results, or none), candidate, gap."
    ),
}

CLAUDE_MUTABLE_TOOLS = "Read, Grep, Glob, Bash, Edit, Write, Agent, SendMessage, TaskOutput, TaskStop"
CLAUDE_READONLY_TOOLS = "Read, Grep, Glob, Bash"
QWEN_MUTABLE_TOOLS = (
    "  - read_file\n"
    "  - grep_search\n"
    "  - glob\n"
    "  - list_directory\n"
    "  - run_shell_command\n"
    "  - write_file\n"
    "  - edit\n"
    "  - agent\n"
    "  - list_agents\n"
    "  - send_message\n"
    "  - task_stop"
)
QWEN_READONLY_TOOLS = (
    "  - read_file\n"
    "  - grep_search\n"
    "  - glob\n"
    "  - list_directory\n"
    "  - run_shell_command"
)

# Antigravity's documented tool names: an omitted list leaves a subagent without
# edit or terminal tools, and an unmapped name can hang it.
ANTIGRAVITY_MUTABLE_TOOLS = (
    "[view_file, grep_search, run_command, replace_file_content, write_to_file, invoke_subagent, send_message]"
)
ANTIGRAVITY_READONLY_TOOLS = "[view_file, grep_search, run_command]"

LEAD_DESC = (
    "Unit: Use when bound ACCEPTANCE_UNIT_LEAD for an implementation unit or "
    "final delivery validation. Own the assigned boundary; Skip sibling scheduling."
)
ORCH_DESC = (
    "Ledger: Use when this session must route a ready persisted Implementation "
    "ledger as LEDGER_ORCHESTRATOR. Own routing; Skip unit work."
)

RUNTIME_START = "# template-owned-codex-runtime:start"
RUNTIME_END = "# template-owned-codex-runtime:end"
REGISTRY_START = "# template-owned-codex-agents:start"
REGISTRY_END = "# template-owned-codex-agents:end"

_ROLE_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
_MODEL_HEAD_RE = re.compile(r"^[A-Za-z0-9]")
_MODEL_BODY_RE = re.compile(r"^[A-Za-z0-9._/:=-]+$")

MIN_PARAGRAPHS = 3
MIN_WORDS, MAX_WORDS = 150, 1000
MAX_PARAGRAPH_WORDS = 125
WORKFLOW_MAX_WORDS = 600
DECISION_KEYS = {"name", "description"}
WORKFLOW_KEYS = {"name", "description", "metadata", "disable-model-invocation"}
WORKFLOW_METADATA_KEYS = {"invocation", "kind"}
WORKFLOW_METADATA_PAIRS = {("user", "workflow"), ("role", "carrier")}
_MD_LINK_RE = re.compile(r"\]\(([^)#\s]+)(?:#[^)]*)?\)")


class HarnessSyncError(Exception):
    pass


def _fail(message: str) -> None:
    raise HarnessSyncError(message)


def _resolve_selected_harness(repo: Path, explicit: str | None) -> str:
    if explicit is not None:
        return explicit
    lock_path = repo / "template.lock"
    if not lock_path.exists() and not lock_path.is_symlink():
        return "all"
    try:
        return selected_profile_map(repo)["agent_harness"]
    except Exception as exc:
        _fail(f"cannot determine selected agent harness: {exc}")
        raise AssertionError("unreachable")


def _selected_set(harness: str) -> tuple[str, ...]:
    try:
        return selected_adapters(harness)
    except Refusal:
        _fail(f"unsupported --harness selection: {harness}")
        raise AssertionError("unreachable")


def _check_no_symlink(repo: Path, relative: str, *, kind: str | None = None) -> None:
    path = repo / relative
    if path.is_symlink():
        _fail(f"{relative} is a symlink")
    if not path.exists():
        return
    if kind == "dir" and not path.is_dir():
        _fail(f"{relative} is not a directory")
    if kind == "file" and not path.is_file():
        _fail(f"{relative} is not a regular file")


def _preflight(repo: Path, selected: tuple[str, ...]) -> None:
    for rel in (".agents", ".agents/roles", ".agents/role-classes", ".agents/skills"):
        _check_no_symlink(repo, rel, kind="dir")
    if (repo / ".agents/adapters").exists() or (repo / ".agents/adapters").is_symlink():
        _check_no_symlink(repo, ".agents/adapters", kind="dir")
    for rel in (
        ".codex",
        ".codex/agents",
        ".claude",
        ".claude/agents",
        ".qwen",
        ".qwen/agents",
        ".grok",
        ".grok/agents",
        ".grok/roles",
        ".cursor",
        ".cursor/agents",
        ".opencode",
        ".opencode/agents",
        ".agents/agents",
    ):
        _check_no_symlink(repo, rel, kind="dir")

    if "codex" in selected:
        _check_no_symlink(repo, ".codex/config.toml", kind="file")
        _check_no_symlink(repo, ".agents/codex-project.toml", kind="file")
        _validate_codex_config_shape(repo / ".codex/config.toml")

    for adapter in ("claude", "qwen"):
        if adapter not in selected:
            continue
        links_rel = f".{adapter}/skills"
        _check_no_symlink(repo, links_rel, kind="dir")
        links_root = repo / links_rel
        if links_root.is_dir():
            for entry in sorted(links_root.iterdir()):
                if not entry.is_symlink():
                    _fail(
                        f"{entry.relative_to(repo).as_posix()} is not a generated symlink; "
                        "move or remove it before rebuilding generated links"
                    )

    for adapter in selected:
        pack = ADAPTERS[adapter]
        for rel_dir, ext in pack.role_dirs:
            role_dir = repo / rel_dir
            if role_dir.is_dir():
                for entry in sorted(role_dir.iterdir()):
                    if entry.is_symlink() or not entry.is_file() or not entry.name.endswith(f".{ext}"):
                        _fail(
                            f"{entry.relative_to(repo).as_posix()} is not a regular .{ext} role carrier; "
                            "move or remove it before syncing roles"
                        )
        for src_rel, dst_rel in pack.static_copies:
            _check_no_symlink(repo, src_rel, kind="file")
            for parent in Path(dst_rel).parents[:-1]:
                _check_no_symlink(repo, parent.as_posix(), kind="dir")
            _check_no_symlink(repo, dst_rel, kind="file")
        if pack.setting_spec is not None:
            src_rel, dst_rel, _, _ = pack.setting_spec
            _check_no_symlink(repo, src_rel, kind="file")
            _check_no_symlink(repo, dst_rel, kind="file")


def _validate_model(key: str, value: str, rel: str) -> None:
    if not value:
        return
    sanitized = value.replace("[", "").replace("]", "")
    if not _MODEL_HEAD_RE.match(value) or not _MODEL_BODY_RE.fullmatch(sanitized):
        _fail(f"{rel} has an unsafe {key} value")


def _read_text_no_symlink(repo: Path, relative: str) -> str:
    path = repo / relative
    if path.is_symlink():
        _fail(f"{relative} is a symlink")
    if not path.is_file():
        _fail(f"{relative} is missing")
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        _fail(f"{relative} cannot be read as UTF-8: {exc}")
        raise AssertionError("unreachable")


def _render_roles(repo: Path) -> tuple[list[str], dict[str, dict[str, bytes]]]:
    roles_dir = repo / ".agents/roles"
    classes_dir = repo / ".agents/role-classes"
    if not roles_dir.is_dir():
        _fail(".agents/roles is missing")
    for root in (roles_dir, classes_dir):
        if root.exists():
            for descendant in sorted(root.rglob("*")):
                if descendant.is_symlink():
                    _fail(f"{descendant.relative_to(repo).as_posix()} is a symlink")

    rendered: dict[str, dict[str, bytes]] = {
        ".codex/agents": {},
        ".claude/agents": {},
        ".qwen/agents": {},
        ".grok/agents": {},
        ".grok/roles": {},
        ".cursor/agents": {},
        ".opencode/agents": {},
        ".agents/agents": {},
    }
    role_names: list[str] = []

    for source_file in sorted(roles_dir.glob("*.toml")):
        rel = source_file.relative_to(repo).as_posix()
        raw = source_file.read_bytes()
        if b"\r" in raw:
            _fail(f"{rel} contains carriage returns (CRLF)")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            _fail(f"{rel} invalid UTF-8: {exc}")
            raise AssertionError("unreachable")
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError as exc:
            _fail(f"{rel} invalid TOML: {exc}")
            raise AssertionError("unreachable")

        unknown = sorted(set(data) - set(ALLOWED_ROLE_KEYS))
        if unknown:
            _fail(f"{rel} has unsupported role metadata keys: {', '.join(unknown)}")
        for key, value in data.items():
            if not isinstance(value, str):
                _fail(f"{rel} role key {key} must be a string")
        if "instructions" not in data:
            _fail(f"{rel} has no instructions block")
        body = data["instructions"].rstrip("\n")
        if not body:
            _fail(f"{rel} has an empty instructions block")
        if any("\0" in str(value) for value in data.values()):
            _fail(f"{rel} contains NUL bytes")

        name = data.get("name", "")
        if not _ROLE_NAME_RE.fullmatch(name):
            _fail(f"{rel} has an unsafe role name {name or '<empty>'}")
        if name != source_file.stem:
            _fail(f"{rel} name must match its filename")
        description = data.get("description", "")
        if not description:
            _fail(f"{rel} has no description")
        if any(ch in description for ch in ("\n", '"', "\\")):
            _fail(f"{rel} description contains unsupported quoting")

        claude_model = data.get("claude_model", "")
        claude_effort = data.get("claude_effort", "")
        qwen_model = data.get("qwen_model", "")
        cursor_model = data.get("cursor_model", "")
        grok_model = data.get("grok_model", "")
        grok_effort = data.get("grok_effort", "")
        role_class = data.get("class", "")
        output_schema = data.get("output_schema", "")

        _validate_model("claude_model", claude_model, rel)
        _validate_model("qwen_model", qwen_model, rel)
        _validate_model("cursor_model", cursor_model, rel)
        _validate_model("grok_model", grok_model, rel)

        if role_class == "read-only-specialist":
            common = _read_text_no_symlink(repo, ".agents/role-classes/read-only-specialist.md").rstrip("\n")
            fallback = _read_text_no_symlink(repo, ".agents/role-classes/read-only-specialist-fallback.md").rstrip("\n")
            sandbox_mode = "read-only"
            cursor_readonly = "true"
            claude_tools = CLAUDE_READONLY_TOOLS
            qwen_tools = QWEN_READONLY_TOOLS
            grok_capability = "read-only"
            antigravity_tools = ANTIGRAVITY_READONLY_TOOLS
        elif role_class == "mutable-worker":
            common = _read_text_no_symlink(repo, ".agents/role-classes/mutable-worker.md").rstrip("\n")
            fallback = _read_text_no_symlink(repo, ".agents/role-classes/mutable-worker-fallback.md").rstrip("\n")
            sandbox_mode = "workspace-write"
            cursor_readonly = "false"
            claude_tools = CLAUDE_MUTABLE_TOOLS
            qwen_tools = QWEN_MUTABLE_TOOLS
            grok_capability = "all"
            antigravity_tools = ANTIGRAVITY_MUTABLE_TOOLS
        else:
            _fail(f"{rel} has unsupported class {role_class}")
            raise AssertionError("unreachable")

        if not claude_model:
            _fail(f"{rel} has no Claude model")
        if not cursor_model:
            _fail(f"{rel} has no Cursor model")
        if not grok_model:
            _fail(f"{rel} has no Grok model")
        if claude_effort not in ("", "low", "medium", "high", "xhigh", "max"):
            _fail(f"{rel} has unsupported claude_effort {claude_effort}")
        if grok_effort not in ("inherit", "low", "medium", "high", "xhigh"):
            _fail(f"{rel} has unsupported grok_effort {grok_effort or '<empty>'}")
        if output_schema not in OUTPUT_SCHEMA_LINES:
            _fail(f"{rel} has unsupported output schema {output_schema}")
        schema_line = OUTPUT_SCHEMA_LINES[output_schema]
        role_names.append(name)

        rendered[".codex/agents"][f"{name}.toml"] = (
            f'name = "{name}"\n'
            f'description = "{description}"\n'
            f'sandbox_mode = "{sandbox_mode}"\n\n'
            f'developer_instructions = """\n{common}\n\n{schema_line}\n\n{body}\n"""\n'
        ).encode("utf-8")

        effort_line = f"effort: {claude_effort}\n" if claude_effort else ""
        rendered[".claude/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\ntools: {claude_tools}\n'
            f"model: {claude_model}\n{effort_line}---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        qwen_model_line = f"model: {qwen_model}\n" if qwen_model else ""
        rendered[".qwen/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\n{qwen_model_line}'
            f"tools:\n{qwen_tools}\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        grok_model_line = "" if grok_model == "inherit" else f"model: {grok_model}\n"
        grok_fallback = f"\n{fallback}\n" if role_class == "mutable-worker" else ""
        rendered[".grok/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\n{grok_model_line}'
            f"permission_mode: bypassPermissions\nagents_md: true\n---\n\n"
            f"{common}\n{grok_fallback}\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        grok_toml = f'description = "{description}"\ndefault_capability_mode = "{grok_capability}"\n'
        if grok_model != "inherit":
            grok_toml += f'model = "{grok_model}"\n'
        if grok_effort != "inherit":
            grok_toml += f'reasoning_effort = "{grok_effort}"\n'
        rendered[".grok/roles"][f"{name}.toml"] = grok_toml.encode("utf-8")

        rendered[".cursor/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\nmodel: {cursor_model}\n'
            f"readonly: {cursor_readonly}\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        opencode_lines = ["---", f'description: "{description}"', "mode: subagent", "hidden: true"]
        if grok_model != "inherit":
            opencode_lines.append(f"model: xai/{grok_model}")
        elif grok_effort != "inherit":
            opencode_lines.append("model: xai/grok-4.6")
        if grok_effort != "inherit":
            opencode_lines.append(f"variant: {grok_effort}")
        opencode_lines.append("permission:")
        if role_class == "read-only-specialist":
            opencode_lines.append("  edit: deny")
        if role_class == "mutable-worker":
            opencode_lines.extend(
                ["  task:", '    "*": deny', "    worker-agent: allow", "    evidence-agent: allow"]
            )
        else:
            opencode_lines.append("  task: deny")
        opencode_lines.extend(["  question: deny", "---", ""])
        rendered[".opencode/agents"][f"{name}.md"] = (
            "\n".join(opencode_lines) + f"\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        rendered[".agents/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\ntools: {antigravity_tools}\n'
            f"mainAgent: false\nsubagent: true\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

    if not role_names:
        _fail(".agents/roles contains no canonical role files")

    implementation = "../../docs/spec-first-workflow/phases/implementation.md"
    lead_phase = implementation.removesuffix(".md")
    lead_method = (
        f"Apply [Unit Implementation]({lead_phase}/unit.md) for an assigned unit, or\n"
        f"[Final Validation]({lead_phase}/final-validation.md) as Delivery Owner,\n"
    )
    orchestrator_method = f"Apply [Implementation as Ledger Orchestrator]({implementation}#carrier)\n"
    lead_tail = (
        "Apply the assigned packet, repair brief, or ledger scope and its named Method. Return\n"
        "`docs/spec-first-workflow/interfaces/acceptance-result-v1.md`.\n"
    )
    rendered[".claude/agents"]["acceptance-unit-lead.md"] = (
        f'---\nname: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\n'
        f"tools: {CLAUDE_MUTABLE_TOOLS}\nmodel: inherit\neffort: high\n---\n\n"
        f"{lead_method}"
        f"and [Claude Code adapter](../../docs/agent-harness/claude-code.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".qwen/agents"]["acceptance-unit-lead.md"] = (
        f'---\nname: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\n'
        f"model: inherit\ntools:\n{QWEN_MUTABLE_TOOLS}\n---\n\n"
        f"{lead_method}"
        f"and [Qwen Code adapter](../../docs/agent-harness/qwen-code.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".cursor/agents"]["acceptance-unit-lead.md"] = (
        f'---\nname: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\n'
        "model: grok-4.6[effort=xhigh]\nreadonly: false\n---\n\n"
        f"{lead_method}"
        f"and [Cursor adapter](../../docs/agent-harness/cursor.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".grok/agents"]["acceptance-unit-lead.md"] = (
        f'---\nname: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\n'
        "agents_md: true\npermission_mode: bypassPermissions\nprompt_mode: full\n---\n\n"
        f"{lead_method}"
        f"and [Grok Build adapter](../../docs/agent-harness/grok-build.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".grok/agents"]["orchestrator.md"] = (
        f'---\nname: orchestrator\ndescription: "{ORCH_DESC}"\n'
        "agents_md: true\npermission_mode: bypassPermissions\nprompt_mode: full\n---\n\n"
        f"{orchestrator_method}"
        "and [Grok Build adapter](../../docs/agent-harness/grok-build.md).\n"
    ).encode("utf-8")
    rendered[".opencode/agents"]["acceptance-unit-lead.md"] = (
        f'---\ndescription: "{LEAD_DESC}"\nmode: all\nmodel: xai/grok-4.6\nvariant: xhigh\n'
        'permission:\n  task:\n    "*": deny\n    worker-agent: allow\n    specialist-agent: allow\n'
        "    evidence-agent: allow\n    reviewer-agent: allow\n    adjudicator-agent: allow\n"
        "  question: deny\n---\n\n"
        f"{lead_method}"
        f"and [OpenCode adapter](../../docs/agent-harness/opencode.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".opencode/agents"]["orchestrator.md"] = (
        f'---\ndescription: "{ORCH_DESC}"\nmode: primary\nmodel: xai/grok-4.6\nvariant: high\n'
        'permission:\n  task:\n    "*": deny\n    acceptance-unit-lead: allow\n    worker-agent: allow\n'
        "    specialist-agent: allow\n    evidence-agent: allow\n    reviewer-agent: allow\n"
        "    adjudicator-agent: allow\n---\n\n"
        f"{orchestrator_method}"
        "and [OpenCode adapter](../../docs/agent-harness/opencode.md).\n"
    ).encode("utf-8")
    rendered[".agents/agents"]["acceptance-unit-lead.md"] = (
        f'---\nname: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\n'
        f"tools: {ANTIGRAVITY_MUTABLE_TOOLS}\nmainAgent: false\nsubagent: true\n---\n\n"
        f"{lead_method}"
        f"and [Antigravity adapter](../../docs/agent-harness/antigravity.md).\n{lead_tail}"
    ).encode("utf-8")
    rendered[".agents/agents"]["orchestrator.md"] = (
        f'---\nname: orchestrator\ndescription: "{ORCH_DESC}"\n'
        f"tools: {ANTIGRAVITY_MUTABLE_TOOLS}\nmainAgent: true\nsubagent: false\n---\n\n"
        f"{orchestrator_method}"
        "and [Antigravity adapter](../../docs/agent-harness/antigravity.md).\n"
    ).encode("utf-8")

    return role_names, rendered


def _validate_codex_config_shape(config_path: Path) -> None:
    if not config_path.is_file():
        return
    lines = config_path.read_text(encoding="utf-8", errors="replace").splitlines()
    rt_starts = [i for i, ln in enumerate(lines) if ln == RUNTIME_START]
    rt_ends = [i for i, ln in enumerate(lines) if ln == RUNTIME_END]
    reg_starts = [i for i, ln in enumerate(lines) if ln == REGISTRY_START]
    reg_ends = [i for i, ln in enumerate(lines) if ln == REGISTRY_END]
    if len(rt_starts) > 1 or len(rt_ends) > 1 or len(rt_starts) != len(rt_ends):
        _fail(".codex/config.toml has an invalid managed runtime marker pair")
    if len(reg_starts) > 1 or len(reg_ends) > 1 or len(reg_starts) != len(reg_ends):
        _fail(".codex/config.toml has an invalid managed registry marker pair")
    if rt_starts and rt_starts[0] >= rt_ends[0]:
        _fail(".codex/config.toml has reversed managed runtime markers")
    if reg_starts and reg_starts[0] >= reg_ends[0]:
        _fail(".codex/config.toml has reversed managed registry markers")
    if rt_starts and reg_starts and rt_ends[0] >= reg_starts[0]:
        _fail(".codex/config.toml has interleaved managed marker pairs")


def _render_codex_config(repo: Path, role_names: list[str]) -> bytes:
    text = _read_text_no_symlink(repo, ".agents/codex-project.toml")
    lines = text.splitlines()
    if not any(re.match(r"^developer_instructions\s*=", ln) for ln in lines):
        _fail(".agents/codex-project.toml has no developer_instructions")
    if "[agents]" not in lines:
        _fail(".agents/codex-project.toml has no [agents] table")
    if not any(re.match(r"^max_concurrent_threads_per_session\s*=", ln) for ln in lines):
        _fail(".agents/codex-project.toml has no concurrency ceiling")
    tables = [ln for ln in lines if re.fullmatch(r"\[[^]]*\]", ln)]
    if any(tbl != "[agents]" for tbl in tables):
        _fail(".agents/codex-project.toml may contain only top-level keys and [agents]")
    if RUNTIME_START in text or RUNTIME_END in text:
        _fail(".agents/codex-project.toml contains a generated runtime marker")
    if not text.endswith("\n"):
        text += "\n"
    registry = [
        REGISTRY_START,
        "# Registry compatibility: agents.<name>.config_file entries are required by Codex.",
    ]
    for role in role_names:
        registry.append(f"[agents.{role}]")
        registry.append(f'config_file = "agents/{role}.toml"')
        registry.append("")
    registry.append(REGISTRY_END)
    return (f"{RUNTIME_START}\n{text}{RUNTIME_END}\n\n" + "\n".join(registry) + "\n").encode("utf-8")


def _unquote_scalar(raw_value: str) -> str:
    value = raw_value.strip()
    if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
        return value[1:-1]
    if value.startswith('"') or value.endswith('"'):
        raise ValueError(f"unbalanced quotes in frontmatter value: {raw_value!r}")
    return value


def parse_frontmatter(text: str) -> tuple[dict[str, object], str]:
    if not text.startswith("---\n"):
        raise ValueError("missing frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        if text.endswith("\n---"):
            end = len(text) - 4
        else:
            raise ValueError("unterminated frontmatter")
    header = text[4:end]
    if "\r" in header:
        raise ValueError("frontmatter contains carriage returns (CRLF)")
    fields: dict[str, object] = {}
    parent: str | None = None
    for line in header.splitlines():
        if not line.strip():
            continue
        indented = line.startswith((" ", "\t"))
        key, sep, raw_value = line.strip().partition(":")
        if not sep or not key.strip():
            raise ValueError(f"frontmatter line is not `key: value`: {line!r}")
        key = key.strip()
        value = _unquote_scalar(raw_value)
        if indented:
            if parent != "metadata":
                raise ValueError(f"only `metadata` may hold nested keys, got {line!r}")
            nested = fields["metadata"]
            assert isinstance(nested, dict)
            if key in nested:
                raise ValueError(f"duplicate metadata key: {key!r}")
            nested[key] = value
            continue
        if key in fields:
            raise ValueError(f"duplicate frontmatter key: {key!r}")
        if key == "metadata":
            if raw_value.strip():
                raise ValueError("`metadata` must be a nested mapping, not a scalar")
            fields["metadata"] = {}
            parent = "metadata"
        else:
            fields[key] = value
            parent = None
    body = text[end + 5 :] if end + 5 <= len(text) else ""
    return fields, body


def _is_user_workflow(fields: dict[str, object]) -> bool:
    metadata = fields.get("metadata")
    return (
        isinstance(metadata, dict)
        and metadata.get("invocation") == "user"
        and metadata.get("kind") == "workflow"
    )


def _check_skill_description(
    name: str,
    fields: dict[str, object],
    *,
    require_use_trigger: bool = True,
) -> list[str]:
    problems: list[str] = []
    if fields.get("name") != name or not _ROLE_NAME_RE.fullmatch(name):
        problems.append(f"{name}: frontmatter name must equal the lowercase-hyphen directory name")
    raw_description = fields.get("description", "")
    description = raw_description if isinstance(raw_description, str) else ""
    if not description:
        problems.append(f"{name}: empty description")
        return problems
    if require_use_trigger and "Use " not in description:
        problems.append(
            f"{name}: description must state its trigger with `Use when`, `Use for`, `Use to`, or `Use only`"
        )
    if len(re.findall(r"[.!?](\s|$)", description)) > 2:
        problems.append(f"{name}: description longer than two sentences")
    if len(description) > 1024:
        problems.append(f"{name}: description longer than 1024 characters")
    return problems


def _check_decision_body(name: str, body: str) -> list[str]:
    problems: list[str] = []
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", body.strip()) if p.strip()]
    headings = [p for p in paragraphs if p.startswith("#")]
    if len(headings) != 1 or not paragraphs or not paragraphs[0].startswith("# "):
        problems.append(f"{name}: body must start with one H1 heading and contain no other headings")
    prose = paragraphs[1:] if paragraphs and paragraphs[0].startswith("# ") else paragraphs
    if not prose or not prose[0].startswith("**"):
        problems.append(f"{name}: first paragraph must open with the bold leading concept")
    elif not all(p.startswith("**") for p in prose[1:]):
        problems.append(f"{name}: each paragraph must open with a bold concept or cluster anchor")
    if len(prose) < MIN_PARAGRAPHS:
        problems.append(
            f"{name}: body has {len(prose)} paragraphs, expected at least {MIN_PARAGRAPHS}"
        )
    for index, paragraph in enumerate(prose, start=1):
        if re.match(r"^\s*([-*]|\d+\.)\s", paragraph, re.M):
            problems.append(f"{name}: body contains a list; write paragraphs")
            break
        if paragraph.startswith("```"):
            problems.append(f"{name}: body contains a code block; name APIs in prose")
            break
        paragraph_words = len(paragraph.split())
        if paragraph_words > MAX_PARAGRAPH_WORDS:
            problems.append(
                f"{name}: paragraph {index} has {paragraph_words} words, limit is {MAX_PARAGRAPH_WORDS}"
            )
    words = len(" ".join(prose).split())
    if not MIN_WORDS <= words <= MAX_WORDS:
        problems.append(f"{name}: body has {words} words, budget is {MIN_WORDS}-{MAX_WORDS}")
    return problems


def _check_workflow_body(name: str, body: str) -> list[str]:
    problems: list[str] = []
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", body.strip()) if p.strip()]
    if not paragraphs or not paragraphs[0].startswith("# "):
        problems.append(f"{name}: body must start with one H1 heading")
    if sum(1 for p in paragraphs if p.startswith("# ")) != 1:
        problems.append(f"{name}: body must have exactly one H1 heading")
    words = len(" ".join(paragraphs[1:]).split())
    if not 0 < words <= WORKFLOW_MAX_WORDS:
        problems.append(f"{name}: body has {words} words, workflow budget is 1-{WORKFLOW_MAX_WORDS}")
    return problems


def _check_openai_carrier(name: str, skill_dir: Path) -> list[str]:
    agents_dir = skill_dir / "agents"
    policy = agents_dir / "openai.yaml"
    if agents_dir.is_symlink() or policy.is_symlink() or not policy.is_file():
        return [f"{name}: workflow skills require a regular file at agents/openai.yaml"]
    try:
        content = policy.read_text(encoding="utf-8")
    except UnicodeDecodeError as err:
        return [f"{name}: agents/openai.yaml is not valid UTF-8: {err}"]
    if "\r" in content:
        return [f"{name}: agents/openai.yaml contains carriage returns (CRLF)"]
    if "  allow_implicit_invocation: false" not in content.splitlines():
        return [f"{name}: agents/openai.yaml must set `allow_implicit_invocation: false`"]
    return []


def _check_skill_links(name: str, skill_dir: Path, body: str) -> tuple[set[str], list[str]]:
    problems: list[str] = []
    links: set[str] = set()
    resolved_skill_dir = skill_dir.resolve()
    for target in _MD_LINK_RE.findall(body):
        links.add(target)
        if target.startswith(("http://", "https://", "mailto:")):
            continue
        if target.startswith("/"):
            problems.append(f"{name}: absolute Markdown link {target} is unsupported")
            continue
        if target.startswith("../"):
            continue
        candidate = (skill_dir / target).resolve()
        if (
            not candidate.is_relative_to(resolved_skill_dir)
            or candidate.is_symlink()
            or not candidate.is_file()
        ):
            problems.append(f"{name}: broken skill Markdown link {target}")
    return links, problems


def _check_skill_entries(
    name: str,
    skill_dir: Path,
    *,
    workflow: bool,
    body: str,
) -> list[str]:
    links, problems = _check_skill_links(name, skill_dir, body)
    for entry in sorted(skill_dir.iterdir()):
        if entry.is_symlink():
            problems.append(f"{name}: {entry.name} is a symlink")
            continue
        if entry.name == "SKILL.md":
            continue
        if entry.name == "references" and entry.is_dir():
            for stray in sorted(entry.rglob("*")):
                if stray.is_symlink():
                    problems.append(f"{name}: references/ contains symlink {stray.relative_to(skill_dir)}")
                elif stray.is_file() and stray.suffix != ".md":
                    problems.append(
                        f"{name}: references/ holds {stray.relative_to(skill_dir)}; only Markdown belongs there"
                    )
                elif not workflow and stray.is_file():
                    rel_target = stray.relative_to(skill_dir).as_posix()
                    if rel_target not in links:
                        problems.append(f"{name}: {rel_target} has no selecting link in SKILL.md")
            continue
        if workflow and entry.name == "agents" and entry.is_dir():
            if sorted(p.name for p in entry.iterdir()) != ["openai.yaml"]:
                problems.append(f"{name}: agents/ may hold only the Codex carrier openai.yaml")
            continue
        allowed = (
            "SKILL.md, references/*.md, agents/openai.yaml"
            if workflow
            else "SKILL.md, references/*.md"
        )
        problems.append(f"{name}: unexpected entry {entry.name}; a skill is {allowed} only")
    if workflow:
        problems.extend(_check_openai_carrier(name, skill_dir))
    return problems


def _check_workflow_metadata(name: str, metadata: object, prefix: str = "") -> list[str]:
    label = f"{name}: {prefix}" if prefix else f"{name}: "
    if not isinstance(metadata, dict) or set(metadata) != WORKFLOW_METADATA_KEYS:
        return [f"{label}metadata must hold exactly {sorted(WORKFLOW_METADATA_KEYS)}"]
    pair = (metadata.get("invocation"), metadata.get("kind"))
    if pair not in WORKFLOW_METADATA_PAIRS:
        return [
            (
                f"{label}metadata (invocation, kind) must be ('user', 'workflow') or ('role', 'carrier'), "
                f"got ({pair[0]!r}, {pair[1]!r})"
            )
        ]
    return []


def _check_service_owned_skill(name: str, skill_dir: Path) -> list[str]:
    marker = skill_dir / ".service-owned"
    if marker.is_symlink() or not marker.is_file() or marker.read_bytes():
        return [f"{name}: .service-owned must be an empty regular file"]
    for descendant in sorted(skill_dir.rglob("*")):
        if descendant.is_symlink():
            return [f"{name}: .service-owned skill contains a symlink: {descendant.relative_to(skill_dir)}"]

    skill = skill_dir / "SKILL.md"
    if skill.is_symlink() or not skill.is_file():
        return [f"{name}: .service-owned requires a real SKILL.md"]
    try:
        fields, _ = parse_frontmatter(skill.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, ValueError) as err:
        return [f"{name}: .service-owned SKILL.md {err}"]

    metadata = fields.get("metadata")
    problems = _check_skill_description(name, fields, require_use_trigger=not _is_user_workflow(fields))
    if metadata is not None:
        meta_problems = _check_workflow_metadata(name, metadata, prefix=".service-owned ")
        problems.extend(meta_problems)
        if not meta_problems:
            if fields.get("disable-model-invocation") != "true":
                problems.append(f"{name}: .service-owned workflow skills set `disable-model-invocation: true`")
            problems.extend(_check_openai_carrier(name, skill_dir))
    elif fields.get("disable-model-invocation") == "true":
        problems.append(f"{name}: .service-owned model skills must not set `disable-model-invocation: true`")
    return problems


def _check_single_skill(skill_dir: Path, lock: dict[str, Any] | None) -> list[str]:
    name = skill_dir.name
    marker = skill_dir / ".service-owned"
    if marker.exists() or marker.is_symlink():
        if lock is None:
            return [f"{name}: canonical source must not contain .service-owned"]
        if lock["state"] != "complete":
            return [f"{name}: .service-owned requires a complete initialized template.lock"]
        return _check_service_owned_skill(name, skill_dir)

    skill = skill_dir / "SKILL.md"
    if skill.is_symlink() or not skill.is_file():
        return [f"{name}: missing SKILL.md"]
    try:
        skill_text = skill.read_text(encoding="utf-8")
        fields, body = parse_frontmatter(skill_text)
    except (UnicodeDecodeError, ValueError) as err:
        return [f"{name}: {err}"]
    if "\r" in body:
        return [f"{name}: SKILL.md contains carriage returns (CRLF)"]

    workflow = "metadata" in fields
    problems = _check_skill_description(
        name,
        fields,
        require_use_trigger=not _is_user_workflow(fields),
    )
    if workflow:
        if set(fields) != WORKFLOW_KEYS:
            problems.append(
                f"{name}: workflow frontmatter keys must be exactly {sorted(WORKFLOW_KEYS)}, got {sorted(fields)}"
            )
        problems.extend(_check_workflow_metadata(name, fields.get("metadata")))
        if fields.get("disable-model-invocation") != "true":
            problems.append(f"{name}: workflow skills set `disable-model-invocation: true`")
        problems.extend(_check_workflow_body(name, body))
    else:
        if set(fields) != DECISION_KEYS:
            problems.append(
                f"{name}: frontmatter keys must be exactly {sorted(DECISION_KEYS)}, got {sorted(fields)}"
            )
        problems.extend(_check_decision_body(name, body))
    problems.extend(_check_skill_entries(name, skill_dir, workflow=workflow, body=body))
    return problems


def _validate_skills(repo: Path) -> list[str]:
    skills_root = repo / ".agents/skills"
    if not skills_root.is_dir():
        _fail(".agents/skills is missing")
    lock = load_lock(repo, required=False)
    names: list[str] = []
    problems: list[str] = []
    for entry in sorted(skills_root.iterdir()):
        if not entry.is_dir() and not entry.is_symlink():
            problems.append(f".agents/skills/{entry.name} is not a skill directory")
            continue
        rel_dir = entry.relative_to(repo).as_posix()
        if entry.is_symlink():
            _fail(f"{rel_dir} is a symlink; canonical skill directories must be real")
        problems.extend(_check_single_skill(entry, lock))
        names.append(entry.name)
    if not names:
        _fail(".agents/skills contains no skills")
    if problems:
        _fail("\n".join(problems))
    return names


def _read_setting_leaf(repo: Path, adapter: str) -> Any:
    spec = ADAPTERS[adapter].setting_spec
    assert spec is not None
    src_rel, _, parent_key, leaf_key = spec
    raw = (repo / src_rel).read_bytes()
    data = parse_json_bytes(raw, src_rel)
    if not isinstance(data, dict):
        _fail(f"{src_rel} root must be an object")
    parent = data.get(parent_key)
    if not isinstance(parent, dict) or leaf_key not in parent:
        _fail(f"{src_rel} has no managed setting")
        raise AssertionError("unreachable")
    leaf = parent[leaf_key]
    if adapter == "claude":
        if not isinstance(leaf, str) or not re.fullmatch(r"[0-9]+", leaf):
            _fail(f"{src_rel} managed setting has an invalid type")
    else:
        if type(leaf) is not int or leaf <= 0:
            _fail(f"{src_rel} managed setting has an invalid type")
    return leaf


def _adapter_is_materialized_on_disk(repo: Path, adapter: str) -> bool:
    pack = ADAPTERS[adapter]
    candidates: list[str] = [rel_dir for rel_dir, _ in pack.role_dirs]
    if adapter == "codex":
        candidates.append(".codex/config.toml")
    if adapter in ("claude", "qwen"):
        candidates.append(f".{adapter}/skills")
    candidates.extend(dst for _, dst in pack.static_copies)
    if pack.setting_spec is not None:
        candidates.append(pack.setting_spec[1])
    return any((repo / rel).exists() or (repo / rel).is_symlink() for rel in candidates)


def _check_file_bytes(repo: Path, rel: str, expected: bytes) -> None:
    path = repo / rel
    if path.is_symlink() or not path.is_file():
        _fail(f"{rel} is missing or stale; run scripts/harness/sync.py --apply")
    try:
        actual = path.read_bytes()
    except OSError:
        _fail(f"{rel} is stale (unreadable); run scripts/harness/sync.py --apply")
        raise AssertionError("unreachable")
    if actual != expected:
        diff = difflib.unified_diff(
            actual.decode("utf-8", errors="replace").splitlines(keepends=True),
            expected.decode("utf-8", errors="replace").splitlines(keepends=True),
            fromfile=rel,
            tofile=f"{rel} (expected)",
        )
        sys.stderr.writelines(diff)
        _fail(f"{rel} is stale; run scripts/harness/sync.py --apply")


def _sync_or_check(repo: Path, mode: str, explicit_harness: str | None, require_materialized: bool) -> None:
    harness = _resolve_selected_harness(repo, explicit_harness)
    selected = _selected_set(harness)
    _preflight(repo, selected)

    role_names, rendered_roles = _render_roles(repo)
    skill_names = _validate_skills(repo)
    codex_config = _render_codex_config(repo, role_names) if "codex" in selected else b""

    static_payloads: dict[str, bytes] = {}
    setting_values: dict[str, Any] = {}
    for adapter in selected:
        pack = ADAPTERS[adapter]
        for src_rel, dst_rel in pack.static_copies:
            _read_text_no_symlink(repo, src_rel)
            static_payloads[dst_rel] = (repo / src_rel).read_bytes()
        if pack.setting_spec is not None:
            setting_values[adapter] = _read_setting_leaf(repo, adapter)

    must_exist = require_materialized or (repo / "template.lock").exists()

    if mode == "apply":
        for adapter in selected:
            pack = ADAPTERS[adapter]
            for rel_dir, ext in pack.role_dirs:
                target_dir = repo / rel_dir
                target_dir.mkdir(parents=True, exist_ok=True)
                expected_map = rendered_roles[rel_dir]
                for existing in sorted(target_dir.glob(f"*.{ext}")):
                    if existing.name not in expected_map:
                        existing.unlink()
                for filename, payload in sorted(expected_map.items()):
                    atomic_write(target_dir / filename, payload, 0o644)

            if adapter == "codex":
                cfg = repo / ".codex/config.toml"
                cfg.parent.mkdir(parents=True, exist_ok=True)
                atomic_write(cfg, codex_config, 0o644)

            if adapter in ("claude", "qwen"):
                links_root = repo / f".{adapter}/skills"
                links_root.mkdir(parents=True, exist_ok=True)
                for entry in list(links_root.iterdir()):
                    entry.unlink()
                for name in skill_names:
                    (links_root / name).symlink_to(f"../../.agents/skills/{name}")

            for _, dst_rel in pack.static_copies:
                dst = repo / dst_rel
                dst.parent.mkdir(parents=True, exist_ok=True)
                atomic_write(dst, static_payloads[dst_rel], 0o644)

            if pack.setting_spec is not None:
                _, dst_rel, parent_key, leaf_key = pack.setting_spec
                dst = repo / dst_rel
                dst.parent.mkdir(parents=True, exist_ok=True)
                expected_leaf = setting_values[adapter]
                if dst.exists():
                    data = parse_json_bytes(dst.read_bytes(), dst_rel)
                    if not isinstance(data, dict):
                        _fail(f"{dst_rel} root must be an object")
                        raise AssertionError("unreachable")
                    parent = data.get(parent_key)
                    if parent is not None and not isinstance(parent, dict):
                        _fail(f"{dst_rel} managed parent must be an object")
                    if (
                        isinstance(parent, dict)
                        and type(parent.get(leaf_key)) is type(expected_leaf)
                        and parent.get(leaf_key) == expected_leaf
                    ):
                        pass
                    else:
                        data.setdefault(parent_key, {})[leaf_key] = expected_leaf
                        rendered = (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
                        atomic_write(dst, rendered, 0o644)
                else:
                    data = {parent_key: {leaf_key: expected_leaf}}
                    rendered = (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
                    atomic_write(dst, rendered, 0o644)

        print(f"agent harness: {len(role_names)} roles and {len(skill_names)} skills synced for {harness}")
        return

    # mode == "check"
    checked_adapters = 0
    for adapter in selected:
        pack = ADAPTERS[adapter]
        if not must_exist and not _adapter_is_materialized_on_disk(repo, adapter):
            continue
        checked_adapters += 1
        for rel_dir, ext in pack.role_dirs:
            target_dir = repo / rel_dir
            if not target_dir.is_dir():
                _fail(f"{rel_dir} is missing")
            expected_map = rendered_roles[rel_dir]
            for existing in sorted(target_dir.glob(f"*.{ext}")):
                if existing.name not in expected_map:
                    _fail(f"{existing.relative_to(repo).as_posix()} has no canonical role source")
            for filename, payload in sorted(expected_map.items()):
                _check_file_bytes(repo, f"{rel_dir}/{filename}", payload)

        if adapter == "codex":
            _check_file_bytes(repo, ".codex/config.toml", codex_config)

        if adapter in ("claude", "qwen"):
            links_rel = f".{adapter}/skills"
            links_root = repo / links_rel
            if not links_root.is_dir():
                _fail(f"{links_rel} is missing")
            for name in skill_names:
                link = links_root / name
                link_rel = f"{links_rel}/{name}"
                expected_target = f"../../.agents/skills/{name}"
                if not link.is_symlink():
                    _fail(f"{link_rel} is missing or not a symlink")
                actual_target = os.readlink(link)
                if actual_target != expected_target:
                    _fail(f"{link_rel} points at {actual_target}, expected {expected_target}")
                if not (link / "SKILL.md").is_file():
                    _fail(f"{link_rel} does not resolve to a readable SKILL.md")
            for entry in sorted(links_root.iterdir()):
                if entry.name not in skill_names:
                    _fail(f"{entry.relative_to(repo).as_posix()} has no owner in .agents/skills")

        for _, dst_rel in pack.static_copies:
            _check_file_bytes(repo, dst_rel, static_payloads[dst_rel])
        if pack.setting_spec is not None:
            _, dst_rel, parent_key, leaf_key = pack.setting_spec
            dst = repo / dst_rel
            if dst.is_symlink() or not dst.is_file():
                _fail(f"{dst_rel} is missing")
            data = parse_json_bytes(dst.read_bytes(), dst_rel)
            if not isinstance(data, dict):
                _fail(f"{dst_rel} root must be an object")
                raise AssertionError("unreachable")
            parent = data.get(parent_key)
            expected_leaf = setting_values[adapter]
            if (
                not isinstance(parent, dict)
                or type(parent.get(leaf_key)) is not type(expected_leaf)
                or parent.get(leaf_key) != expected_leaf
            ):
                _fail(f"{dst_rel} managed setting is stale; run scripts/harness/sync.py --apply")

    status_note = (
        f"{checked_adapters} materialized adapters verified"
        if checked_adapters
        else "canonical sources verified"
    )
    print(
        f"agent harness: {len(role_names)} roles and {len(skill_names)} skills current for {harness} ({status_note})"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--apply", dest="mode", action="store_const", const="apply")
    modes.add_argument("--check", dest="mode", action="store_const", const="check")
    parser.add_argument("--repo", default=".", help="repository root (default: current directory)")
    parser.add_argument("--harness", choices=HARNESS_CHOICES, default=None, help="staged harness selection")
    parser.add_argument(
        "--require-materialized",
        action="store_true",
        help="require selected adapter outputs to exist on disk during --check",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    repo_candidate = Path(args.repo)
    if repo_candidate.is_symlink() or not repo_candidate.is_dir():
        print("agent harness: repository directory is missing or unsafe", file=sys.stderr)
        return 1
    repo = repo_candidate.resolve(strict=True)
    try:
        _sync_or_check(repo, args.mode, args.harness, args.require_materialized)
    except (HarnessSyncError, Refusal) as exc:
        print(f"agent harness: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
