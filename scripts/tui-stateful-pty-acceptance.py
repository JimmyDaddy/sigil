#!/usr/bin/env python3
"""Exercise compact/resume/checkpoint/focus/reply boundaries in one real TUI flow."""

from __future__ import annotations

import argparse
import codecs
import dataclasses
import datetime as dt
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import ssl
import stat
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unicodedata
from typing import Callable


SCHEMA_VERSION = 1
MODEL_NAME = "deepseek-v4-flash"
TOKENIZER_SNAPSHOT = "60d8d70770c6776ff598c94bb586a859a38244f1"
TOKENIZER_SHA256 = "8f9f37ca37fdc4f5fd36d5cf4d3b0e8392edb4e894fd10cc0d70b4957c8633cf"
FINAL_CANARY = "STATEFUL-FINAL-REPLY-CANARY-6419"
EDIT_PROMPT = "STATEFUL-EDIT-TURN-CANARY-3074"
ORIGINAL_CONTENT = "stateful checkpoint original\n"
MUTATED_CONTENT = "stateful checkpoint mutated\n"
EDIT_PATH = "stateful-checkpoint.txt"
TOOL_CALL_ID = "stateful-write-call"
CJK_PATH = "stateful-cjk-boundary.txt"
CJK_CONTENT = "UTF-8 boundary regression 到"
CJK_TOOL_CALL_ID = "stateful-cjk-read-call"
PLAN_TOOL_CALL_ID = "stateful-plan-draft-call"
PLAN_PROMPT = "review the stateful TUI rendering path without modifying files"
PLAN_SUMMARY_CANARY = "STATEFUL-PLAN-PREVIEW-CANARY-5821"
PLAN_MODE_MARKER = "Plan mode is active for this turn."
PLAN_RESEARCH_MARKER = "You are running a read-only plan review for the current request."
PLAN_FINALIZER_MARKER = (
    "The research phase is complete and this is the single submit-only finalization turn."
)
QUEUED_FOLLOW_UP_PROMPT = "STATEFUL-QUEUED-FOLLOW-UP-PROMPT-7346"
QUEUED_FOLLOW_UP_REPLY = "STATEFUL-QUEUED-FOLLOW-UP-REPLY-8462"
HISTORY_HEAD_CANARY_PREFIX = "STATEFUL-HISTORY-HEAD-"
PLAN_DRAFT_ARGUMENTS = json.dumps(
    {
        "schema_version": 2,
        "summary": PLAN_SUMMARY_CANARY,
        "steps": [
            {
                "step_id": "inspect_tui_rendering",
                "title": "Inspect stateful TUI rendering",
                "role": "subagent_read",
                "mode": "read",
                "isolation": "shared_read_only",
                "target_paths": [EDIT_PATH],
            }
        ],
        "target_paths": [EDIT_PATH],
        "suggested_checks": [],
        "risk": None,
        "notes": [],
    },
    separators=(",", ":"),
)
PLAN_DRAFT_TEXT = f"```sigil-plan-v2\n{PLAN_DRAFT_ARGUMENTS}\n```"


def history_head_canary(request_index: int) -> str:
    return f"{HISTORY_HEAD_CANARY_PREFIX}{request_index}"


def cache_usage_for_request(request_index: int) -> dict[str, int]:
    """Return a deterministic cold-then-warm DeepSeek cache observation."""
    hit_tokens = 0 if request_index == 1 else 50_000
    miss_tokens = 500_000 if request_index == 1 else 450_000
    prompt_tokens = hit_tokens + miss_tokens
    return {
        "prompt_tokens": prompt_tokens,
        "completion_tokens": 1,
        "total_tokens": prompt_tokens + 1,
        "prompt_cache_hit_tokens": hit_tokens,
        "prompt_cache_miss_tokens": miss_tokens,
    }


def stateful_history_response_body(request_index: int) -> str:
    suffix = f"STATEFUL-HISTORY-{request_index}"
    if request_index == 2:
        suffix = f"{suffix} {QUEUED_FOLLOW_UP_REPLY}"
    # Ctrl-Home renders the beginning of the oldest long response. Keep a dedicated marker at
    # that boundary; the suffix remains useful for durable session de-duplication checks.
    return f"{history_head_canary(request_index)} " + ("verified-history " * 4000) + suffix


COMMIT_PATTERN = re.compile(r"^[0-9a-f]{12,40}$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
CURSOR_POSITION_QUERY = b"\x1b[6n"
CURSOR_POSITION_RESPONSE = b"\x1b[1;1R"
ENTER_ALTERNATE_SCREEN = b"\x1b[?1049h"
LEAVE_ALTERNATE_SCREEN = b"\x1b[?1049l"
ERASE_SCROLLBACK = b"\x1b[3J"
CLEAR_SCREEN = b"\x1b[2J"
CURSOR_HOME = b"\x1b[1;1H"
# Use Crossterm's complete CSI-u encoding for Escape. Unlike a lone ESC byte, it cannot remain
# buffered as the prefix of a later key while the PTY is being resized or redrawn.
ESCAPE_KEY_SEQUENCE = b"\x1b[27u"
# XTerm-compatible modified Home/End sequences decoded by Crossterm as Ctrl-Home/Ctrl-End.
CTRL_HOME_KEY_SEQUENCE = b"\x1b[1;5H"
CTRL_END_KEY_SEQUENCE = b"\x1b[1;5F"
# SGR mouse coordinates are one-based. Row 10 is inside the live transcript for the fixed
# 140x42 acceptance viewport, so this exercises application-owned wheel scrolling.
LIVE_PANEL_SCROLL_UP_SEQUENCE = b"\x1b[<64;10;10M"
SINGLE_LINE_SCROLL_REGION_PATTERN = re.compile(rb"\x1b\[(\d+);\1r\x1b\[\d+S\x1b\[r")
DEFAULT_PTY_ROWS = 42
DEFAULT_PTY_COLS = 140
MACH_O_MAGICS = {
    b"\xca\xfe\xba\xbe",
    b"\xca\xfe\xba\xbf",
    b"\xbe\xba\xfe\xca",
    b"\xbf\xba\xfe\xca",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xfe\xed\xfa\xce",
    b"\xfe\xed\xfa\xcf",
}
SAFE_ENV_NAMES = {
    "COLORTERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "PATH",
    "SHELL",
    "TERM",
    "TERMINFO",
    "TERMINFO_DIRS",
    "USER",
}


class AcceptanceError(RuntimeError):
    """Raised when admission or a stateful contract check fails."""


@dataclasses.dataclass(frozen=True)
class BinaryIdentity:
    label: str
    sha256: str
    version: str
    commit: str
    target: str
    profile: str

    def as_dict(self) -> dict[str, str]:
        return dataclasses.asdict(self)


@dataclasses.dataclass(frozen=True)
class SessionAudit:
    path: Path
    session_id: str
    event_counts: dict[str, int]
    final_canary_count: int
    failed_run_count: int


@dataclasses.dataclass(frozen=True)
class TerminalResize:
    output_offset: int
    rows: int
    cols: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run Sigil's combined stateful real-PTY alpha acceptance.",
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--tokenizer-json", type=Path, required=True)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-commit")
    parser.add_argument("--expected-binary-sha256")
    parser.add_argument(
        "--expected-tokenizer-sha256",
        default=TOKENIZER_SHA256,
        help="Checksum pin for tokenizer.json.",
    )
    parser.add_argument(
        "--tokenizer-snapshot",
        default=TOKENIZER_SNAPSHOT,
        help="Installed provider-profile snapshot directory name.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-stateful-acceptance"),
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=150.0,
        help="Overall campaign deadline in seconds; individual waits are capped at 30 seconds.",
    )
    parser.add_argument("--keep-fixture", action="store_true")
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True)
    return Path(output.strip()).resolve()


class CampaignDeadline:
    def __init__(self, timeout: float) -> None:
        if timeout <= 0:
            raise AcceptanceError("timeout must be greater than zero")
        self._deadline = time.monotonic() + timeout

    def remaining(self, step_cap: float = 30.0) -> float:
        remaining = self._deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("stateful campaign exceeded its overall deadline")
        return min(step_cap, remaining)


def raw_artifact_policy(root: Path, output_dir: Path) -> str:
    try:
        output_dir.relative_to(root)
    except ValueError:
        return "local_only_under_selected_output"
    completed = subprocess.run(
        ["git", "check-ignore", "-q", "--", str(output_dir)],
        cwd=root,
        check=False,
        timeout=10,
    )
    if completed.returncode == 0:
        return "local_only_under_git_ignored_output"
    if completed.returncode == 1:
        raise AcceptanceError("output directory inside the repository must be git-ignored")
    raise AcceptanceError("failed to verify the repository output ignore policy")


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def native_executable_format(path: Path) -> str | None:
    with path.open("rb") as executable:
        header = executable.read(64)
        if header.startswith(b"\x7fELF"):
            return "elf"
        if header[:4] in MACH_O_MAGICS:
            return "mach-o"
        if len(header) >= 64 and header.startswith(b"MZ"):
            executable.seek(int.from_bytes(header[60:64], byteorder="little"))
            if executable.read(4) == b"PE\x00\x00":
                return "pe"
    return None


def identity_environment(source: dict[str, str]) -> dict[str, str]:
    return {name: value for name, value in source.items() if name in SAFE_ENV_NAMES}


