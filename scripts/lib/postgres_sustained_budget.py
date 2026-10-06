#!/usr/bin/env python3
"""Charged clock and remaining-program custody for the fixed PostgreSQL lab."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import time


LIMIT_MS = 16_200_000
CLEANUP_MS = 1_200_000
REPLACEMENT_MS = 2 * 420_000
ORIGINAL_START_MS = 1_791_319_908_641
ORIGINAL_DEADLINE_MS = ORIGINAL_START_MS + LIMIT_MS
ABSENCE_MS = 1_791_319_956_688
HOLD_START_MS = 1_791_319_956_723
DECISION = "postgres-sustained-operation/one-verified-absence-hold-v1"


def campaign_clock(started, resumed, recovery=None):
    """The one accepted hold changes the deadline, never the original lineage."""
    if not 0 < started <= resumed:
        raise ValueError("invalid original campaign clock")
    original = started + LIMIT_MS
    hold = None
    if recovery is not None:
        if started != ORIGINAL_START_MS or resumed < HOLD_START_MS:
            raise ValueError("recovery differs from the accepted campaign")
        if recovery["previous_absence_unix_ms"] != ABSENCE_MS:
            raise ValueError("recovery absence differs from the accepted receipt")
        if recovery["previous_exported_unix_ms"] != HOLD_START_MS:
            raise ValueError("recovery hold precedes the accepted export")
        hold = {
            "decision_id": DECISION,
            "absence_started_unix_ms": ABSENCE_MS,
            "hold_started_unix_ms": HOLD_START_MS,
            "resumed_unix_ms": resumed,
            "excluded_ms": resumed - HOLD_START_MS,
            "charged_before_absence_ms": ABSENCE_MS - started,
            "charged_before_hold_ms": HOLD_START_MS - started,
            "through_export_wall_ms": 48_082,
            **{name: recovery[name] for name in (
                "prior_export_sha256", "prior_absence_sha256",
                "continued_absence_sha256", "review_receipt_sha256",
            )},
        }
        for name, value in hold.items():
            if name.endswith("_sha256") and (
                not isinstance(value, str) or len(value) != 64
                or any(char not in "0123456789abcdef" for char in value)
            ):
                raise ValueError("invalid recovery receipt hash")
    return {
        "version": 1,
        "original_target_created_unix_ms": started,
        "original_deadline_unix_ms": original,
        "charged_limit_ms": LIMIT_MS,
        "effective_deadline_unix_ms": original + (hold["excluded_ms"] if hold else 0),
        "recovery": hold,
    }


def initial_state(clock, preparation_debits, now):
    stages = {}

    def add(name, maximum, closure=0):
        if maximum <= 0:
            raise ValueError("no preparation budget remains")
        stages[name] = {"maximum_ms": maximum, "hard_ms": maximum + closure,
                        "used_ms": 0, "complete": False}

    for regime in ("resident", "pressured"):
        add(f"prepare-{regime}", 1_500_000 - preparation_debits.get(regime, 0))
        add(f"qualify-{regime}", 60_000, 60_000)
    for index in range(1, 5):
        add(f"calibrate-{index}", 120_000, 60_000)
    for index in range(1, 21):
        add(f"cell-{index}", 360_000, 60_000)
    for index in range(1, 28):
        add(f"reset-{index}", 60_000)
    add("composed", 1_200_000, 60_000)
    mandatory = sum(stage["maximum_ms"] for stage in stages.values())
    remaining = clock["effective_deadline_unix_ms"] - now
    overhead = remaining - mandatory - REPLACEMENT_MS - CLEANUP_MS
    if overhead < 0:
        raise ValueError("maximum remaining program, replacements and cleanup cannot fit")
    state = {
        "version": 1, "clock": clock, "resumed_unix_ms": now,
        "last_observed_unix_ms": now, "stages": stages,
        "active": None, "main_cells": 20,
        "replacement_reserve_ms": REPLACEMENT_MS,
        "cleanup_reserve_ms": CLEANUP_MS,
        "other_overhead_limit_ms": overhead,
        "cleanup_started_unix_ms": None,
    }
    observe(state, now)
    return state


def observe(state, now):
    if now < state["last_observed_unix_ms"] or now < state["resumed_unix_ms"]:
        raise ValueError("campaign clock moved backwards")
    active = state["active"]
    active_elapsed = now - active["started_unix_ms"] if active else 0
    spent = sum(min(stage["used_ms"] + (
        active_elapsed if active and active["name"] == name else 0
    ), stage["maximum_ms"]) for name, stage in state["stages"].items())
    wall = now - state["resumed_unix_ms"]
    cleanup_start = state["cleanup_started_unix_ms"]
    cleanup_elapsed = now - cleanup_start if cleanup_start is not None else 0
    overhead = wall - spent - cleanup_elapsed
    if overhead < 0:
        raise ValueError("stage accounting exceeds elapsed wall time")
    remaining_stages = 0
    for name, stage in state["stages"].items():
        used = stage["used_ms"] + (active_elapsed if active and active["name"] == name else 0)
        if used > stage["hard_ms"]:
            raise ValueError(f"stage exceeded its fixed bound: {name}")
        if not stage["complete"]:
            remaining_stages += max(stage["maximum_ms"] - used, 0)
    if overhead > state["other_overhead_limit_ms"]:
        raise ValueError("bounded setup and closure overhead exceeded")
    required = (
        remaining_stages + state["replacement_reserve_ms"]
        + state["cleanup_reserve_ms"] + state["other_overhead_limit_ms"] - overhead
    ) if cleanup_start is None else 0
    deadline = state["clock"]["effective_deadline_unix_ms"]
    if now + required > deadline:
        raise ValueError("maximum remaining program cannot fit effective deadline")
    if now > deadline:
        raise ValueError("effective campaign deadline exceeded")
    original = state["clock"]["original_target_created_unix_ms"]
    excluded = (state["clock"]["recovery"] or {}).get("excluded_ms", 0)
    state["last_observed_unix_ms"] = now
    return {
        "observed_unix_ms": now, "effective_deadline_unix_ms": deadline,
        "full_wall_elapsed_ms": now - original, "excluded_ms": excluded,
        "charged_elapsed_ms": now - original - excluded,
        "remaining_charged_ms": deadline - now,
        "maximum_remaining_stages_ms": remaining_stages,
        "replacement_reserve_ms": state["replacement_reserve_ms"],
        "cleanup_reserve_ms": state["cleanup_reserve_ms"],
        "other_overhead_elapsed_ms": overhead,
        "other_overhead_limit_ms": state["other_overhead_limit_ms"],
        "maximum_remaining_required_ms": required,
        "active_stage": active["name"] if active else None,
    }


def transition(state, action, name, now):
    if action == "cleanup":
        # A failed admission must still be able to enter cleanup and retain costs.
        if state["cleanup_started_unix_ms"] is None:
            if state["active"]:
                active = state["active"]
                state["stages"][active["name"]]["used_ms"] += now - active["started_unix_ms"]
                state["active"] = None
            state["cleanup_started_unix_ms"] = now
        state["last_observed_unix_ms"] = max(state["last_observed_unix_ms"], now)
        return {"event": "cleanup_started", "observed_unix_ms": now}
    observe(state, now)
    if state["cleanup_started_unix_ms"] is not None:
        raise ValueError("campaign already entered cleanup")
    if action == "confirmation":
        cells = int(name)
        if cells not in (16, 20) or any(
            state["stages"][f"cell-{index}"]["used_ms"] for index in range(9, 21)
        ):
            raise ValueError("confirmation must fix the accepted branch before its first cell")
        if any(not state["stages"][f"cell-{index}"]["complete"] for index in range(1, 9)):
            raise ValueError("confirmation cannot discard unfinished screening")
        for prefix, upper in (("cell", 20), ("reset", 27)):
            keep = cells if prefix == "cell" else cells + 7
            for index in range(keep + 1, upper + 1):
                stage = state["stages"][f"{prefix}-{index}"]
                if stage["used_ms"] or stage["complete"]:
                    raise ValueError("cannot remove a spent stage")
                del state["stages"][f"{prefix}-{index}"]
        state["main_cells"] = cells
    elif action == "close-matrix":
        if any(not stage["complete"] for label, stage in state["stages"].items()
               if label.startswith("cell-")):
            raise ValueError("cannot close an incomplete comparison matrix")
        state["replacement_reserve_ms"] = 0
    elif action == "begin":
        if state["active"] or name not in state["stages"] or state["stages"][name]["complete"]:
            raise ValueError("unknown, overlapping or already completed stage")
        state["active"] = {"name": name, "started_unix_ms": now}
    elif action in ("pause", "finish"):
        active = state["active"]
        if not active or active["name"] != name:
            raise ValueError("stage custody does not match")
        stage = state["stages"][name]
        stage["used_ms"] += now - active["started_unix_ms"]
        stage["complete"] = action == "finish"
        state["active"] = None
    elif action != "check":
        raise ValueError("unknown budget transition")
    return observe(state, now)


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def consume_recovery(path, receipt):
    """A failed or interrupted resumed effect cannot mint another exclusion."""
    with path.open("x") as stream:
        json.dump(receipt, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def export_evidence(root, status, started):
    files = []
    total = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("evidence contains a symlink")
        if not path.is_file() or path.name in ("export.json", "export.json.tmp"):
            continue
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
            os.fsync(stream.fileno())
        size = path.stat().st_size
        total += size
        files.append({"path": str(path.relative_to(root)), "bytes": size,
                      "sha256": digest.hexdigest()})
    if total > 2 * 1024**3:
        raise ValueError("2 GiB campaign evidence bound exceeded")
    now = time.time_ns() // 1_000_000
    record = {"status": status, "target_created_unix_ms": started,
              "exported_unix_ms": now, "bytes": total, "files": files}
    campaign = root / "campaign.json"
    clock = json.loads(campaign.read_text())["campaign_clock"] if campaign.exists() else None
    excluded = (clock["recovery"] or {}).get("excluded_ms", 0) if clock else 0
    if clock:
        record.update(campaign_clock=clock,
                      full_wall_elapsed_ms=now - clock["original_target_created_unix_ms"],
                      excluded_ms=excluded,
                      charged_elapsed_ms=now - clock["original_target_created_unix_ms"] - excluded,
                      effective_deadline_unix_ms=clock["effective_deadline_unix_ms"])

    def persist():
        write_json(root / "export.json", record)
        descriptor = os.open(root, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    persist()
    completed = time.time_ns() // 1_000_000
    earliest = ((clock["recovery"] or {}).get("resumed_unix_ms")
                or clock["original_target_created_unix_ms"]) if clock else now
    if clock and (completed > clock["effective_deadline_unix_ms"]
                  or completed < now or now < earliest):
        record.update(status="failed", deadline_failure_observed_unix_ms=completed,
                      full_wall_elapsed_ms=completed - clock["original_target_created_unix_ms"],
                      charged_elapsed_ms=completed - clock["original_target_created_unix_ms"] - excluded)
        persist()
        return False
    return True


def locked_transition(path, action, name):
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = json.loads(path.read_text())
        event = transition(state, action, name, time.time_ns() // 1_000_000)
        write_json(path, state)
        with path.with_suffix(".jsonl").open("a") as stream:
            stream.write(json.dumps({"action": action, "name": name, **event}) + "\n")
        return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("state", type=Path)
    parser.add_argument("action", choices=("check", "begin", "pause", "finish", "confirmation", "close-matrix", "cleanup", "watch", "export"))
    parser.add_argument("name", nargs="?", default="")
    parser.add_argument("value", nargs="?", default="")
    args = parser.parse_args()
    if args.action == "export":
        raise SystemExit(0 if export_evidence(args.state, args.name, int(args.value)) else 1)
    if args.action != "watch":
        print(json.dumps(locked_transition(args.state, args.action, args.name)))
        return
    parent = int(args.name)
    if parent != os.getppid():
        raise ValueError("watchdog parent custody mismatch")
    while os.getppid() == parent:
        try:
            locked_transition(args.state, "check", "")
        except Exception as error:
            try:
                write_json(args.state.with_name("budget-stop.json"), {
                    "observed_unix_ms": time.time_ns() // 1_000_000, "reason": str(error),
                })
                for ready in (args.state.parent.parent / "attempts").rglob("*.ready"):
                    (ready.parent / "stop").write_text("campaign budget stop\n")
            finally:
                os.kill(parent, signal.SIGTERM)
            return
        time.sleep(1)


if __name__ == "__main__":
    try:
        main()
    except (KeyError, TypeError, ValueError) as error:
        raise SystemExit(str(error)) from error
