#!/usr/bin/env python3
"""Verify terminal attention bytes through the production Sigil TUI binary."""

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
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)
PROMPT_CANARY = "ATTENTION-PROMPT-CANARY-4107"
REPLY_CANARY = "ATTENTION-REPLY-CANARY-8923"
PATH_CANARY = "attention-path-canary-5271"
TOOL_CANARY = "ATTENTION-TOOL-CANARY-6649"
ERROR_CANARY = "ATTENTION-ERROR-CANARY-7315"
MCP_CANARY = "attention-mcp-canary-2486"
NOTIFICATION_TEXT = (
    "Sigil session complete",
    "Long run finished.",
    "Sigil needs your attention",
    "Sigil run failed",
    "Tool approval required.",
    "Open Sigil for details.",
    "Input required to continue.",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run default-off and explicit-BEL terminal attention PTY acceptance.",
    )
    parser.add_argument(
        "--binary",
        type=Path,
        help="Existing sigil binary. Builds target/debug/sigil when omitted.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-attention-acceptance"),
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--keep-fixture", action="store_true")
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True
    )
    return Path(output.strip()).resolve()


def build_binary(root: Path) -> Path:
    subprocess.run(["cargo", "build", "--locked", "-p", "sigil"], cwd=root, check=True)
    binary = root / "target" / "debug" / "sigil"
    if not binary.is_file():
        raise RuntimeError(f"built binary is missing: {binary}")
    return binary


def strip_control(data: bytes) -> str:
    without_ansi = ANSI_RE.sub(b"", data)
    without_controls = bytes(
        byte for byte in without_ansi if byte in (9, 10, 13) or byte >= 32
    )
    return without_controls.decode("utf-8", errors="replace")


class FixtureServer(ThreadingHTTPServer):
    request_count: int
    lock: threading.Lock


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        if not self.path.endswith("/chat/completions"):
            self.send_error(404)
            return
        self._read_request_body()
        server = self.server
        assert isinstance(server, FixtureServer)
        with server.lock:
            server.request_count += 1
        time.sleep(1.2)
        payload = {
            "choices": [
                {
                    "delta": {"content": REPLY_CANARY},
                    "finish_reason": "stop",
                }
            ]
        }
        body = (
            f"data: {json.dumps(payload, separators=(',', ':'))}\n\n"
            "data: [DONE]\n\n"
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_request_body(self) -> bytes:
        if self.headers.get("Transfer-Encoding", "").lower() != "chunked":
            length = int(self.headers.get("Content-Length", "0"))
            return self.rfile.read(length)
        chunks: list[bytes] = []
        while True:
            size_line = self.rfile.readline()
            size = int(size_line.split(b";", 1)[0].strip(), 16)
            if size == 0:
                while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                    pass
                return b"".join(chunks)
            chunks.append(self.rfile.read(size))
            if self.rfile.read(2) != b"\r\n":
                raise ValueError("invalid chunked request framing")


class PtyRunner:
    def __init__(
        self, command: list[str], cwd: Path, env: dict[str, str], raw_log: Path
    ) -> None:
        self.command = command
        self.cwd = cwd
        self.env = env
        self.raw_log = raw_log
        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.output = bytearray()

    def start(self) -> None:
        master_fd, slave_fd = pty.openpty()
        termios.tcsetwinsize(slave_fd, (42, 140))
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

    def send(self, value: str) -> None:
        if self.master_fd is None:
            raise RuntimeError("PTY is not running")
        os.write(self.master_fd, value.encode("utf-8"))

    def type_text(self, value: str) -> None:
        for character in value:
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
                    f"process exited while waiting for {description}: {self.process.returncode}"
                )
        raise TimeoutError(f"timed out waiting for {description}")

    def settle(self, timeout: float = 2.0) -> None:
        deadline = time.monotonic() + timeout
        quiet_since = time.monotonic()
        size = len(self.output)
        while time.monotonic() < deadline:
            self.read_available(0.05)
            if len(self.output) != size:
                size = len(self.output)
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= 0.2:
                return
        raise TimeoutError("PTY output did not settle")

    def wait_for_exit(self, timeout: float) -> int:
        if self.process is None:
            raise RuntimeError("PTY process was not started")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read_available(0.05)
            return_code = self.process.poll()
            if return_code is not None:
                return return_code
        raise TimeoutError("TUI did not exit after /quit")

    def close(self) -> None:
        if self.process is not None and self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
                self.process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except OSError:
                    pass
        self.read_available(0.0)
        self.raw_log.write_bytes(bytes(self.output))
        if self.master_fd is not None:
            os.close(self.master_fd)
            self.master_fd = None


