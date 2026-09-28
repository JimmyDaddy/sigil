#!/usr/bin/env python3
"""Exercise review-first Direct Task execution through a real TUI PTY."""

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
import re
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
ANSI_CSI_PATTERN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
MODEL_NAME = "direct-task-fixture-model"
FINAL_CANARY = "DIRECT-TASK-PTY-FINAL-CANARY-7319"
APPROVAL_FINAL_CANARY = "DIRECT-TASK-PTY-APPROVAL-FINAL-8427"
USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-2486"
APPROVAL_USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-3597"
APPROVAL_PATH = "approval-note.txt"
APPROVAL_CONTENT = "approved task write\n"
APPROVAL_TOOL_CALL_ID = "approval-write-call"
CONTINUE_FINAL_CANARY = "DIRECT-TASK-PTY-CONTINUE-FINAL-9531"
CONTINUE_USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-4608"
CANCEL_USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-5719"
INTEGRATION_FINAL_CANARY = "DIRECT-TASK-PTY-INTEGRATION-FINAL-6842"
INTEGRATION_USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-6820"
INTEGRATION_PATHS = ("integration-a.txt", "integration-b.txt")
INTEGRATION_TOOL_CALL_IDS = ("integration-write-a", "integration-write-b")
TERMINAL_FINAL_CANARY = "DIRECT-TASK-PTY-TERMINAL-FINAL-7953"
TERMINAL_USER_PROMPT = "DIRECT-TASK-PTY-OPAQUE-REQUEST-7931"
TERMINAL_START_TOOL_CALL_ID = "terminal-start-call"
TERMINAL_OUTPUT_PATH = "terminal-approval-note.txt"
TERMINAL_READY_CANARY = "DIRECT-TASK-PTY-TERMINAL-READY-8174"
TERMINAL_PROGRESS_CANARY = "DIRECT-TASK-PTY-TERMINAL-PROGRESS-9285"
PLAN_REVIEW_ARGS = json.dumps(
    {"reason_codes": ["architectural_tradeoff"]},
    separators=(",", ":"),
)
APPROVAL_WRITE_ARGS = json.dumps(
    {"path": APPROVAL_PATH, "content": APPROVAL_CONTENT},
    separators=(",", ":"),
)
TERMINAL_START_ARGS = json.dumps(
    {
        "command": (
            f"printf '{TERMINAL_READY_CANARY}\\n' > {TERMINAL_OUTPUT_PATH}; "
            f"cat {TERMINAL_OUTPUT_PATH}; "
            f"printf '{TERMINAL_PROGRESS_CANARY}\\n'"
        ),
        "yield_time_ms": 1000,
    },
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
    """Raised when the direct-task PTY contract is violated."""


@dataclasses.dataclass(frozen=True)
class SessionAudit:
    event_counts: dict[str, int]
    final_answer_count: int
    approval_final_answer_count: int
    continue_final_answer_count: int
    integration_final_answer_count: int
    terminal_final_answer_count: int
    task_final_count: int
    approved_tool_call_count: int
    approved_terminal_start_count: int
    terminal_start_completed_count: int
    terminal_task_statuses: tuple[str, ...]
    terminal_readiness_states: tuple[str, ...]
    terminal_max_output_bytes: int
    paused_task_run_count: int
    interrupted_task_run_count: int
    promotion_preview_count: int
    promotion_authority_count: int
    promoted_integration_count: int
    parent_verification_count: int
    failed_run_count: int
    cancelled_run_count: int


@dataclasses.dataclass
class FixtureState:
    diagnostics_path: Path | None = None
    request_payload_path: Path | None = None
    request_counts: dict[str, int] = dataclasses.field(default_factory=dict)
    request_order: list[str] = dataclasses.field(default_factory=list)
    protocol_errors: list[str] = dataclasses.field(default_factory=list)
    expected_disconnects: int = 0
    crash_release: threading.Event = dataclasses.field(default_factory=threading.Event)
    cancel_release: threading.Event = dataclasses.field(default_factory=threading.Event)
    cancel_settled: threading.Event = dataclasses.field(default_factory=threading.Event)
    lock: threading.Lock = dataclasses.field(default_factory=threading.Lock)

    def record_request_payload(self, payload: object) -> None:
        # Only isolated fixture request bodies are retained, never HTTP headers/credentials.
        if self.request_payload_path is not None:
            with self.lock, self.request_payload_path.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(payload, ensure_ascii=False) + "\n")

    def start_request(self, kind: str) -> int:
        with self.lock:
            self.request_counts[kind] = self.request_counts.get(kind, 0) + 1
            request_number = self.request_counts[kind]
            self.request_order.append(kind)
            return request_number

    def record_error(self, error: Exception) -> None:
        with self.lock:
            self.protocol_errors.append(f"{type(error).__name__}: {error}")
            if self.diagnostics_path is not None:
                self.diagnostics_path.write_text(
                    json.dumps({"protocol_errors": self.protocol_errors,
                                "request_counts": self.request_counts,
                                "request_order": self.request_order}, indent=2) + "\n",
                    encoding="utf-8",
                )

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
            self.fixture.record_request_payload(payload)
            if not self.path.endswith("/chat/completions"):
                raise AcceptanceError(f"unexpected fixture path {self.path}")
            kind = classify_request(payload)
            request_number = self.fixture.start_request(kind)
            if kind == "routing:plan_review":
                self._send_tool_call(
                    f"plan-review-call-{request_number}",
                    "request_plan_review",
                    PLAN_REVIEW_ARGS,
                )
            elif kind == "plan_review_research":
                self._send_tool_call(
                    f"plan-review-result-{request_number}",
                    "submit_plan_review_result",
                    json.dumps({
                        "schema_version": 1,
                        "outcome": "draft",
                        "content": f"Review scenario {request_number} before execution",
                    }),
                )
            elif kind == "direct:read":
                self._send_text(FINAL_CANARY)
            elif kind == "direct:write:request":
                self._send_tool_call(
                    APPROVAL_TOOL_CALL_ID,
                    "write_file",
                    APPROVAL_WRITE_ARGS,
                )
            elif kind == "direct:write:after_tool":
                self._send_text(APPROVAL_FINAL_CANARY)
            elif kind == "direct:continue":
                if request_number == 1:
                    if not self.fixture.crash_release.wait(timeout=30):
                        raise TimeoutError("direct crash fixture was not released")
                    self._send_text("obsolete pre-crash response")
                else:
                    self._send_text(CONTINUE_FINAL_CANARY)
            elif kind == "direct:cancel":
                if not self.fixture.cancel_release.wait(timeout=30):
                    raise TimeoutError("direct cancel fixture was not released")
                self._send_text("obsolete cancelled response")
            elif kind == "direct:integration:a":
                self._send_tool_call(
                    INTEGRATION_TOOL_CALL_IDS[0],
                    "write_file",
                    INTEGRATION_WRITE_ARGS[0],
                )
            elif kind == "direct:integration:b":
                self._send_tool_call(
                    INTEGRATION_TOOL_CALL_IDS[1],
                    "write_file",
                    INTEGRATION_WRITE_ARGS[1],
                )
            elif kind == "direct:integration:final":
                self._send_text(INTEGRATION_FINAL_CANARY)
            elif kind == "direct:terminal:start":
                self._send_tool_call(
                    TERMINAL_START_TOOL_CALL_ID,
                    "exec_command",
                    TERMINAL_START_ARGS,
                )
            elif kind == "direct:terminal:after_start":
                self._send_text(TERMINAL_FINAL_CANARY)
            elif kind == "title":
                self._send_text("Orchestration acceptance")
            else:
                raise AcceptanceError(f"unsupported request kind {kind}")
        except Exception as error:  # noqa: BLE001 - retain fixture diagnostics.
            expected_disconnect = (
                kind in {"direct:continue", "direct:cancel"}
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
            if kind == "direct:cancel":
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
        description="Run Sigil's model-owned direct-task real-PTY acceptance.",
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


def current_direct_scenario(payload: object) -> int | None:
    """Fixture routing follows the latest user objective, excluding assistant/tool history."""
    if not isinstance(payload, dict) or not isinstance(payload.get("messages"), list):
        return None
    for message in reversed(payload["messages"]):
        if not isinstance(message, dict) or message.get("role") != "user":
            continue
        text = request_text({"messages": [message]})
        matches = [(text.rfind(f"Review scenario {scenario} before execution"), scenario)
                   for scenario in range(1, 7)]
        position, scenario = max(matches)
        if position >= 0:
            return scenario
    return None


def classify_request(payload: object) -> str:
    names = tool_names(payload)
    text = request_text(payload)
    if (
        not names
        and "Generate a concise semantic title for a coding-agent conversation" in text
    ):
        return "title"
    if "request_plan_review" in names:
        return "routing:plan_review"
    if "submit_plan_review_result" in names:
        return "plan_review_research"
    direct_scenario = current_direct_scenario(payload)
    if direct_scenario == 1:
        return "direct:read"
    if direct_scenario == 2:
        return (
            "direct:write:after_tool"
            if has_tool_result(payload, APPROVAL_TOOL_CALL_ID)
            else "direct:write:request"
        )
    if direct_scenario == 3:
        return "direct:continue"
    if direct_scenario == 4:
        return "direct:cancel"
    if direct_scenario == 5:
        if has_tool_result(payload, INTEGRATION_TOOL_CALL_IDS[1]):
            return "direct:integration:final"
        if has_tool_result(payload, INTEGRATION_TOOL_CALL_IDS[0]):
            return "direct:integration:b"
        return "direct:integration:a"
    if direct_scenario == 6:
        if has_tool_result(payload, TERMINAL_START_TOOL_CALL_ID):
            return "direct:terminal:after_start"
        return "direct:terminal:start"
    raise AcceptanceError("provider request does not match the Direct Task fixture contract")


def managed_parent_session_files(root: Path) -> list[Path]:
    """Find parent sessions in the managed store, excluding plan-review children."""
    candidates = sorted(
        root.glob("*/records.jsonl"),
        key=lambda path: path.stat().st_mtime_ns,
    )
    # A resume boot may create a short-lived session identity stream before it attaches to
    # the requested durable parent.  It contains only trust/route metadata, not a user run;
    # treating it as a second parent makes the acceptance harness report a false fork.
    parent_event_types = {
        "user_message_recorded",
        "assistant_message_recorded",
        "plan_draft_created",
        "plan_decision_recorded",
        "task_created_from_plan",
        "run_status_changed",
        "run_finalized",
        "tool_execution_started",
        "tool_execution_finished",
        "tool_result_recorded_v3",
    }
    managed = []
    for path in candidates:
        try:
            records = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
                       if line.strip()]
            # RA names storage leaves by hash. These lifecycle records are emitted
            # by the application parent, never by plan-review or task child agents.
            event_types = {record.get("event_type") for record in records}
            has_parent_activity = bool(event_types & {
                "plan_review_attempt", "task_created_from_plan", "plan_decision_recorded",
                "conversation_route_decision_recorded", "public_event_outbox",
            }) or (path.parent.name.startswith("session-")
                   and bool(event_types & parent_event_types))
        except (OSError, json.JSONDecodeError):
            continue
        if has_parent_activity:
            managed.append(path)
    if managed:
        return managed
    return SUPPORT.session_files(root)


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
connection = "direct-task-fixture"
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