def parse_version_output(output: str, *, label: str, digest: str) -> BinaryIdentity:
    fields: dict[str, str] = {}
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if not lines or not lines[0].startswith("sigil "):
        raise AcceptanceError("binary --version output is missing the sigil version line")
    fields["version"] = lines[0].removeprefix("sigil ").strip()
    for line in lines[1:]:
        key, separator, value = line.partition(":")
        if separator:
            fields[key.strip()] = value.strip()
    missing = [key for key in ("version", "commit", "target", "profile") if not fields.get(key)]
    if missing:
        raise AcceptanceError(f"binary --version output is missing fields: {', '.join(missing)}")
    commit = fields["commit"].lower()
    if not COMMIT_PATTERN.fullmatch(commit):
        raise AcceptanceError("binary build commit is not a supported hexadecimal id")
    return BinaryIdentity(label, digest, fields["version"], commit, fields["target"], fields["profile"])


def inspect_binary(path: Path, *, timeout: float = 15.0) -> tuple[Path, BinaryIdentity]:
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as error:
        raise AcceptanceError("binary does not exist") from error
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise AcceptanceError("binary must be an executable regular file")
    if native_executable_format(resolved) is None:
        raise AcceptanceError("binary must be a standalone Mach-O, ELF, or PE executable")
    digest = sha256_file(resolved)
    completed = subprocess.run(
        [str(resolved), "--version"],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
        env=identity_environment(os.environ),
    )
    if completed.returncode != 0:
        raise AcceptanceError("binary --version failed")
    return resolved, parse_version_output(completed.stdout, label=resolved.name, digest=digest)


def assert_expected_identity(identity: BinaryIdentity, args: argparse.Namespace) -> None:
    if args.expected_version is not None and identity.version != args.expected_version:
        raise AcceptanceError("binary version mismatch")
    if args.expected_commit is not None:
        expected = args.expected_commit.lower()
        if not COMMIT_PATTERN.fullmatch(expected):
            raise AcceptanceError("expected commit must be 12 to 40 hexadecimal characters")
        if not expected.startswith(identity.commit) and not identity.commit.startswith(expected):
            raise AcceptanceError("binary commit mismatch")
    if args.expected_binary_sha256 is not None:
        expected_sha = args.expected_binary_sha256.lower()
        if not SHA256_PATTERN.fullmatch(expected_sha):
            raise AcceptanceError("expected binary SHA-256 must be 64 hexadecimal characters")
        if identity.sha256 != expected_sha:
            raise AcceptanceError("binary SHA-256 mismatch")


def freeze_binary(source: Path, root: Path, expected: BinaryIdentity) -> Path:
    frozen_dir = root / "frozen-bin"
    frozen_dir.mkdir(mode=0o700)
    destination = frozen_dir / ("sigil.exe" if source.suffix.lower() == ".exe" else "sigil")
    shutil.copyfile(source, destination)
    destination.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    if sha256_file(destination) != expected.sha256:
        raise AcceptanceError("frozen binary checksum changed during copy")
    if native_executable_format(destination) is None:
        raise AcceptanceError("frozen copy lost its native executable format")
    return destination


def install_tokenizer(
    source: Path,
    cache_root: Path,
    *,
    snapshot: str,
    expected_sha256: str,
) -> dict[str, str]:
    try:
        resolved = source.expanduser().resolve(strict=True)
    except OSError as error:
        raise AcceptanceError("tokenizer.json does not exist") from error
    expected_sha256 = expected_sha256.lower()
    if not SHA256_PATTERN.fullmatch(expected_sha256):
        raise AcceptanceError("expected tokenizer SHA-256 must be 64 hexadecimal characters")
    if not re.fullmatch(r"[0-9a-f]{40}", snapshot):
        raise AcceptanceError("tokenizer snapshot must be a 40-character lowercase hex id")
    if not resolved.is_file() or resolved.name != "tokenizer.json":
        raise AcceptanceError("tokenizer source must be a tokenizer.json regular file")
    actual = sha256_file(resolved)
    if actual != expected_sha256:
        raise AcceptanceError("tokenizer SHA-256 mismatch")
    destination = cache_root / "provider-profiles" / MODEL_NAME / snapshot / "tokenizer.json"
    destination.parent.mkdir(parents=True)
    shutil.copyfile(resolved, destination)
    if sha256_file(destination) != expected_sha256:
        raise AcceptanceError("installed tokenizer checksum changed during copy")
    return {"model": MODEL_NAME, "snapshot": snapshot, "sha256": expected_sha256}


def advertises_tool(payload: object, name: str) -> bool:
    request = payload if isinstance(payload, dict) else {}
    tools = request.get("tools")
    if not isinstance(tools, list):
        return False
    return any(
        isinstance(tool, dict)
        and isinstance(tool.get("function"), dict)
        and tool["function"].get("name") == name
        for tool in tools
    )


def is_plan_mode_request(payload: object) -> bool:
    request = payload if isinstance(payload, dict) else {}
    # The submit-only finalizer is capability-bound even when its prompt wording evolves. The
    # research turn has no submit tool, so retain explicit markers for both the current durable
    # workbench and the pre-workbench `/plan` protocol used by older release binaries.
    if advertises_tool(payload, "submit_plan_draft"):
        return True
    messages = request.get("messages")
    if not isinstance(messages, list):
        return False
    return any(
        isinstance(message, dict)
        and isinstance(message.get("content"), str)
        and any(
            marker in message["content"]
            for marker in (
                PLAN_RESEARCH_MARKER,
                PLAN_FINALIZER_MARKER,
                PLAN_MODE_MARKER,
            )
        )
        for message in messages
    )


@dataclasses.dataclass
class FixtureState:
    cache_usage_enabled: bool = False
    provider_requests: list[dict[str, bool]] = dataclasses.field(default_factory=list)
    protocol_errors: list[str] = dataclasses.field(default_factory=list)
    semantic_summary_requests: int = 0
    semantic_title_requests: int = 0
    plan_requests: int = 0
    response_release_events: dict[int, threading.Event] = dataclasses.field(
        default_factory=lambda: {1: threading.Event(), 3: threading.Event()}
    )
    lock: threading.Lock = dataclasses.field(default_factory=threading.Lock)

    def record_request(self, payload: object) -> int:
        request = payload if isinstance(payload, dict) else {}
        messages = request.get("messages")
        message_items = messages if isinstance(messages, list) else []
        has_tool = advertises_tool(payload, "write_file")
        has_read_file = advertises_tool(payload, "read_file")
        has_result = any(
            isinstance(message, dict)
            and message.get("role") == "tool"
            and message.get("tool_call_id") == TOOL_CALL_ID
            for message in message_items
        )
        has_cjk_result = any(
            isinstance(message, dict)
            and message.get("role") == "tool"
            and message.get("tool_call_id") == CJK_TOOL_CALL_ID
            for message in message_items
        )
        has_queued_follow_up = any(
            isinstance(message, dict)
            and message.get("role") == "user"
            and isinstance(message.get("content"), str)
            and QUEUED_FOLLOW_UP_PROMPT in message["content"]
            for message in message_items
        )
        with self.lock:
            self.provider_requests.append(
                {
                    "has_write_file": has_tool,
                    "has_read_file": has_read_file,
                    "has_tool_result": has_result,
                    "has_cjk_tool_result": has_cjk_result,
                    "has_queued_follow_up": has_queued_follow_up,
                }
            )
            return len(self.provider_requests)

    def record_plan_request(self) -> None:
        with self.lock:
            self.plan_requests += 1

    def wait_for_response_release(self, request_index: int) -> None:
        release = self.response_release_events.get(request_index)
        if release is not None and not release.wait(timeout=30):
            raise TimeoutError(f"conversation response {request_index} was not released")

    def release_response(self, request_index: int) -> None:
        release = self.response_release_events.get(request_index)
        if release is None:
            raise AcceptanceError(f"conversation response {request_index} is not gated")
        release.set()

    def record_error(self, error: Exception) -> None:
        with self.lock:
            self.protocol_errors.append(type(error).__name__)

    def record_semantic_summary(self) -> None:
        with self.lock:
            self.semantic_summary_requests += 1

    def record_semantic_title(self) -> None:
        with self.lock:
            self.semantic_title_requests += 1


def is_semantic_title_request(payload: object) -> bool:
    if not isinstance(payload, dict):
        return False
    tools = payload.get("tools")
    if isinstance(tools, list) and tools:
        return False
    messages = payload.get("messages")
    if not isinstance(messages, list):
        return False
    return any(
        isinstance(message, dict)
        and isinstance(message.get("content"), str)
        and "Generate a concise semantic title for a coding-agent conversation"
        in message["content"]
        for message in messages
    )


