#!/usr/bin/env python3
"""Exercise model-owned auto handoff and parallel task execution through a real TUI PTY."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import Callable


SUPPORT_SCRIPT = Path(__file__).with_name("tui-stateful-pty-acceptance.py")
SUPPORT_SPEC = importlib.util.spec_from_file_location("tui_stateful_support", SUPPORT_SCRIPT)
assert SUPPORT_SPEC is not None and SUPPORT_SPEC.loader is not None
SUPPORT = importlib.util.module_from_spec(SUPPORT_SPEC)
sys.modules[SUPPORT_SPEC.name] = SUPPORT
SUPPORT_SPEC.loader.exec_module(SUPPORT)

SCHEMA_VERSION = 1
MODEL_NAME = "orchestration-fixture-model"
FINAL_CANARY = "ORCHESTRATION-PTY-FINAL-CANARY-7319"
APPROVAL_FINAL_CANARY = "ORCHESTRATION-PTY-APPROVAL-FINAL-8427"
USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-2486"
APPROVAL_USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-3597"
APPROVAL_PATH = "approval-note.txt"
APPROVAL_CONTENT = "approved task write\n"
APPROVAL_TOOL_CALL_ID = "approval-write-call"
CONTINUE_FINAL_CANARY = "ORCHESTRATION-PTY-CONTINUE-FINAL-9531"
CONTINUE_USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-4608"
CANCEL_USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-5719"
INTEGRATION_FINAL_CANARY = "ORCHESTRATION-PTY-INTEGRATION-FINAL-6842"
INTEGRATION_USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-6820"
INTEGRATION_PATHS = ("integration-a.txt", "integration-b.txt")
INTEGRATION_TOOL_CALL_IDS = ("integration-write-a", "integration-write-b")
TERMINAL_FINAL_CANARY = "ORCHESTRATION-PTY-TERMINAL-FINAL-7953"
TERMINAL_USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-7931"
TERMINAL_TASK_ID = "orchestration-pty-terminal"
TERMINAL_START_TOOL_CALL_ID = "terminal-start-call"
TERMINAL_CANCEL_TOOL_CALL_ID = "terminal-cancel-call"
TERMINAL_READY_CANARY = "ORCHESTRATION-PTY-TERMINAL-READY-8174"
TERMINAL_PROGRESS_CANARY = "ORCHESTRATION-PTY-TERMINAL-PROGRESS-9285"
READ_STEP_IDS = ("inspect_kernel", "inspect_runtime")
READ_STEP_TITLES = ("Inspect kernel route", "Inspect runtime route")
PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": READ_STEP_IDS[0],
                "title": READ_STEP_TITLES[0],
                "role": "subagent_read",
                "mode": "read",
                "isolation": "shared_read_only",
            },
            {
                "step_id": READ_STEP_IDS[1],
                "title": READ_STEP_TITLES[1],
                "role": "subagent_read",
                "mode": "read",
                "isolation": "shared_read_only",
            },
        ],
    },
    separators=(",", ":"),
)
HANDOFF_ARGS = json.dumps(
    {"reason_codes": ["parallel_research", "multi_stage_change"]},
    separators=(",", ":"),
)
APPROVAL_PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "write_note",
                "title": "Write approved note",
                "role": "executor",
                "mode": "write",
                "isolation": "sequential_workspace_write",
            }
        ],
    },
    separators=(",", ":"),
)
APPROVAL_WRITE_ARGS = json.dumps(
    {"path": APPROVAL_PATH, "content": APPROVAL_CONTENT},
    separators=(",", ":"),
)
CONTINUE_PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "continue_after_crash",
                "title": "Continue after crash",
                "role": "executor",
                "mode": "read",
                "isolation": "shared_read_only",
            }
        ],
    },
    separators=(",", ":"),
)
CANCEL_PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "cancel_in_flight",
                "title": "Cancel in-flight task",
                "role": "executor",
                "mode": "read",
                "isolation": "shared_read_only",
            }
        ],
    },
    separators=(",", ":"),
)
INTEGRATION_PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "integrate_a",
                "title": "Propose integration A",
                "role": "subagent_write",
                "mode": "write",
                "isolation": "worktree",
            },
            {
                "step_id": "integrate_b",
                "title": "Propose integration B",
                "role": "subagent_write",
                "mode": "write",
                "isolation": "worktree",
            },
        ],
    },
    separators=(",", ":"),
)
TERMINAL_PLAN_ARGS = json.dumps(
    {
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "terminal_lifecycle",
                "title": "Exercise persistent terminal lifecycle",
                "role": "executor",
                "mode": "write",
                "isolation": "sequential_workspace_write",
            }
        ],
    },
    separators=(",", ":"),
)
TERMINAL_START_ARGS = json.dumps(
    {
        "task_id": TERMINAL_TASK_ID,
        "command": (
            f"printf '{TERMINAL_READY_CANARY}\\n'; "
            f"printf '{TERMINAL_PROGRESS_CANARY}\\n'; "
            "while :; do sleep 1; done"
        ),
        "mode": "background",
        "readiness": {
            "kind": "output_contains",
            "value": TERMINAL_READY_CANARY,
            "timeout_secs": 5,
        },
    },
    separators=(",", ":"),
)
TERMINAL_CANCEL_ARGS = json.dumps(
    {"task_id": TERMINAL_TASK_ID},
    separators=(",", ":"),
)


INTEGRATION_WRITE_ARGS = tuple(
    json.dumps(
        {"path": path, "content": f"integrated {suffix}\n"},
        separators=(",", ":"),
    )
    for path, suffix in zip(INTEGRATION_PATHS, ("a", "b"), strict=True)
)


class AcceptanceError(RuntimeError):
    """Raised when the orchestration PTY contract is violated."""


@dataclasses.dataclass(frozen=True)
class SessionAudit:
    event_counts: dict[str, int]
    completed_steps: tuple[str, ...]
    final_answer_count: int
    approval_final_answer_count: int
    continue_final_answer_count: int
    integration_final_answer_count: int
    terminal_final_answer_count: int
    task_final_count: int
    approval_route_resolved_count: int
    approved_tool_call_count: int
    terminal_approval_route_resolved_count: int
    approved_terminal_start_count: int
    terminal_start_completed_count: int
    terminal_cancel_completed_count: int
    terminal_task_statuses: tuple[str, ...]
    terminal_readiness_states: tuple[str, ...]
    terminal_max_output_bytes: int
    paused_task_run_count: int
    interrupted_step_count: int
    cancelled_task_run_count: int
    promotion_preview_count: int
    promotion_authority_count: int
    promoted_integration_count: int
    parent_verification_count: int
    failed_run_count: int
    cancelled_run_count: int


@dataclasses.dataclass
class FixtureState:
    request_counts: dict[str, int] = dataclasses.field(default_factory=dict)
    request_order: list[str] = dataclasses.field(default_factory=list)
    protocol_errors: list[str] = dataclasses.field(default_factory=list)
    active_reads: int = 0
    max_concurrent_reads: int = 0
    expected_disconnects: int = 0
    crash_release: threading.Event = dataclasses.field(default_factory=threading.Event)
    cancel_release: threading.Event = dataclasses.field(default_factory=threading.Event)
    cancel_settled: threading.Event = dataclasses.field(default_factory=threading.Event)
    lock: threading.Lock = dataclasses.field(default_factory=threading.Lock)

    def start_request(self, kind: str) -> int:
        with self.lock:
            self.request_counts[kind] = self.request_counts.get(kind, 0) + 1
            request_number = self.request_counts[kind]
            self.request_order.append(kind)
            if kind.startswith("read:"):
                self.active_reads += 1
                self.max_concurrent_reads = max(
                    self.max_concurrent_reads,
                    self.active_reads,
                )
            return request_number

    def finish_request(self, kind: str) -> None:
        if not kind.startswith("read:"):
            return
        with self.lock:
            self.active_reads -= 1

    def record_error(self, error: Exception) -> None:
        with self.lock:
            self.protocol_errors.append(f"{type(error).__name__}: {error}")

    def record_expected_disconnect(self) -> None:
        with self.lock:
            self.expected_disconnects += 1


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    fixture: FixtureState

    def handle_error(
        self,
        request: object,
        client_address: tuple[str, int],
    ) -> None:
        error = sys.exc_info()[1]
        if isinstance(error, (BrokenPipeError, ConnectionResetError)):
            self.fixture.record_expected_disconnect()
            return
        super().handle_error(request, client_address)


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    @property
    def fixture(self) -> FixtureState:
        server = self.server
        assert isinstance(server, FixtureServer)
        return server.fixture

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        kind = ""
        request_number = 0
        try:
            payload = self._read_json()
            if not self.path.endswith("/chat/completions"):
                raise AcceptanceError(f"unexpected fixture path {self.path}")
            kind = classify_request(payload)
            request_number = self.fixture.start_request(kind)
            if kind == "conversation":
                self._send_tool_call(
                    f"handoff-call-{request_number}",
                    "request_task_planning",
                    HANDOFF_ARGS,
                )
            elif kind == "planner":
                arguments = {
                    1: PLAN_ARGS,
                    2: APPROVAL_PLAN_ARGS,
                    3: CONTINUE_PLAN_ARGS,
                    4: CANCEL_PLAN_ARGS,
                    5: INTEGRATION_PLAN_ARGS,
                    6: TERMINAL_PLAN_ARGS,
                }.get(request_number)
                if arguments is None:
                    raise AcceptanceError(
                        f"unexpected planner request {request_number}"
                    )
                self._send_tool_call(
                    f"task-plan-call-{request_number}",
                    "task_plan_update",
                    arguments,
                )
            elif kind.startswith("read:"):
                time.sleep(0.35)
                step_id = kind.removeprefix("read:")
                self._send_text(f"bounded result for {step_id}")
            elif kind == "write:request":
                self._send_tool_call(
                    APPROVAL_TOOL_CALL_ID,
                    "write_file",
                    APPROVAL_WRITE_ARGS,
                )
            elif kind == "write:after_tool":
                self._send_text("approved write completed")
            elif kind == "continue:step":
                if request_number == 1:
                    if not self.fixture.crash_release.wait(timeout=30):
                        raise TimeoutError("crash fixture was not released")
                    self._send_text("obsolete pre-crash response")
                else:
                    self._send_text("resumed step completed")
            elif kind == "cancel:step":
                if not self.fixture.cancel_release.wait(timeout=30):
                    raise TimeoutError("cancel fixture was not released")
                self._send_text("obsolete cancelled response")
            elif kind.startswith("integration:step:"):
                suffix = kind.removeprefix("integration:step:")
                index = {"a": 0, "b": 1}.get(suffix)
                if index is None:
                    raise AcceptanceError(f"unexpected integration step {kind}")
                if has_tool_result(payload, INTEGRATION_TOOL_CALL_IDS[index]):
                    self._send_text(f"integration step {suffix} complete")
                else:
                    self._send_tool_call(
                        INTEGRATION_TOOL_CALL_IDS[index],
                        "write_file",
                        INTEGRATION_WRITE_ARGS[index],
                    )
            elif kind == "terminal:start":
                self._send_tool_call(
                    TERMINAL_START_TOOL_CALL_ID,
                    "terminal_start",
                    TERMINAL_START_ARGS,
                )
            elif kind == "terminal:after_start":
                self._send_tool_call(
                    TERMINAL_CANCEL_TOOL_CALL_ID,
                    "terminal_cancel",
                    TERMINAL_CANCEL_ARGS,
                )
            elif kind == "terminal:after_cancel":
                self._send_text("persistent terminal lifecycle completed")
            elif kind == "synthesis":
                final = {
                    1: FINAL_CANARY,
                    2: APPROVAL_FINAL_CANARY,
                    3: CONTINUE_FINAL_CANARY,
                    4: INTEGRATION_FINAL_CANARY,
                    5: TERMINAL_FINAL_CANARY,
                }.get(request_number)
                if final is None:
                    raise AcceptanceError(
                        f"unexpected synthesis request {request_number}"
                    )
                self._send_text(final)
            elif kind == "title":
                self._send_text("Orchestration acceptance")
            else:
                raise AcceptanceError(f"unsupported request kind {kind}")
        except Exception as error:  # noqa: BLE001 - retain fixture diagnostics.
            expected_disconnect = (
                kind in {"continue:step", "cancel:step"}
                and request_number == 1
                and isinstance(error, (BrokenPipeError, ConnectionResetError))
            )
            if expected_disconnect:
                self.fixture.record_expected_disconnect()
            else:
                self.fixture.record_error(error)
                try:
                    self._send_json({"error": f"fixture failure: {error}"}, status=500)
                except (BrokenPipeError, ConnectionResetError):
                    pass
        finally:
            if kind:
                self.fixture.finish_request(kind)
            if kind == "cancel:step":
                self.fixture.cancel_settled.set()

    def _read_json(self) -> object:
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            chunks: list[bytes] = []
            while True:
                size = int(self.rfile.readline().split(b";", 1)[0].strip(), 16)
                if size == 0:
                    while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                        pass
                    break
                chunks.append(self.rfile.read(size))
                if self.rfile.read(2) != b"\r\n":
                    raise ValueError("invalid chunked request")
            raw = b"".join(chunks)
        else:
            length = int(self.headers.get("Content-Length", "0"))
            if length > 2 * 1024 * 1024:
                raise ValueError("fixture request exceeds 2 MiB")
            raw = self.rfile.read(length)
        return json.loads(raw.decode("utf-8"))

    def _send_tool_call(
        self,
        call_id: str,
        tool_name: str,
        arguments: str,
    ) -> None:
        self._send_sse(
            {
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        }
                    ]
                },
                "finish_reason": "tool_calls",
            }
        )

    def _send_text(self, content: str) -> None:
        self._send_sse(
            {
                "delta": {"content": content},
                "finish_reason": "stop",
            }
        )

    def _send_sse(self, choice: dict[str, object]) -> None:
        body = (
            f"data: {json.dumps({'choices': [choice]}, separators=(',', ':'))}\n\n"
            "data: [DONE]\n\n"
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_json(self, payload: object, status: int = 200) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run Sigil's model-owned orchestration real-PTY acceptance.",
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-orchestration-acceptance"),
    )
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--keep-fixture", action="store_true")
    return parser.parse_args()


def tool_names(payload: object) -> set[str]:
    if not isinstance(payload, dict):
        return set()
    tools = payload.get("tools")
    if not isinstance(tools, list):
        return set()
    names = set()
    for tool in tools:
        function = tool.get("function") if isinstance(tool, dict) else None
        name = function.get("name") if isinstance(function, dict) else None
        if isinstance(name, str):
            names.add(name)
    return names


def request_text(payload: object) -> str:
    if not isinstance(payload, dict):
        return ""
    messages = payload.get("messages")
    if not isinstance(messages, list):
        return ""
    values: list[str] = []
    for message in messages:
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if isinstance(content, str):
            values.append(content)
        elif content is not None:
            values.append(json.dumps(content, sort_keys=True))
    return "\n".join(values)


def has_tool_result(payload: object, call_id: str) -> bool:
    if not isinstance(payload, dict):
        return False
    messages = payload.get("messages")
    if not isinstance(messages, list):
        return False
    return any(
        isinstance(message, dict)
        and message.get("role") == "tool"
        and message.get("tool_call_id") == call_id
        for message in messages
    )


def classify_request(payload: object) -> str:
    names = tool_names(payload)
    text = request_text(payload)
    if (
        not names
        and "Generate a concise semantic title for a coding-agent conversation" in text
    ):
        return "title"
    if "request_task_planning" in names:
        return "conversation"
    if "task_plan_update" in names:
        return "planner"
    if "Produce the single user-visible final answer" in text:
        return "synthesis"
    if "Role: subagent_read" in text:
        matching = [step_id for step_id in READ_STEP_IDS if f"Step: {step_id}" in text]
        if len(matching) == 1:
            return f"read:{matching[0]}"
    if "Step: write_note" in text and "Role: executor" in text:
        if has_tool_result(payload, APPROVAL_TOOL_CALL_ID):
            return "write:after_tool"
        return "write:request"
    if "Step: continue_after_crash" in text and "Role: executor" in text:
        return "continue:step"
    if "Step: cancel_in_flight" in text and "Role: executor" in text:
        return "cancel:step"
    if "Step: integrate_a" in text and "Role: subagent_write" in text:
        return "integration:step:a"
    if "Step: integrate_b" in text and "Role: subagent_write" in text:
        return "integration:step:b"
    if "Step: terminal_lifecycle" in text and "Role: executor" in text:
        if has_tool_result(payload, TERMINAL_CANCEL_TOOL_CALL_ID):
            return "terminal:after_cancel"
        if has_tool_result(payload, TERMINAL_START_TOOL_CALL_ID):
            return "terminal:after_start"
        return "terminal:start"
    raise AcceptanceError("provider request does not match a production orchestration role")


def write_config(
    path: Path,
    *,
    workspace: Path,
    state_root: Path,
    cache_root: Path,
    session_dir: Path,
    port: int,
) -> None:
    endpoint = f"http://127.0.0.1:{port}/v1"
    path.write_text(
        f'''config_version = 2

[workspace]
root = "{workspace}"

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[session]
log_dir = "{session_dir}"

[agent]
connection = "orchestration-fixture"
model = "{MODEL_NAME}"
max_turns = 8
tool_timeout_secs = 10

[model_request]
request_timeout_secs = 10
stream_idle_timeout_secs = 10

[task]
enabled = true
routing_policy = "auto"
multi_agent_mode = "proactive"
max_subagents = 4
max_parallel_read_steps = 2
max_parallel_changeset_steps = 2
allow_write_subagents = true

[permission]
mode = "manual"

[permission.tools]
terminal_cancel = "allow"

[[permission.rules]]
tool_name = "write_file"
subject_glob = "integration-*.txt"
mode = "allow"

[terminal]
keyboard_enhancement = "off"
mouse_capture = true
osc52_clipboard = false

[connections.orchestration-fixture]
label = "Orchestration fixture"
provider = "custom"
protocol = "chat_completions"
base_url = "{endpoint}"
credential = {{ source = "none" }}
''',
        encoding="utf-8",
    )
    os.chmod(path, 0o600)


def read_session_audit(path: Path) -> SessionAudit:
    counts: dict[str, int] = {}
    completed_steps: list[str] = []
    final_answer_count = 0
    approval_final_answer_count = 0
    continue_final_answer_count = 0
    integration_final_answer_count = 0
    terminal_final_answer_count = 0
    task_final_count = 0
    approval_route_resolved_count = 0
    approved_tool_call_count = 0
    terminal_approval_route_resolved_count = 0
    approved_terminal_start_count = 0
    terminal_start_completed_count = 0
    terminal_cancel_completed_count = 0
    terminal_task_statuses: set[str] = set()
    terminal_readiness_states: set[str] = set()
    terminal_max_output_bytes = 0
    paused_task_run_count = 0
    interrupted_step_count = 0
    cancelled_task_run_count = 0
    promotion_preview_count = 0
    promotion_authority_count = 0
    promoted_integration_count = 0
    parent_verification_count = 0
    approval_child_refs: set[str] = set()
    failed_run_count = 0
    cancelled_run_count = 0
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(raw_line)
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            counts[event_type] = counts.get(event_type, 0) + 1
        payload = record.get("payload")
        if not isinstance(payload, dict):
            continue
        if event_type == "run_finalized":
            outcome = payload.get("outcome")
            if outcome == "cancelled" and payload.get("cleanup_complete") is True:
                cancelled_run_count += 1
            else:
                failed_run_count += 1
        entry = payload.get("session_log_entry")
        if not isinstance(entry, dict):
            continue
        assistant = entry.get("assistant")
        if (
            isinstance(assistant, dict)
            and assistant.get("assistant_kind") == "final_answer"
        ):
            if assistant.get("content") == FINAL_CANARY:
                final_answer_count += 1
            elif assistant.get("content") == APPROVAL_FINAL_CANARY:
                approval_final_answer_count += 1
            elif assistant.get("content") == CONTINUE_FINAL_CANARY:
                continue_final_answer_count += 1
            elif assistant.get("content") == INTEGRATION_FINAL_CANARY:
                integration_final_answer_count += 1
            elif assistant.get("content") == TERMINAL_FINAL_CANARY:
                terminal_final_answer_count += 1
        control = entry.get("control")
        if not isinstance(control, dict):
            continue
        step = control.get("task_step")
        if (
            isinstance(step, dict)
            and step.get("status") == "completed"
            and isinstance(step.get("step_id"), str)
        ):
            completed_steps.append(step["step_id"])
        if isinstance(step, dict) and step.get("status") == "interrupted":
            interrupted_step_count += 1
        if isinstance(control.get("task_final_answer_committed"), dict):
            task_final_count += 1
        task_run = control.get("task_run")
        if isinstance(task_run, dict) and task_run.get("status") == "paused":
            paused_task_run_count += 1
        if isinstance(task_run, dict) and task_run.get("status") == "cancelled":
            cancelled_task_run_count += 1
        if isinstance(control.get("task_promotion_preview_recorded"), dict):
            promotion_preview_count += 1
        if isinstance(control.get("task_promotion_authority_consumed"), dict):
            promotion_authority_count += 1
        promotion = control.get("integration_promotion_recorded")
        if isinstance(promotion, dict) and promotion.get("status") == "promoted":
            promoted_integration_count += 1
        if isinstance(control.get("task_parent_verification_recorded"), dict):
            parent_verification_count += 1
        approval_route = control.get("task_subagent_approval_route")
        if isinstance(approval_route, dict) and approval_route.get("status") == "resolved":
            call_id = approval_route.get("call_id")
            if call_id == APPROVAL_TOOL_CALL_ID:
                approval_route_resolved_count += 1
            elif call_id == TERMINAL_START_TOOL_CALL_ID:
                terminal_approval_route_resolved_count += 1
            else:
                continue
            child_ref = approval_route.get("child_session_ref")
            relative = child_ref.get("path") if isinstance(child_ref, dict) else None
            if isinstance(relative, str):
                approval_child_refs.add(relative)
    for relative in approval_child_refs:
        child_path = path.parent / relative
        for raw_line in child_path.read_text(encoding="utf-8").splitlines():
            record = json.loads(raw_line)
            payload = record.get("payload")
            entry = payload.get("session_log_entry") if isinstance(payload, dict) else None
            control = entry.get("control") if isinstance(entry, dict) else None
            approval = control.get("tool_approval") if isinstance(control, dict) else None
            if (
                isinstance(approval, dict)
                and approval.get("action") == "resolved"
                and approval.get("user_decision") == "approved"
            ):
                call_id = approval.get("call_id")
                if call_id == APPROVAL_TOOL_CALL_ID:
                    approved_tool_call_count += 1
                elif call_id == TERMINAL_START_TOOL_CALL_ID:
                    approved_terminal_start_count += 1
            execution = control.get("tool_execution") if isinstance(control, dict) else None
            if isinstance(execution, dict) and execution.get("status") == "completed":
                call_id = execution.get("call_id")
                tool_name = execution.get("tool_name")
                if (
                    call_id == TERMINAL_START_TOOL_CALL_ID
                    and tool_name == "terminal_start"
                ):
                    terminal_start_completed_count += 1
                elif (
                    call_id == TERMINAL_CANCEL_TOOL_CALL_ID
                    and tool_name == "terminal_cancel"
                ):
                    terminal_cancel_completed_count += 1
            terminal_task = control.get("terminal_task") if isinstance(control, dict) else None
            handle = terminal_task.get("handle") if isinstance(terminal_task, dict) else None
            if not (
                isinstance(handle, dict)
                and handle.get("task_id") == TERMINAL_TASK_ID
            ):
                continue
            status = terminal_task.get("status")
            status_state = status.get("state") if isinstance(status, dict) else None
            if isinstance(status_state, str):
                terminal_task_statuses.add(status_state)
            readiness = terminal_task.get("readiness")
            readiness_state = (
                readiness.get("state") if isinstance(readiness, dict) else None
            )
            if isinstance(readiness_state, str):
                terminal_readiness_states.add(readiness_state)
            output_total_bytes = terminal_task.get("output_total_bytes")
            if isinstance(output_total_bytes, int):
                terminal_max_output_bytes = max(
                    terminal_max_output_bytes,
                    output_total_bytes,
                )
    return SessionAudit(
        event_counts=counts,
        completed_steps=tuple(completed_steps),
        final_answer_count=final_answer_count,
        approval_final_answer_count=approval_final_answer_count,
        continue_final_answer_count=continue_final_answer_count,
        integration_final_answer_count=integration_final_answer_count,
        terminal_final_answer_count=terminal_final_answer_count,
        task_final_count=task_final_count,
        approval_route_resolved_count=approval_route_resolved_count,
        approved_tool_call_count=approved_tool_call_count,
        terminal_approval_route_resolved_count=(
            terminal_approval_route_resolved_count
        ),
        approved_terminal_start_count=approved_terminal_start_count,
        terminal_start_completed_count=terminal_start_completed_count,
        terminal_cancel_completed_count=terminal_cancel_completed_count,
        terminal_task_statuses=tuple(sorted(terminal_task_statuses)),
        terminal_readiness_states=tuple(sorted(terminal_readiness_states)),
        terminal_max_output_bytes=terminal_max_output_bytes,
        paused_task_run_count=paused_task_run_count,
        interrupted_step_count=interrupted_step_count,
        cancelled_task_run_count=cancelled_task_run_count,
        promotion_preview_count=promotion_preview_count,
        promotion_authority_count=promotion_authority_count,
        promoted_integration_count=promoted_integration_count,
        parent_verification_count=parent_verification_count,
        failed_run_count=failed_run_count,
        cancelled_run_count=cancelled_run_count,
    )


def wait_for_audit(
    session_dir: Path,
    runner: object,
    predicate: Callable[[SessionAudit], bool],
    timeout: float,
) -> tuple[Path, SessionAudit]:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        runner.read_available(0.01)
        files = SUPPORT.session_files(session_dir)
        if len(files) > 1:
            raise AcceptanceError("orchestration run created more than one parent session")
        if files:
            try:
                audit = read_session_audit(files[0])
                if predicate(audit):
                    return files[0], audit
            except (OSError, json.JSONDecodeError) as error:
                last_error = error
        time.sleep(0.05)
    suffix = f": {last_error}" if last_error is not None else ""
    raise TimeoutError(f"timed out waiting for durable orchestration completion{suffix}")


def validate_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    expected_events = {
        "task_handoff_requested": 1,
        "task_handoff_resolved": 1,
    }
    for event_type, count in expected_events.items():
        if audit.event_counts.get(event_type) != count:
            raise AcceptanceError(
                f"expected {count} {event_type} event(s), got "
                f"{audit.event_counts.get(event_type, 0)}"
            )
    if audit.event_counts.get("orchestration_route_disabled", 0) != 0:
        raise AcceptanceError("healthy orchestration unexpectedly triggered the route kill switch")
    if tuple(sorted(audit.completed_steps)) != tuple(sorted(READ_STEP_IDS)):
        raise AcceptanceError("durable task did not complete both parallel read steps exactly once")
    if audit.final_answer_count != 1 or audit.task_final_count != 1:
        raise AcceptanceError("task must commit one parent-visible final answer")
    if audit.failed_run_count != 0:
        raise AcceptanceError("orchestration run finalized with a failure")

    expected_requests = {
        "conversation": 1,
        "planner": 1,
        f"read:{READ_STEP_IDS[0]}": 1,
        f"read:{READ_STEP_IDS[1]}": 1,
        "synthesis": 1,
        "title": 1,
    }
    if fixture.request_counts != expected_requests:
        raise AcceptanceError(
            f"unexpected provider request distribution {fixture.request_counts}"
        )
    if fixture.max_concurrent_reads != 2:
        raise AcceptanceError(
            "parallel read steps did not overlap at the provider boundary"
        )
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )


def validate_approval_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.event_counts.get("task_handoff_requested") != 2:
        raise AcceptanceError("approval phase did not append its model-owned handoff")
    if audit.event_counts.get("task_handoff_resolved") != 2:
        raise AcceptanceError("approval phase did not resolve its task handoff")
    if audit.completed_steps.count("write_note") != 1:
        raise AcceptanceError("approved write step did not complete exactly once")
    if (
        audit.approval_route_resolved_count != 1
        or audit.approved_tool_call_count != 1
    ):
        raise AcceptanceError(
            "write approval was not durably resolved once in both parent route and child audit"
        )
    if audit.final_answer_count != 1 or audit.approval_final_answer_count != 1:
        raise AcceptanceError("approval phase violated the one-final-per-task contract")
    if audit.task_final_count != 2 or audit.failed_run_count != 0:
        raise AcceptanceError("approval task did not complete through the shared root terminal")
    expected_requests = {
        "conversation": 2,
        "planner": 2,
        f"read:{READ_STEP_IDS[0]}": 1,
        f"read:{READ_STEP_IDS[1]}": 1,
        "write:request": 1,
        "write:after_tool": 1,
        "synthesis": 2,
        "title": 1,
    }
    if fixture.request_counts != expected_requests:
        raise AcceptanceError(
            f"unexpected approval provider request distribution {fixture.request_counts}"
        )
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )


def validate_continue_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.paused_task_run_count != 1 or audit.interrupted_step_count != 1:
        raise AcceptanceError(
            "crashed task did not persist one paused task and one interrupted step"
        )
    if audit.completed_steps.count("continue_after_crash") != 1:
        raise AcceptanceError("continued task did not complete its interrupted step")
    if audit.continue_final_answer_count != 1 or audit.task_final_count != 3:
        raise AcceptanceError("continued task did not commit one unique parent final")
    if fixture.request_counts.get("continue:step") != 2:
        raise AcceptanceError("continued task did not dispatch once before and once after crash")
    if fixture.request_counts.get("synthesis") != 3:
        raise AcceptanceError("continued task synthesis did not run exactly once")


def validate_cancel_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.cancelled_task_run_count != 1:
        raise AcceptanceError("cancelled task did not persist exactly one cancelled terminal")
    if audit.cancelled_run_count != 1:
        raise AcceptanceError("cancelled task did not finalize exactly one cancelled run")
    if audit.failed_run_count != 0:
        raise AcceptanceError("cancelled task finalized with a hard failure")
    if audit.completed_steps.count("cancel_in_flight") != 0:
        raise AcceptanceError("cancelled in-flight step was incorrectly committed")
    if audit.task_final_count != 3:
        raise AcceptanceError("cancelled task incorrectly committed a parent final")
    if fixture.request_counts.get("cancel:step") != 1:
        raise AcceptanceError("cancelled task provider dispatch count is not exactly one")
    if fixture.request_counts.get("synthesis") != 3:
        raise AcceptanceError("cancelled task incorrectly reached synthesis")


def validate_integration_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.promotion_preview_count != 1:
        raise AcceptanceError("integration task did not produce one exact promotion preview")
    if audit.promotion_authority_count != 1 or audit.promoted_integration_count != 1:
        raise AcceptanceError("reviewed integration was not promoted by one exact authority")
    if audit.parent_verification_count != 1:
        raise AcceptanceError("reviewed integration has no authoritative parent verification")
    if audit.integration_final_answer_count != 1 or audit.task_final_count != 4:
        raise AcceptanceError("reviewed integration did not commit one unique parent final")
    if fixture.request_counts.get("integration:step:a") != 2:
        raise AcceptanceError("integration step A did not run one tool turn and one final turn")
    if fixture.request_counts.get("integration:step:b") != 2:
        raise AcceptanceError("integration step B did not run one tool turn and one final turn")
    if fixture.request_counts.get("synthesis") != 4:
        raise AcceptanceError("reviewed integration synthesis did not run exactly once")


def validate_terminal_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.event_counts.get("task_handoff_requested") != 6:
        raise AcceptanceError("terminal phase did not append its model-owned handoff")
    if audit.event_counts.get("task_handoff_resolved") != 6:
        raise AcceptanceError("terminal phase did not resolve its task handoff")
    if audit.completed_steps.count("terminal_lifecycle") != 1:
        raise AcceptanceError("terminal lifecycle step did not complete exactly once")
    if (
        audit.terminal_approval_route_resolved_count != 1
        or audit.approved_terminal_start_count != 1
    ):
        raise AcceptanceError(
            "terminal_start approval was not durably resolved once in parent and child"
        )
    if (
        audit.terminal_start_completed_count != 1
        or audit.terminal_cancel_completed_count != 1
    ):
        raise AcceptanceError(
            "terminal_start and terminal_cancel must each complete exactly once"
        )
    if not {"running", "cancelled"}.issubset(audit.terminal_task_statuses):
        raise AcceptanceError(
            "terminal lifecycle did not durably progress from running to cancelled"
        )
    if "ready" not in audit.terminal_readiness_states:
        raise AcceptanceError("terminal readiness was not durably satisfied")
    expected_output_bytes = len(TERMINAL_READY_CANARY) + len(TERMINAL_PROGRESS_CANARY) + 2
    if audit.terminal_max_output_bytes < expected_output_bytes:
        raise AcceptanceError("terminal lifecycle did not durably report output progress")
    if audit.terminal_final_answer_count != 1 or audit.task_final_count != 5:
        raise AcceptanceError("terminal task did not commit one unique parent final")
    if audit.failed_run_count != 0:
        raise AcceptanceError("terminal task finalized with a failure")
    if audit.cancelled_run_count != 1:
        raise AcceptanceError("terminal phase lost the prior user-cancelled run terminal")
    expected_requests = {
        "conversation": 6,
        "planner": 6,
        f"read:{READ_STEP_IDS[0]}": 1,
        f"read:{READ_STEP_IDS[1]}": 1,
        "write:request": 1,
        "write:after_tool": 1,
        "continue:step": 2,
        "cancel:step": 1,
        "integration:step:a": 2,
        "integration:step:b": 2,
        "terminal:start": 1,
        "terminal:after_start": 1,
        "terminal:after_cancel": 1,
        "synthesis": 5,
        "title": 1,
    }
    if fixture.request_counts != expected_requests:
        raise AcceptanceError(
            f"unexpected terminal provider request distribution {fixture.request_counts}"
        )
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )


def wait_for_fixture_request(
    fixture: FixtureState,
    kind: str,
    count: int,
    runner: object,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        runner.read_available(0.01)
        with fixture.lock:
            observed = fixture.request_counts.get(kind, 0)
            errors = tuple(fixture.protocol_errors)
        if errors:
            raise AcceptanceError(f"fixture protocol errors: {errors}")
        if observed >= count:
            return
        time.sleep(0.02)
    raise TimeoutError(f"timed out waiting for fixture request {kind} #{count}")


def submit_user_prompt(runner: object, prompt: str) -> None:
    # Completed tool activity retains focus so ordinary characters do not
    # accidentally edit the composer. Esc returns an idle TUI to the composer.
    runner.send(b"\x1b")
    runner.type_text(prompt)
    runner.send("\r")


def click_screen_text(runner: object, screen: str, needle: str) -> None:
    for row, line in enumerate(screen.splitlines(), start=1):
        column = line.find(needle)
        if column < 0:
            continue
        column += 1
        runner.send(f"\x1b[<0;{column};{row}M")
        runner.send(f"\x1b[<0;{column};{row}m")
        return
    raise AcceptanceError(f"cannot click missing screen text {needle!r}")


def activate_screen_text_until(
    runner: object,
    needle: str,
    predicate: Callable[[str], bool],
    timeout: float,
    description: str,
) -> str:
    """Activate a moving TUI card through its real mouse/key path until its response is visible."""
    deadline = time.monotonic() + timeout
    next_activation = 0.0
    while time.monotonic() < deadline:
        runner.read_available(0.05)
        screen = runner.screen()
        if predicate(screen):
            return screen
        now = time.monotonic()
        if needle in screen and now >= next_activation:
            click_screen_text(runner, screen, needle)
            runner.read_available(0.02)
            runner.send("\r")
            # The card may become visible one redraw before the root run releases its busy
            # ownership. Retry from fresh coordinates instead of depending on that race.
            next_activation = now + 0.5
        time.sleep(0.02)
    raise TimeoutError(f"timed out waiting for {description}")


def run_git(workspace: Path, env: dict[str, str], *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=workspace,
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=15,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise AcceptanceError(f"git {' '.join(args)} failed: {detail}")
    return result.stdout


def initialize_git_workspace(workspace: Path, env: dict[str, str]) -> None:
    for path, content in zip(INTEGRATION_PATHS, ("old a\n", "old b\n"), strict=True):
        (workspace / path).write_text(content, encoding="utf-8")
    run_git(workspace, env, "init", "-q")
    run_git(workspace, env, "config", "user.name", "Sigil PTY Fixture")
    run_git(workspace, env, "config", "user.email", "sigil-pty@example.invalid")
    run_git(workspace, env, "add", "--all")
    run_git(workspace, env, "commit", "-qm", "initialize PTY fixture")


def checkpoint_workspace(workspace: Path, env: dict[str, str], message: str) -> None:
    run_git(workspace, env, "add", "--all")
    run_git(workspace, env, "commit", "-qm", message)
    if run_git(workspace, env, "status", "--porcelain").strip():
        raise AcceptanceError("integration fixture workspace is not clean after checkpoint")


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def main() -> int:
    args = parse_args()
    root = SUPPORT.repo_root()
    output_dir = (
        args.output_dir
        if args.output_dir.is_absolute()
        else root / args.output_dir
    ).expanduser().resolve()
    fixture_root: Path | None = None
    runner: object | None = None
    server: FixtureServer | None = None
    server_thread: threading.Thread | None = None
    started_at = utc_now()
    started = time.monotonic()
    try:
        deadline = SUPPORT.CampaignDeadline(args.timeout)
        SUPPORT.raw_artifact_policy(root, output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)
        binary_source, identity = SUPPORT.inspect_binary(
            args.binary,
            timeout=deadline.remaining(15.0),
        )
        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-orchestration-pty-"))
        frozen_binary = SUPPORT.freeze_binary(binary_source, fixture_root, identity)
        workspace = fixture_root / "workspace"
        state_root = fixture_root / "state"
        cache_root = fixture_root / "cache"
        session_dir = fixture_root / "sessions"
        for directory in (workspace, state_root, cache_root, session_dir):
            directory.mkdir()
        (workspace / "README.md").write_text("orchestration fixture\n", encoding="utf-8")
        SUPPORT.generate_fixture_tls_identity(fixture_root)
        env = SUPPORT.isolated_environment(fixture_root)
        initialize_git_workspace(workspace, env)
        fixture = FixtureState()
        server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
        server.fixture = fixture
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        config_path = fixture_root / "sigil.toml"
        write_config(
            config_path,
            workspace=workspace,
            state_root=state_root,
            cache_root=cache_root,
            session_dir=session_dir,
            port=int(server.server_address[1]),
        )
        runner = SUPPORT.PtyRunner(
            [str(frozen_binary), "--config", str(config_path)],
            workspace,
            env,
            output_dir / "tui-process.log",
        )
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        submit_user_prompt(runner, USER_PROMPT)
        session_path, audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.final_answer_count == 1
            and value.task_final_count == 1,
            deadline.remaining(),
        )
        settled_screen = runner.wait_until(
            lambda text: FINAL_CANARY in text
            and "Thinking..." not in text
            and "Replying..." not in text,
            deadline.remaining(),
            "settled orchestration final answer",
            final_screen=True,
        )
        if settled_screen.count(FINAL_CANARY) != 1:
            raise AcceptanceError("TUI rendered the orchestration final answer more than once")
        raw_text = runner.raw_text()
        if READ_STEP_TITLES[0] not in raw_text or "0/2" not in raw_text:
            raise AcceptanceError("TUI never rendered the parallel task batch progress")
        validate_audit(audit, fixture)

        submit_user_prompt(runner, APPROVAL_USER_PROMPT)
        runner.wait_until(
            lambda text: "Review Tool Call" in text
            and "write_file" in text
            and APPROVAL_PATH in text,
            deadline.remaining(),
            "task participant write approval",
            final_screen=True,
        )
        runner.send("y")
        session_path, approval_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.approval_final_answer_count == 1
            and value.task_final_count == 2,
            deadline.remaining(),
        )
        approval_screen = runner.wait_until(
            lambda text: APPROVAL_FINAL_CANARY in text
            and "Thinking..." not in text
            and "Replying..." not in text,
            deadline.remaining(),
            "settled approved task final answer",
            final_screen=True,
        )
        if approval_screen.count(APPROVAL_FINAL_CANARY) != 1:
            raise AcceptanceError("TUI rendered the approved task final more than once")
        if (workspace / APPROVAL_PATH).read_text(encoding="utf-8") != APPROVAL_CONTENT:
            raise AcceptanceError("approved task write did not reach the workspace")
        validate_approval_audit(approval_audit, fixture)
        checkpoint_workspace(workspace, env, "record approved write fixture")

        submit_user_prompt(runner, CONTINUE_USER_PROMPT)
        wait_for_fixture_request(
            fixture,
            "continue:step",
            1,
            runner,
            deadline.remaining(),
        )
        runner.stop()
        runner = None
        fixture.crash_release.set()

        runner = SUPPORT.PtyRunner(
            [
                str(frozen_binary),
                "--config",
                str(config_path),
                "resume",
                str(session_path),
            ],
            workspace,
            env,
            output_dir / "resume-process.log",
        )
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        session_path, interrupted_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.paused_task_run_count == 1
            and value.interrupted_step_count == 1,
            deadline.remaining(),
        )
        if interrupted_audit.task_final_count != 2:
            raise AcceptanceError("crash-interrupted task committed a final before continue")
        runner.type_text("/task continue")
        runner.send("\r")
        session_path, continue_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.continue_final_answer_count == 1
            and value.task_final_count == 3,
            deadline.remaining(),
        )
        continued_screen = runner.wait_until(
            lambda text: CONTINUE_FINAL_CANARY in text
            and "Thinking..." not in text
            and "Replying..." not in text,
            deadline.remaining(),
            "settled continued task final answer",
            final_screen=True,
        )
        if continued_screen.count(CONTINUE_FINAL_CANARY) != 1:
            raise AcceptanceError("TUI rendered the continued task final more than once")
        validate_continue_audit(continue_audit, fixture)

        submit_user_prompt(runner, CANCEL_USER_PROMPT)
        wait_for_fixture_request(
            fixture,
            "cancel:step",
            1,
            runner,
            deadline.remaining(),
        )
        runner.send(b"\x1b")
        session_path, cancel_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.cancelled_task_run_count == 1,
            deadline.remaining(),
        )
        fixture.cancel_release.set()
        if not fixture.cancel_settled.wait(timeout=deadline.remaining(5.0)):
            raise AcceptanceError("cancelled provider request did not settle")
        validate_cancel_audit(cancel_audit, fixture)
        if fixture.protocol_errors:
            raise AcceptanceError(
                f"fixture observed provider protocol errors: {fixture.protocol_errors}"
            )
        runner.wait_until(
            lambda text: "cancelled" in text.lower(),
            deadline.remaining(),
            "visible cancelled task state",
            final_screen=True,
        )

        submit_user_prompt(runner, INTEGRATION_USER_PROMPT)
        session_path, review_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.promotion_preview_count == 1,
            deadline.remaining(),
        )
        if (
            review_audit.integration_final_answer_count != 0
            or review_audit.task_final_count != 3
            or review_audit.promotion_authority_count != 0
        ):
            raise AcceptanceError(
                "integration task advanced past the exact user review boundary"
            )
        runner.wait_until(
            lambda text: "Integration review" in text
            and "ready · exact target bound" in text
            and "review diff" in text,
            deadline.remaining(),
            "visible integration review card",
            final_screen=True,
        )
        activate_screen_text_until(
            runner,
            "Integration review",
            lambda text: "reviewed · exact diff loaded" in text
            and "Enter accept integration" in text,
            deadline.remaining(),
            "loaded exact integration diff",
        )
        runner.send("\r")
        session_path, integration_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.integration_final_answer_count == 1
            and value.task_final_count == 4,
            deadline.remaining(),
        )
        integration_screen = runner.wait_until(
            lambda text: INTEGRATION_FINAL_CANARY in text
            and "Thinking..." not in text
            and "Replying..." not in text,
            deadline.remaining(),
            "settled integration task final answer",
            final_screen=True,
        )
        if integration_screen.count(INTEGRATION_FINAL_CANARY) != 1:
            raise AcceptanceError("TUI rendered the integration task final more than once")
        validate_integration_audit(integration_audit, fixture)
        for path, expected in zip(
            INTEGRATION_PATHS,
            ("integrated a\n", "integrated b\n"),
            strict=True,
        ):
            if (workspace / path).read_text(encoding="utf-8") != expected:
                raise AcceptanceError(f"reviewed integration did not promote {path}")

        submit_user_prompt(runner, TERMINAL_USER_PROMPT)
        runner.wait_until(
            lambda text: "Review Tool Call" in text
            and "terminal_start" in text
            and TERMINAL_READY_CANARY in text,
            deadline.remaining(),
            "structured terminal_start approval",
            final_screen=True,
        )
        runner.send("y")
        session_path, terminal_audit = wait_for_audit(
            session_dir,
            runner,
            lambda value: value.terminal_final_answer_count == 1
            and value.task_final_count == 5
            and "cancelled" in value.terminal_task_statuses,
            deadline.remaining(),
        )
        terminal_screen = runner.wait_until(
            lambda text: TERMINAL_FINAL_CANARY in text
            and TERMINAL_TASK_ID in text
            and "status: cancelled" in text
            and "Thinking..." not in text
            and "Replying..." not in text,
            deadline.remaining(),
            "settled terminal lifecycle final answer and terminal state",
            final_screen=True,
        )
        if terminal_screen.count(TERMINAL_FINAL_CANARY) != 1:
            raise AcceptanceError("TUI rendered the terminal task final more than once")
        validate_terminal_audit(terminal_audit, fixture)

        runner.quit(timeout=deadline.remaining(10.0))
        runner.stop()
        runner = None

        evidence_dir = output_dir / "sessions"
        evidence_dir.mkdir(mode=0o700, exist_ok=True)
        evidence_path = evidence_dir / "parent.jsonl"
        shutil.copyfile(session_path, evidence_path)
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "campaign": "sigil-orchestration-tui-v1",
            "status": "passed",
            "started_at": started_at,
            "finished_at": utc_now(),
            "duration_ms": int((time.monotonic() - started) * 1000),
            "binary": identity.as_dict(),
            "checks": {
                "model_owned_auto_handoff": True,
                "parallel_read_provider_overlap": fixture.max_concurrent_reads,
                "completed_step_count": len(audit.completed_steps),
                "unique_parent_final_count": audit.final_answer_count,
                "approved_write_count": approval_audit.approved_tool_call_count,
                "continued_task_interruption_count": (
                    continue_audit.interrupted_step_count
                ),
                "cancelled_task_count": cancel_audit.cancelled_task_run_count,
                "reviewed_integration_promotion_count": (
                    integration_audit.promoted_integration_count
                ),
                "approved_terminal_start_count": (
                    terminal_audit.approved_terminal_start_count
                ),
                "terminal_start_completed_count": (
                    terminal_audit.terminal_start_completed_count
                ),
                "terminal_cancel_completed_count": (
                    terminal_audit.terminal_cancel_completed_count
                ),
                "terminal_readiness_ready": (
                    "ready" in terminal_audit.terminal_readiness_states
                ),
                "terminal_output_progress_bytes": (
                    terminal_audit.terminal_max_output_bytes
                ),
                "terminal_terminal_state": "cancelled",
                "completed_task_count": terminal_audit.task_final_count,
                "route_kill_switch_count": audit.event_counts.get(
                    "orchestration_route_disabled",
                    0,
                ),
            },
            "evidence": {
                "pty_log": "tui-process.log",
                "resume_pty_log": "resume-process.log",
                "session": "sessions/parent.jsonl",
            },
            "privacy": {
                "raw_artifacts_local_only": True,
                "automatic_upload": False,
            },
        }
        manifest_path = output_dir / "manifest.json"
        manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        (output_dir / "manifest.sha256").write_text(
            f"{SUPPORT.sha256_file(manifest_path)}  manifest.json\n",
            encoding="utf-8",
        )
        print(f"orchestration PTY acceptance passed: {manifest_path}")
        return 0
    except (
        AcceptanceError,
        OSError,
        TimeoutError,
        json.JSONDecodeError,
    ) as error:
        print(f"orchestration PTY acceptance failed: {error}", file=sys.stderr)
        if runner is not None:
            try:
                runner.read_available(0.0)
                (output_dir / "failure-screen.txt").write_text(
                    runner.screen() + "\n",
                    encoding="utf-8",
                )
            except OSError:
                pass
        if args.keep_fixture and fixture_root is not None:
            print(f"retained fixture: {fixture_root}", file=sys.stderr)
        return 1
    finally:
        if runner is not None:
            runner.stop()
        if server is not None:
            server.shutdown()
            server.server_close()
        if server_thread is not None:
            server_thread.join(timeout=5)
        if fixture_root is not None and not args.keep_fixture:
            shutil.rmtree(fixture_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