[[permission.rules]]
tool_name = "write_file"
subject_glob = "integration-*.txt"
mode = "allow"

[terminal]
keyboard_enhancement = "off"
mouse_capture = true
osc52_clipboard = false

[connections.direct-task-fixture]
label = "Direct Task fixture"
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
    final_answer_count = 0
    approval_final_answer_count = 0
    continue_final_answer_count = 0
    integration_final_answer_count = 0
    terminal_final_answer_count = 0
    task_final_count = 0
    approved_tool_call_count = 0
    approved_terminal_start_count = 0
    terminal_start_completed_count = 0
    terminal_task_statuses: set[str] = set()
    terminal_readiness_states: set[str] = set()
    terminal_max_output_bytes = 0
    paused_task_run_count = 0
    interrupted_task_run_count = 0
    promotion_preview_count = 0
    promotion_authority_count = 0
    promoted_integration_count = 0
    parent_verification_count = 0
    failed_run_count = 0
    cancelled_run_count = 0

    def observe_tool_control(control: dict[str, object]) -> None:
        nonlocal approved_tool_call_count
        nonlocal approved_terminal_start_count
        nonlocal terminal_start_completed_count
        approval = control.get("tool_approval")
        if isinstance(approval, dict):
            if (
                approval.get("action") == "resolved"
                and approval.get("user_decision") == "approved"
            ):
                call_id = approval.get("call_id")
                if call_id == APPROVAL_TOOL_CALL_ID:
                    approved_tool_call_count += 1
                elif call_id == TERMINAL_START_TOOL_CALL_ID:
                    approved_terminal_start_count += 1
        execution = control.get("tool_execution")
        if isinstance(execution, dict) and execution.get("status") == "completed":
            call_id = execution.get("call_id")
            tool_name = execution.get("tool_name")
            if call_id == TERMINAL_START_TOOL_CALL_ID and tool_name == "exec_command":
                terminal_start_completed_count += 1

    def observe_terminal_control(control: dict[str, object]) -> None:
        nonlocal terminal_max_output_bytes
        terminal_task = control.get("terminal_task")
        if not isinstance(terminal_task, dict):
            return
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
            if payload.get("record") == "conversation_run_finalized_v1":
                if payload.get("status") not in {"succeeded", "cancelled", "paused", "interrupted", "blocked"}:
                    failed_run_count += 1
            elif outcome == "cancelled" and payload.get("cleanup_complete") is True:
                cancelled_run_count += 1
            elif payload.get("run_status") not in {
                "completed",
                "cancelled",
                "paused",
                "interrupted",
            } and outcome not in {"completed", "success"}:
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
        observe_tool_control(control)
        observe_terminal_control(control)
        direct_attempt = control.get("task_direct_execution_attempt_v1")
        if (
            isinstance(direct_attempt, dict)
            and direct_attempt.get("status") == "completed"
            and isinstance(direct_attempt.get("final_message_id"), str)
        ):
            task_final_count += 1
        task_run = control.get("task_run")
        if isinstance(task_run, dict) and task_run.get("status") == "paused":
            paused_task_run_count += 1
        if isinstance(task_run, dict) and task_run.get("status") == "interrupted":
            interrupted_task_run_count += 1
        if isinstance(control.get("task_promotion_preview_recorded"), dict):
            promotion_preview_count += 1
        if isinstance(control.get("task_promotion_authority_consumed"), dict):
            promotion_authority_count += 1
        promotion = control.get("integration_promotion_recorded")
        if isinstance(promotion, dict) and promotion.get("status") == "promoted":
            promoted_integration_count += 1
        if isinstance(control.get("task_parent_verification_recorded"), dict):
            parent_verification_count += 1
    return SessionAudit(
        event_counts=counts,
        final_answer_count=final_answer_count,
        approval_final_answer_count=approval_final_answer_count,
        continue_final_answer_count=continue_final_answer_count,
        integration_final_answer_count=integration_final_answer_count,
        terminal_final_answer_count=terminal_final_answer_count,
        task_final_count=task_final_count,
        approved_tool_call_count=approved_tool_call_count,
        approved_terminal_start_count=approved_terminal_start_count,
        terminal_start_completed_count=terminal_start_completed_count,
        terminal_task_statuses=tuple(sorted(terminal_task_statuses)),
        terminal_readiness_states=tuple(sorted(terminal_readiness_states)),
        terminal_max_output_bytes=terminal_max_output_bytes,
        paused_task_run_count=paused_task_run_count,
        interrupted_task_run_count=interrupted_task_run_count,
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
    last_audit: SessionAudit | None = None
    while time.monotonic() < deadline:
        runner.read_available(0.01)
        files = managed_parent_session_files(session_dir)
        if len(files) > 1:
            raise AcceptanceError("direct-task run created more than one parent session")
        if files:
            try:
                audit = read_session_audit(files[0])
                last_audit = audit
                if predicate(audit):
                    return files[0], audit
            except (OSError, json.JSONDecodeError) as error:
                last_error = error
        time.sleep(0.05)
    try:
        runner.read_available(0.0)
        runner.raw_log.write_bytes(bytes(runner.output))
    except OSError:
        pass
    suffix = f": {last_error}" if last_error is not None else ""
    if last_audit is not None:
        suffix += (
            f"; observed finals={last_audit.task_final_count}, "
            f"interrupted_tasks={last_audit.interrupted_task_run_count}, "
            f"cancelled_runs={last_audit.cancelled_run_count}, "
            f"failed_runs={last_audit.failed_run_count}"
        )
    raise TimeoutError(f"timed out waiting for durable Direct Task completion{suffix}")


def validate_terminal_audit(audit: SessionAudit, fixture: FixtureState) -> None:
    if audit.terminal_final_answer_count != 1:
        raise AcceptanceError("terminal lifecycle task did not complete exactly once")
    if audit.approved_terminal_start_count != 1:
        raise AcceptanceError("exec_command approval was not durably resolved exactly once")
    if audit.terminal_start_completed_count != 1:
        raise AcceptanceError("exec_command must complete exactly once")
    if "exited" not in audit.terminal_task_statuses:
        raise AcceptanceError("command exit was not durable")
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
        "routing:plan_review": 6,
        "plan_review_research": 6,
        "direct:read": 1,
        "direct:write:request": 1,
        "direct:write:after_tool": 1,
        "direct:continue": 2,
        "direct:cancel": 1,
        "direct:integration:a": 1,
        "direct:integration:b": 1,
        "direct:integration:final": 1,
        "direct:terminal:start": 1,
        "direct:terminal:after_start": 1,
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


def validate_direct_task_audit(
    audit: SessionAudit,
    fixture: FixtureState,
    *,
    plan_count: int,
    task_count: int,
    final_count: int,
) -> None:
    if audit.event_counts.get("plan_draft_created", 0) != plan_count:
        raise AcceptanceError(
            f"direct task phase expected {plan_count} durable plan drafts, "
            f"got {audit.event_counts.get('plan_draft_created', 0)}"
        )
    if audit.task_final_count != task_count:
        raise AcceptanceError(
            f"direct task phase expected {task_count} task finals, got {audit.task_final_count}"
        )
    observed_finals = (
        audit.final_answer_count
        + audit.approval_final_answer_count
        + audit.continue_final_answer_count
        + audit.integration_final_answer_count
        + audit.terminal_final_answer_count
    )
    if observed_finals != final_count:
        raise AcceptanceError(
            f"direct task phase expected {final_count} canary finals, got {observed_finals}"
        )
    if audit.failed_run_count != 0:
        raise AcceptanceError("direct task phase finalized with a hard failure")
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )


