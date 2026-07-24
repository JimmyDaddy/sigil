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
USER_PROMPT = "ORCHESTRATION-PTY-OPAQUE-REQUEST-2486"
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


class AcceptanceError(RuntimeError):
    """Raised when the orchestration PTY contract is violated."""


@dataclasses.dataclass(frozen=True)
class SessionAudit:
    event_counts: dict[str, int]
    completed_steps: tuple[str, ...]
    final_answer_count: int
    task_final_count: int
    failed_run_count: int


@dataclasses.dataclass
class FixtureState:
    request_counts: dict[str, int] = dataclasses.field(default_factory=dict)
    request_order: list[str] = dataclasses.field(default_factory=list)
    protocol_errors: list[str] = dataclasses.field(default_factory=list)
    active_reads: int = 0
    max_concurrent_reads: int = 0
    lock: threading.Lock = dataclasses.field(default_factory=threading.Lock)

    def start_request(self, kind: str) -> None:
        with self.lock:
            self.request_counts[kind] = self.request_counts.get(kind, 0) + 1
            self.request_order.append(kind)
            if kind.startswith("read:"):
                self.active_reads += 1
                self.max_concurrent_reads = max(
                    self.max_concurrent_reads,
                    self.active_reads,
                )

    def finish_request(self, kind: str) -> None:
        if not kind.startswith("read:"):
            return
        with self.lock:
            self.active_reads -= 1

    def record_error(self, error: Exception) -> None:
        with self.lock:
            self.protocol_errors.append(f"{type(error).__name__}: {error}")


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    fixture: FixtureState


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
        try:
            payload = self._read_json()
            if not self.path.endswith("/chat/completions"):
                raise AcceptanceError(f"unexpected fixture path {self.path}")
            kind = classify_request(payload)
            self.fixture.start_request(kind)
            if kind == "conversation":
                self._send_tool_call(
                    "handoff-call",
                    "request_task_planning",
                    HANDOFF_ARGS,
                )
            elif kind == "planner":
                self._send_tool_call("task-plan-call", "task_plan_update", PLAN_ARGS)
            elif kind.startswith("read:"):
                time.sleep(0.35)
                step_id = kind.removeprefix("read:")
                self._send_text(f"bounded result for {step_id}")
            elif kind == "synthesis":
                self._send_text(FINAL_CANARY)
            else:
                raise AcceptanceError(f"unsupported request kind {kind}")
        except Exception as error:  # noqa: BLE001 - retain fixture diagnostics.
            self.fixture.record_error(error)
            self._send_json({"error": f"fixture failure: {error}"}, status=500)
        finally:
            if kind:
                self.fixture.finish_request(kind)

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


def classify_request(payload: object) -> str:
    names = tool_names(payload)
    text = request_text(payload)
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
        f'''[workspace]
root = "{workspace}"

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[session]
log_dir = "{session_dir}"

[agent]
provider = "openai_compat"
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

[permission]
mode = "auto-edit"

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[providers.openai_compat]
base_url = "{endpoint}"
api_key = "orchestration-fixture-key"
''',
        encoding="utf-8",
    )
    os.chmod(path, 0o600)


def read_session_audit(path: Path) -> SessionAudit:
    counts: dict[str, int] = {}
    completed_steps: list[str] = []
    final_answer_count = 0
    task_final_count = 0
    failed_run_count = 0
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(raw_line)
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            counts[event_type] = counts.get(event_type, 0) + 1
        payload = record.get("payload")
        if not isinstance(payload, dict):
            continue
        if event_type == "run_finalized" and payload.get("run_status") != "completed":
            failed_run_count += 1
        entry = payload.get("session_log_entry")
        if not isinstance(entry, dict):
            continue
        assistant = entry.get("assistant")
        if (
            isinstance(assistant, dict)
            and assistant.get("assistant_kind") == "final_answer"
            and assistant.get("content") == FINAL_CANARY
        ):
            final_answer_count += 1
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
        if isinstance(control.get("task_final_answer_committed"), dict):
            task_final_count += 1
    return SessionAudit(
        event_counts=counts,
        completed_steps=tuple(completed_steps),
        final_answer_count=final_answer_count,
        task_final_count=task_final_count,
        failed_run_count=failed_run_count,
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
        env = SUPPORT.isolated_environment(fixture_root)
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
        runner.type_text(USER_PROMPT)
        runner.send("\r")
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
                "route_kill_switch_count": audit.event_counts.get(
                    "orchestration_route_disabled",
                    0,
                ),
            },
            "evidence": {
                "pty_log": "tui-process.log",
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
