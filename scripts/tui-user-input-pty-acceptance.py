#!/usr/bin/env python3
"""Exercise durable agent user input across a real TUI process restart."""

from __future__ import annotations

import argparse
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


SUPPORT_SCRIPT = Path(__file__).with_name("tui-stateful-pty-acceptance.py")
SUPPORT_SPEC = importlib.util.spec_from_file_location("tui_stateful_support", SUPPORT_SCRIPT)
assert SUPPORT_SPEC is not None and SUPPORT_SPEC.loader is not None
SUPPORT = importlib.util.module_from_spec(SUPPORT_SPEC)
sys.modules[SUPPORT_SPEC.name] = SUPPORT
SUPPORT_SPEC.loader.exec_module(SUPPORT)

SCHEMA_VERSION = 1
MODEL_NAME = "user-input-fixture-model"
USER_PROMPT = "USER-INPUT-PTY-OPAQUE-REQUEST-1842"
QUESTION_PROMPT = "Which compatibility boundary should be preserved?"
ANSWER = "legacy-sessions"
TOOL_CALL_ID = "durable-user-input-call"
FINAL_CANARY = "USER-INPUT-PTY-FINAL-CANARY-7316"
TOOL_ARGUMENTS = json.dumps(
    {
        "prompt": "Choose the compatibility boundary before implementation.",
        "questions": [
            {
                "id": "compatibility",
                "header": "Compatibility",
                "question": QUESTION_PROMPT,
                "required": True,
                "field": {
                    "kind": "text",
                    "multiline": False,
                    "max_chars": 80,
                },
            }
        ],
    },
    separators=(",", ":"),
)


class AcceptanceError(RuntimeError):
    """Raised when a durable user-input contract check fails."""


class FixtureState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.request_count = 0
        self.continuation_count = 0
        self.title_count = 0
        self.protocol_errors: list[str] = []
        self.continuation_payload: object | None = None

    def next_response(self, payload: object) -> str:
        with self.lock:
            self.request_count += 1
            if has_exact_tool_result(payload):
                if self.continuation_count == 0:
                    self.continuation_count = 1
                    self.continuation_payload = payload
                    return "continuation"
                self.title_count += 1
                return "title"
            if self.request_count == 1:
                return "question"
            self.title_count += 1
            return "title"

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
        try:
            if not self.path.endswith("/chat/completions"):
                raise AcceptanceError(f"unexpected fixture path {self.path}")
            payload = self._read_json()
            response = self.fixture.next_response(payload)
            if response == "question":
                if "request_user_input" not in tool_names(payload):
                    raise AcceptanceError("ordinary conversation omitted request_user_input")
                self._send_tool_call()
            elif response == "continuation":
                self._send_text(FINAL_CANARY)
            else:
                self._send_text("Durable user input acceptance")
        except Exception as error:  # noqa: BLE001 - retain fixture diagnostics.
            self.fixture.record_error(error)
            try:
                self._send_json({"error": f"fixture failure: {error}"}, status=500)
            except (BrokenPipeError, ConnectionResetError):
                pass

    def _read_json(self) -> object:
        length = int(self.headers.get("Content-Length", "0"))
        if length > 2 * 1024 * 1024:
            raise AcceptanceError("fixture request exceeds 2 MiB")
        return json.loads(self.rfile.read(length).decode("utf-8"))

    def _send_tool_call(self) -> None:
        self._send_sse(
            {
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": TOOL_CALL_ID,
                            "type": "function",
                            "function": {
                                "name": "request_user_input",
                                "arguments": TOOL_ARGUMENTS,
                            },
                        }
                    ]
                },
                "finish_reason": "tool_calls",
            }
        )

    def _send_text(self, content: str) -> None:
        self._send_sse({"delta": {"content": content}, "finish_reason": "stop"})

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

    def _send_json(self, payload: object, status: int) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def tool_names(payload: object) -> set[str]:
    if not isinstance(payload, dict) or not isinstance(payload.get("tools"), list):
        return set()
    names: set[str] = set()
    for tool in payload["tools"]:
        function = tool.get("function") if isinstance(tool, dict) else None
        name = function.get("name") if isinstance(function, dict) else None
        if isinstance(name, str):
            names.add(name)
    return names


def has_exact_tool_result(payload: object) -> bool:
    if not isinstance(payload, dict) or not isinstance(payload.get("messages"), list):
        return False
    return any(
        isinstance(message, dict)
        and message.get("role") == "tool"
        and message.get("tool_call_id") == TOOL_CALL_ID
        for message in payload["messages"]
    )