def direct_task_controls(path: Path, key: str) -> list[dict[str, object]]:
    """Read the real native run's append-only controls for identity assertions."""
    controls = []
    for line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        control = record.get("payload", {}).get("session_log_entry", {}).get("control", {})
        value = control.get(key)
        if isinstance(value, dict):
            controls.append(value)
    return controls


def bound_approval_state(path: Path, call_id: str, request_id: str) -> str:
    approvals = direct_task_controls(path, "tool_approval")
    matching = [value for value in approvals if value.get("call_id") == call_id]
    if not matching or matching[-1].get("identity", {}).get("approval_request_id") != request_id:
        raise AcceptanceError("approval identity changed before fixture decision settled")
    latest = matching[-1]
    if latest.get("action") == "resolved":
        if latest.get("user_decision") != "approved":
            raise AcceptanceError("fixture approval was not approved")
        return "resolved"
    requested = [value for value in approvals if value.get("action") == "requested"]
    if (latest.get("action") not in ("requested", "decision_accepted") or not requested
            or requested[-1].get("identity", {}).get("approval_request_id") != request_id):
        raise AcceptanceError("another approval replaced the fixture's active request")
    if latest.get("action") == "decision_accepted":
        if latest.get("user_decision") != "approved":
            raise AcceptanceError("fixture approval decision was not approved")
        return "accepted"
    return "requested"