def semantic_compaction_output(payload: object) -> str | None:
    if not isinstance(payload, dict):
        return None
    messages = payload.get("messages")
    if not isinstance(messages, list) or not messages:
        return None
    final_message = messages[-1]
    if not isinstance(final_message, dict):
        return None
    instruction = final_message.get("content")
    if not isinstance(instruction, str) or "SOURCE_INDEX=" not in instruction:
        return None
    source_index = instruction.split("SOURCE_INDEX=", 1)[1]
    sources = json.loads(source_index)
    if not isinstance(sources, list) or not sources:
        raise AcceptanceError("semantic compaction source index is empty")
    first_source = sources[0]
    if not isinstance(first_source, dict):
        raise AcceptanceError("semantic compaction source index entry is malformed")
    source_event_id = first_source.get("event_id")
    if not isinstance(source_event_id, str) or not source_event_id:
        raise AcceptanceError("semantic compaction source event id is missing")
    return json.dumps(
        {
            "in_progress": [],
            "pending_actions": [],
            "provider_continuity": [],
            "model_notes": [
                {
                    "text": (
                        "The stateful acceptance retains long-history and checkpoint "
                        "mutation context."
                    ),
                    "source_event_ids": [source_event_id],
                    "priority": "normal",
                }
            ],
        },
        separators=(",", ":"),
    )


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    fixture: FixtureState
    tls_context: ssl.SSLContext


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    @property
    def fixture(self) -> FixtureState:
        server = self.server
        assert isinstance(server, FixtureServer)
        return server.fixture

    def do_CONNECT(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        if self.path != "api.deepseek.com:443":
            self.send_error(502, "unsupported fixture tunnel")
            return
        self.send_response(200, "Connection Established")
        self.end_headers()
        self.wfile.flush()
        server = self.server
        assert isinstance(server, FixtureServer)
        tls_socket = server.tls_context.wrap_socket(
            self.connection,
            server_side=True,
        )
        self.connection = tls_socket
        self.rfile = tls_socket.makefile("rb", self.rbufsize)
        self.wfile = tls_socket.makefile("wb", self.wbufsize)
        self.close_connection = False

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        self._send_json({"is_available": True, "balance_infos": []})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract.
        try:
            payload = self._read_json()
            if not self.path.endswith("/chat/completions"):
                self._send_json({"is_available": True, "balance_infos": []})
                return
            compaction_output = semantic_compaction_output(payload)
            if compaction_output is not None:
                self.fixture.record_semantic_summary()
                self._send_sse(
                    {
                        "delta": {"content": compaction_output},
                        "finish_reason": "stop",
                    },
                    usage={"prompt_tokens": 9000, "completion_tokens": 80},
                )
                return
            if is_semantic_title_request(payload):
                self.fixture.record_semantic_title()
                self._send_sse(
                    {
                        "delta": {"content": "Stateful acceptance"},
                        "finish_reason": "stop",
                    }
                )
                return
            if is_plan_mode_request(payload):
                self.fixture.record_plan_request()
                if advertises_tool(payload, "submit_plan_draft"):
                    self._send_sse(
                        {
                            "delta": {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": PLAN_TOOL_CALL_ID,
                                        "type": "function",
                                        "function": {
                                            "name": "submit_plan_draft",
                                            "arguments": PLAN_DRAFT_ARGUMENTS,
                                        },
                                    }
                                ]
                            },
                            "finish_reason": "tool_calls",
                        }
                    )
                else:
                    self._send_sse(
                        {
                            "delta": {"content": PLAN_DRAFT_TEXT},
                            "finish_reason": "stop",
                        }
                    )
                return
            request_index = self.fixture.record_request(payload)
            if request_index <= 3:
                self.fixture.wait_for_response_release(request_index)
                # Keep the campaign above the real RFC-0057 economics floor even after the
                # provider/tool schema prefix and the billed semantic-summary usage are counted.
                body = stateful_history_response_body(request_index)
                self._send_sse(
                    {"delta": {"content": body}, "finish_reason": "stop"},
                    usage=(
                        cache_usage_for_request(request_index)
                        if self.fixture.cache_usage_enabled
                        else None
                    ),
                )
            elif request_index == 4:
                self._send_sse(
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": CJK_TOOL_CALL_ID,
                                    "type": "function",
                                    "function": {
                                        "name": "read_file",
                                        "arguments": json.dumps(
                                            {"path": CJK_PATH},
                                            separators=(",", ":"),
                                        ),
                                    },
                                }
                            ]
                        },
                        "finish_reason": "tool_calls",
                    },
                    usage=(
                        cache_usage_for_request(request_index)
                        if self.fixture.cache_usage_enabled
                        else None
                    ),
                )
            elif request_index == 5:
                self._send_sse(
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": TOOL_CALL_ID,
                                    "type": "function",
                                    "function": {
                                        "name": "write_file",
                                        "arguments": json.dumps(
                                            {"path": EDIT_PATH, "content": MUTATED_CONTENT},
                                            separators=(",", ":"),
                                        ),
                                    },
                                }
                            ]
                        },
                        "finish_reason": "tool_calls",
                    },
                    usage=(
                        cache_usage_for_request(request_index)
                        if self.fixture.cache_usage_enabled
                        else None
                    ),
                )
            elif request_index in (6, 7):
                self._send_sse(
                    {"delta": {"content": FINAL_CANARY}, "finish_reason": "stop"},
                    usage=(
                        cache_usage_for_request(request_index)
                        if self.fixture.cache_usage_enabled
                        else None
                    ),
                )
            else:
                raise AcceptanceError(f"unexpected provider request {request_index}")
        except Exception as error:  # noqa: BLE001 - retain fixture diagnostics.
            self.fixture.record_error(error)
            self._send_json({"error": f"fixture failure: {error}"}, status=500)

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

    def _send_sse(
        self,
        choice: dict[str, object],
        *,
        usage: dict[str, int] | None = None,
    ) -> None:
        payload: dict[str, object] = {"choices": [choice]}
        if usage is not None:
            payload["usage"] = usage
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

    def _send_json(self, payload: object, status: int = 200) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


@dataclasses.dataclass
class VtBufferState:
    cells: list[list[str]]
    history: list[list[str]]
    row: int
    col: int
    saved: tuple[int, int]
    scroll_top: int
    scroll_bottom: int
    autowrap: bool
    wrap_pending: bool