def write_config(
    path: Path,
    *,
    workspace: Path,
    state_root: Path,
    cache_root: Path,
    session_dir: Path,
    port: int,
) -> None:
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
connection = "user-input-fixture"
model = "{MODEL_NAME}"
max_turns = 6
tool_timeout_secs = 10

[model_request]
request_timeout_secs = 10
stream_idle_timeout_secs = 10

[task]
enabled = true
routing_policy = "manual"

[permission]
mode = "auto-edit"

[web]
enabled = false

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[connections.user-input-fixture]
label = "Durable user input fixture"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:{port}/v1"
credential = {{ source = "none" }}
''',
        encoding="utf-8",
    )
    os.chmod(path, 0o600)


def read_audit(path: Path) -> dict[str, object]:
    event_counts: dict[str, int] = {}
    control_counts: dict[str, int] = {}
    continuation_physical_attempt_ids: list[str] = []
    provider_started_ids: set[str] = set()
    provider_terminal_ids: set[str] = set()
    resolutions: list[str] = []
    tool_results = 0
    final_answers = 0
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(raw_line)
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            event_counts[event_type] = event_counts.get(event_type, 0) + 1
        payload = record.get("payload")
        if not isinstance(payload, dict):
            continue
        if event_type == "provider_physical_attempt_started":
            attempt_id = payload.get("physical_attempt_id")
            if isinstance(attempt_id, str):
                provider_started_ids.add(attempt_id)
        elif event_type == "provider_physical_attempt_terminal":
            attempt_id = payload.get("physical_attempt_id")
            if isinstance(attempt_id, str):
                provider_terminal_ids.add(attempt_id)
        entry = payload.get("session_log_entry")
        if not isinstance(entry, dict):
            continue
        tool_result = entry.get("tool_result_v3")
        if isinstance(tool_result, dict) and tool_result.get("call_id") == TOOL_CALL_ID:
            tool_results += 1
        assistant = entry.get("assistant")
        if isinstance(assistant, dict) and assistant.get("content") == FINAL_CANARY:
            final_answers += 1
        control = entry.get("control")
        if not isinstance(control, dict):
            continue
        for key, value in control.items():
            if isinstance(value, dict):
                control_counts[key] = control_counts.get(key, 0) + 1
        started = control.get("user_input_continuation_started")
        if isinstance(started, dict) and isinstance(started.get("physical_attempt_id"), str):
            continuation_physical_attempt_ids.append(started["physical_attempt_id"])
        resolved = control.get("user_input_resolved")
        resolution = resolved.get("resolution") if isinstance(resolved, dict) else None
        if isinstance(resolution, dict) and isinstance(resolution.get("kind"), str):
            resolutions.append(resolution["kind"])
    return {
        "event_counts": event_counts,
        "control_counts": control_counts,
        "continuation_physical_attempt_ids": continuation_physical_attempt_ids,
        "provider_started_ids": provider_started_ids,
        "provider_terminal_ids": provider_terminal_ids,
        "resolutions": resolutions,
        "tool_results": tool_results,
        "final_answers": final_answers,
    }


def validate_audit(audit: dict[str, object]) -> None:
    controls = audit["control_counts"]
    assert isinstance(controls, dict)
    expected = {
        "user_input_requested": 1,
        "user_input_decision_accepted": 1,
        "user_input_continuation_claimed": 1,
        "user_input_continuation_started": 1,
        "user_input_resolved": 1,
    }
    for key, count in expected.items():
        if controls.get(key) != count:
            raise AcceptanceError(f"expected one {key}, observed {controls.get(key, 0)}")
    if controls.get("user_input_continuation_released", 0) != 0:
        raise AcceptanceError("successful continuation unexpectedly released its claim")
    attempt_ids = audit["continuation_physical_attempt_ids"]
    started_ids = audit["provider_started_ids"]
    terminal_ids = audit["provider_terminal_ids"]
    if not (
        isinstance(attempt_ids, list)
        and len(attempt_ids) == 1
        and isinstance(started_ids, set)
        and isinstance(terminal_ids, set)
        and attempt_ids[0] in started_ids
        and attempt_ids[0] in terminal_ids
    ):
        raise AcceptanceError("continuation is not bound to one exact physical provider attempt")
    if audit["resolutions"] != ["consumed"]:
        raise AcceptanceError(f"unexpected user-input resolution {audit['resolutions']}")
    if audit["tool_results"] != 1 or audit["final_answers"] != 1:
        raise AcceptanceError("answer continuation did not commit one tool result and one final")


def wait_for_audit(
    session_dir: Path,
    runner: object,
    predicate: object,
    timeout: float,
) -> tuple[Path, dict[str, object]]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        runner.read_available(0.01)
        files = SUPPORT.session_files(session_dir)
        if len(files) > 1:
            raise AcceptanceError("user-input run created more than one parent session")
        if files:
            try:
                audit = read_audit(files[0])
                if predicate(audit):
                    return files[0], audit
            except (OSError, json.JSONDecodeError):
                pass
        time.sleep(0.05)
    raise TimeoutError("timed out waiting for durable user-input audit state")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-user-input-acceptance"),
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--keep-fixture", action="store_true")
    return parser.parse_args()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def main() -> int:
    args = parse_args()
    root = SUPPORT.repo_root()
    output_dir = (args.output_dir if args.output_dir.is_absolute() else root / args.output_dir)
    output_dir = output_dir.expanduser().resolve()
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
        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-user-input-pty-"))
        frozen_binary = SUPPORT.freeze_binary(binary_source, fixture_root, identity)
        workspace = fixture_root / "workspace"
        state_root = fixture_root / "state"
        cache_root = fixture_root / "cache"
        session_dir = fixture_root / "sessions"
        for directory in (workspace, state_root, cache_root, session_dir):
            directory.mkdir()
        (workspace / "README.md").write_text("durable input fixture\n", encoding="utf-8")
        # The shared isolated environment always points SSL_CERT_FILE at this deterministic
        # identity, even though this fixture's loopback custom endpoint itself uses HTTP.
        SUPPORT.generate_fixture_tls_identity(fixture_root)
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
            output_dir / "request-process.log",
        )
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        runner.type_text(USER_PROMPT)
        runner.send("\r")
        runner.wait_until(
            lambda screen: QUESTION_PROMPT in screen and "Submit" in screen,
            deadline.remaining(),
            "durable user-input form",
            final_screen=True,
        )
        session_path, requested_audit = wait_for_audit(
            session_dir,
            runner,
            lambda audit: audit["control_counts"].get("user_input_requested") == 1,
            deadline.remaining(),
        )
        if requested_audit["control_counts"].get("user_input_decision_accepted", 0) != 0:
            raise AcceptanceError("unanswered request was accepted before user input")
        runner.stop()
        runner = None

        runner = SUPPORT.PtyRunner(
            [str(frozen_binary), "--config", str(config_path), "resume", str(session_path)],
            workspace,
            env,
            output_dir / "resume-process.log",
        )
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        runner.wait_until(
            lambda screen: QUESTION_PROMPT in screen and "Submit" in screen,
            deadline.remaining(),
            "restored durable user-input form",
            final_screen=True,
        )
        runner.type_text(ANSWER)
        runner.send("\r")
        runner.send("\r")
        session_path, final_audit = wait_for_audit(
            session_dir,
            runner,
            lambda audit: audit["final_answers"] == 1,
            deadline.remaining(),
        )
        final_screen = runner.wait_until(
            lambda screen: FINAL_CANARY in screen and "Thinking..." not in screen,
            deadline.remaining(),
            "settled post-answer continuation",
            final_screen=True,
        )
        if final_screen.count(FINAL_CANARY) != 1:
            raise AcceptanceError("continuation final rendered more than once")
        validate_audit(final_audit)
        with fixture.lock:
            if fixture.continuation_count != 1 or fixture.protocol_errors:
                raise AcceptanceError(
                    "provider did not observe exactly one clean answer continuation: "
                    f"continuations={fixture.continuation_count}, errors={fixture.protocol_errors}"
                )
            continuation_payload = fixture.continuation_payload
        if continuation_payload is None or ANSWER not in json.dumps(continuation_payload):
            raise AcceptanceError("exact persisted answer was absent from the continuation")
        runner.quit(timeout=min(10.0, deadline.remaining()))
        runner = None

        manifest = {
            "schema_version": SCHEMA_VERSION,
            "kind": "sigil_tui_durable_user_input_acceptance",
            "started_at": started_at,
            "duration_seconds": round(time.monotonic() - started, 3),
            "binary": identity.as_dict(),
            "checks": {
                "request_durable_before_restart": True,
                "form_restored_after_restart": True,
                "unique_answer_continuation": True,
                "continuation_physical_attempt_bound": True,
                "tool_result_count": final_audit["tool_results"],
                "final_answer_count": final_audit["final_answers"],
            },
            "evidence": {
                "request_pty_log": "request-process.log",
                "resume_pty_log": "resume-process.log",
                "session": "sessions/parent.jsonl",
            },
            "privacy": {"raw_artifacts_local_only": True, "automatic_upload": False},
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
        print(f"durable user-input PTY acceptance passed: {manifest_path}")
        return 0
    except (AcceptanceError, OSError, TimeoutError, json.JSONDecodeError) as error:
        print(f"durable user-input PTY acceptance failed: {error}", file=sys.stderr)
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