def approve_bound_tool_once(
    runner: object, session_path: Path, call_id: str, tool_name: str,
    visible_subject: str, timeout: float,
) -> None:
    approvals = [value for value in direct_task_controls(session_path, "tool_approval")
                 if value.get("call_id") == call_id and value.get("action") == "requested"]
    if not approvals:
        raise AcceptanceError(f"missing durable approval request for {call_id}")
    request_id = approvals[-1].get("identity", {}).get("approval_request_id")
    if not isinstance(request_id, str):
        raise AcceptanceError("approval request omitted its durable identity")
    deadline = time.monotonic() + timeout
    next_attempt = 0.0
    attempts = 0
    while time.monotonic() < deadline:
        runner.read_available(0.05)
        state = bound_approval_state(session_path, call_id, request_id)
        if state == "resolved":
            return
        now = time.monotonic()
        if state == "requested" and now >= next_attempt and attempts < 3:
            screen = runner.screen()
            if ("Allow once" in screen and tool_name in screen and visible_subject in screen
                    and active_review_overlay_present(screen)):
                # A stale underlying plan workbench can consume a keyboard shortcut during
                # projection catch-up. Click the fresh visible approval action instead.
                if bound_approval_state(session_path, call_id, request_id) == "resolved":
                    return
                click_screen_text(runner, screen, "Allow once")
                attempts += 1
                next_attempt = now + 1.0
    raise TimeoutError(f"approval {call_id} did not resolve after {attempts} bound clicks")


def task_cancellation_settled(path: Path, task_id: str) -> bool:
    requests: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        payload = record.get("payload", {})
        target = payload.get("target", {})
        if (payload.get("record") == "requested"
                and target == {"kind": "task", "task_id": task_id}):
            requests[payload["request_id"]] = payload["run_scope_id"]
        if (payload.get("record") == "finalized"
                and payload.get("request_id") in requests
                and payload.get("run_scope_id") == requests[payload["request_id"]]):
            if (payload.get("outcome") != "cancelled"
                    or payload.get("cleanup_complete") is not True
                    or payload.get("active_effects") != 0
                    or payload.get("active_tasks") != 0):
                raise AcceptanceError(
                    f"task cancellation did not confirm quiescence: {payload.get('reason')}"
                )
            return True
    return False


def validate_resumed_direct_identity(path: Path, task_id: str, admission_id: str) -> None:
    admissions = [entry for entry in direct_task_controls(path, "task_direct_execution_admitted_v1") if entry.get("task_id") == task_id]
    if {entry.get("admission_id") for entry in admissions} != {admission_id}:
        raise AcceptanceError("resume replaced the original Direct admission")
    attempts = [entry for entry in direct_task_controls(path, "task_direct_execution_attempt_v1") if entry.get("task_id") == task_id]
    if len({entry.get("attempt_id") for entry in attempts}) != 2:
        raise AcceptanceError("one interrupted Direct attempt must create exactly one resumed attempt")
    if any(entry.get("task_id") == task_id for entry in direct_task_controls(path, "task_plan")):
        raise AcceptanceError("Direct recovery unexpectedly created a TaskPlan")