class VtScreen:
    """Small deterministic VT screen with distinct primary and alternate buffers."""

    def __init__(self, rows: int = 42, cols: int = 140) -> None:
        self.rows = rows
        self.cols = cols
        self.cells = [[" " for _ in range(cols)] for _ in range(rows)]
        self.history: list[list[str]] = []
        self.row = 0
        self.col = 0
        self.saved = (0, 0)
        self.scroll_top = 0
        self.scroll_bottom = rows
        self.autowrap = True
        self.wrap_pending = False
        self.alternate_screen_active = False
        self._primary_state: VtBufferState | None = None
        self._decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self._pending_text = ""

    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def history_text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.history)

    def full_text(self) -> str:
        parts = [text for text in (self.history_text(), self.text()) if text]
        return "\n".join(parts)

    def resize(self, rows: int, cols: int) -> None:
        if rows <= 0 or cols <= 0:
            raise ValueError("terminal dimensions must be positive")

        if self._primary_state is not None:
            primary = self._primary_state
            primary.cells = self._resized_cells(primary.cells, rows, cols)
            primary.row = min(primary.row, rows - 1)
            primary.col = min(primary.col, cols - 1)
            primary.saved = (
                min(primary.saved[0], rows - 1),
                min(primary.saved[1], cols - 1),
            )

        old_rows = self.rows
        old_full_region = self.scroll_top == 0 and self.scroll_bottom == old_rows
        # Terminal reflow on resize is emulator-specific. Keep this model deterministic and only
        # treat explicit top-anchored scroll commands as native scrollback ownership changes.
        resized = self._resized_cells(self.cells, rows, cols)
        self.rows = rows
        self.cols = cols
        self.cells = resized
        self.row = min(self.row, rows - 1)
        self.col = min(self.col, cols - 1)
        self.saved = (min(self.saved[0], rows - 1), min(self.saved[1], cols - 1))
        if old_full_region:
            self.scroll_top = 0
            self.scroll_bottom = rows
        else:
            self.scroll_top = min(self.scroll_top, rows - 1)
            self.scroll_bottom = min(max(self.scroll_top + 1, self.scroll_bottom), rows)
        self.wrap_pending = False

    @staticmethod
    def _resized_cells(cells: list[list[str]], rows: int, cols: int) -> list[list[str]]:
        resized = [row[:cols] + [" "] * max(0, cols - len(row)) for row in cells[:rows]]
        resized.extend([[" " for _ in range(cols)] for _ in range(rows - len(resized))])
        return resized

    def feed(self, data: bytes) -> None:
        text = self._pending_text + self._decoder.decode(data, final=False)
        self._pending_text = ""
        index = 0
        while index < len(text):
            character = text[index]
            if character == "\x1b":
                next_index = self._escape(text, index + 1)
                if next_index is None:
                    self._pending_text = text[index:]
                    return
                index = next_index
                continue
            if character == "\r":
                self.col = 0
                self.wrap_pending = False
            elif character == "\n":
                self._line_feed()
                self.wrap_pending = False
            elif character == "\b":
                self.col = max(0, self.col - 1)
                self.wrap_pending = False
            elif character == "\t":
                self.col = min(self.cols - 1, (self.col // 8 + 1) * 8)
                self.wrap_pending = False
            elif ord(character) >= 32 and character != "\x7f":
                self._write(character)
            index += 1

    def _escape(self, text: str, index: int) -> int | None:
        if index >= len(text):
            return None
        if text[index] == "[":
            end = index + 1
            while end < len(text) and not ("@" <= text[end] <= "~"):
                end += 1
            if end >= len(text):
                return None
            self._csi(text[index + 1 : end], text[end])
            return end + 1
        if text[index] == "]":
            end = index + 1
            while end < len(text):
                if text[end] == "\x07":
                    return end + 1
                if text[end] == "\x1b" and end + 1 < len(text) and text[end + 1] == "\\":
                    return end + 2
                end += 1
            return None
        if text[index] == "7":
            self.saved = (self.row, self.col)
        elif text[index] == "8":
            self.row, self.col = self.saved
            self.wrap_pending = False
        elif text[index] == "D":
            self._line_feed()
            self.wrap_pending = False
        elif text[index] == "E":
            self._line_feed()
            self.col = 0
            self.wrap_pending = False
        return index + 1

    def _csi(self, raw: str, command: str) -> None:
        private = raw.startswith("?")
        raw = raw.lstrip("?>!")
        values = [int(value) if value else 0 for value in raw.split(";")] if raw else []
        first = values[0] if values else 0
        amount = max(1, first)
        if command in ("H", "f"):
            self.row = min(self.rows - 1, max(0, (values[0] if values else 1) - 1))
            self.col = min(self.cols - 1, max(0, (values[1] if len(values) > 1 else 1) - 1))
            self.wrap_pending = False
        elif command == "A":
            self.row = max(0, self.row - amount)
            self.wrap_pending = False
        elif command == "B":
            self.row = min(self.rows - 1, self.row + amount)
            self.wrap_pending = False
        elif command == "C":
            self.col = min(self.cols - 1, self.col + amount)
            self.wrap_pending = False
        elif command == "D":
            self.col = max(0, self.col - amount)
            self.wrap_pending = False
        elif command == "E":
            self.row = min(self.rows - 1, self.row + amount)
            self.col = 0
            self.wrap_pending = False
        elif command == "F":
            self.row = max(0, self.row - amount)
            self.col = 0
            self.wrap_pending = False
        elif command in ("G", "`"):
            self.col = min(self.cols - 1, max(0, amount - 1))
            self.wrap_pending = False
        elif command == "d":
            self.row = min(self.rows - 1, max(0, amount - 1))
            self.wrap_pending = False
        elif command == "J":
            self._erase_display(first)
            self.wrap_pending = False
        elif command == "K":
            self._erase_line(first)
            self.wrap_pending = False
        elif command == "X":
            for column in range(self.col, min(self.cols, self.col + amount)):
                self.cells[self.row][column] = " "
            self.wrap_pending = False
        elif command == "r" and not private:
            self._set_scrolling_region(values)
        elif command == "S" and not private:
            self._scroll_up(amount)
            self.wrap_pending = False
        elif command == "T" and not private:
            self._scroll_down(amount)
            self.wrap_pending = False
        elif command == "s":
            self.saved = (self.row, self.col)
        elif command == "u":
            self.row, self.col = self.saved
            self.wrap_pending = False
        elif command in ("h", "l") and private and 7 in values:
            self.autowrap = command == "h"
            self.wrap_pending = False
        elif command in ("h", "l") and private and first in (47, 1047, 1049):
            if command == "h" and not self.alternate_screen_active:
                self._primary_state = VtBufferState(
                    cells=[row.copy() for row in self.cells],
                    history=[row.copy() for row in self.history],
                    row=self.row,
                    col=self.col,
                    saved=self.saved,
                    scroll_top=self.scroll_top,
                    scroll_bottom=self.scroll_bottom,
                    autowrap=self.autowrap,
                    wrap_pending=self.wrap_pending,
                )
                self.alternate_screen_active = True
                self.cells = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
                self.history = []
                self.row = 0
                self.col = 0
                self.saved = (0, 0)
                self.scroll_top = 0
                self.scroll_bottom = self.rows
                self.autowrap = True
                self.wrap_pending = False
            elif command == "l" and self.alternate_screen_active:
                primary = self._primary_state
                assert primary is not None
                self.cells = primary.cells
                self.history = primary.history
                self.row = primary.row
                self.col = primary.col
                self.saved = primary.saved
                self.scroll_top = primary.scroll_top
                self.scroll_bottom = primary.scroll_bottom
                self.autowrap = primary.autowrap
                self.wrap_pending = primary.wrap_pending
                self.alternate_screen_active = False
                self._primary_state = None

    def _set_scrolling_region(self, values: list[int]) -> None:
        top = values[0] if values and values[0] else 1
        bottom = values[1] if len(values) > 1 and values[1] else self.rows
        if 1 <= top <= bottom <= self.rows:
            self.scroll_top = top - 1
            self.scroll_bottom = bottom
            self.row = 0
            self.col = 0
            self.wrap_pending = False

    def _scroll_up(self, amount: int) -> None:
        height = self.scroll_bottom - self.scroll_top
        amount = min(amount, height)
        if amount <= 0:
            return
        if self.scroll_top == 0:
            self.history.extend(row.copy() for row in self.cells[:amount])
        retained = self.cells[self.scroll_top + amount : self.scroll_bottom]
        blanks = [[" " for _ in range(self.cols)] for _ in range(amount)]
        self.cells[self.scroll_top : self.scroll_bottom] = retained + blanks

    def _scroll_down(self, amount: int) -> None:
        height = self.scroll_bottom - self.scroll_top
        amount = min(amount, height)
        if amount <= 0:
            return
        retained = self.cells[self.scroll_top : self.scroll_bottom - amount]
        blanks = [[" " for _ in range(self.cols)] for _ in range(amount)]
        self.cells[self.scroll_top : self.scroll_bottom] = blanks + retained

    def _line_feed(self) -> None:
        if self.row == self.scroll_bottom - 1 and self.scroll_top <= self.row:
            self._scroll_up(1)
        elif self.row < self.rows - 1:
            self.row += 1

    def _erase_display(self, mode: int) -> None:
        if mode in (2, 3):
            self.cells = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
            if mode == 3:
                self.history.clear()
        elif mode == 0:
            self._erase_line(0)
            for row in range(self.row + 1, self.rows):
                self.cells[row] = [" " for _ in range(self.cols)]
        elif mode == 1:
            self._erase_line(1)
            for row in range(self.row):
                self.cells[row] = [" " for _ in range(self.cols)]

    def _erase_line(self, mode: int) -> None:
        start, end = (self.col, self.cols) if mode == 0 else (0, self.col + 1)
        if mode == 2:
            start, end = 0, self.cols
        for column in range(start, end):
            self.cells[self.row][column] = " "

    def _write(self, character: str) -> None:
        if unicodedata.combining(character):
            return
        width = 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1
        if self.autowrap and (
            self.wrap_pending or (width > 1 and self.col + width > self.cols)
        ):
            self.col = 0
            self._line_feed()
            self.wrap_pending = False
        self.cells[self.row][self.col] = character
        if width == 2 and self.col + 1 < self.cols:
            self.cells[self.row][self.col + 1] = " "
        if self.col + width >= self.cols:
            self.col = self.cols - 1
            self.wrap_pending = self.autowrap
        else:
            self.col += width
            self.wrap_pending = False


def posix_descendant_pids(root_pid: int) -> set[int]:
    try:
        completed = subprocess.run(
            ["ps", "-axo", "pid=,ppid="],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return set()
    if completed.returncode != 0:
        return set()
    children: dict[int, list[int]] = {}
    for line in completed.stdout.splitlines():
        fields = line.split()
        if len(fields) != 2:
            continue
        try:
            pid, parent_pid = (int(field) for field in fields)
        except ValueError:
            continue
        children.setdefault(parent_pid, []).append(pid)
    descendants: set[int] = set()
    pending = list(children.get(root_pid, []))
    while pending:
        pid = pending.pop()
        if pid in descendants:
            continue
        descendants.add(pid)
        pending.extend(children.get(pid, []))
    return descendants


def process_is_running(pid: int) -> bool:
    try:
        completed = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(pid)],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    state = completed.stdout.strip()
    return completed.returncode == 0 and bool(state) and not state.startswith("Z")


def signal_processes(process_ids: set[int], requested_signal: signal.Signals) -> None:
    for pid in sorted(process_ids, reverse=True):
        try:
            os.kill(pid, requested_signal)
        except OSError:
            continue


def wait_for_processes(process_ids: set[int], timeout: float) -> set[int]:
    deadline = time.monotonic() + timeout
    remaining = set(process_ids)
    while remaining and time.monotonic() < deadline:
        remaining = {pid for pid in remaining if process_is_running(pid)}
        if remaining:
            time.sleep(0.05)
    return remaining


class PtyRunner:
    def __init__(
        self,
        command: list[str],
        cwd: Path,
        env: dict[str, str],
        raw_log: Path,
        *,
        rows: int = DEFAULT_PTY_ROWS,
        cols: int = DEFAULT_PTY_COLS,
    ) -> None:
        if rows <= 0 or cols <= 0:
            raise ValueError("terminal dimensions must be positive")
        self.command = command
        self.cwd = cwd
        self.env = env
        self.raw_log = raw_log
        self.initial_rows = rows
        self.initial_cols = cols
        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.output = bytearray()
        self._terminal_resizes: list[TerminalResize] = []
        self.cpr_request_count = 0
        self._terminal_query_tail = b""
        self._descendants: set[int] = set()
        self._descendants_lock = threading.Lock()
        self._monitor_stop = threading.Event()
        self._monitor_thread: threading.Thread | None = None

    def start(self) -> None:
        master_fd, slave_fd = pty.openpty()
        termios.tcsetwinsize(slave_fd, (self.initial_rows, self.initial_cols))
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
        self._monitor_stop.clear()
        self._monitor_thread = threading.Thread(
            target=self._monitor_descendants,
            name="sigil-stateful-pty-descendants",
            daemon=True,
        )
        self._monitor_thread.start()

    def _monitor_descendants(self) -> None:
        while not self._monitor_stop.is_set():
            if self.process is not None:
                observed = posix_descendant_pids(self.process.pid)
                if observed:
                    with self._descendants_lock:
                        self._descendants.update(observed)
            self._monitor_stop.wait(0.05)

    def _finish_descendant_monitor(self) -> set[int]:
        if self.process is not None:
            with self._descendants_lock:
                self._descendants.update(posix_descendant_pids(self.process.pid))
        self._monitor_stop.set()
        if self._monitor_thread is not None:
            self._monitor_thread.join(timeout=1)
            self._monitor_thread = None
        with self._descendants_lock:
            return set(self._descendants)

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
            for response in self._terminal_query_responses(chunk):
                os.write(self.master_fd, response)
            timeout = 0.0

    def _terminal_query_responses(self, chunk: bytes) -> list[bytes]:
        scanned = self._terminal_query_tail + chunk
        count = scanned.count(CURSOR_POSITION_QUERY)
        self.cpr_request_count += count
        self._terminal_query_tail = scanned[-(len(CURSOR_POSITION_QUERY) - 1) :]
        return [CURSOR_POSITION_RESPONSE] * count

    def observed_native_inline_scrollback(self) -> bool:
        return bool(self.terminal().history)

    def entered_alternate_screen(self) -> bool:
        return ENTER_ALTERNATE_SCREEN in self.output

    def left_alternate_screen(self) -> bool:
        return LEAVE_ALTERNATE_SCREEN in self.output

    def emitted_single_line_scrolling_region(self) -> bool:
        return SINGLE_LINE_SCROLL_REGION_PATTERN.search(bytes(self.output)) is not None

    def resize(self, rows: int, cols: int) -> int:
        if rows <= 0 or cols <= 0:
            raise ValueError("terminal dimensions must be positive")
        if self.master_fd is None or self.process is None:
            raise AcceptanceError("PTY is not running")
        if self.process.poll() is not None:
            raise AcceptanceError("cannot resize an exited PTY process")

        # Drain bytes emitted at the old size before recording the replay boundary. Bytes that
        # arrive after the ioctl belong to the new geometry, even if the child redraw is delayed.
        self.read_available(0.0)
        termios.tcsetwinsize(self.master_fd, (rows, cols))
        self._terminal_resizes.append(TerminalResize(len(self.output), rows, cols))
        try:
            os.killpg(self.process.pid, signal.SIGWINCH)
        except OSError as error:
            raise AcceptanceError(
                f"failed to deliver SIGWINCH after PTY resize: {error}"
            ) from error
        return self._terminal_resizes[-1].output_offset

    def send(self, value: str | bytes) -> None:
        if self.master_fd is None:
            raise AcceptanceError("PTY is not running")
        os.write(self.master_fd, value if isinstance(value, bytes) else value.encode("utf-8"))

    def type_text(self, value: str) -> None:
        for character in value:
            self.send(character)
            time.sleep(0.001)

    def screen(self) -> str:
        return self.terminal().text()

    def terminal(self) -> VtScreen:
        screen = VtScreen(rows=self.initial_rows, cols=self.initial_cols)
        output = bytes(self.output)
        offset = 0
        for resize in self._terminal_resizes:
            boundary = min(max(offset, resize.output_offset), len(output))
            screen.feed(output[offset:boundary])
            screen.resize(resize.rows, resize.cols)
            offset = boundary
        screen.feed(output[offset:])
        return screen

    def history_text(self) -> str:
        return self.terminal().history_text()

    def full_text(self) -> str:
        return self.terminal().full_text()

    def raw_text(self) -> str:
        return bytes(self.output).decode("utf-8", errors="replace")

    def wait_until(
        self,
        predicate: Callable[[str], bool],
        timeout: float,
        description: str,
        *,
        final_screen: bool = False,
    ) -> str:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read_available(0.1)
            rendered = self.screen() if final_screen else self.raw_text()
            if predicate(rendered):
                return rendered
            if self.process is not None and self.process.poll() is not None:
                raise AcceptanceError(f"TUI exited while waiting for {description}")
        raise TimeoutError(f"timed out waiting for {description}")

    def quit(self, timeout: float = 10.0) -> None:
        self.send(b"\x01\x0b")
        self.type_text("/quit")
        self.send("\r")
        if self.process is None:
            raise AcceptanceError("PTY process was not started")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read_available(0.05)
            if self.process.poll() is not None:
                self.raw_log.write_bytes(bytes(self.output))
                if self.process.returncode != 0:
                    raise AcceptanceError(f"TUI exited with {self.process.returncode}")
                return
        raise TimeoutError("TUI did not exit after /quit")

    def stop(self) -> None:
        descendants = self._finish_descendant_monitor()
        try:
            self.read_available(0.0)
            self.raw_log.write_bytes(bytes(self.output))
        except OSError:
            pass
        if self.process is not None and self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
                self.process.wait(timeout=3)
            except OSError:
                pass
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except OSError:
                    pass
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
        signal_processes(descendants, signal.SIGTERM)
        remaining = wait_for_processes(descendants, 2)
        if remaining:
            signal_processes(remaining, signal.SIGKILL)
            wait_for_processes(remaining, 2)
        if self.master_fd is not None:
            try:
                os.close(self.master_fd)
            except OSError:
                pass
            self.master_fd = None


def looks_like_trust_gate(text: str) -> bool:
    lowered = text.lower()
    return "trust this workspace" in lowered or "workspace trust" in lowered


def looks_like_main_tui(text: str) -> bool:
    lowered = text.lower()
    return "agent:" in lowered and ("build" in lowered or "session" in lowered)


def looks_like_busy_tui(text: str) -> bool:
    lowered = text.lower()
    return any(
        marker in lowered
        for marker in ("replying...", "thinking...", "receiving response")
    )


def looks_like_ready_plan_preview(text: str) -> bool:
    return (
        "Plan ready" in text
        and PLAN_SUMMARY_CANARY in text
        and not looks_like_busy_tui(text)
    )


def looks_like_open_plan_workbench(text: str) -> bool:
    return (
        "Plan Review" in text
        and "Summary" in text
        and PLAN_SUMMARY_CANARY in text
        and "Reject" in text
        and "Esc close" in text
        and not looks_like_busy_tui(text)
    )


def looks_like_dismissed_plan_preview(text: str) -> bool:
    lowered = text.lower()
    return (
        "Plan ready" not in text
        and "Plan Review" not in text
        and not looks_like_busy_tui(text)
        and "build · agent:" in lowered
    )


def looks_like_visible_queued_follow_up(text: str) -> bool:
    lowered = text.lower()
    return count_on_screen(text, QUEUED_FOLLOW_UP_PROMPT) > 0 and any(
        marker in lowered
        for marker in (
            "follow-up pending",
            "pending · main · follow-up",
        )
    )


def reject_open_plan_workbench(runner: PtyRunner, timeout: float) -> str:
    # Run is selected initially. Move across Save and Revise to the explicit Reject action; Esc is
    # intentionally not used because the durable workbench contract defines it as close-only.
    for _ in range(3):
        runner.send("\t")
    runner.send("\r")
    return runner.wait_until(
        looks_like_dismissed_plan_preview,
        timeout,
        "dismissed plan preview after explicit rejection",
        final_screen=True,
    )


def wait_for_busy_tui(runner: PtyRunner, timeout: float, description: str) -> str:
    return runner.wait_until(
        looks_like_busy_tui,
        timeout,
        description,
        final_screen=True,
    )


def resize_while_busy(
    runner: PtyRunner,
    rows: int,
    cols: int,
    timeout: float,
    description: str,
) -> int:
    wait_for_busy_tui(runner, timeout, description)
    return runner.resize(rows, cols)


def wait_for_main_tui(runner: PtyRunner, timeout: float) -> None:
    initial = runner.wait_until(
        lambda text: looks_like_trust_gate(text) or looks_like_main_tui(text),
        timeout,
        "initial TUI",
    )
    if looks_like_trust_gate(initial):
        runner.send("\r")
        runner.wait_until(looks_like_main_tui, timeout, "trusted main TUI")
    if not runner.entered_alternate_screen():
        raise AcceptanceError(
            "TUI did not enter the alternate screen before rendering its first frame"
        )
    startup = bytes(runner.output)
    alternate_offset = startup.find(ENTER_ALTERNATE_SCREEN)
    purge_offset = startup.find(ERASE_SCROLLBACK, alternate_offset)
    clear_offset = startup.find(CLEAR_SCREEN, alternate_offset)
    home_offset = startup.find(CURSOR_HOME, alternate_offset)
    if not alternate_offset <= purge_offset < clear_offset < home_offset:
        raise AcceptanceError(
            "TUI did not purge history, clear the alternate screen, and home the cursor before its first frame"
        )
    if runner.cpr_request_count != 0:
        raise AcceptanceError("full-screen TUI unexpectedly queried the terminal cursor position")


def write_config(
    path: Path,
    *,
    workspace: Path,
    state_root: Path,
    cache_root: Path,
    session_dir: Path,
    port: int | None,
) -> None:
    del port
    endpoint = "https://api.deepseek.com"
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
connection = "stateful-fixture"
model = "{MODEL_NAME}"
max_turns = 12
tool_timeout_secs = 10

[model_request]
request_timeout_secs = 10
stream_idle_timeout_secs = 10

[permission]
mode = "auto-edit"

[compaction]
enabled = true

[task]
routing_policy = "manual"

[web]
enabled = false

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[connections.stateful-fixture]
label = "Stateful acceptance fixture"
provider = "deepseek"
protocol = "deepseek"
base_url = "{endpoint}"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}

[connections.stateful-fixture.options]
strict_tools_mode = "auto"
''',
        encoding="utf-8",
    )


def isolated_environment(root: Path, proxy_port: int | None = None) -> dict[str, str]:
    proxy_url = (
        f"http://127.0.0.1:{proxy_port}"
        if proxy_port is not None
        else "http://127.0.0.1:9"
    )
    env = identity_environment(os.environ)
    env.update(
        {
            "HOME": str(root / "home"),
            "XDG_CONFIG_HOME": str(root / "xdg-config"),
            "XDG_STATE_HOME": str(root / "xdg-state"),
            "XDG_CACHE_HOME": str(root / "xdg-cache"),
            "TMPDIR": str(root / "tmp"),
            "TERM": env.get("TERM", "xterm-256color"),
            "HTTP_PROXY": proxy_url,
            "HTTPS_PROXY": proxy_url,
            "http_proxy": proxy_url,
            "https_proxy": proxy_url,
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
            "SSL_CERT_FILE": str(root / "tls-ca.pem"),
            "SIGIL_API_KEY": "stateful-fixture-key",
            "RUST_LOG": "trace",
        }
    )
    for directory in ("home", "xdg-config", "xdg-state", "xdg-cache", "tmp"):
        (root / directory).mkdir()
    return env


def generate_fixture_tls_identity(root: Path) -> tuple[Path, Path]:
    ca_certificate = root / "tls-ca.pem"
    ca_private_key = root / "tls-ca-key.pem"
    certificate = root / "tls-cert.pem"
    private_key = root / "tls-key.pem"
    certificate_request = root / "tls-cert.csr"
    extensions = root / "tls-cert.ext"
    extensions.write_text(
        "\n".join(
            (
                "basicConstraints=critical,CA:FALSE",
                "keyUsage=critical,digitalSignature,keyEncipherment",
                "extendedKeyUsage=serverAuth",
                "subjectAltName=DNS:api.deepseek.com",
                "",
            )
        ),
        encoding="utf-8",
    )
    commands = (
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-keyout",
            str(ca_private_key),
            "-out",
            str(ca_certificate),
            "-days",
            "1",
            "-subj",
            "/CN=Sigil stateful fixture CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
        [
            "openssl",
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-keyout",
            str(private_key),
            "-out",
            str(certificate_request),
            "-subj",
            "/CN=api.deepseek.com",
        ],
        [
            "openssl",
            "x509",
            "-req",
            "-in",
            str(certificate_request),
            "-CA",
            str(ca_certificate),
            "-CAkey",
            str(ca_private_key),
            "-CAcreateserial",
            "-out",
            str(certificate),
            "-days",
            "1",
            "-sha256",
            "-extfile",
            str(extensions),
        ],
    )
    for command in commands:
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        if completed.returncode != 0:
            raise AcceptanceError(
                f"failed to create local TLS fixture identity: {completed.stderr.strip()}"
            )
    os.chmod(ca_private_key, 0o600)
    os.chmod(private_key, 0o600)
    return certificate, private_key


def read_session_audit(path: Path) -> SessionAudit:
    event_counts: dict[str, int] = {}
    final_canary_count = 0
    failed_run_count = 0
    session_id = ""
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(raw_line)
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            event_counts[event_type] = event_counts.get(event_type, 0) + 1
        if event_type == "run_finalized" and record.get("payload", {}).get("run_status") != "completed":
            failed_run_count += 1
        if not session_id and isinstance(record.get("session_id"), str):
            session_id = record["session_id"]
        entry = record.get("payload", {}).get("session_log_entry", {})
        assistant = entry.get("assistant") if isinstance(entry, dict) else None
        if not isinstance(assistant, dict):
            continue
        if assistant.get("assistant_kind") != "final_answer":
            continue
        content = assistant.get("content")
        if isinstance(content, str) and content == FINAL_CANARY:
            final_canary_count += 1
    if not session_id:
        raise AcceptanceError("session stream is missing its session id")
    return SessionAudit(path, session_id, event_counts, final_canary_count, failed_run_count)


def session_files(session_dir: Path) -> list[Path]:
    return sorted(
        (path for path in session_dir.glob("*.jsonl") if path.is_file()),
        key=lambda path: path.stat().st_mtime_ns,
    )


def wait_for_session_audit(
    path: Path,
    predicate: Callable[[SessionAudit], bool],
    timeout: float,
    description: str,
) -> SessionAudit:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            audit = read_session_audit(path)
            if predicate(audit):
                return audit
        except (OSError, json.JSONDecodeError, AcceptanceError) as error:
            last_error = error
        time.sleep(0.05)
    suffix = f": {last_error}" if last_error is not None else ""
    raise TimeoutError(f"timed out waiting for {description}{suffix}")


def wait_for_fork_path(
    session_dir: Path,
    source_path: Path,
    timeout: float,
) -> Path:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        candidates = [path for path in session_files(session_dir) if path != source_path]
        if len(candidates) == 1:
            return candidates[0]
        if len(candidates) > 1:
            raise AcceptanceError("modal F created more than one fork session")
        time.sleep(0.05)
    raise TimeoutError("timed out waiting for the durable conversation fork")


def count_on_screen(screen: str, value: str) -> int:
    normalized_screen = "".join(screen.split())
    normalized_value = "".join(value.split())
    return normalized_screen.count(normalized_value)


def session_display_token(path: Path) -> str:
    stem = path.stem
    if stem.startswith("session-"):
        stem = stem.removeprefix("session-")
    return stem[:8]


def preserve_session_evidence(
    output_dir: Path,
    *,
    source_path: Path,
    fork_path: Path,
) -> dict[str, dict[str, str]]:
    session_output = output_dir / "sessions"
    session_output.mkdir(mode=0o700, exist_ok=True)
    evidence: dict[str, dict[str, str]] = {}
    for label, source in (("source_session", source_path), ("fork_session", fork_path)):
        destination = session_output / f"{label.removesuffix('_session')}.jsonl"
        shutil.copyfile(source, destination)
        source_digest = sha256_file(source)
        destination_digest = sha256_file(destination)
        if source_digest != destination_digest:
            raise AcceptanceError(f"{label} checksum changed while preserving durable evidence")
        evidence[label] = {
            "path": destination.relative_to(output_dir).as_posix(),
            "sha256": destination_digest,
        }
    return evidence


def validate_passed_contract(
    checks: dict[str, object],
    session_evidence: dict[str, dict[str, str]],
) -> None:
    expected = {
        "provider_request_count": 6,
        "plan_request_count": 1,
        "queued_follow_up_observed": True,
        "cjk_tool_result_observed": True,
        "live_final_reply_screen_count": 1,
        "resumed_final_reply_screen_count": 1,
        "source_final_answer_count": 1,
        "fork_final_answer_count": 1,
        "compaction_applied_v2_count": 1,
        "checkpoint_restored_count": 1,
        "conversation_forked_count": 1,
        "modal_f_preserved_file": True,
        "resumed_session_is_fork": True,
        "resume_preserved_file": True,
    }
    for name, value in expected.items():
        if checks.get(name) != value:
            raise AcceptanceError(f"passed manifest has invalid check {name}")
    final_file_sha256 = checks.get("final_file_sha256")
    if not isinstance(final_file_sha256, str) or not SHA256_PATTERN.fullmatch(final_file_sha256):
        raise AcceptanceError("passed manifest has invalid final file SHA-256")
    if set(session_evidence) != {"source_session", "fork_session"}:
        raise AcceptanceError("passed manifest requires source and fork session evidence")
    for item in session_evidence.values():
        path = item.get("path")
        digest = item.get("sha256")
        if not isinstance(path, str) or Path(path).is_absolute() or ".." in Path(path).parts:
            raise AcceptanceError("session evidence path must be output-relative")
        if not isinstance(digest, str) or not SHA256_PATTERN.fullmatch(digest):
            raise AcceptanceError("session evidence SHA-256 is invalid")


def write_manifest(
    output_dir: Path,
    *,
    status: str,
    started_at: str,
    finished_at: str,
    duration_ms: int,
    binary: BinaryIdentity,
    tokenizer: dict[str, str],
    checks: dict[str, object],
    session_evidence: dict[str, dict[str, str]],
    artifact_policy: str,
    notes: list[str],
) -> Path:
    if status == "passed":
        validate_passed_contract(checks, session_evidence)
    evidence: dict[str, object] = {
        "turns_pty_log": "turns-process.log",
        "stateful_pty_log": "stateful-process.log",
        "resume_pty_log": "resume-process.log",
    }
    evidence.update(session_evidence)
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "campaign": "sigil-stateful-tui-v1",
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_ms": duration_ms,
        "binary": binary.as_dict(),
        "tokenizer": tokenizer,
        "checks": checks,
        "evidence": evidence,
        "privacy": {
            "raw_artifacts_local_only": True,
            "raw_artifact_policy": artifact_policy,
            "automatic_upload": False,
            "manifest_contains_prompt_or_session_content": False,
        },
    }
    if notes:
        manifest["notes"] = notes
    serialized = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    forbidden = ("/Users/", "/home/", FINAL_CANARY, EDIT_PROMPT, MUTATED_CONTENT)
    if any(value in serialized for value in forbidden):
        raise AcceptanceError("safe manifest contains private path or session content")
    path = output_dir / "manifest.json"
    path.write_text(serialized, encoding="utf-8")
    (output_dir / "manifest.sha256").write_text(
        f"{sha256_file(path)}  manifest.json\n", encoding="utf-8"
    )
    return path


def main() -> int:
    args = parse_args()
    root = repo_root()
    try:
        deadline = CampaignDeadline(args.timeout)
        selected_output = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
        output_dir = selected_output.expanduser().resolve()
        artifact_policy = raw_artifact_policy(root, output_dir)
    except (AcceptanceError, OSError, subprocess.SubprocessError) as error:
        print(f"stateful campaign admission failed: {error}", file=sys.stderr)
        return 2
    output_dir.mkdir(parents=True, exist_ok=True)
    started_at = utc_now()
    started = time.monotonic()
    notes: list[str] = []
    checks: dict[str, object] = {}
    identity = BinaryIdentity("unadmitted", "", "", "", "", "")
    tokenizer_identity = {"model": MODEL_NAME, "snapshot": "", "sha256": ""}
    session_evidence: dict[str, dict[str, str]] = {}
    status = "failed"
    fixture_root: Path | None = None
    first_runner: PtyRunner | None = None
    resume_runner: PtyRunner | None = None
    fixture: FixtureState | None = None
    server: FixtureServer | None = None
    server_thread: threading.Thread | None = None
    try:
        binary_source, identity = inspect_binary(args.binary, timeout=deadline.remaining(15.0))
        assert_expected_identity(identity, args)
        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-stateful-pty-"))
        frozen_binary = freeze_binary(binary_source, fixture_root, identity)
        workspace = fixture_root / "workspace"
        state_root = fixture_root / "state"
        cache_root = fixture_root / "cache"
        session_dir = fixture_root / "sessions"
        for directory in (workspace, state_root, cache_root, session_dir):
            directory.mkdir()
        edit_file = workspace / EDIT_PATH
        edit_file.write_text(ORIGINAL_CONTENT, encoding="utf-8")
        (workspace / CJK_PATH).write_text(CJK_CONTENT, encoding="utf-8")
        tokenizer_identity = install_tokenizer(
            args.tokenizer_json,
            cache_root,
            snapshot=args.tokenizer_snapshot,
            expected_sha256=args.expected_tokenizer_sha256,
        )
        tls_certificate, tls_private_key = generate_fixture_tls_identity(fixture_root)
        fixture = FixtureState()
        server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
        server.fixture = fixture
        tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_context.load_cert_chain(
            certfile=str(tls_certificate),
            keyfile=str(tls_private_key),
        )
        server.tls_context = tls_context
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        fixture_port = int(server.server_address[1])
        env = isolated_environment(fixture_root, fixture_port)
        config_path = fixture_root / "sigil.toml"
        write_config(
            config_path,
            workspace=workspace,
            state_root=state_root,
            cache_root=cache_root,
            session_dir=session_dir,
            port=fixture_port,
        )
        first_runner = PtyRunner(
            [str(frozen_binary), "--config", str(config_path)],
            workspace,
            env,
            output_dir / "turns-process.log",
        )
        first_runner.start()
        wait_for_main_tui(first_runner, deadline.remaining())

        first_runner.type_text(f"/plan {PLAN_PROMPT}")
        first_runner.send("\r")
        first_runner.wait_until(
            looks_like_ready_plan_preview,
            deadline.remaining(),
            "visible stateful plan preview",
            final_screen=True,
        )
        first_runner.send("\r")
        first_runner.wait_until(
            looks_like_open_plan_workbench,
            deadline.remaining(),
            "open complete plan workbench",
            final_screen=True,
        )
        plan_shrink_boundary = first_runner.resize(28, 100)
        first_runner.wait_until(
            lambda text: len(first_runner.output) > plan_shrink_boundary
            and looks_like_open_plan_workbench(text),
            deadline.remaining(),
            "plan workbench redraw after shrink",
            final_screen=True,
        )
        plan_grow_boundary = first_runner.resize(42, 140)
        first_runner.wait_until(
            lambda text: len(first_runner.output) > plan_grow_boundary
            and looks_like_open_plan_workbench(text),
            deadline.remaining(),
            "plan workbench redraw after grow",
            final_screen=True,
        )
        reject_open_plan_workbench(first_runner, deadline.remaining())

        first_runner.type_text("stateful history turn 1")
        first_runner.send("\r")
        wait_for_busy_tui(first_runner, deadline.remaining(), "busy first history turn")
        first_runner.type_text(QUEUED_FOLLOW_UP_PROMPT)
        first_runner.send("\r")
        first_runner.wait_until(
            looks_like_visible_queued_follow_up,
            deadline.remaining(),
            "visible queued follow-up",
            final_screen=True,
        )
        resize_while_busy(
            first_runner,
            24,
            96,
            deadline.remaining(),
            "busy queued follow-up before shrink",
        )
        fixture.release_response(1)
        first_runner.wait_until(
            lambda text: count_on_screen(text, QUEUED_FOLLOW_UP_REPLY) > 0,
            deadline.remaining(),
            "queued follow-up reply",
            final_screen=True,
        )
        first_runner.wait_until(
            lambda text: looks_like_main_tui(text) and not looks_like_busy_tui(text),
            deadline.remaining(),
            "settled queued follow-up reply",
            final_screen=True,
        )
        resize_boundary = first_runner.resize(36, 120)
        first_runner.wait_until(
            lambda text: len(first_runner.output) > resize_boundary and looks_like_main_tui(text),
            deadline.remaining(),
            "post-queue partial-grow redraw",
            final_screen=True,
        )

        first_runner.type_text("stateful history turn 3")
        first_runner.send("\r")
        resize_while_busy(
            first_runner,
            20,
            88,
            deadline.remaining(),
            "busy third history turn before shrink",
        )
        fixture.release_response(3)
        first_runner.wait_until(
            lambda text: count_on_screen(text, "STATEFUL-HISTORY-3") > 0,
            deadline.remaining(),
            "history reply 3",
            final_screen=True,
        )
        first_runner.wait_until(
            lambda text: looks_like_main_tui(text) and not looks_like_busy_tui(text),
            deadline.remaining(),
            "settled history reply 3",
            final_screen=True,
        )
        resize_boundary = first_runner.resize(50, 160)
        first_runner.wait_until(
            lambda text: len(first_runner.output) > resize_boundary and looks_like_main_tui(text),
            deadline.remaining(),
            "post-history high-water grow redraw",
            final_screen=True,
        )
        first_runner.type_text(EDIT_PROMPT)
        first_runner.send("\r")
        first_runner.wait_until(
            lambda text: FINAL_CANARY in text,
            deadline.remaining(),
            "streamed unique final reply",
            final_screen=True,
        )
        if edit_file.read_text(encoding="utf-8") != MUTATED_CONTENT:
            raise AcceptanceError("write_file fixture did not mutate the controlled file")
        if CJK_CONTENT.encode("utf-8") not in bytes(first_runner.output):
            raise AcceptanceError("CJK read_file result was not rendered through the real PTY")
        current_files = session_files(session_dir)
        if len(current_files) != 1:
            raise AcceptanceError("first process did not create exactly one source session")
        source_path = current_files[0]
        wait_for_session_audit(
            source_path,
            lambda audit: audit.final_canary_count == 1
            # PlanReview now owns a dedicated attempt lifecycle instead of synthesizing a root
            # conversation run. The four user messages therefore settle as three foreground runs
            # (the queued follow-up remains part of the first run) plus one rejected plan decision.
            and audit.event_counts.get("run_finalized", 0) == 3
            and audit.event_counts.get("plan_draft_created", 0) == 1
            and audit.event_counts.get("plan_decision_recorded", 0) == 1
            and audit.failed_run_count == 0,
            deadline.remaining(),
            "three finalized runs, rejected plan, and one durable final-answer canary",
        )
        live_screen = first_runner.wait_until(
            lambda text: FINAL_CANARY in text
            and "Replying..." not in text
            and "Thinking..." not in text,
            deadline.remaining(),
            "settled unique final reply",
            final_screen=True,
        )
        if count_on_screen(live_screen, FINAL_CANARY) != 1:
            raise AcceptanceError("live completion screen rendered the final reply more than once")
        for _ in range(12):
            first_runner.send(LIVE_PANEL_SCROLL_UP_SEQUENCE)
        first_runner.wait_until(
            lambda text: FINAL_CANARY not in text and "verified-history" in text,
            deadline.remaining(),
            "mouse-owned history inspection immediately after direct resume",
            final_screen=True,
        )
        first_runner.send(CTRL_HOME_KEY_SEQUENCE)
        oldest_history_head = history_head_canary(1)
        history_inspection_screen = first_runner.wait_until(
            lambda text: count_on_screen(text, oldest_history_head) > 0
            and FINAL_CANARY not in text,
            deadline.remaining(),
            "in-app history inspection after final-answer viewport growth",
            final_screen=True,
        )
        if count_on_screen(history_inspection_screen, oldest_history_head) != 1:
            raise AcceptanceError(
                "in-app history inspection rendered the oldest completed turn more than once"
            )
        first_runner.send(CTRL_END_KEY_SEQUENCE)
        first_runner.wait_until(
            lambda text: FINAL_CANARY in text and oldest_history_head not in text,
            deadline.remaining(),
            "return from in-app history inspection to the live tail",
            final_screen=True,
        )
        if first_runner.observed_native_inline_scrollback():
            raise AcceptanceError(
                "full-screen flow leaked application transcript rows into native scrollback"
            )
        if first_runner.emitted_single_line_scrolling_region():
            raise AcceptanceError(
                "stateful flow used the non-portable single-line scrolling-region fast path"
            )
        first_runner.quit(timeout=deadline.remaining(10.0))
        if not first_runner.left_alternate_screen():
            raise AcceptanceError("TUI did not restore the primary screen before exiting")
        if first_runner.cpr_request_count != 0:
            raise AcceptanceError(
                "full-screen TUI queried the cursor during rendering or exit cleanup"
            )
        raw_output = bytes(first_runner.output)
        leave_offset = raw_output.rfind(LEAVE_ALTERNATE_SCREEN)
        hint_offset = raw_output.rfind(b"Sigil session:")
        if hint_offset >= 0 and leave_offset > hint_offset:
            raise AcceptanceError("resume hint was printed before the primary screen was restored")
        first_runner.stop()
        first_runner = None
        write_config(
            config_path,
            workspace=workspace,
            state_root=state_root,
            cache_root=cache_root,
            session_dir=session_dir,
            port=fixture_port,
        )
        first_runner = PtyRunner(
            [
                str(frozen_binary),
                "--config",
                str(config_path),
                "resume",
                str(source_path),
            ],
            workspace,
            env,
            output_dir / "stateful-process.log",
        )
        first_runner.start()
        wait_for_main_tui(first_runner, deadline.remaining())

        # Session restore must publish a scrollable history projection before any resize event.
        # This catches launch paths that restore against a placeholder viewport and only become
        # scrollable after the terminal emits SIGWINCH.
        first_runner.send(CTRL_HOME_KEY_SEQUENCE)
        oldest_history_head = history_head_canary(1)
        restored_history_screen = first_runner.wait_until(
            lambda text: count_on_screen(text, oldest_history_head) > 0
            and FINAL_CANARY not in text,
            deadline.remaining(),
            "in-app history inspection immediately after direct resume",
            final_screen=True,
        )
        if count_on_screen(restored_history_screen, oldest_history_head) != 1:
            raise AcceptanceError(
                "direct resume rendered the oldest completed turn more than once"
            )
        first_runner.send(CTRL_END_KEY_SEQUENCE)
        first_runner.wait_until(
            lambda text: FINAL_CANARY in text and oldest_history_head not in text,
            deadline.remaining(),
            "return to live tail immediately after direct resume history inspection",
            final_screen=True,
        )

        first_runner.type_text("/compact")
        first_runner.send("\r")
        first_runner.wait_until(
            lambda text: "Context compacted:" in text,
            deadline.remaining(),
            "semantic compaction apply receipt",
        )
        compact_audit = read_session_audit(source_path)
        if compact_audit.event_counts.get("compaction_applied_v2", 0) != 1:
            raise AcceptanceError("source session must contain exactly one compaction_applied_v2")
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        server = None
        server_thread = None

        first_runner.send(b"\x12")
        first_runner.wait_until(
            lambda text: "Restore Checkpoint" in text and "Reverse diff" in text,
            deadline.remaining(),
            "checkpoint reverse-diff modal",
            final_screen=True,
        )
        first_runner.send("\r")
        wait_for_session_audit(
            source_path,
            lambda audit: audit.event_counts.get("checkpoint_restored", 0) == 1,
            deadline.remaining(),
            "durable checkpoint restore completion",
        )
        first_runner.wait_until(
            lambda text: "Restored checkpoint files" in text,
            deadline.remaining(),
            "restored checkpoint card",
            final_screen=True,
        )
        if edit_file.read_text(encoding="utf-8") != ORIGINAL_CONTENT:
            raise AcceptanceError("checkpoint restore did not restore the original file")
        restored_hash = sha256_file(edit_file)

        first_runner.send(b"\x12")
        first_runner.wait_until(
            lambda text: "Restore Checkpoint" in text and "Blocked" in text and "fork" in text,
            deadline.remaining(),
            "blocked checkpoint modal with fork action",
            final_screen=True,
        )
        first_runner.send("F")
        fork_path = wait_for_fork_path(session_dir, source_path, deadline.remaining())
        fork_audit = wait_for_session_audit(
            fork_path,
            lambda audit: audit.event_counts.get("conversation_forked", 0) == 1
            and audit.final_canary_count == 1,
            deadline.remaining(),
            "durable conversation fork event",
        )
        first_runner.wait_until(
            lambda text: "Conversation fork created." in text,
            deadline.remaining(),
            "conversation fork timeline notice",
            final_screen=True,
        )
        if sha256_file(edit_file) != restored_hash:
            raise AcceptanceError("modal F changed the restored workspace file")
        if fork_audit.final_canary_count != 1:
            raise AcceptanceError("fork session did not copy exactly one final-answer canary")
        first_runner.quit(timeout=deadline.remaining(10.0))
        first_runner.stop()
        first_runner = None

        resume_runner = PtyRunner(
            [
                str(frozen_binary),
                "--config",
                str(config_path),
                "resume",
                str(source_path),
            ],
            workspace,
            env,
            output_dir / "resume-process.log",
        )
        resume_runner.start()
        wait_for_main_tui(resume_runner, deadline.remaining())
        resume_runner.type_text("/resume")
        selector_screen = resume_runner.wait_until(
            lambda text: "Resume session" in text,
            deadline.remaining(),
            "resume selector",
            final_screen=True,
        )
        if (
            fork_path.name not in selector_screen
            and "STATEFUL-EDIT-TURN" not in selector_screen
            and "stateful history turn 1" not in selector_screen
        ):
            raise AcceptanceError("resume selector did not expose the non-current fork")
        resume_runner.send("\r")
        fork_display_token = session_display_token(fork_path)
        resumed_screen = resume_runner.wait_until(
            lambda text: FINAL_CANARY in text and fork_display_token in text,
            deadline.remaining(),
            "active fork identity and resumed final reply",
            final_screen=True,
        )
        if count_on_screen(resumed_screen, FINAL_CANARY) != 1:
            raise AcceptanceError("resumed fork screen rendered the final reply more than once")
        if edit_file.read_text(encoding="utf-8") != ORIGINAL_CONTENT:
            raise AcceptanceError("session resume changed the restored workspace file")
        resume_runner.quit(timeout=deadline.remaining(10.0))
        resume_runner.stop()
        resume_runner = None

        final_source_audit = read_session_audit(source_path)
        final_fork_audit = read_session_audit(fork_path)
        if (
            final_source_audit.final_canary_count != 1
            or final_source_audit.failed_run_count != 0
            or final_source_audit.event_counts.get("compaction_applied_v2", 0) != 1
            or final_source_audit.event_counts.get("checkpoint_restored", 0) != 1
        ):
            raise AcceptanceError("source session failed terminal durable invariants")
        if (
            final_fork_audit.final_canary_count != 1
            or final_fork_audit.failed_run_count != 0
            or final_fork_audit.event_counts.get("conversation_forked", 0) != 1
        ):
            raise AcceptanceError("fork session failed terminal durable invariants")
        session_evidence = preserve_session_evidence(
            output_dir,
            source_path=source_path,
            fork_path=fork_path,
        )

        with fixture.lock:
            if fixture.protocol_errors:
                raise AcceptanceError(f"provider fixture errors: {fixture.protocol_errors}")
            requests = list(fixture.provider_requests)
            plan_requests = fixture.plan_requests
            semantic_summary_requests = fixture.semantic_summary_requests
            semantic_title_requests = fixture.semantic_title_requests
        if len(requests) != 6:
            raise AcceptanceError(
                f"expected 6 conversation provider requests, observed {len(requests)}"
            )
        if semantic_summary_requests != 1:
            raise AcceptanceError(
                "expected one billed semantic summary request, observed "
                f"{semantic_summary_requests}"
            )
        if semantic_title_requests != 1:
            raise AcceptanceError(
                "expected one semantic title request, observed "
                f"{semantic_title_requests}"
            )
        if plan_requests != 1:
            raise AcceptanceError(f"expected one plan provider request, observed {plan_requests}")
        if not requests[1]["has_queued_follow_up"]:
            raise AcceptanceError("queued follow-up was not injected into the active provider run")
        if not requests[3]["has_read_file"] or not requests[4]["has_cjk_tool_result"]:
            raise AcceptanceError("CJK read_file tool-call continuation contract was not observed")
        if not requests[4]["has_write_file"] or not requests[5]["has_tool_result"]:
            raise AcceptanceError("write_file tool-call continuation contract was not observed")

        checks = {
            "provider_request_count": len(requests),
            "plan_request_count": plan_requests,
            "queued_follow_up_observed": requests[1]["has_queued_follow_up"],
            "cjk_tool_result_observed": requests[4]["has_cjk_tool_result"],
            "live_final_reply_screen_count": 1,
            "resumed_final_reply_screen_count": 1,
            "source_final_answer_count": final_source_audit.final_canary_count,
            "fork_final_answer_count": final_fork_audit.final_canary_count,
            "compaction_applied_v2_count": final_source_audit.event_counts.get(
                "compaction_applied_v2", 0
            ),
            "checkpoint_restored_count": final_source_audit.event_counts.get(
                "checkpoint_restored", 0
            ),
            "conversation_forked_count": final_fork_audit.event_counts.get(
                "conversation_forked", 0
            ),
            "modal_f_preserved_file": True,
            "resumed_session_is_fork": True,
            "resume_preserved_file": True,
            "final_file_sha256": sha256_file(edit_file),
        }
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - persist actionable local evidence.
        print(f"stateful campaign failed: {type(error).__name__}: {error}", file=sys.stderr)
        notes.append(f"{type(error).__name__}; inspect the local PTY logs")
        return_code = 1
    finally:
        if fixture is not None:
            for release in fixture.response_release_events.values():
                release.set()
        if first_runner is not None:
            first_runner.stop()
        if resume_runner is not None:
            resume_runner.stop()
        if server is not None:
            server.shutdown()
            server.server_close()
        if server_thread is not None:
            server_thread.join(timeout=5)
        finished_at = utc_now()
        duration_ms = round((time.monotonic() - started) * 1000)
        manifest_path = write_manifest(
            output_dir,
            status=status,
            started_at=started_at,
            finished_at=finished_at,
            duration_ms=duration_ms,
            binary=identity,
            tokenizer=tokenizer_identity,
            checks=checks,
            session_evidence=session_evidence,
            artifact_policy=artifact_policy,
            notes=notes,
        )
        print(f"stateful campaign {status}: {manifest_path}")
        if fixture_root is not None:
            if status != "passed" or args.keep_fixture:
                print(f"fixture retained at {fixture_root}")
            else:
                shutil.rmtree(fixture_root)
    return return_code


if __name__ == "__main__":
    sys.exit(main())