@dataclass
class RunAudit:
    label: str
    bell_count: int
    request_count: int
    focus_enable_count: int
    focus_disable_count: int
    notification_event_count: int
    notification_text_in_state: bool
    notification_text_in_cache: bool
    session_path: str | None
    raw_log: str


def write_config(
    path: Path,
    workspace: Path,
    state_dir: Path,
    cache_dir: Path,
    port: int,
    enabled: bool,
) -> None:
    endpoint = f"http://127.0.0.1:{port}/provider"
    path.write_text(
        f'''[workspace]
root = "{workspace}"

[storage]
state_root = "{state_dir}"
cache_root = "{cache_dir}"

[session]
log_dir = "{state_dir / 'sessions'}"

[agent]
provider = "deepseek"
model = "fixture-model"

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[terminal.notifications]
enabled = {str(enabled).lower()}
method = "bell"
minimum_run_duration_ms = 1000

[providers.deepseek]
base_url = "{endpoint}"
beta_base_url = "{endpoint}"
anthropic_base_url = "{endpoint}"
api_key = "fixture-key"
strict_tools_mode = "off"

[[mcp_servers]]
name = "{MCP_CANARY}"
transport = "stdio"
command = "{TOOL_CANARY}"
startup = "lazy"
required = false
''',
        encoding="utf-8",
    )


def tree_contains(root: Path, needles: tuple[str, ...]) -> bool:
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        data = path.read_bytes()
        if any(needle.encode("utf-8") in data for needle in needles):
            return True
    return False


def inspect_session(state_dir: Path) -> tuple[Path | None, int]:
    sessions = sorted(
        state_dir.rglob("session-*.jsonl"),
        key=lambda candidate: candidate.stat().st_mtime,
        reverse=True,
    )
    if not sessions:
        return None, 0
    session = sessions[0]
    attention_events = 0
    for line in session.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_type = record.get("event_type")
        if isinstance(event_type, str) and (
            "attention" in event_type or "notification" in event_type
        ):
            attention_events += 1
    return session, attention_events


def looks_like_trust_gate(text: str) -> bool:
    lowered = text.lower()
    return "workspace trust" in lowered or "trust this workspace" in lowered


def looks_like_main_tui(text: str) -> bool:
    lowered = text.lower()
    return "agent:" in lowered and ("build" in lowered or "session" in lowered)