def run_direct_task_acceptance(
    runner: object,
    *,
    frozen_binary: Path,
    config_path: Path,
    workspace: Path,
    env: dict[str, str],
    output_dir: Path,
    fixture: FixtureState,
    managed_session_dir: Path,
    deadline: object,
    runner_holder: list[object | None],
) -> tuple[
    object,
    Path,
    SessionAudit,
    SessionAudit,
    SessionAudit,
    SessionAudit,
    SessionAudit,
    SessionAudit,
]:
    """Exercise the current direct-Task plan approval contract through a real TUI PTY."""
    submit_user_prompt(runner, USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=1, fixture=fixture)
    session_path, audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.final_answer_count == 1 and value.task_final_count == 1,
        deadline.remaining(),
    )
    settled_screen = wait_for_visible_screen(
        lambda text: FINAL_CANARY in text
        and "Thinking..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled direct task final answer",
        runner=runner,
    )
    if settled_screen.count(FINAL_CANARY) != 1:
        raise AcceptanceError("TUI rendered the direct task final answer more than once")
    validate_direct_task_audit(
        audit,
        fixture,
        plan_count=1,
        task_count=1,
        final_count=1,
    )

    submit_user_prompt(runner, APPROVAL_USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=2, fixture=fixture)
    wait_for_visible_screen(
        lambda text: ("Approve action?" in text or "Review file changes" in text)
        and "write_file" in text
        and APPROVAL_PATH in text,
        deadline.remaining(),
        "direct task write approval",
        runner=runner,
    )
    approve_bound_tool_once(
        runner, session_path, APPROVAL_TOOL_CALL_ID, "write_file", APPROVAL_PATH,
        deadline.remaining(15.0),
    )
    session_path, approval_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.approval_final_answer_count == 1
        and value.task_final_count == 2,
        deadline.remaining(),
    )
    approval_screen = wait_for_visible_screen(
        lambda text: APPROVAL_FINAL_CANARY in text
        and "Thinking..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled approved direct task final answer",
        runner=runner,
    )
    if approval_screen.count(APPROVAL_FINAL_CANARY) != 1:
        raise AcceptanceError("TUI rendered the approved direct task final more than once")
    if (workspace / APPROVAL_PATH).read_text(encoding="utf-8") != APPROVAL_CONTENT:
        raise AcceptanceError("approved direct task write did not reach the workspace")
    validate_direct_task_audit(
        approval_audit,
        fixture,
        plan_count=2,
        task_count=2,
        final_count=2,
    )
    checkpoint_workspace(workspace, env, "record approved write fixture")

    submit_user_prompt(runner, CONTINUE_USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=3, fixture=fixture)
    wait_for_fixture_request(
        fixture,
        "direct:continue",
        1,
        runner,
        deadline.remaining(),
    )
    direct_admission = direct_task_controls(session_path, "task_direct_execution_admitted_v1")[-1]
    resumed_task_id = str(direct_admission["task_id"])
    resumed_admission_id = str(direct_admission["admission_id"])
    runner.stop()
    fixture.crash_release.set()
    runner = SUPPORT.PtyRunner(
        [str(frozen_binary), "--config", str(config_path), "resume", str(session_path)],
        workspace,
        env,
        output_dir / "resume-process.log",
    )
    runner_holder[0] = runner
    runner.start()
    SUPPORT.wait_for_main_tui(runner, deadline.remaining())
    wait_for_visible_screen(
        lambda text: "sigil ready." in text
        and "Working..." not in text
        and "Thinking..." not in text,
        deadline.remaining(60.0),
        "resumed worker ready for continuation",
        runner=runner,
    )
    session_path, interrupted_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.paused_task_run_count >= 1 and value.task_final_count == 2,
        deadline.remaining(),
    )
    runner.type_text("/task continue")
    runner.send("\r")
    session_path, blocked_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.paused_task_run_count >= 2 and value.task_final_count == 2,
        deadline.remaining(),
    )
    blocked_attempts = [entry for entry in direct_task_controls(
        session_path, "task_direct_execution_attempt_v1"
    ) if entry.get("task_id") == resumed_task_id]
    if (not blocked_attempts or blocked_attempts[-1].get("status") != "blocked"
            or "provider_recovery_schedule_missing" not in str(blocked_attempts[-1].get("reason"))):
        raise AcceptanceError("unscheduled interrupted provider request was not blocked")
    with fixture.lock:
        if fixture.request_counts.get("direct:continue", 0) != 1:
            raise AcceptanceError("unscheduled interrupted provider request was replayed")
    if blocked_audit.failed_run_count != 0:
        raise AcceptanceError("recoverable provider block was counted as a hard failure")
    # The first explicit continuation inspects the interrupted attempt without replaying it.
    # A second user action starts a new attempt after that uncertainty is made visible.
    wait_for_visible_screen(
        lambda text: "Thinking..." not in text
        and "Working..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled blocked direct Task surface",
        runner=runner,
    )
    time.sleep(2.0)
    runner.type_text("/task continue")
    runner.send("\r")
    session_path, continue_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.continue_final_answer_count == 1
        and value.task_final_count == 3,
        deadline.remaining(),
    )
    continued_screen = wait_for_visible_screen(
        lambda text: CONTINUE_FINAL_CANARY in text
        and "Thinking..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled continued direct task final answer",
        runner=runner,
    )
    if continued_screen.count(CONTINUE_FINAL_CANARY) != 1:
        raise AcceptanceError("TUI rendered the continued direct task final more than once")
    if interrupted_audit.task_final_count != 2:
        raise AcceptanceError("interrupted direct task committed a final before continue")
    validate_direct_task_audit(
        continue_audit,
        fixture,
        plan_count=3,
        task_count=3,
        final_count=3,
    )

    validate_resumed_direct_identity(session_path, resumed_task_id, resumed_admission_id)
    requests_before_stale_continue = dict(fixture.request_counts)
    runner.type_text("/task continue")
    runner.send("\r")
    wait_for_visible_screen(
        lambda text: "no unfinished Task to continue" in text,
        deadline.remaining(),
        "stale Direct continuation rejection",
        runner=runner,
    )
    validate_resumed_direct_identity(session_path, resumed_task_id, resumed_admission_id)
    if fixture.request_counts != requests_before_stale_continue:
        raise AcceptanceError("stale Direct continuation dispatched provider work")

    submit_user_prompt(runner, CANCEL_USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=4, fixture=fixture)
    wait_for_fixture_request(
        fixture,
        "direct:cancel",
        1,
        runner,
        deadline.remaining(),
    )
    cancelled_task_id = str(direct_task_controls(
        session_path, "task_direct_execution_admitted_v1",
    )[-1]["task_id"])
    # Ctrl-C is the production cancellation binding while the TUI is busy. Escape only clears
    # focus and would leave the provider fixture blocked forever.
    runner.send(b"\x03")
    session_path, cancel_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: any(
            entry.get("task_id") == cancelled_task_id and entry.get("status") == "interrupted"
            for entry in direct_task_controls(session_path, "task_run")
        ),
        deadline.remaining(),
    )
    fixture.cancel_release.set()
    if not fixture.cancel_settled.wait(timeout=deadline.remaining(5.0)):
        raise AcceptanceError("cancelled direct provider request did not settle")
    # The task terminal is durable before the root run's cleanup/finalization record. Do not
    # submit the next plan while the worker still owns the cancelled run; otherwise the TUI may
    # queue the prompt behind a run that is already visibly cancelled and the fixture will never
    # receive the next provider request.
    session_path, cancel_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: task_cancellation_settled(session_path, cancelled_task_id)
        and value.cancelled_run_count == 1,
        deadline.remaining(),
    )
    if cancel_audit.interrupted_task_run_count < 1:
        raise AcceptanceError("cancelled direct task did not persist an interrupted task terminal")
    if cancel_audit.task_final_count != 3:
        raise AcceptanceError("cancelled direct task incorrectly committed a parent final")
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )
    wait_for_visible_screen(
        lambda text: "Thinking..." not in text
        and "Working..." not in text
        and "Replying..." not in text
        and not active_review_overlay_present(text),
        deadline.remaining(10.0),
        "idle TUI after cancelling direct task",
        runner=runner,
    )
    time.sleep(2.0)
    # The durable child task/run terminal above is the source of truth for the stopped state.
    # The activity pane may legitimately retain the prior provider card until the next prompt;
    # the terminal phase below separately asserts the user-visible cancelled terminal card.

    submit_user_prompt(runner, INTEGRATION_USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=5, fixture=fixture)
    session_path, integration_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.integration_final_answer_count == 1
        and value.task_final_count == 4,
        deadline.remaining(),
    )
    integration_screen = wait_for_visible_screen(
        lambda text: INTEGRATION_FINAL_CANARY in text
        and "Thinking..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled direct integration task final answer",
        runner=runner,
    )
    if integration_screen.count(INTEGRATION_FINAL_CANARY) != 1:
        raise AcceptanceError("TUI rendered the direct integration final more than once")
    validate_direct_task_audit(
        integration_audit,
        fixture,
        plan_count=5,
        task_count=4,
        final_count=4,
    )
    for path, expected in zip(
        INTEGRATION_PATHS,
        ("integrated a\n", "integrated b\n"),
        strict=True,
    ):
        if (workspace / path).read_text(encoding="utf-8") != expected:
            raise AcceptanceError(f"direct integration task did not write {path}")

    submit_user_prompt(runner, TERMINAL_USER_PROMPT)
    approve_review_first_plan(runner, deadline.remaining(180.0), scenario=6, fixture=fixture)
    wait_for_visible_screen(
        lambda text: ("Approve action?" in text or "Review file changes" in text)
        and "exec_command" in text
        and TERMINAL_READY_CANARY in text,
        deadline.remaining(),
        "direct command approval",
        runner=runner,
    )
    approve_bound_tool_once(
        runner, session_path, TERMINAL_START_TOOL_CALL_ID, "exec_command",
        TERMINAL_READY_CANARY, deadline.remaining(15.0),
    )
    session_path, terminal_audit = wait_for_audit(
        managed_session_dir,
        runner,
        lambda value: value.terminal_final_answer_count == 1
        and value.task_final_count == 5,
        deadline.remaining(),
    )
    terminal_screen = wait_for_visible_screen(
        lambda text: TERMINAL_FINAL_CANARY in text
        and "Thinking..." not in text
        and "Replying..." not in text,
        deadline.remaining(),
        "settled direct terminal lifecycle final answer and terminal state",
        runner=runner,
    )
    if terminal_screen.count(TERMINAL_FINAL_CANARY) != 1:
        raise AcceptanceError("TUI rendered the direct terminal final more than once")
    validate_direct_task_audit(
        terminal_audit,
        fixture,
        plan_count=6,
        task_count=5,
        final_count=5,
    )
    validate_terminal_audit(terminal_audit, fixture)
    if (workspace / TERMINAL_OUTPUT_PATH).read_text(encoding="utf-8") != f"{TERMINAL_READY_CANARY}\n":
        raise AcceptanceError("approved command did not write the expected workspace file")
    if fixture.protocol_errors:
        raise AcceptanceError(
            f"fixture observed provider protocol errors: {fixture.protocol_errors}"
        )
    return (
        runner,
        session_path,
        audit,
        approval_audit,
        continue_audit,
        cancel_audit,
        integration_audit,
        terminal_audit,
    )


