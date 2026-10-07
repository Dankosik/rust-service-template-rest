#!/usr/bin/env python3
"""Charged clock and remaining-program custody for the fixed PostgreSQL lab."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time


LIMIT_MS = 16_200_000
CLEANUP_MS = 1_200_000
REPLACEMENT_MS = 2 * 420_000
ORIGINAL_START_MS = 1_791_319_908_641
ORIGINAL_DEADLINE_MS = ORIGINAL_START_MS + LIMIT_MS
ABSENCE_MS = 1_791_319_956_688
HOLD_START_MS = 1_791_319_956_723
DECISION = "postgres-sustained-operation/one-verified-absence-hold-v1"
CHILD_TAIL_NS = 15_000_000_000
RPC_SECONDS = 2
STOP_GUARD_MS = 20_000  # Q's 15-second tail, two RPCs and one polling margin.
POLL_SECONDS = 1


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


def transition(state, action, name, now, monotonic_ns=None):
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
        if monotonic_ns is not None:
            state["active"]["started_monotonic_ns"] = monotonic_ns
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
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


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


def locked_transition(path, action, name, cancellation_guard=False):
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = json.loads(path.read_text())
        now_ns = time.monotonic_ns()
        now = time.time_ns() // 1_000_000
        event = transition(state, action, name, now, now_ns)
        if cancellation_guard:
            active = state["active"]
            useful_remaining = 0
            if active:
                stage = state["stages"][active["name"]]
                elapsed = now - active["started_unix_ms"]
                if "started_monotonic_ns" in active:
                    elapsed = max(elapsed, (now_ns - active["started_monotonic_ns"]) // 1_000_000)
                used = stage["used_ms"] + elapsed
                if stage["hard_ms"] - used <= STOP_GUARD_MS:
                    event["cancel_reason"] = "active stage needs its cancellation tail: " + active["name"]
                useful_remaining = max(stage["maximum_ms"] - used, 0)
            overhead_left = state["other_overhead_limit_ms"] - event["other_overhead_elapsed_ms"]
            if overhead_left + useful_remaining <= STOP_GUARD_MS:
                event["cancel_reason"] = "bounded overhead needs its cancellation tail"
            event["cancellation_guard_ms"] = STOP_GUARD_MS
        write_json(path, state)
        with path.with_suffix(".jsonl").open("a") as stream:
            stream.write(json.dumps({"action": action, "name": name, **event}) + "\n")
        return event


class ChildScope:
    """P owns accounting and evidence; only Q owns process/resource custody."""

    def __init__(self, context_path):
        self.context_path = context_path.absolute()
        data = self.context_path.read_bytes()
        self.context = json.loads(data)
        self.context_hash = hashlib.sha256(data).hexdigest()
        if self.context["version"] != 1:
            raise ValueError("unsupported work context")
        for name in ("output", "source_copy", "build_root", "queue_script"):
            if not Path(self.context[name]).is_absolute():
                raise ValueError("work context paths must be absolute")
        self.root = Path(self.context["output"])
        self.control = self.root / "control"
        self.record_path = self.control / "child-scope.json"
        self.result_path = self.control / "scope-result.json"
        self.budget_path = self.control / "budget.json"
        for name, filename in (("source_manifest_sha256", "source.json"),
                               ("campaign_manifest_sha256", "campaign.json")):
            if hashlib.sha256((self.root / filename).read_bytes()).hexdigest() != self.context[name]:
                raise ValueError("work context manifest changed: " + filename)
        self.clock = json.loads((self.root / "campaign.json").read_text())["campaign_clock"]
        expected = ["bash", str(Path(self.context["source_copy"]) / "scripts/postgres-sustained.sh"),
                    "--work-scope", str(self.context_path)]
        if self.context["work_command"] != expected:
            raise ValueError("work command differs from the fixed context entry")
        self.record = json.loads(self.record_path.read_text()) if self.record_path.exists() else None
        if self.record and (self.record["context_sha256"] != self.context_hash
                            or self.record["context"] != self.context):
            raise ValueError("child reservation belongs to a different work context")
        self.interrupted = None
        self.sequence = 0

    def save(self):
        write_json(self.record_path, self.record)

    def pending_calls(self):
        return [path.stem for path in sorted(self.control.glob("q-*.request.json"))
                if not path.with_name(path.name.replace(".request.json", ".result.json")).exists()]

    def rpc(self, operation, *arguments, seconds=RPC_SECONDS, label=None):
        """Bound the caller wait without signalling a Q helper or its descendants.

        Q's native operation may finish after this wait. File-backed output and
        terminal receipts retain that response; a timeout is unknown, not stop.
        """
        if label is None:
            self.sequence += 1
            label = f"{time.monotonic_ns()}-{self.sequence}-{operation}"
        prefix = self.control / ("q-" + label)
        request = prefix.with_suffix(".request.json")
        command = ["bash", self.context["queue_script"], "--" + operation, *arguments]
        consume_recovery(request, {"command": command, "started_monotonic_ns": time.monotonic_ns(),
                                   "caller_wait_seconds": seconds})
        completed = threading.Event()
        outcome = {}

        def call():
            try:
                with prefix.with_suffix(".stdout").open("w") as output, prefix.with_suffix(".stderr").open("w") as errors:
                    response = subprocess.run(command, stdout=output, stderr=errors, check=False)
                    output.flush()
                    errors.flush()
                    os.fsync(output.fileno())
                    os.fsync(errors.fileno())
                    outcome["returncode"] = response.returncode
            except OSError as error:
                outcome["error"] = str(error)
            finally:
                outcome["completed_monotonic_ns"] = time.monotonic_ns()
                write_json(prefix.with_suffix(".result.json"), outcome)
                completed.set()

        threading.Thread(target=call, daemon=True).start()
        if not completed.wait(max(0, seconds)):
            write_json(prefix.with_suffix(".unknown.json"), {"reason": "Q response pending after caller wait",
                                                          "observed_monotonic_ns": time.monotonic_ns()})
            raise ValueError("Q response pending: " + label)
        if outcome.get("returncode") != 0:
            raise ValueError("Q operation failed: " + label)
        return prefix.with_suffix(".stdout").read_text().strip()

    def snapshot(self, seconds=RPC_SECONDS):
        value = json.loads(self.rpc("child-status", self.record["handle"], seconds=seconds))
        for name in ("ordinary_stop", "wait_completed", "no_command_effect", "launch_may_have_occurred"):
            if not isinstance(value[name], bool):
                raise ValueError("invalid Q child status: " + name)
        if not isinstance(value["unresolved_resources"], list):
            raise ValueError("invalid Q resource status")
        if "generation" in self.record and self.record["generation"] != value["generation"]:
            raise ValueError("Q child generation changed")
        self.record["generation"] = value["generation"]
        if value["cancel_at_ns"] is not None:
            self.latch_cancel(value["cancel_at_ns"])
        self.record["last_status"] = value
        self.record["status_observed_monotonic_ns"] = time.monotonic_ns()
        self.save()
        write_json(self.control / "child-status.json", value)
        return value

    def latch_cancel(self, when):
        if not isinstance(when, int) or when <= 0:
            raise ValueError("invalid Q cancellation time")
        previous = self.record.get("cancel_at_ns")
        self.record["cancel_at_ns"] = min(previous, when) if previous is not None else when

    def cancel(self, reason):
        write_json(self.control / "budget-stop.json", {
            "observed_unix_ms": time.time_ns() // 1_000_000, "reason": reason,
            "cancellation_guard_ms": STOP_GUARD_MS,
        })
        if self.record.get("cancel_requested_ns") is None:
            self.record["cancel_requested_ns"] = time.monotonic_ns()
            self.record["stop_reason"] = reason
            self.save()
        value = json.loads(self.rpc("child-cancel", self.record["handle"]))
        if value["accepted"] is not True:
            raise ValueError("Q did not accept child cancellation")
        self.latch_cancel(value["cancel_at_ns"])
        self.record["cancel_acknowledgement"] = value
        self.save()

    def tail_deadline(self):
        # A missing acknowledgement never creates a fresh tail on cleanup.
        first = self.record.get("cancel_at_ns")
        if first is None:
            first = min(self.record["cutoff_monotonic_ns"],
                        self.record.get("cancel_requested_ns", self.record["cutoff_monotonic_ns"]))
        return first + CHILD_TAIL_NS

    def result(self, status, reason, snapshot=None, **fields):
        if status in ("failed", "unknown"):
            write_json(self.control / "budget-stop.json", {
                "reason": reason, "observed_unix_ms": time.time_ns() // 1_000_000,
                "cancellation_guard_ms": STOP_GUARD_MS,
            })
        record = {"version": 1, "status": status, "reason": reason,
                  "context_sha256": self.context_hash,
                  "observed_unix_ms": time.time_ns() // 1_000_000,
                  "pending_q_calls": self.pending_calls(), **fields}
        if snapshot is not None:
            record.update(ordinary_stop=snapshot["ordinary_stop"],
                          no_command_effect=snapshot["no_command_effect"],
                          wait_completed=snapshot["wait_completed"],
                          command_exit=snapshot["command_exit"],
                          unresolved_resources=snapshot["unresolved_resources"])
        else:
            record.update(ordinary_stop=None,
                          no_command_effect=self.record is None or not self.record["launch_attempted"],
                          wait_completed=None, command_exit=None, unresolved_resources=None)
        write_json(self.result_path, record)
        return record

    def completion(self):
        path = self.control / "work-complete.json"
        if not path.exists():
            return False
        value = json.loads(path.read_text())
        return (value.get("version") == 1 and value.get("status") == "observations_complete"
                and all(value.get(name) == self.context[name] for name in
                        ("source_manifest_sha256", "campaign_manifest_sha256")))

    def run(self):
        if self.record is not None:
            raise ValueError("child reservation already consumed; never reserve or launch a second executor")
        mono = time.monotonic_ns()
        wall_ns = time.time_ns()
        wall = wall_ns // 1_000_000
        cutoff_wall = self.clock["effective_deadline_unix_ms"] - CLEANUP_MS
        cutoff = mono + cutoff_wall * 1_000_000 - wall_ns
        self.record = {"version": 1, "context": self.context, "context_sha256": self.context_hash,
                       "conversion_monotonic_ns": mono, "conversion_unix_ms": wall,
                       "conversion_unix_ns": wall_ns,
                       "cutoff_unix_ms": cutoff_wall, "cutoff_monotonic_ns": cutoff,
                       "handle": None, "launch_attempted": False, "reservation_attempted": False}
        consume_recovery(self.record_path, self.record)
        helper = None
        snapshot = None
        failure = None
        stopped_at = None
        try:
            event = locked_transition(self.budget_path, "check", "", cancellation_guard=True)
            if cutoff <= time.monotonic_ns() or event.get("cancel_reason"):
                raise ValueError(event.get("cancel_reason", "work cutoff already reached"))
            self.record["reservation_attempted"] = True
            self.save()
            handle = self.rpc("child-reserve", "--cancel-at-monotonic-ns", str(cutoff), label="reserve")
            if not handle or any(char.isspace() for char in handle):
                raise ValueError("invalid opaque Q handle")
            self.record["handle"] = handle
            self.save()  # Handle and absolute cutoff are durable before launch intent.
            event = locked_transition(self.budget_path, "check", "", cancellation_guard=True)
            if self.interrupted or cutoff <= time.monotonic_ns() or event.get("cancel_reason"):
                raise ValueError(event.get("cancel_reason", "controller stopped before child launch"))
            self.record["launch_attempted"] = True
            self.save()
            helper = subprocess.Popen(["bash", self.context["queue_script"], "--child-run", handle,
                                       "--", *self.context["work_command"]])
            while True:
                if self.interrupted and failure is None:
                    failure = "parent controller interrupted: " + str(self.interrupted)
                if failure is None:
                    try:
                        event = locked_transition(self.budget_path, "check", "", cancellation_guard=True)
                        failure = event.get("cancel_reason")
                        if time.monotonic_ns() >= cutoff:
                            failure = "work cutoff reached"
                    except (OSError, KeyError, TypeError, ValueError) as error:
                        failure = str(error)
                # Do not spend another status RPC before a known stop request.
                # Detection (one status + one poll), cancel RPC and Q's tail
                # together consume the fixed twenty-second early guard.
                if failure and self.record.get("cancel_requested_ns") is None:
                    try:
                        self.cancel(failure)
                    except (OSError, KeyError, TypeError, ValueError) as error:
                        failure += "; " + str(error)
                try:
                    snapshot = self.snapshot()
                except (OSError, KeyError, TypeError, ValueError) as error:
                    failure = failure or str(error)
                    snapshot = None
                if snapshot and snapshot["ordinary_stop"]:
                    stopped_at = stopped_at or time.monotonic_ns()
                    code = helper.poll()
                    if snapshot["wait_completed"] and code is not None:
                        success = (failure is None and snapshot["cancel_at_ns"] is None
                                   and snapshot["command_exit"] == 0 and code == 0 and self.completion()
                                   and not self.pending_calls())
                        self.result("observations_complete" if success else "failed",
                                    failure or ("work completed" if success else "work completion not established"),
                                    snapshot, child_run_exit=code)
                        return 0 if success else 1
                    if time.monotonic_ns() - stopped_at >= RPC_SECONDS * 1_000_000_000:
                        failure = failure or "ordinary stop has no completed command wait"
                        break
                if helper.poll() is not None and not (snapshot and snapshot["ordinary_stop"]):
                    failure = failure or "child-run exited before positive ordinary stop"
                if snapshot and snapshot["cancel_at_ns"] is not None:
                    failure = failure or "Q child cancellation observed"
                if failure and self.record.get("cancel_requested_ns") is None:
                    try:
                        self.cancel(failure)
                    except (OSError, KeyError, TypeError, ValueError) as error:
                        failure += "; " + str(error)
                if failure and time.monotonic_ns() >= self.tail_deadline():
                    break
                time.sleep(POLL_SECONDS)
        except (OSError, KeyError, TypeError, ValueError) as error:
            failure = str(error)
            if self.record.get("handle"):
                try:
                    self.cancel(failure)
                except (OSError, KeyError, TypeError, ValueError) as cancel_error:
                    failure += "; " + str(cancel_error)
        write_json(self.control / "budget-stop.json", {"reason": failure,
                                                      "observed_unix_ms": time.time_ns() // 1_000_000})
        self.result("failed" if snapshot and snapshot["ordinary_stop"] else "unknown", failure, snapshot)
        return 1

    def cleanup(self):
        errors = []
        snapshot = None
        if self.record and not self.record["handle"] and self.record["reservation_attempted"]:
            # A late exact reservation response is usable, but is never retried.
            terminal = self.control / "q-reserve.result.json"
            if terminal.exists() and json.loads(terminal.read_text()).get("returncode") == 0:
                handle = (self.control / "q-reserve.stdout").read_text().strip()
                if handle and not any(char.isspace() for char in handle):
                    self.record["handle"] = handle
                    self.save()
        if self.record and self.record["handle"]:
            try:
                snapshot = self.snapshot()
            except (OSError, KeyError, TypeError, ValueError) as error:
                errors.append(str(error))
            if not snapshot or not snapshot["ordinary_stop"]:
                try:
                    self.cancel("parent cleanup")
                except (OSError, KeyError, TypeError, ValueError) as error:
                    errors.append(str(error))
                while time.monotonic_ns() < self.tail_deadline():
                    try:
                        remaining = (self.tail_deadline() - time.monotonic_ns()) / 1_000_000_000
                        snapshot = self.snapshot(seconds=min(RPC_SECONDS, max(0, remaining)))
                        if snapshot["ordinary_stop"]:
                            break
                    except (OSError, KeyError, TypeError, ValueError) as error:
                        errors.append(str(error))
                    time.sleep(POLL_SECONDS)
        try:
            # Tail time remains charged to the active stage until this observation.
            locked_transition(self.budget_path, "cleanup", "")
            if not export_evidence(self.root, "partial_before_cleanup", self.clock["original_target_created_unix_ms"]):
                errors.append("partial export crossed the campaign deadline")
        except (OSError, KeyError, TypeError, ValueError) as error:
            errors.append(str(error))
        if self.record and self.record["handle"]:
            try:
                # Only Q's current child-scoped resource tokens authorize cleanup.
                snapshot = self.snapshot()
                for resource in snapshot["unresolved_resources"]:
                    remaining = (self.clock["effective_deadline_unix_ms"] - time.time_ns() // 1_000_000) / 1000
                    remaining = min(remaining, (self.record["cutoff_monotonic_ns"] + CLEANUP_MS * 1_000_000
                                                - time.monotonic_ns()) / 1_000_000_000)
                    if remaining <= 0:
                        errors.append("campaign deadline reached before resource cleanup")
                    try:
                        # An overrun forbids success, not bounded teardown. Q
                        # retains its existing thirty-second cleanup operation;
                        # P allows its response two further seconds, then unknown.
                        self.rpc("resource-cleanup", resource["token"], seconds=32)
                    except (OSError, KeyError, TypeError, ValueError) as error:
                        errors.append(str(error))
            except (OSError, KeyError, TypeError, ValueError) as error:
                errors.append(str(error))
            try:
                snapshot = self.snapshot()
            except (OSError, KeyError, TypeError, ValueError) as error:
                errors.append(str(error))
                snapshot = None
        no_launch = self.record is None or not self.record["launch_attempted"]
        absent = bool(snapshot and snapshot["ordinary_stop"] and snapshot["unresolved_resources"] == [])
        if not self.record or not self.record["reservation_attempted"]:
            absent = True  # No Q reservation or work launch was attempted by P.
        pending = self.pending_calls()
        if self.interrupted:
            errors.append("parent cleanup interrupted: " + str(self.interrupted))
        observed_ns = time.monotonic_ns()
        observed_ms = time.time_ns() // 1_000_000
        overrun_ms = max(0, observed_ms - self.clock["effective_deadline_unix_ms"])
        if self.record:
            overrun_ns = observed_ns - self.record["cutoff_monotonic_ns"] - CLEANUP_MS * 1_000_000
            overrun_ms = max(overrun_ms, (overrun_ns + 999_999) // 1_000_000)
        if overrun_ms > 0:
            errors.append("campaign deadline exceeded during cleanup")
        target_path = self.control / "laboratory-target.json"
        target = json.loads(target_path.read_text()) if target_path.exists() else {}
        write_json(self.root / "resource-absence.json", {
            "version": 1, "status": "verified" if absent else "unknown",
            "basis": "Q child status" if snapshot else "no work launch" if no_launch else "unavailable Q status",
            "ordinary_stop": snapshot["ordinary_stop"] if snapshot else None,
            "no_command_effect": snapshot["no_command_effect"] if snapshot else no_launch,
            "unresolved_resources": snapshot["unresolved_resources"] if snapshot else None,
            "observed_unix_ms": observed_ms, "deadline_overrun_ms": overrun_ms,
            "pending_q_calls": pending, "errors": errors,
            **{name: target.get(name) for name in
               ("compose_project", "daemon_id", "target_identity", "container_id")},
        })
        previous = json.loads(self.result_path.read_text()) if self.result_path.exists() else {}
        success = absent and not errors and not pending
        failure_status = "failed" if absent and not pending else "unknown"
        self.result(previous.get("status", "failed") if success else failure_status,
                    previous.get("reason", "parent cleanup"), snapshot,
                    child_run_exit=previous.get("child_run_exit"),
                    cleanup_complete=success, resource_absence="verified" if absent else "unknown",
                    cleanup_errors=errors, deadline_overrun_ms=overrun_ms)
        return 0 if success else 1


def control_scope(context, action):
    scope = ChildScope(context)

    def interrupted(signum, _frame):
        scope.interrupted = signum

    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    return scope.run() if action == "run-scope" else scope.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("state", type=Path)
    parser.add_argument("action", choices=("check", "begin", "pause", "finish", "confirmation", "close-matrix", "cleanup", "run-scope", "cleanup-scope", "export"))
    parser.add_argument("name", nargs="?", default="")
    parser.add_argument("value", nargs="?", default="")
    args = parser.parse_args()
    if args.action == "export":
        raise SystemExit(0 if export_evidence(args.state, args.name, int(args.value)) else 1)
    if args.action in ("run-scope", "cleanup-scope"):
        raise SystemExit(control_scope(args.state, args.action))
    print(json.dumps(locked_transition(args.state, args.action, args.name)))


if __name__ == "__main__":
    try:
        main()
    except (KeyError, TypeError, ValueError) as error:
        raise SystemExit(str(error)) from error