def run_case(
    *,
    label: str,
    enabled: bool,
    binary: Path,
    fixture_root: Path,
    port: int,
    output_dir: Path,
    timeout: float,
    server: FixtureServer,
) -> RunAudit:
    case_root = fixture_root / label
    workspace = case_root / PATH_CANARY
    state_dir = case_root / "state"
    cache_dir = case_root / "cache"
    home_dir = case_root / "home"
    for directory in (workspace, state_dir, cache_dir, home_dir):
        directory.mkdir(parents=True, exist_ok=True)
    config_path = home_dir / "sigil.toml"
    write_config(config_path, workspace, state_dir, cache_dir, port, enabled)
    raw_log = output_dir / f"tui-attention-{label}.bin"
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home_dir),
            "SIGIL_STATE_HOME": str(state_dir),
            "SIGIL_CACHE_HOME": str(cache_dir),
            "SIGIL_ATTENTION_UNUSED_ERROR_CANARY": ERROR_CANARY,
            "TERM": "xterm-256color",
            "TERM_PROGRAM": "attention-terminal-canary-3094",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
    )
    runner = PtyRunner(
        [str(binary), "--config", str(config_path)], workspace, env, raw_log
    )
    with server.lock:
        before_requests = server.request_count
    prompt_start = 0
    try:
        runner.start()
        initial = runner.wait_until(
            lambda text: looks_like_trust_gate(text) or looks_like_main_tui(text),
            timeout,
            f"{label} initial TUI",
        )
        if looks_like_trust_gate(initial):
            runner.send("\r")
            runner.wait_until(looks_like_main_tui, timeout, f"{label} trusted TUI")
        runner.settle()
        prompt_start = len(runner.output)
        runner.type_text(PROMPT_CANARY)
        runner.send("\r")
        runner.wait_until(
            lambda text: REPLY_CANARY in text,
            timeout,
            f"{label} fixture reply",
        )
        runner.settle()
        runner.type_text("/quit")
        runner.send("\r")
        if runner.wait_for_exit(timeout) != 0:
            raise RuntimeError(f"{label} TUI returned a non-zero exit code")
    finally:
        runner.close()

    segment = bytes(runner.output[prompt_start:])
    bell_count = segment.count(b"\x07")
    expected_bells = 1 if enabled else 0
    if bell_count != expected_bells:
        raise RuntimeError(
            f"{label} expected {expected_bells} BEL notification byte(s), found {bell_count}"
        )
    focus_enable_count = bytes(runner.output).count(b"\x1b[?1004h")
    focus_disable_count = bytes(runner.output).count(b"\x1b[?1004l")
    expected_focus_count = 1 if enabled else 0
    if (focus_enable_count, focus_disable_count) != (
        expected_focus_count,
        expected_focus_count,
    ):
        raise RuntimeError(
            f"{label} focus reporting lifecycle was not balanced: "
            f"enable={focus_enable_count} disable={focus_disable_count}"
        )
    for canary in (PROMPT_CANARY, REPLY_CANARY, PATH_CANARY, TOOL_CANARY, ERROR_CANARY, MCP_CANARY):
        if canary.encode("utf-8") in b"\x07" * bell_count:
            raise RuntimeError(f"{label} canary leaked into notification frame: {canary}")

    session_path, notification_event_count = inspect_session(state_dir)
    if notification_event_count != 0:
        raise RuntimeError(f"{label} created durable notification events")
    notification_text_in_state = tree_contains(state_dir, NOTIFICATION_TEXT)
    notification_text_in_cache = tree_contains(cache_dir, NOTIFICATION_TEXT)
    if notification_text_in_state or notification_text_in_cache:
        raise RuntimeError(f"{label} persisted fixed notification payload text")
    with server.lock:
        request_count = server.request_count - before_requests
    if request_count != 1:
        raise RuntimeError(f"{label} expected one provider request, found {request_count}")

    return RunAudit(
        label=label,
        bell_count=bell_count,
        request_count=request_count,
        focus_enable_count=focus_enable_count,
        focus_disable_count=focus_disable_count,
        notification_event_count=notification_event_count,
        notification_text_in_state=notification_text_in_state,
        notification_text_in_cache=notification_text_in_cache,
        session_path=str(session_path) if session_path else None,
        raw_log=str(raw_log),
    )


def main() -> int:
    if os.name != "posix":
        print("attention PTY acceptance requires a POSIX host", file=sys.stderr)
        return 2
    args = parse_args()
    root = repo_root()
    output_dir = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve() if args.binary else build_binary(root)
    fixture_root = Path(tempfile.mkdtemp(prefix="sigil-attention-pty-"))
    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.request_count = 0
    server.lock = threading.Lock()
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    port = int(server.server_address[1])
    report_path = output_dir / "tui-attention-signals-pty-acceptance.json"
    passed = False
    try:
        audits = [
            run_case(
                label="default-off",
                enabled=False,
                binary=binary,
                fixture_root=fixture_root,
                port=port,
                output_dir=output_dir,
                timeout=args.timeout,
                server=server,
            ),
            run_case(
                label="explicit-bell",
                enabled=True,
                binary=binary,
                fixture_root=fixture_root,
                port=port,
                output_dir=output_dir,
                timeout=args.timeout,
                server=server,
            ),
        ]
        report = {
            "status": "passed",
            "binary": str(binary),
            "default_off_zero_notification_bytes": audits[0].bell_count == 0,
            "explicit_bell_exactly_once": audits[1].bell_count == 1,
            "focus_reporting_cleanup_balanced": all(
                audit.focus_enable_count == audit.focus_disable_count for audit in audits
            ),
            "notification_payload_is_fixed_and_canary_free": True,
            "no_durable_notification_events": all(
                audit.notification_event_count == 0 for audit in audits
            ),
            "no_notification_payload_in_state_or_cache": all(
                not audit.notification_text_in_state
                and not audit.notification_text_in_cache
                for audit in audits
            ),
            "runs": [audit.__dict__ for audit in audits],
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {report_path}")
        passed = True
        return 0
    except Exception as error:  # noqa: BLE001 - retain bounded acceptance diagnostics.
        report_path.write_text(
            json.dumps({"status": "failed", "error": str(error)}, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"attention PTY acceptance failed: {error}", file=sys.stderr)
        print(f"fixture {fixture_root}", file=sys.stderr)
        return 1
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        if passed and not args.keep_fixture:
            shutil.rmtree(fixture_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