def submit_user_prompt(runner: object, prompt: str) -> None:
    # Completed tool activity retains focus so ordinary characters do not
    # accidentally edit the composer. Esc returns an idle TUI to the composer.
    runner.send(b"\x1b")
    runner.read_available(0.05)
    # Resume can restore a plan/approval overlay one redraw behind the durable task terminal
    # state. Close that stale presentation before typing; otherwise the opaque prompt bytes are
    # consumed by the overlay and no new user record is admitted.
    screen = runner.screen()
    if active_review_overlay_present(screen):
        wait_for_visible_screen(
            lambda text: not active_review_overlay_present(text),
            10.0,
            "idle TUI after dismissing stale review overlay",
            runner=runner,
        )
    # A slash is the production activity-to-composer focus binding. Use it as a sentinel instead
    # of relying on how many Escape presses a just-completed tool card needs, then let Escape clear
    # the sentinel and any slash selector before submitting the actual prompt.
    runner.send("/")
    runner.read_available(0.05)
    runner.send(b"\x7f")
    runner.read_available(0.05)
    runner.send(b"\x1b")
    runner.read_available(0.05)
    runner.type_text(prompt)
    runner.send("\r")


def active_review_overlay_present(screen: str) -> bool:
    """Detect modal titles, excluding historical timeline notices with the same words."""
    return (
        "Review file changes" in screen
        or "Approve action?" in screen
        or "Approve command?" in screen
        or any(line.lstrip().startswith("Plan Review ·") for line in screen.splitlines()[:4])
    )


