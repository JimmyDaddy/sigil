#!/usr/bin/env python3
"""Exercise Web V1 through the production TUI binary in a deterministic PTY.

The fixture is a real loopback HTTP peer for the DeepSeek-compatible SSE and
Streamable HTTP MCP protocols. Sigil itself runs without test-only flags or
code paths, so this verifies the assembled provider, tool, egress, session,
and rendered-TUI contracts without contacting a real provider or the network.
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import re
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import termios
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)
FINAL_ANSWER = "Fixture websearch completed."
TOOL_CALL_ID = "fixture-websearch-call"
MAX_FIXTURE_REQUEST_BYTES = 1024 * 1024


@dataclass
class FixtureState:
    mcp_methods: list[str] = field(default_factory=list)
    http_methods: list[str] = field(default_factory=list)
    protocol_error_kinds: list[str] = field(default_factory=list)
    provider_requests: list[dict[str, bool]] = field(default_factory=list)
    tool_call_started: threading.Event = field(default_factory=threading.Event)
    release_tool_call: threading.Event = field(default_factory=threading.Event)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record_mcp(self, method: str) -> None:
        with self.lock:
            self.mcp_methods.append(method)

    def record_http(self, method: str) -> None:
        with self.lock:
            self.http_methods.append(method)

    def record_protocol_error(self, error: Exception) -> None:
        with self.lock:
            self.protocol_error_kinds.append(type(error).__name__)

    def record_provider(self, payload: object) -> int:
        request = payload if isinstance(payload, dict) else {}
        messages = request.get("messages")
        tools = request.get("tools")
        has_websearch_tool = any(
            isinstance(tool, dict)
            and isinstance(tool.get("function"), dict)
            and tool["function"].get("name") == "websearch"
            for tool in tools if isinstance(tools, list)
        )
        has_tool_result = any(
            isinstance(message, dict)
            and message.get("role") == "tool"
            and message.get("tool_call_id") == TOOL_CALL_ID
            for message in messages if isinstance(messages, list)
        )
        with self.lock:
            self.provider_requests.append(
                {
                    "has_websearch_tool": has_websearch_tool,
                    "has_tool_result": has_tool_result,
                }
            )
            return len(self.provider_requests)

    def snapshot(self) -> tuple[list[str], list[str], list[str], list[dict[str, bool]]]:
        with self.lock:
            return (
                list(self.mcp_methods),
                list(self.http_methods),
                list(self.protocol_error_kinds),
                list(self.provider_requests),
            )


class FixtureServer(ThreadingHTTPServer):
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

    def do_POST(self) -> None:  # noqa: N802 - required BaseHTTPRequestHandler hook.
        self.fixture.record_http("POST")
        try:
            payload = self._read_json()
            if self.path.endswith("/chat/completions"):
                self._handle_provider(payload)
                return
            if self.path.endswith("/mcp"):
                self._handle_mcp(payload)
                return
            self._send_json(404, {"error": "unknown fixture route"})
        except Exception as error:  # noqa: BLE001 - return fixture diagnostics to the client.
            self.fixture.record_protocol_error(error)
            self._send_json(500, {"error": f"fixture protocol failure: {error}"})

    def do_CONNECT(self) -> None:  # noqa: N802 - required BaseHTTPRequestHandler hook.
        self.fixture.record_http("CONNECT")
        self.send_error(501, "fixture only accepts plaintext HTTP proxy traffic")

    def _read_json(self) -> object:
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            raw = self._read_chunked_body()
        else:
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0 or length > MAX_FIXTURE_REQUEST_BYTES:
                raise ValueError("fixture request body exceeds its safe limit")
            raw = self.rfile.read(length)
        return json.loads(raw.decode("utf-8"))

    def _read_chunked_body(self) -> bytes:
        chunks: list[bytes] = []
        total = 0
        while True:
            line = self.rfile.readline()
            if not line:
                raise ValueError("fixture chunked request ended before its terminator")
            size_text = line.split(b";", 1)[0].strip()
            size = int(size_text, 16)
            if size < 0 or total + size > MAX_FIXTURE_REQUEST_BYTES:
                raise ValueError("fixture chunked request body exceeds its safe limit")
            if size == 0:
                while True:
                    trailer = self.rfile.readline()
                    if trailer in (b"\r\n", b"\n", b""):
                        return b"".join(chunks)
            chunk = self.rfile.read(size)
            if len(chunk) != size or self.rfile.read(2) != b"\r\n":
                raise ValueError("fixture chunked request has invalid framing")
            chunks.append(chunk)
            total += size

    def _handle_provider(self, payload: object) -> None:
        request_index = self.fixture.record_provider(payload)
        if request_index == 1:
            self._send_sse(
                {
                    "choices": [
                        {
                            "delta": {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": TOOL_CALL_ID,
                                        "type": "function",
                                        "function": {
                                            "name": "websearch",
                                            "arguments": '{"query":"fixture Rust release"}',
                                        },
                                    }
                                ]
                            },
                            "finish_reason": "tool_calls",
                        }
                    ]
                }
            )
            return
        self._send_sse(
            {
                "choices": [
                    {
                        "delta": {"content": FINAL_ANSWER},
                        "finish_reason": "stop",
                    }
                ]
            }
        )

    def _handle_mcp(self, payload: object) -> None:
        request = payload if isinstance(payload, dict) else {}
        method = request.get("method")
        request_id = request.get("id")
        if not isinstance(method, str):
            self._send_json(400, {"error": "fixture MCP method is missing"})
            return
        self.fixture.record_mcp(method)

        if method == "initialize":
            self._send_mcp_result(
                request_id,
                {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture-search", "version": "1.0.0"},
                },
            )
            return
        if method == "notifications/initialized":
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if method == "tools/list":
            self._send_mcp_result(
                request_id,
                {
                    "tools": [
                        {
                            "name": "search",
                            "description": "Deterministic fixture search.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"query": {"type": "string"}},
                                "required": ["query"],
                                "additionalProperties": False,
                            },
                        }
                    ]
                },
            )
            return
        if method == "tools/call":
            self.fixture.tool_call_started.set()
            if not self.fixture.release_tool_call.wait(timeout=20):
                self._send_mcp_error(request_id, -32000, "fixture tool release timed out")
                return
            self._send_mcp_result(
                request_id,
                {
                    "content": [
                        {
                            "type": "text",
                            "text": "fixture search result: Rust release details are available.",
                        }
                    ],
                    "isError": False,
                },
            )
            return
        self._send_mcp_error(request_id, -32601, f"unsupported fixture MCP method {method}")

    def _send_mcp_result(self, request_id: object, result: object) -> None:
        self._send_json(
            200,
            {"jsonrpc": "2.0", "id": request_id, "result": result},
            {"Mcp-Session-Id": "fixture-session"},
        )

    def _send_mcp_error(self, request_id: object, code: int, message: str) -> None:
        self._send_json(
            200,
            {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}},
            {"Mcp-Session-Id": "fixture-session"},
        )

    def _send_json(
        self,
        status: int,
        payload: object,
        headers: dict[str, str] | None = None,
    ) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def _send_sse(self, payload: object) -> None:
        body = f"data: {json.dumps(payload, separators=(',', ':'))}\n\ndata: [DONE]\n\n".encode(
            "utf-8"
        )
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class PtyRunner:
    def __init__(self, command: list[str], cwd: Path, env: dict[str, str], raw_log_path: Path) -> None:
        self.command = command
        self.cwd = cwd
        self.env = env
        self.raw_log_path = raw_log_path
        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.output = bytearray()

    def start(self) -> None:
        master_fd, slave_fd = pty.openpty()
        termios.tcsetwinsize(slave_fd, (40, 120))
        try:
            self.process = subprocess.Popen(
                self.command,
                cwd=self.cwd,
                env=self.env,
                stdin=slave_fd,
                stdout=slave_fd,
                stderr=slave_fd,
                close_fds=True,
                start_new_session=True,
            )
        finally:
            os.close(slave_fd)
        self.master_fd = master_fd

    def read_available(self, timeout: float = 0.05) -> None:
        if self.master_fd is None:
            return
        while True:
            ready, _, _ = select.select([self.master_fd], [], [], timeout)
            if not ready:
                return
            try:
                chunk = os.read(self.master_fd, 8192)
            except OSError:
                return
            if not chunk:
                return
            self.output.extend(chunk)
            timeout = 0.0

    def send(self, text: str) -> None:
        if self.master_fd is None:
            raise RuntimeError("PTY is not running")
        os.write(self.master_fd, text.encode("utf-8"))

    def type_text(self, text: str) -> None:
        for character in text:
            self.send(character)
            time.sleep(0.002)

    def rendered(self) -> str:
        return strip_control(bytes(self.output))

    def wait_until(
        self,
        predicate: Callable[[str], bool],
        timeout: float,
        description: str,
    ) -> str:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read_available(0.1)
            rendered = self.rendered()
            if predicate(rendered):
                return rendered
            if self.process is not None and self.process.poll() is not None:
                raise RuntimeError(
                    f"sigil exited while waiting for {description}: {self.process.returncode}"
                )
        raise TimeoutError(f"timed out waiting for {description}")

    def stop(self) -> None:
        if self.master_fd is not None:
            try:
                os.write(self.master_fd, b"\x03")
            except OSError:
                pass
        if self.process is not None:
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                for signal_number in (signal.SIGTERM, signal.SIGKILL):
                    try:
                        os.killpg(self.process.pid, signal_number)
                    except OSError:
                        pass
                    try:
                        self.process.wait(timeout=5)
                        break
                    except subprocess.TimeoutExpired:
                        continue
        if self.master_fd is not None:
            self.read_available(0.0)
            os.close(self.master_fd)
            self.master_fd = None
        self.raw_log_path.write_bytes(bytes(self.output))


@dataclass
class SessionAudit:
    session_path: Path | None
    disclosure_count: int
    query_start_count: int
    query_outcome_count: int
    has_scope_mismatch: bool


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run deterministic Web V1 acceptance through the real Sigil TUI PTY.",
    )
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/debug/sigil"),
        help="Prebuilt sigil binary. Defaults to target/debug/sigil.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-smoke"),
        help="Directory for the raw PTY log and safe Markdown report.",
    )
    parser.add_argument("--timeout", type=float, default=90.0, help="Overall timeout in seconds.")
    parser.add_argument(
        "--keep-workspace",
        action="store_true",
        help="Keep the temporary workspace, config, state, and cache after success.",
    )
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True)
    return Path(output.strip()).resolve()


def strip_control(data: bytes) -> str:
    without_ansi = ANSI_RE.sub(b"", data)
    without_controls = bytes(
        byte for byte in without_ansi if byte in (9, 10, 13) or byte >= 32
    )
    return without_controls.decode("utf-8", errors="replace")


def looks_like_trust_gate(text: str) -> bool:
    lowered = text.lower()
    return (
        "trust this workspace" in lowered
        or "workspace trust" in lowered
        or "trust workspace" in lowered
    )


def looks_like_main_tui_ready(text: str) -> bool:
    lowered = text.lower()
    return "agent:" in lowered and ("build" in lowered or "session" in lowered)


def write_config(path: Path, port: int) -> None:
    endpoint = f"http://127.0.0.1:{port}"
    path.write_text(
        f'''[agent]
provider = "deepseek"
model = "fixture-model"

[permission]
mode = "danger-full-access"

[web]
enabled = true
network_mode = "allow"
proxy_mode = "environment"
allow_http = true
search_route = "mcp"
allowed_ports = [80]

[web.search_mcp]
server = "fixture-search"
tool = "search"

[providers.deepseek]
base_url = "{endpoint}/provider"
beta_base_url = "{endpoint}/provider"
anthropic_base_url = "{endpoint}/provider"
api_key = "fixture-key"
strict_tools_mode = "off"

[[mcp_servers]]
name = "fixture-search"
transport = "streamable_http"
url = "http://93.184.216.34/mcp"
startup = "lazy"
required = true

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "allow"
egress_logging = true
''',
        encoding="utf-8",
    )


def inspect_session(state_dir: Path) -> SessionAudit:
    sessions = sorted(
        state_dir.glob("workspaces/*/sessions/session-*.jsonl"),
        key=lambda candidate: candidate.stat().st_mtime,
        reverse=True,
    )
    if not sessions:
        return SessionAudit(None, 0, 0, 0, False)
    session_path = sessions[0]
    event_types: list[str] = []
    raw = session_path.read_text(encoding="utf-8", errors="replace")
    for line in raw.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            event_types.append(event_type)
    return SessionAudit(
        session_path=session_path,
        disclosure_count=event_types.count("egress_disclosure_presented"),
        query_start_count=event_types.count("query_egress_started"),
        query_outcome_count=event_types.count("query_egress_outcome"),
        has_scope_mismatch="external source belongs to a different session scope" in raw,
    )


def assert_subsequence(observed: list[str], expected: list[str]) -> None:
    position = 0
    for value in observed:
        if position < len(expected) and value == expected[position]:
            position += 1
    if position != len(expected):
        raise RuntimeError(
            f"fixture MCP methods missing required sequence {expected}; observed {observed}"
        )


def assert_acceptance(
    rendered: str,
    fixture: FixtureState,
    audit: SessionAudit,
) -> None:
    if FINAL_ANSWER not in rendered:
        raise RuntimeError("final provider answer was not rendered by the TUI")
    if "websearch" not in rendered.lower():
        raise RuntimeError("websearch tool card was not rendered by the TUI")
    methods, http_methods, protocol_error_kinds, provider_requests = fixture.snapshot()
    assert_subsequence(
        methods,
        ["initialize", "notifications/initialized", "tools/list", "tools/call"],
    )
    if "CONNECT" in http_methods:
        raise RuntimeError("fixture expected plaintext HTTP proxy traffic, not CONNECT tunneling")
    if protocol_error_kinds:
        raise RuntimeError(
            f"fixture rejected an HTTP payload: {', '.join(protocol_error_kinds)}"
    )
    if not provider_requests[0]["has_websearch_tool"]:
        raise RuntimeError("initial provider request did not expose websearch")
    if not any(request["has_tool_result"] for request in provider_requests[1:]):
        raise RuntimeError("no continuation provider request contained the websearch tool result")
    if audit.session_path is None:
        raise RuntimeError("TUI did not create a session JSONL")
    if audit.disclosure_count == 0:
        raise RuntimeError("session is missing durable egress disclosure evidence")
    if audit.query_start_count == 0 or audit.query_outcome_count == 0:
        raise RuntimeError("session is missing durable query lifecycle evidence")
    if audit.query_start_count != audit.query_outcome_count:
        raise RuntimeError("session has an unfinished query lifecycle")
    if audit.has_scope_mismatch:
        raise RuntimeError("session recorded the external-source scope mismatch regression")


def write_report(
    path: Path,
    *,
    status: str,
    binary: Path,
    raw_log_path: Path,
    audit: SessionAudit,
    fixture: FixtureState,
    disclosure_visible_before_tool_body: bool,
    notes: list[str],
) -> None:
    methods, http_methods, protocol_error_kinds, provider_requests = fixture.snapshot()
    lines = [
        "# Sigil Web V1 Real-PTY Acceptance",
        "",
        f"Status: `{status}`",
        f"Binary: `{binary}`",
        f"Raw PTY log: `{raw_log_path}`",
        f"Session: `{audit.session_path or '-'}'",
        "",
        "## Checks",
        "",
        f"- Network disclosure visible before fixture tool body: `{disclosure_visible_before_tool_body}`",
        f"- Provider request count: `{len(provider_requests)}`",
        f"- Fixture HTTP verbs: `{', '.join(http_methods) or '-'}`",
        f"- Fixture protocol errors: `{', '.join(protocol_error_kinds) or '-'}`",
        f"- MCP methods: `{', '.join(methods) or '-'}`",
        f"- Durable disclosure records: `{audit.disclosure_count}`",
        f"- Durable query starts/outcomes: `{audit.query_start_count}/{audit.query_outcome_count}`",
        f"- External-source session scope mismatch: `{audit.has_scope_mismatch}`",
        "",
        "The report intentionally excludes provider/MCP request bodies, query text, and credentials.",
    ]
    if notes:
        lines.extend(["", "## Notes", ""])
        lines.extend(f"- {note}" for note in notes)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    root = repo_root()
    binary = args.binary if args.binary.is_absolute() else root / args.binary
    binary = binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"sigil binary is missing: {binary}; run cargo build --locked -p sigil first")

    timestamp = time.strftime("%Y%m%d-%H%M%S")
    output_dir = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    raw_log_path = output_dir / f"tui-web-pty-acceptance-{timestamp}.log"
    report_path = output_dir / f"tui-web-pty-acceptance-{timestamp}.md"

    fixture = FixtureState()
    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.fixture = fixture
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    port = int(server.server_address[1])

    temp_root = Path(tempfile.mkdtemp(prefix="sigil-web-pty-"))
    workspace = temp_root / "workspace"
    state_dir = temp_root / "state"
    cache_dir = temp_root / "cache"
    config_path = temp_root / "sigil.toml"
    workspace.mkdir()
    state_dir.mkdir()
    cache_dir.mkdir()
    write_config(config_path, port)

    env = os.environ.copy()
    env.update(
        {
            "SIGIL_STATE_HOME": str(state_dir),
            "SIGIL_CACHE_HOME": str(cache_dir),
            "TERM": env.get("TERM", "xterm-256color"),
            # The Web V1 destination guard validates this public logical destination. Reqwest then
            # uses the production environment-proxy route to reach the local fixture, so no test
            # code bypasses loopback/SSRF policy and no request leaves the machine.
            "HTTP_PROXY": f"http://127.0.0.1:{port}",
            "HTTPS_PROXY": f"http://127.0.0.1:{port}",
            "http_proxy": f"http://127.0.0.1:{port}",
            "https_proxy": f"http://127.0.0.1:{port}",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
    )
    runner = PtyRunner([str(binary), "--config", str(config_path)], workspace, env, raw_log_path)
    audit = SessionAudit(None, 0, 0, 0, False)
    notes: list[str] = []
    status = "failed"
    disclosure_visible_before_tool_body = False
    try:
        runner.start()
        initial = runner.wait_until(
            lambda text: looks_like_trust_gate(text) or looks_like_main_tui_ready(text),
            min(30.0, args.timeout),
            "initial TUI screen",
        )
        if looks_like_trust_gate(initial):
            runner.send("\r")
            runner.wait_until(
                looks_like_main_tui_ready,
                min(30.0, args.timeout),
                "main TUI screen after workspace trust",
            )

        runner.type_text("Use websearch and answer from the fixture result.")
        runner.send("\r")
        deadline = time.monotonic() + min(45.0, args.timeout)
        while not fixture.tool_call_started.is_set() and time.monotonic() < deadline:
            runner.read_available(0.1)
        if not fixture.tool_call_started.is_set():
            raise TimeoutError("timed out waiting for fixture MCP tools/call")
        runner.wait_until(
            lambda text: "Network disclosure" in text,
            min(10.0, args.timeout),
            "Network disclosure before fixture tool body",
        )
        disclosure_visible_before_tool_body = True
        fixture.release_tool_call.set()
        rendered = runner.wait_until(
            lambda text: FINAL_ANSWER in text,
            min(30.0, args.timeout),
            "final provider answer",
        )
        audit = inspect_session(state_dir)
        assert_acceptance(rendered, fixture, audit)
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - retain the actionable smoke failure.
        notes.append(str(error))
        return_code = 1
    finally:
        fixture.release_tool_call.set()
        runner.stop()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        audit = inspect_session(state_dir)
        write_report(
            report_path,
            status=status,
            binary=binary,
            raw_log_path=raw_log_path,
            audit=audit,
            fixture=fixture,
            disclosure_visible_before_tool_body=disclosure_visible_before_tool_body,
            notes=notes,
        )
        print(f"wrote {report_path}")
        print(f"raw log {raw_log_path}")
        if status != "passed" or args.keep_workspace:
            print(f"workspace {workspace}")
            print(f"state {state_dir}")
        else:
            shutil.rmtree(temp_root)
    return return_code


if __name__ == "__main__":
    sys.exit(main())
