#!/usr/bin/env python3
"""Verify Image & Attachment Input V1 through the production Sigil TUI binary.

The harness drives only public terminal input. Supported-provider requests are
captured by one loopback server and checked in memory; evidence stores hashes
and structural facts, never the inline image payload. Unsupported providers
must fail before the loopback server observes a request.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
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
from dataclasses import asdict, dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)
MAX_REQUEST_BYTES = 8 * 1024 * 1024
PATH_CANARY = "image-path-canary-7319"
PROMPT_CANARY = "IMAGE-ATTACHMENT-PROMPT-4821"
FIXTURE_KEY = "image-attachment-fixture-key"
WEBP_1X1 = base64.b64decode("UklGRhoAAABXRUJQVlA4TA4AAAAvAAAAEM1VICIC0f+IBA==")


@dataclass(frozen=True)
class AcceptanceCase:
    label: str
    provider: str
    model: str
    image_kind: str
    mime_type: str
    supported: bool
    clipboard: bool = False
    exercise_delete: bool = False
    exercise_lifecycle: bool = False


CASES = (
    AcceptanceCase(
        "openai_png",
        "openai_responses",
        "gpt-4.1",
        "png",
        "image/png",
        True,
        exercise_delete=True,
        exercise_lifecycle=True,
    ),
    AcceptanceCase(
        "anthropic_jpeg",
        "anthropic",
        "claude-sonnet-4-6",
        "jpeg",
        "image/jpeg",
        True,
    ),
    AcceptanceCase(
        "gemini_webp",
        "gemini",
        "gemini-2.5-pro",
        "webp",
        "image/webp",
        True,
    ),
    AcceptanceCase(
        "openai_clipboard_png",
        "openai_responses",
        "gpt-4.1-mini",
        "png",
        "image/png",
        True,
        clipboard=True,
    ),
    AcceptanceCase(
        "deepseek_rejected",
        "deepseek",
        "deepseek-v4-flash",
        "png",
        "image/png",
        False,
    ),
    AcceptanceCase(
        "compatible_rejected",
        "openai_compat",
        "unknown-vision-model",
        "png",
        "image/png",
        False,
    ),
)


@dataclass
class RecordedRequest:
    path: str
    headers: dict[str, str]
    payload: dict[str, Any]


@dataclass(frozen=True)
class ClipboardSnapshot:
    kind: str
    text: bytes | None = None
    image_path: Path | None = None


@dataclass
class FixtureState:
    requests: list[RecordedRequest] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record(self, request: RecordedRequest) -> int:
        with self.lock:
            self.requests.append(request)
            return len(self.requests)

    def request_count(self) -> int:
        with self.lock:
            return len(self.requests)

    def request_at(self, index: int) -> RecordedRequest:
        with self.lock:
            return self.requests[index]


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

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        try:
            body = self._read_request_body()
            if len(body) > MAX_REQUEST_BYTES:
                raise ValueError("provider request exceeded the acceptance bound")
            payload = json.loads(body.decode("utf-8"))
            if not isinstance(payload, dict):
                raise ValueError("provider request must be a JSON object")
            index = self.fixture.record(
                RecordedRequest(
                    path=self.path,
                    headers={key.lower(): value for key, value in self.headers.items()},
                    payload=payload,
                )
            )
            reply = f"Image attachment fixture response {index}."
            if self.path.endswith("/responses"):
                stream = (
                    "event: response.output_text.delta\n"
                    + f"data: {json.dumps({'delta': reply}, separators=(',', ':'))}\n\n"
                    + "event: response.completed\n"
                    + "data: "
                    + json.dumps(
                        {
                            "response": {
                                "id": f"resp_image_{index}",
                                "status": "completed",
                                "output": [
                                    {
                                        "id": f"msg_image_{index}",
                                        "type": "message",
                                        "role": "assistant",
                                        "content": [
                                            {"type": "output_text", "text": reply}
                                        ],
                                    }
                                ],
                            }
                        },
                        separators=(",", ":"),
                    )
                    + "\n\n"
                )
            elif self.path.endswith("/v1/messages"):
                stream = (
                    "data: "
                    + json.dumps(
                        {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": reply},
                        },
                        separators=(",", ":"),
                    )
                    + "\n\n"
                    + 'data: {"type":"message_stop"}\n\n'
                )
            elif ":streamGenerateContent" in self.path:
                stream = (
                    "data: "
                    + json.dumps(
                        {
                            "candidates": [
                                {
                                    "content": {"parts": [{"text": reply}]},
                                    "finishReason": "STOP",
                                }
                            ]
                        },
                        separators=(",", ":"),
                    )
                    + "\n\ndata: [DONE]\n\n"
                )
            else:
                stream = (
                    "data: "
                    + json.dumps(
                        {
                            "choices": [
                                {
                                    "delta": {"content": reply},
                                    "finish_reason": "stop",
                                }
                            ]
                        },
                        separators=(",", ":"),
                    )
                    + "\n\ndata: [DONE]\n\n"
                )
            encoded = stream.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(encoded)
        except Exception as error:  # noqa: BLE001 - retain bounded diagnostics.
            with self.fixture.lock:
                self.fixture.errors.append(f"{type(error).__name__}: {error}")
            encoded = b'{"error":"fixture request failed"}'
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(encoded)

    def _read_request_body(self) -> bytes:
        if self.headers.get("Transfer-Encoding", "").lower() != "chunked":
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_REQUEST_BYTES:
                raise ValueError("provider request length is outside the acceptance bound")
            return self.rfile.read(length)
        chunks: list[bytes] = []
        total = 0
        while True:
            size_line = self.rfile.readline()
            size = int(size_line.split(b";", 1)[0].strip(), 16)
            if size == 0:
                while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                    pass
                return b"".join(chunks)
            total += size
            if total > MAX_REQUEST_BYTES:
                raise ValueError("chunked provider request exceeded the acceptance bound")
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
        termios.tcsetwinsize(slave_fd, (44, 150))
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
            time.sleep(0.001)

    def paste(self, value: str) -> None:
        self.send(f"\x1b[200~{value}\x1b[201~")

    def rendered(self) -> str:
        without_ansi = ANSI_RE.sub(b"", bytes(self.output))
        without_controls = bytes(
            byte for byte in without_ansi if byte in (9, 10, 13) or byte >= 32
        )
        return without_controls.decode("utf-8", errors="replace")

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

    def settle(self, timeout: float = 2.0) -> None:
        deadline = time.monotonic() + timeout
        quiet_since = time.monotonic()
        size = len(self.output)
        while time.monotonic() < deadline:
            self.read_available(0.05)
            if len(self.output) != size:
                size = len(self.output)
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= 0.25:
                return
        raise TimeoutError("PTY output did not settle")

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
class CaseEvidence:
    label: str
    provider: str
    model: str
    input_route: str
    mime_type: str
    request_count: int
    request_path: str | None
    request_sha256: str | None
    image_sha256: str
    image_bytes: int
    session_sha256: str
    prefix_snapshot_count: int
    export_sha256: str | None
    compaction_previewed: bool
    zero_transport_rejection: bool
    raw_log: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run Image & Attachment Input V1 through a real Sigil TUI binary."
    )
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/release/sigil"),
        help="Production Sigil binary to execute.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/image-attachment-v1-acceptance"),
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument(
        "--skip-clipboard",
        action="store_true",
        help="Skip the system-clipboard case on headless/non-macOS hosts.",
    )
    parser.add_argument("--keep-temp", action="store_true")
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True
    )
    return Path(output.strip()).resolve()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def toml_string(value: Path | str) -> str:
    return str(value).replace("\\", "\\\\").replace('"', '\\"')


def make_images(root: Path, repository: Path) -> dict[str, Path]:
    root.mkdir(parents=True, exist_ok=True)
    png = root / f"{PATH_CANARY}.png"
    jpeg = root / f"{PATH_CANARY}.jpg"
    webp = root / f"{PATH_CANARY}.webp"
    shutil.copyfile(repository / "assets/logo/sigil-mark-staff-glow.png", png)
    sips = shutil.which("sips")
    if sips is None:
        raise RuntimeError("R33.6 JPEG fixture generation currently requires macOS sips")
    subprocess.run(
        [sips, "-s", "format", "jpeg", str(png), "--out", str(jpeg)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    webp.write_bytes(WEBP_1X1)
    return {"png": png, "jpeg": jpeg, "webp": webp}


def provider_config(case: AcceptanceCase, port: int) -> str:
    endpoint = f"http://127.0.0.1:{port}"
    if case.provider == "openai_responses":
        return (
            "[providers.openai_responses]\n"
            f'base_url = "{endpoint}/openai/v1"\n'
            f'api_key = "{FIXTURE_KEY}"\n'
        )
    if case.provider == "anthropic":
        return (
            "[providers.anthropic]\n"
            f'base_url = "{endpoint}/anthropic"\n'
            f'api_key = "{FIXTURE_KEY}"\n'
            'anthropic_version = "2023-06-01"\n'
            "max_tokens = 512\n"
        )
    if case.provider == "gemini":
        return (
            "[providers.gemini]\n"
            f'base_url = "{endpoint}/gemini"\n'
            f'api_key = "{FIXTURE_KEY}"\n'
        )
    if case.provider == "openai_compat":
        return (
            "[providers.openai_compat]\n"
            f'base_url = "{endpoint}/compatible/v1"\n'
            f'api_key = "{FIXTURE_KEY}"\n'
        )
    if case.provider == "deepseek":
        return (
            "[providers.deepseek]\n"
            f'base_url = "{endpoint}/deepseek"\n'
            f'beta_base_url = "{endpoint}/deepseek/beta"\n'
            f'anthropic_base_url = "{endpoint}/deepseek/anthropic"\n'
            'fim_model = "deepseek-v4-pro"\n'
            f'api_key = "{FIXTURE_KEY}"\n'
            'strict_tools_mode = "off"\n'
        )
    raise AssertionError(f"unsupported acceptance provider: {case.provider}")


def write_config(
    path: Path,
    case: AcceptanceCase,
    workspace: Path,
    state_root: Path,
    cache_root: Path,
    port: int,
) -> None:
    path.write_text(
        "\n".join(
            (
                "[workspace]",
                f'root = "{toml_string(workspace)}"',
                "",
                "[storage]",
                f'state_root = "{toml_string(state_root)}"',
                f'cache_root = "{toml_string(cache_root)}"',
                "",
                "[session]",
                f'log_dir = "{toml_string(state_root / "sessions")}"',
                "",
                "[agent]",
                f'provider = "{case.provider}"',
                f'model = "{case.model}"',
                "tool_timeout_secs = 5",
                "",
                "[model_request]",
                "request_timeout_secs = 10",
                "stream_idle_timeout_secs = 10",
                "",
                "[terminal]",
                'keyboard_enhancement = "off"',
                "mouse_capture = false",
                "osc52_clipboard = false",
                "",
                "[compaction]",
                "enabled = true",
                "fallback_context_window_tokens = 128000",
                "tail_messages = 1",
                "",
                provider_config(case, port),
            )
        ),
        encoding="utf-8",
    )


def clean_env(home: Path, state_root: Path, cache_root: Path) -> dict[str, str]:
    env = os.environ.copy()
    for key in (
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "SIGIL_API_KEY",
        "SIGIL_OPENAI_RESPONSES_API_KEY",
        "SIGIL_OPENAI_RESPONSES_BASE_URL",
        "SIGIL_OPENAI_COMPATIBLE_API_KEY",
        "SIGIL_OPENAI_COMPATIBLE_BASE_URL",
        "SIGIL_ANTHROPIC_API_KEY",
        "SIGIL_ANTHROPIC_BASE_URL",
        "SIGIL_GEMINI_API_KEY",
        "SIGIL_GEMINI_BASE_URL",
    ):
        env.pop(key, None)
    env.update(
        {
            "HOME": str(home),
            "SIGIL_STATE_HOME": str(state_root),
            "SIGIL_CACHE_HOME": str(cache_root),
            "TERM": "xterm-256color",
            "TERM_PROGRAM": "sigil-image-acceptance",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
    )
    return env


def looks_like_trust_gate(text: str) -> bool:
    lowered = text.lower()
    return "workspace trust" in lowered or "trust this workspace" in lowered


def looks_like_main_tui(text: str) -> bool:
    lowered = text.lower()
    return "agent:" in lowered and ("build" in lowered or "session" in lowered)


def start_tui(runner: PtyRunner, timeout: float, label: str) -> None:
    runner.start()
    initial = runner.wait_until(
        lambda text: looks_like_trust_gate(text) or looks_like_main_tui(text),
        timeout,
        f"{label} initial TUI",
    )
    if looks_like_trust_gate(initial):
        runner.send("\r")
        runner.wait_until(looks_like_main_tui, timeout, f"{label} trusted TUI")


def attach_image(
    runner: PtyRunner,
    case: AcceptanceCase,
    image_path: Path,
    timeout: float,
) -> None:
    before = len(runner.output)
    if case.clipboard:
        runner.send("\x16")
    else:
        runner.paste(str(image_path))
    extension = "JPG" if case.image_kind == "jpeg" else case.image_kind.upper()
    runner.wait_until(
        lambda text: "attached image 1 of 4" in text
        and f"image 1 · {extension}" in text,
        timeout,
        f"{case.label} attachment chip",
    )
    segment = bytes(runner.output[before:])
    if b"data:image" in segment or base64.b64encode(image_path.read_bytes()) in segment:
        raise AssertionError(f"{case.label} rendered raw image data in the TUI")


def send_prompt_and_wait(
    runner: PtyRunner,
    fixture: FixtureState,
    prompt: str,
    timeout: float,
    label: str,
) -> RecordedRequest:
    before = fixture.request_count()
    runner.type_text(prompt)
    runner.send("\r")
    expected_reply = f"Image attachment fixture response {before + 1}."
    runner.wait_until(
        lambda text: expected_reply in text,
        timeout,
        f"{label} fixture response",
    )
    runner.settle()
    deadline = time.monotonic() + timeout
    while fixture.request_count() != before + 1 and time.monotonic() < deadline:
        time.sleep(0.02)
    if fixture.request_count() != before + 1:
        raise AssertionError(f"{label} did not produce exactly one provider request")
    return fixture.request_at(before)


def image_wire(request: RecordedRequest, provider: str) -> tuple[str, str]:
    if provider == "openai_responses":
        blocks = [
            block
            for item in request.payload.get("input", [])
            if isinstance(item, dict)
            for block in item.get("content", [])
            if isinstance(block, dict) and block.get("type") == "input_image"
        ]
        if len(blocks) != 1:
            raise AssertionError(f"expected one OpenAI input_image block, found {len(blocks)}")
        url = blocks[0].get("image_url")
        if not isinstance(url, str) or not url.startswith("data:image/"):
            raise AssertionError("OpenAI image block did not contain a data URL")
        header, data = url.split(",", 1)
        return header.removeprefix("data:").removesuffix(";base64"), data
    if provider == "anthropic":
        blocks = [
            block
            for message in request.payload.get("messages", [])
            if isinstance(message, dict)
            for block in message.get("content", [])
            if isinstance(block, dict) and block.get("type") == "image"
        ]
        if len(blocks) != 1:
            raise AssertionError(f"expected one Anthropic image block, found {len(blocks)}")
        source = blocks[0].get("source")
        if not isinstance(source, dict) or source.get("type") != "base64":
            raise AssertionError("Anthropic image block did not contain Base64 source")
        return str(source.get("media_type")), str(source.get("data"))
    if provider == "gemini":
        blocks = [
            part["inline_data"]
            for content in request.payload.get("contents", [])
            if isinstance(content, dict)
            for part in content.get("parts", [])
            if isinstance(part, dict) and isinstance(part.get("inline_data"), dict)
        ]
        if len(blocks) != 1:
            raise AssertionError(f"expected one Gemini inline_data block, found {len(blocks)}")
        return str(blocks[0].get("mime_type")), str(blocks[0].get("data"))
    raise AssertionError(f"provider has no supported image wire: {provider}")


def newest_session(state_root: Path) -> Path:
    sessions = sorted(
        (state_root / "sessions").glob("session-*.jsonl"),
        key=lambda path: path.stat().st_mtime_ns,
        reverse=True,
    )
    if not sessions:
        raise AssertionError("acceptance run did not create a session JSONL")
    return sessions[0]


def prefix_snapshot_count(session_text: str) -> int:
    count = 0
    for line in session_text.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "prefix_snapshot_captured" in json.dumps(record, separators=(",", ":")):
            count += 1
    return count


def find_export(state_root: Path) -> Path:
    exports = [
        path
        for path in state_root.rglob("*.json")
        if "export" in path.name or "export" in str(path.parent)
    ]
    exports.sort(key=lambda path: path.stat().st_mtime_ns, reverse=True)
    if not exports:
        raise AssertionError("TUI lifecycle acceptance did not create a safe export")
    return exports[0]


def assert_safe_projection(
    label: str,
    text: str,
    image_bytes: bytes,
    require_placeholder: bool = True,
) -> None:
    forbidden = (
        PATH_CANARY,
        base64.b64encode(image_bytes).decode("ascii"),
        "data:image/",
        '"resolved_bytes"',
    )
    leaked = [needle for needle in forbidden if needle and needle in text]
    if leaked:
        raise AssertionError(f"{label} leaked non-durable image material: {leaked}")
    if require_placeholder and "Image attachment 1" not in text:
        raise AssertionError(f"{label} did not retain the durable image placeholder")


def exercise_compaction_preview(runner: PtyRunner, timeout: float) -> bool:
    runner.type_text("Second turn for a foldable compaction boundary.")
    runner.send("\r")
    runner.wait_until(
        lambda text: "Image attachment fixture response 2." in text,
        timeout,
        "second provider response",
    )
    runner.settle()
    runner.type_text("/compact")
    runner.send("\r")
    runner.wait_until(
        lambda text: "Review — no session data has been changed yet." in text,
        timeout,
        "real compaction preview",
    )
    runner.send("\x1b")
    runner.settle()
    return True


def exercise_export(runner: PtyRunner, timeout: float) -> None:
    runner.type_text("/resume")
    runner.wait_until(
        lambda text: "Ctrl-O actions" in text,
        timeout,
        "resume selector",
    )
    runner.send("\x0f")
    runner.wait_until(
        lambda text: "Enter resume  F fork  E export" in text,
        timeout,
        "session actions",
    )
    runner.send("e")
    runner.wait_until(
        lambda text: "exported " in text and " safe message(s)" in text,
        timeout,
        "safe session export",
    )
    runner.send("\x1b")
    runner.settle()


def quit_tui(runner: PtyRunner, timeout: float) -> None:
    for _ in range(2):
        runner.send("\x1b")
        runner.settle()
    runner.type_text("/quit")
    runner.send("\r")
    if runner.wait_for_exit(timeout) != 0:
        raise RuntimeError("TUI returned a non-zero exit code")


def run_osascript(lines: tuple[str, ...], *arguments: str) -> None:
    command = ["osascript"]
    for line in lines:
        command.extend(("-e", line))
    command.extend(("--", *arguments))
    subprocess.run(
        command,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def capture_macos_clipboard_image(
    destination: Path, apple_event_class: str
) -> None:
    run_osascript(
        (
            "on run argv",
            "set outputPath to item 1 of argv",
            f"set clipboardData to the clipboard as «class {apple_event_class}»",
            "set outputFile to open for access POSIX file outputPath with write permission",
            "try",
            "set eof outputFile to 0",
            "write clipboardData to outputFile",
            "on error errorMessage number errorNumber",
            "close access outputFile",
            "error errorMessage number errorNumber",
            "end try",
            "close access outputFile",
            "end run",
        ),
        str(destination),
    )


def restore_macos_clipboard_image(source: Path, apple_event_class: str) -> None:
    run_osascript(
        (
            "on run argv",
            "set inputPath to item 1 of argv",
            f"set clipboardData to read POSIX file inputPath as «class {apple_event_class}»",
            "set the clipboard to clipboardData",
            "end run",
        ),
        str(source),
    )


def set_macos_clipboard_png(path: Path, snapshot_root: Path) -> ClipboardSnapshot:
    if sys.platform != "darwin":
        raise RuntimeError("system clipboard acceptance currently requires macOS")
    snapshot_root.mkdir(parents=True, exist_ok=True)
    info = subprocess.run(
        ["osascript", "-e", "clipboard info"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    ).stdout.lower()
    snapshot: ClipboardSnapshot
    if "class pngf" in info:
        image_path = snapshot_root / "original-clipboard.png"
        capture_macos_clipboard_image(image_path, "PNGf")
        snapshot = ClipboardSnapshot(kind="png", image_path=image_path)
    elif "tiff picture" in info or "class tiff" in info:
        image_path = snapshot_root / "original-clipboard.tiff"
        capture_macos_clipboard_image(image_path, "TIFF")
        snapshot = ClipboardSnapshot(kind="tiff", image_path=image_path)
    elif "unicode text" in info or "string" in info or "class utf8" in info:
        pasted = subprocess.run(
            ["pbpaste"], check=False, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        if pasted.returncode != 0:
            raise RuntimeError("system clipboard text could not be snapshotted")
        snapshot = ClipboardSnapshot(kind="text", text=pasted.stdout)
    else:
        raise RuntimeError(
            "system clipboard cannot be restored safely; rerun with --skip-clipboard"
        )
    restore_macos_clipboard_image(path, "PNGf")
    return snapshot


def restore_macos_clipboard(snapshot: ClipboardSnapshot) -> None:
    if snapshot.kind == "text":
        subprocess.run(
            ["pbcopy"], input=snapshot.text or b"", check=True, stderr=subprocess.DEVNULL
        )
        return
    if snapshot.image_path is None:
        raise RuntimeError("clipboard image snapshot is missing")
    restore_macos_clipboard_image(
        snapshot.image_path,
        "PNGf" if snapshot.kind == "png" else "TIFF",
    )


def run_case(
    *,
    case: AcceptanceCase,
    binary: Path,
    fixture_root: Path,
    images: dict[str, Path],
    port: int,
    fixture: FixtureState,
    output_dir: Path,
    timeout: float,
) -> CaseEvidence:
    case_root = fixture_root / case.label
    workspace = case_root / "workspace"
    state_root = case_root / "state"
    cache_root = case_root / "cache"
    home = case_root / "home"
    for directory in (workspace, state_root, cache_root, home):
        directory.mkdir(parents=True, exist_ok=True)
    image_path = workspace / images[case.image_kind].name
    shutil.copyfile(images[case.image_kind], image_path)
    config_path = home / "sigil.toml"
    write_config(config_path, case, workspace, state_root, cache_root, port)
    raw_log = output_dir / f"{case.label}.bin"
    runner = PtyRunner(
        [str(binary), "--config", str(config_path)],
        workspace,
        clean_env(home, state_root, cache_root),
        raw_log,
    )
    before_requests = fixture.request_count()
    request: RecordedRequest | None = None
    wire_bytes = image_path.read_bytes()
    export_path: Path | None = None
    compact_previewed = False
    try:
        start_tui(runner, timeout, case.label)
        attach_image(runner, case, image_path, timeout)
        if case.exercise_delete:
            runner.send("\x7f")
            runner.wait_until(
                lambda text: "image attachment removed" in text,
                timeout,
                "attachment deletion",
            )
            attach_image(runner, case, image_path, timeout)
        if case.supported:
            request = send_prompt_and_wait(
                runner,
                fixture,
                f"{PROMPT_CANARY}-{case.label}",
                timeout,
                case.label,
            )
            mime_type, wire_base64 = image_wire(request, case.provider)
            if mime_type != case.mime_type:
                raise AssertionError(
                    f"{case.label} expected {case.mime_type}, observed {mime_type}"
                )
            wire_bytes = base64.b64decode(wire_base64, validate=True)
            if case.clipboard:
                if not wire_bytes.startswith(b"\x89PNG\r\n\x1a\n"):
                    raise AssertionError("clipboard request did not contain a PNG")
            elif wire_bytes != image_path.read_bytes():
                raise AssertionError(f"{case.label} provider bytes changed during materialization")
            if case.exercise_lifecycle:
                compact_previewed = exercise_compaction_preview(runner, timeout)
                exercise_export(runner, timeout)
                export_path = find_export(state_root)
        else:
            runner.type_text(f"{PROMPT_CANARY}-{case.label}")
            runner.send("\r")
            runner.wait_until(
                lambda text: "does not support image input" in text,
                timeout,
                f"{case.label} fail-closed notice",
            )
            time.sleep(0.2)
            if fixture.request_count() != before_requests:
                raise AssertionError(f"{case.label} reached provider transport")
        quit_tui(runner, timeout)
    finally:
        runner.close()

    observed_requests = fixture.request_count() - before_requests
    expected_requests = 2 if case.exercise_lifecycle else (1 if case.supported else 0)
    if observed_requests != expected_requests:
        raise AssertionError(
            f"{case.label} expected {expected_requests} request(s), observed {observed_requests}"
        )
    session_path = newest_session(state_root)
    session_bytes = session_path.read_bytes()
    session_text = session_bytes.decode("utf-8")
    assert_safe_projection(f"{case.label} session", session_text, wire_bytes)
    prefixes = prefix_snapshot_count(session_text)
    if case.supported and prefixes == 0:
        raise AssertionError(f"{case.label} session did not retain a PrefixSnapshot")
    export_sha256 = None
    if export_path is not None:
        export_bytes = export_path.read_bytes()
        assert_safe_projection("safe session export", export_bytes.decode("utf-8"), wire_bytes)
        export_sha256 = sha256_bytes(export_bytes)
    request_sha256 = None
    request_path = None
    if request is not None:
        request_bytes = json.dumps(
            request.payload, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        request_sha256 = sha256_bytes(request_bytes)
        request_path = request.path
    return CaseEvidence(
        label=case.label,
        provider=case.provider,
        model=case.model,
        input_route="clipboard" if case.clipboard else "path_paste",
        mime_type=case.mime_type,
        request_count=observed_requests,
        request_path=request_path,
        request_sha256=request_sha256,
        image_sha256=sha256_bytes(wire_bytes),
        image_bytes=len(wire_bytes),
        session_sha256=sha256_bytes(session_bytes),
        prefix_snapshot_count=prefixes,
        export_sha256=export_sha256,
        compaction_previewed=compact_previewed,
        zero_transport_rejection=not case.supported and observed_requests == 0,
        raw_log=str(raw_log),
    )


def main() -> int:
    args = parse_args()
    root = repo_root()
    binary = args.binary
    if not binary.is_absolute():
        binary = (root / binary).resolve()
    if not binary.is_file():
        raise FileNotFoundError(f"Sigil binary is missing: {binary}")
    output_dir = args.output_dir
    if not output_dir.is_absolute():
        output_dir = root / output_dir
    output_dir.mkdir(parents=True, exist_ok=True)

    temporary: tempfile.TemporaryDirectory[str] | None = None
    if args.keep_temp:
        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-image-acceptance-"))
    else:
        temporary = tempfile.TemporaryDirectory(prefix="sigil-image-acceptance-")
        fixture_root = Path(temporary.name)
    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.fixture = FixtureState()
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    port = int(server.server_address[1])
    evidence: list[CaseEvidence] = []
    try:
        images = make_images(fixture_root / "images", root)
        cases = tuple(
            case for case in CASES if not (case.clipboard and args.skip_clipboard)
        )
        for case in cases:
            clipboard_snapshot: ClipboardSnapshot | None = None
            try:
                if case.clipboard:
                    clipboard_snapshot = set_macos_clipboard_png(
                        images["png"], fixture_root / "clipboard-snapshot"
                    )
                evidence.append(
                    run_case(
                        case=case,
                        binary=binary,
                        fixture_root=fixture_root,
                        images=images,
                        port=port,
                        fixture=server.fixture,
                        output_dir=output_dir,
                        timeout=args.timeout,
                    )
                )
            finally:
                if clipboard_snapshot is not None:
                    restore_macos_clipboard(clipboard_snapshot)
        if server.fixture.errors:
            raise AssertionError(f"loopback fixture errors: {server.fixture.errors}")
        report = {
            "schema_version": 1,
            "binary": str(binary),
            "binary_sha256": sha256_bytes(binary.read_bytes()),
            "clipboard_exercised": not args.skip_clipboard,
            "cases": [asdict(item) for item in evidence],
        }
        report_path = output_dir / "report.json"
        report_path.write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(
            "Image Attachment V1 acceptance passed: "
            f"{len(evidence)} case(s), {server.fixture.request_count()} loopback request(s)"
        )
        print(f"Evidence: {report_path}")
        return 0
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        if args.keep_temp:
            print(f"Temporary fixture: {fixture_root}")
        elif temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:  # noqa: BLE001 - bounded acceptance failure report.
        print(f"image attachment acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