def wait_for_visible_screen(
    predicate: Callable[[str], bool],
    timeout: float,
    description: str,
    *,
    runner: object,
) -> str:
    """Wait for a current rendered screen without replaying the VT transcript every poll."""
    deadline = time.monotonic() + timeout
    next_screen_check = 0.0
    while time.monotonic() < deadline:
        runner.read_available(0.05)
        now = time.monotonic()
        if now >= next_screen_check:
            screen = runner.screen()
            if predicate(screen):
                return screen
            next_screen_check = now + 0.5
        if runner.process is not None and runner.process.poll() is not None:
            raise AcceptanceError(f"TUI exited while waiting for {description}")
    raise TimeoutError(f"timed out waiting for {description}")


def plan_workbench_identity(screen: str) -> tuple[str, str]:
    lines = screen.splitlines()
    for index, line in enumerate(lines):
        match = re.search(r"\bplan ([a-zA-Z0-9-]+) · sha256:([0-9a-f]+)", line)
        if match is None:
            continue
        digest = match.group(2)
        if len(digest) < 64 and index + 1 < len(lines):
            tail = re.match(r"[0-9a-f]+", lines[index + 1].lstrip())
            if tail is not None:
                digest += tail.group(0)
        if len(digest) == 64:
            return match.group(1), digest
    raise AcceptanceError("visible plan workbench omitted its complete identity")


def retryable_plan_start(screen: str, scenario: int, identity: tuple[str, str]) -> bool:
    if not current_plan_surface_visible(screen, scenario, require_workbench=True):
        return False
    normalized = " ".join(screen.split())
    return (
        "Run failed:" in normalized
        and "the session changed while running" in normalized
        and "retry the same command" in normalized
        and plan_workbench_identity(screen) == identity
    )


def approve_review_first_plan(
    runner: object, timeout: float, *, scenario: int, fixture: FixtureState | None = None,
) -> None:
    # Only the current rendered surface may authorize a key press. Historical transcript
    # cards and workbenches remain visible across scenarios and cannot identify a pending plan.
    deadline = time.monotonic() + timeout
    workbench = wait_for_plan_surface(runner, timeout, scenario=scenario)
    if not plan_review_workbench_visible(workbench):
        runner.send("\r")
        workbench = wait_for_plan_surface(
            runner, max(0.0, deadline - time.monotonic()),
            scenario=scenario, require_workbench=True,
        )
    if fixture is None:
        runner.send("\r")
        return
    identity = plan_workbench_identity(workbench)
    request_key = {
        1: "direct:read", 2: "direct:write:request", 3: "direct:continue",
        4: "direct:cancel", 5: "direct:integration:a", 6: "direct:terminal:start",
    }[scenario]
    expected_count = fixture.request_counts.get(request_key, 0) + 1
    runner.send("\r")
    attempts = 1
    next_retry = time.monotonic() + 1.0
    deadline = min(deadline, time.monotonic() + 30.0)
    while time.monotonic() < deadline:
        runner.read_available(0.05)
        if fixture.protocol_errors:
            raise AcceptanceError(f"fixture protocol errors: {fixture.protocol_errors}")
        count = fixture.request_counts.get(request_key, 0)
        if count == expected_count:
            return
        if count > expected_count:
            raise AcceptanceError("plan approval dispatched more than once")
        screen = runner.screen()
        if time.monotonic() >= next_retry and retryable_plan_start(screen, scenario, identity):
            if attempts == 3:
                raise AcceptanceError("same-plan admission remained stale after three attempts")
            runner.send("r")
            attempts += 1
            next_retry = time.monotonic() + 1.0
        elif current_plan_surface_visible(screen, scenario, require_workbench=True) and "Run failed:" in screen:
            if not retryable_plan_start(screen, scenario, identity):
                raise AcceptanceError("plan approval failed without a same-plan stale retry")
    raise TimeoutError(f"timed out waiting for approved scenario {scenario} to dispatch")


def current_plan_surface_visible(text: str, scenario: int, *, require_workbench: bool) -> bool:
    summary = f"Review scenario {scenario} before execution"
    lines = text.splitlines()
    active_workbench = (
        any(line.lstrip().startswith("Plan Review ·") for line in lines[:4])
        and plan_review_workbench_visible(text)
        and summary in text
    )
    if active_workbench or require_workbench:
        return active_workbench
    # Match the active card, not the assistant's historical "Plan ready:" message.
    return any(
        line.lstrip().startswith("▌ Plan ready") and summary in line
        for line in lines
    )


def wait_for_plan_surface(
    runner: object,
    timeout: float,
    *,
    scenario: int,
    require_workbench: bool = False,
) -> str:
    deadline = time.monotonic() + timeout
    next_screen_check = 0.0
    while time.monotonic() < deadline:
        runner.read_available(0.05)
        now = time.monotonic()
        if now >= next_screen_check:
            screen = runner.screen()
            if current_plan_surface_visible(screen, scenario, require_workbench=require_workbench):
                return screen
            next_screen_check = now + 0.5
        if runner.process is not None and runner.process.poll() is not None:
            raise AcceptanceError("TUI exited while waiting for review-first plan surface")
    raise TimeoutError(
        f"timed out waiting for scenario {scenario} complete plan review workbench"
        if require_workbench
        else f"timed out waiting for scenario {scenario} review-first plan card"
    )


def latest_plan_surface(raw: bytes) -> str:
    marker = b"validated plan draft recorded"
    offset = raw.rfind(marker)
    if offset < 0:
        return ""
    return ANSI_CSI_PATTERN.sub("", raw[offset:].decode("utf-8", errors="replace"))


def plan_review_workbench_visible(text: str) -> bool:
    return "Plan Review" in text and "Run" in text and "Reject" in text


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
    managed_session_dir: Path | None = None
    runner: object | None = None
    runner_holder: list[object | None] = [None]
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
        configured_session_dir = fixture_root / "sessions"
        managed_session_dir = state_root / "managed" / "session-log"
        for directory in (workspace, state_root, cache_root, configured_session_dir):
            directory.mkdir()
        (workspace / "README.md").write_text("direct-task fixture\n", encoding="utf-8")
        SUPPORT.generate_fixture_tls_identity(fixture_root)
        env = SUPPORT.isolated_environment(fixture_root)
        initialize_git_workspace(workspace, env)
        fixture = FixtureState(
            diagnostics_path=output_dir / "fixture-protocol-errors.json",
            request_payload_path=output_dir / "fixture-requests.jsonl",
        )
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
            session_dir=configured_session_dir,
            port=int(server.server_address[1]),
        )
        runner = SUPPORT.PtyRunner(
            [str(frozen_binary), "--config", str(config_path)],
            workspace,
            env,
            output_dir / "tui-process.log",
        )
        runner_holder[0] = runner
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        (
            runner,
            session_path,
            audit,
            approval_audit,
            continue_audit,
            cancel_audit,
            integration_audit,
            terminal_audit,
        ) = run_direct_task_acceptance(
            runner,
            frozen_binary=frozen_binary,
            config_path=config_path,
            workspace=workspace,
            env=env,
            output_dir=output_dir,
            fixture=fixture,
            managed_session_dir=managed_session_dir,
            deadline=deadline,
            runner_holder=runner_holder,
        )
        runner.quit(timeout=deadline.remaining(10.0))
        runner.stop()
        runner = None
        runner_holder[0] = None

        evidence_dir = output_dir / "sessions"
        evidence_dir.mkdir(mode=0o700, exist_ok=True)
        evidence_path = evidence_dir / "parent.jsonl"
        shutil.copyfile(session_path, evidence_path)
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "campaign": "sigil-direct-task-tui-v1",
            "status": "passed",
            "started_at": started_at,
            "finished_at": utc_now(),
            "duration_ms": int((time.monotonic() - started) * 1000),
            "binary": identity.as_dict(),
            "checks": {
                "review_first_direct_task": True,
                "approved_write_count": approval_audit.approved_tool_call_count,
                "continued_task_count": continue_audit.continue_final_answer_count,
                "cancelled_task_count": cancel_audit.interrupted_task_run_count,
                "direct_integration_final_count": (
                    integration_audit.integration_final_answer_count
                ),
                "approved_terminal_start_count": (
                    terminal_audit.approved_terminal_start_count
                ),
                "terminal_start_completed_count": (
                    terminal_audit.terminal_start_completed_count
                ),
                "terminal_readiness_ready": (
                    "ready" in terminal_audit.terminal_readiness_states
                ),
                "terminal_output_progress_bytes": (
                    terminal_audit.terminal_max_output_bytes
                ),
                "terminal_terminal_state": "cancelled",
                "completed_task_count": terminal_audit.task_final_count,
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
        print(f"direct task PTY acceptance passed: {manifest_path}")
        return 0

    except (
        AcceptanceError,
        OSError,
        TimeoutError,
        json.JSONDecodeError,
    ) as error:
        print(f"direct-task PTY acceptance failed: {error}", file=sys.stderr)
        active_runner = runner_holder[0] if runner_holder[0] is not None else runner
        if active_runner is not None:
            try:
                active_runner.read_available(0.0)
                (output_dir / "failure-screen.txt").write_text(
                    active_runner.screen() + "\n",
                    encoding="utf-8",
                )
            except OSError:
                pass
        if managed_session_dir is not None and managed_session_dir.exists():
            try:
                failure_sessions = output_dir / "failure-sessions"
                failure_sessions.mkdir(parents=True, exist_ok=True)
                for source in managed_session_dir.glob("*/records.jsonl"):
                    shutil.copyfile(
                        source,
                        failure_sessions / f"{source.parent.name}-records.jsonl",
                    )
            except (OSError, json.JSONDecodeError):
                pass
        if args.keep_fixture and fixture_root is not None:
            print(f"retained fixture: {fixture_root}", file=sys.stderr)
        return 1
    finally:
        active_runner = runner_holder[0] if runner_holder[0] is not None else runner
        if active_runner is not None:
            active_runner.stop()
        if server is not None:
            server.shutdown()
            server.server_close()
        if server_thread is not None:
            server_thread.join(timeout=5)
        if fixture_root is not None and not args.keep_fixture:
            shutil.rmtree(fixture_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
