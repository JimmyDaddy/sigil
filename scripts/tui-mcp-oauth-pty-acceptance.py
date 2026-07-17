#!/usr/bin/env python3
"""Exercise MCP OAuth through the production Sigil TUI binary.

The script exposes a loopback OAuth/MCP fixture through a temporary Cloudflare
HTTPS tunnel. Sigil therefore uses its production destination guard, network
executor, native credential store, worker, and TUI paths. No provider request is
made. The success path revokes remotely before clearing; a failure cleanup guard
still clears the temporary native credential through a fresh production TUI.
"""

from __future__ import annotations

import argparse
import base64
import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)
OSC52_RE = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]+)\x07")
TUNNEL_URL_RE = re.compile(r"https://[-a-z0-9]+\.trycloudflare\.com")
LOCAL_TUNNEL_URL_RE = re.compile(r"https://[-a-z0-9]+\.loca\.lt")
MAX_REQUEST_BYTES = 1024 * 1024


def redact_transient_pty_output(data: bytes) -> bytes:
    """Remove secret-bearing clipboard payloads before persisting PTY output."""
    return OSC52_RE.sub(lambda _match: b"\x1b]52;c;<redacted>\x07", data)


@dataclass
class FixtureState:
    base_url: str = ""
    paths: list[str] = field(default_factory=list)
    mcp_methods: list[str] = field(default_factory=list)
    token_exchanged: bool = False
    bearer_seen: bool = False
    revoked: bool = False
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record_path(self, path: str) -> None:
        with self.lock:
            self.paths.append(path)

    def record_mcp(self, method: str, bearer_seen: bool) -> None:
        with self.lock:
            self.mcp_methods.append(method)
            self.bearer_seen = self.bearer_seen or bearer_seen

    def snapshot(self) -> tuple[list[str], list[str], bool, bool, bool]:
        with self.lock:
            return (
                list(self.paths),
                list(self.mcp_methods),
                self.token_exchanged,
                self.bearer_seen,
                self.revoked,
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

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler hook.
        path = urllib.parse.urlsplit(self.path).path
        self.fixture.record_path(path)
        if path == "/health":
            self._send_json(200, {"status": "ok"})
            return
        if path in (
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
        ):
            self._send_json(
                200,
                {
                    "resource": f"{self.fixture.base_url}/mcp",
                    "authorization_servers": [f"{self.fixture.base_url}/"],
                    "scopes_supported": ["mcp:tools"],
                },
            )
            return
        if path == "/.well-known/oauth-authorization-server":
            base = self.fixture.base_url
            self._send_json(
                200,
                {
                    "issuer": f"{base}/",
                    "authorization_endpoint": f"{base}/authorize",
                    "token_endpoint": f"{base}/token",
                    "revocation_endpoint": f"{base}/revoke",
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code", "refresh_token"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["none"],
                    "protected_resources": [f"{base}/mcp"],
                },
            )
            return
        if path == "/authorize":
            self._send_json(400, {"error": "the smoke uses the manual callback path"})
            return
        if path == "/mcp":
            self.send_response(405)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self._send_json(404, {"error": "unknown fixture route"})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler hook.
        path = urllib.parse.urlsplit(self.path).path
        self.fixture.record_path(path)
        if path == "/token":
            form = urllib.parse.parse_qs(self._read_body().decode("utf-8"))
            if form.get("grant_type") != ["authorization_code"]:
                self._send_json(400, {"error": "unexpected grant type"})
                return
            if not form.get("code_verifier") or form.get("code") != ["smoke-code"]:
                self._send_json(400, {"error": "missing PKCE verifier or code"})
                return
            with self.fixture.lock:
                self.fixture.token_exchanged = True
            self._send_json(
                200,
                {
                    "access_token": "sigil-oauth-smoke-access-token",
                    "refresh_token": "sigil-oauth-smoke-refresh-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": "mcp:tools",
                },
            )
            return
        if path == "/revoke":
            form = urllib.parse.parse_qs(self._read_body().decode("utf-8"))
            if not form.get("token"):
                self._send_json(400, {"error": "missing token"})
                return
            with self.fixture.lock:
                self.fixture.revoked = True
            self._send_json(200, {})
            return
        if path == "/mcp":
            bearer_seen = self.headers.get("Authorization") == (
                "Bearer sigil-oauth-smoke-access-token"
            )
            if not bearer_seen:
                self.send_response(401)
                self.send_header(
                    "WWW-Authenticate",
                    f'Bearer resource_metadata="{self.fixture.base_url}/.well-known/oauth-protected-resource/mcp"',
                )
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            payload = json.loads(self._read_body().decode("utf-8"))
            method = payload.get("method")
            if not isinstance(method, str):
                self._send_json(400, {"error": "missing MCP method"})
                return
            self.fixture.record_mcp(method, bearer_seen)
            request_id = payload.get("id")
            if method == "initialize":
                self._send_mcp_result(
                    request_id,
                    {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "oauth-smoke", "version": "1.0.0"},
                    },
                )
            elif method == "notifications/initialized":
                self.send_response(202)
                self.send_header("Content-Length", "0")
                self.end_headers()
            elif method == "tools/list":
                self._send_mcp_result(
                    request_id,
                    {
                        "tools": [
                            {
                                "name": "oauth_smoke",
                                "description": "OAuth acceptance fixture",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {},
                                    "additionalProperties": False,
                                },
                            }
                        ]
                    },
                )
            else:
                self._send_json(
                    200,
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32601, "message": "method not found"},
                    },
                )
            return
        self._send_json(404, {"error": "unknown fixture route"})

    def do_DELETE(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler hook.
        self.send_response(405)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length", "0"))
        if length < 0 or length > MAX_REQUEST_BYTES:
            raise ValueError("fixture request exceeds limit")
        return self.rfile.read(length)

    def _send_mcp_result(self, request_id: object, result: object) -> None:
        self._send_json(200, {"jsonrpc": "2.0", "id": request_id, "result": result})

    def _send_json(self, status: int, payload: object) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class PtyRunner:
    def __init__(
        self,
        command: list[str],
        cwd: Path,
        env: dict[str, str],
        raw_log_path: Path,
    ) -> None:
        self.command = command
        self.cwd = cwd
        self.env = env
        self.raw_log_path = raw_log_path
        self.master_fd: int | None = None
        self.process: subprocess.Popen[bytes] | None = None
        self.output = bytearray()

    def start(self) -> None:
        master_fd, slave_fd = pty.openpty()
        fcntl.ioctl(
            slave_fd,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", 42, 132, 0, 0),
        )
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
            time.sleep(0.001)

    def rendered(self) -> str:
        return strip_control(bytes(self.output))

    def rendered_since(self, offset: int) -> str:
        return strip_control(bytes(self.output[offset:]))

    def copied_url(self) -> str | None:
        matches = OSC52_RE.findall(bytes(self.output))
        if not matches:
            return None
        return base64.b64decode(matches[-1]).decode("utf-8")

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

    def wait_for_text(self, text: str, timeout: float) -> str:
        return self.wait_until(lambda rendered: text in rendered, timeout, repr(text))

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
        self.raw_log_path.write_bytes(redact_transient_pty_output(bytes(self.output)))


def strip_control(data: bytes) -> str:
    without_ansi = ANSI_RE.sub(b"", data)
    without_controls = bytes(
        byte for byte in without_ansi if byte in (9, 10, 13) or byte >= 32
    )
    return without_controls.decode("utf-8", errors="replace")


def repo_root() -> Path:
    output = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True)
    return Path(output.strip()).resolve()


def capture_tunnel_url(
    command: list[str], pattern: re.Pattern[str], timeout: float
) -> tuple[subprocess.Popen[str], str]:
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        start_new_session=True,
    )
    lines: list[str] = []
    lock = threading.Lock()

    def capture() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            with lock:
                lines.append(line)

    threading.Thread(target=capture, daemon=True).start()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            break
        with lock:
            joined = "".join(lines)
        match = pattern.search(joined)
        if match:
            return process, match.group(0)
        time.sleep(0.1)
    with lock:
        captured = "".join(lines)[-4000:]
    stop_process_tree(process)
    raise RuntimeError(f"tunnel command did not publish an HTTPS URL: {captured}")


def start_cloudflare_tunnel(
    cloudflared: str, port: int, timeout: float
) -> tuple[subprocess.Popen[str], str]:
    return capture_tunnel_url(
        [
            cloudflared,
            "tunnel",
            "--no-autoupdate",
            "--url",
            f"http://127.0.0.1:{port}",
        ],
        TUNNEL_URL_RE,
        timeout,
    )


def start_local_tunnel(
    npx: str, port: int, timeout: float
) -> tuple[subprocess.Popen[str], str]:
    return capture_tunnel_url(
        [npx, "--yes", "localtunnel", "--port", str(port)],
        LOCAL_TUNNEL_URL_RE,
        timeout,
    )


def stop_process_tree(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    for signal_number in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(process.pid, signal_number)
        except OSError:
            return
        try:
            process.wait(timeout=5)
            return
        except subprocess.TimeoutExpired:
            continue


def wait_for_tunnel(base_url: str, timeout: float) -> None:
    opener = urllib.request.build_opener()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with opener.open(f"{base_url}/health", timeout=5) as response:
                if response.status == 200:
                    return
        except Exception:  # noqa: BLE001 - the tunnel may still be connecting.
            pass
        time.sleep(0.25)
    raise TimeoutError("temporary HTTPS tunnel did not become reachable")


def write_config(path: Path, workspace: Path, state: Path, cache: Path, base_url: str) -> None:
    path.write_text(
        f'''[workspace]
root = "{workspace}"

[storage]
state_root = "{state}"
cache_root = "{cache}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 20

[permission]
mode = "danger-full-access"

[web]
enabled = true
network_mode = "allow"
proxy_mode = "environment"
allowed_ports = [443]

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = true

[providers.deepseek]
api_key = "oauth-smoke-provider-key"
strict_tools_mode = "off"

[[mcp_servers]]
name = "oauth-smoke"
transport = "streamable_http"
url = "{base_url}/mcp"
startup = "lazy"
required = false

[mcp_servers.oauth]
client_id = "sigil-oauth-smoke-client"
scopes = ["mcp:tools"]

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "allow"
egress_logging = true
''',
        encoding="utf-8",
    )


def looks_like_trust_gate(text: str) -> bool:
    lowered = text.lower()
    return "trust this workspace" in lowered or "workspace trust" in lowered


def looks_like_main_tui(text: str) -> bool:
    lowered = text.lower()
    return "agent:" in lowered and ("build" in lowered or "session" in lowered)


def open_oauth_modal(runner: PtyRunner, timeout: float) -> str:
    """Start one production TUI and open the configured MCP OAuth modal."""
    runner.start()
    initial = runner.wait_until(
        lambda text: looks_like_trust_gate(text) or looks_like_main_tui(text),
        min(timeout, 30.0),
        "initial TUI",
    )
    if looks_like_trust_gate(initial):
        runner.send("\r")
        runner.wait_until(looks_like_main_tui, min(timeout, 30.0), "trusted TUI")

    runner.type_text("/config")
    runner.send("\r")
    runner.wait_for_text("provider-specific FIM", min(timeout, 20.0))
    runner.send("\t" * 5)
    runner.wait_for_text("footer activate/refresh", min(timeout, 20.0))
    runner.send("\x1b[B")
    runner.read_available(0.25)
    runner.send("\r")
    return runner.wait_until(
        lambda text: any(
            marker in text
            for marker in (
                "Enter sign in",
                "State: SignedIn",
                "State: Failed",
                "State: RevokedLocallyRetained",
            )
        ),
        min(timeout, 30.0),
        "OAuth credential state",
    )


def clear_local_credential_after_failure(
    binary: Path,
    config: Path,
    workspace: Path,
    env: dict[str, str],
    raw_log: Path,
    timeout: float,
) -> bool:
    """Clear a possibly persisted smoke credential without using the failed TUI."""
    cleanup_log = raw_log.with_name(f"{raw_log.stem}-cleanup{raw_log.suffix}")
    cleanup = PtyRunner(
        [str(binary), "--config", str(config)], workspace, env, cleanup_log
    )
    try:
        modal = open_oauth_modal(cleanup, timeout)
        if "Credential: Missing" in modal:
            return True
        cleanup.send("l")
        cleanup.wait_until(
            lambda text: "Credential: Missing" in text
            and "State: AuthenticationRequired" in text,
            min(timeout, 30.0),
            "failure-path native credential cleanup",
        )
        return True
    finally:
        cleanup.stop()


def assert_subsequence(observed: list[str], expected: list[str]) -> None:
    position = 0
    for value in observed:
        if position < len(expected) and value == expected[position]:
            position += 1
    if position != len(expected):
        raise RuntimeError(f"MCP sequence incomplete: expected {expected}, observed {observed}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run MCP OAuth acceptance through the real Sigil TUI and native keyring."
    )
    parser.add_argument("--binary", type=Path, default=Path("target/debug/sigil"))
    parser.add_argument("--cloudflared", default=shutil.which("cloudflared"))
    parser.add_argument("--npx", default=shutil.which("npx"))
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument(
        "--output-dir", type=Path, default=Path(".repo-local-dev/tui-smoke")
    )
    parser.add_argument("--keep-workspace", action="store_true")
    parser.add_argument(
        "--fail-after-token-for-test", action="store_true", help=argparse.SUPPRESS
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = repo_root()
    binary = (args.binary if args.binary.is_absolute() else root / args.binary).resolve()
    if not binary.is_file():
        raise SystemExit(f"missing {binary}; run cargo build --locked -p sigil")
    if not args.cloudflared and not args.npx:
        raise SystemExit("cloudflared or npx localtunnel is required for the HTTPS path")

    output_dir = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%d-%H%M%S")
    raw_log = output_dir / f"tui-mcp-oauth-pty-acceptance-{timestamp}.log"
    report = output_dir / f"tui-mcp-oauth-pty-acceptance-{timestamp}.md"
    temp_root = Path(tempfile.mkdtemp(prefix="sigil-mcp-oauth-pty-"))
    workspace = temp_root / "workspace"
    state = temp_root / "state"
    cache = temp_root / "cache"
    workspace.mkdir()
    state.mkdir()
    cache.mkdir()

    fixture = FixtureState()
    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.fixture = fixture
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    tunnel: subprocess.Popen[str] | None = None
    runner: PtyRunner | None = None
    config: Path | None = None
    env: dict[str, str] | None = None
    status = "failed"
    return_code = 1
    credential_cleared = False
    tunnel_provider = "none"
    notes: list[str] = []
    try:
        tunnel_errors: list[str] = []
        base_url = ""
        starters: list[tuple[str, Callable[[], tuple[subprocess.Popen[str], str]]]] = []
        if args.cloudflared:
            starters.append(
                (
                    "cloudflared",
                    lambda: start_cloudflare_tunnel(
                        args.cloudflared,
                        int(server.server_address[1]),
                        min(args.timeout, 45.0),
                    ),
                )
            )
        if args.npx:
            starters.append(
                (
                    "localtunnel",
                    lambda: start_local_tunnel(
                        args.npx,
                        int(server.server_address[1]),
                        min(args.timeout, 45.0),
                    ),
                )
            )
        for candidate_name, starter in starters:
            candidate: subprocess.Popen[str] | None = None
            try:
                candidate, candidate_url = starter()
                fixture.base_url = candidate_url
                wait_for_tunnel(candidate_url, min(args.timeout, 45.0))
                tunnel = candidate
                base_url = candidate_url
                tunnel_provider = candidate_name
                break
            except Exception as error:  # noqa: BLE001 - try the next tunnel provider.
                if candidate is not None:
                    stop_process_tree(candidate)
                tunnel_errors.append(f"{candidate_name}: {error}")
        if tunnel is None:
            raise RuntimeError("; ".join(tunnel_errors))
        fixture.base_url = base_url
        config = temp_root / "sigil.toml"
        write_config(config, workspace, state, cache, base_url)
        env = os.environ.copy()
        env.update({"TERM": "xterm-256color"})
        runner = PtyRunner(
            [str(binary), "--config", str(config)], workspace, env, raw_log
        )
        modal = open_oauth_modal(runner, args.timeout)
        if "Enter sign in" not in modal:
            raise RuntimeError("fresh OAuth smoke unexpectedly found a stored credential")
        runner.send("\r")
        runner.wait_for_text("State: AwaitingCallback", min(args.timeout, 45.0))
        runner.send("c")
        runner.wait_until(
            lambda _text: runner is not None and runner.copied_url() is not None,
            min(args.timeout, 10.0),
            "OSC52 authorization URL",
        )
        authorization_url = runner.copied_url()
        if authorization_url is None:
            raise RuntimeError("authorization URL was not copied")
        parsed = urllib.parse.urlsplit(authorization_url)
        query = urllib.parse.parse_qs(parsed.query)
        state_value = query.get("state", [None])[0]
        redirect_uri = query.get("redirect_uri", [None])[0]
        if parsed.scheme != "https" or not state_value or not redirect_uri:
            raise RuntimeError("authorization URL is missing HTTPS, state, or redirect_uri")
        callback = f"{redirect_uri}?code=smoke-code&state={state_value}"
        runner.send("m")
        runner.type_text(callback)
        runner.send("\r")
        runner.wait_for_text("State: SignedIn", min(args.timeout, 45.0))
        if args.fail_after_token_for_test:
            raise RuntimeError("injected post-token failure for cleanup conformance")
        runner.wait_until(
            lambda _text: "tools/list" in fixture.snapshot()[1],
            min(args.timeout, 45.0),
            "OAuth-authenticated MCP activation",
        )
        runner.send("s")
        runner.wait_until(
            lambda text: "Remote revoke" in text and "Revoked" in text,
            min(args.timeout, 30.0),
            "revoked credential retained locally",
        )
        clear_offset = len(runner.output)
        runner.send("l")
        runner.wait_until(
            lambda _text: "Missing" in runner.rendered_since(clear_offset),
            min(args.timeout, 30.0),
            "native credential cleared",
        )
        credential_cleared = True

        paths, methods, token_exchanged, bearer_seen, revoked = fixture.snapshot()
        assert_subsequence(methods, ["initialize", "notifications/initialized", "tools/list"])
        if not token_exchanged or not bearer_seen or not revoked:
            raise RuntimeError(
                "token exchange, authenticated activation, and revocation must all complete"
            )
        if not any("oauth-protected-resource" in path for path in paths):
            raise RuntimeError("protected-resource metadata was not discovered")
        if "/.well-known/oauth-authorization-server" not in paths:
            raise RuntimeError("authorization-server metadata was not discovered")
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - preserve actionable smoke failure.
        notes.append(str(error))
    finally:
        if runner is not None:
            runner.stop()
            runner = None
        if raw_log.is_file() and OSC52_RE.search(raw_log.read_bytes()):
            notes.append("raw PTY log retained an OSC52 clipboard payload")
            status = "failed"
            return_code = 1
        _, _, token_exchanged, _, _ = fixture.snapshot()
        if token_exchanged and not credential_cleared:
            if config is None or env is None:
                notes.append("native credential cleanup could not reconstruct the TUI config")
            else:
                try:
                    credential_cleared = clear_local_credential_after_failure(
                        binary,
                        config,
                        workspace,
                        env,
                        raw_log,
                        args.timeout,
                    )
                except Exception as cleanup_error:  # noqa: BLE001 - make residue explicit.
                    notes.append(
                        f"native credential cleanup failed: {cleanup_error}"
                    )
        if token_exchanged and not credential_cleared:
            status = "failed"
            return_code = 1
        if tunnel is not None:
            stop_process_tree(tunnel)
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        paths, methods, token_exchanged, bearer_seen, revoked = fixture.snapshot()
        report.write_text(
            "\n".join(
                [
                    "# Sigil MCP OAuth Real-PTY Acceptance",
                    "",
                    f"Status: `{status}`",
                    f"Binary: `{binary}`",
                    f"HTTPS tunnel: `{tunnel_provider}`",
                    "",
                    "## Checks",
                    "",
                    f"- Protected-resource metadata: `{any('oauth-protected-resource' in path for path in paths)}`",
                    f"- Authorization-server metadata: `{'/.well-known/oauth-authorization-server' in paths}`",
                    f"- Authorization code exchange with PKCE: `{token_exchanged}`",
                    f"- Authenticated MCP bearer observed: `{bearer_seen}`",
                    f"- MCP methods: `{', '.join(methods) or '-'}`",
                    f"- Remote revocation: `{revoked}`",
                    f"- Native credential cleared in TUI: `{credential_cleared}`",
                    "",
                    "The report excludes authorization URLs, callback values, tokens, and fixture request bodies.",
                    *( ["", "## Notes", "", *(f"- {note}" for note in notes)] if notes else [] ),
                    "",
                ]
            ),
            encoding="utf-8",
        )
        print(f"wrote {report}")
        print(f"raw log {raw_log}")
        if status != "passed" or args.keep_workspace:
            print(f"workspace {workspace}")
        else:
            shutil.rmtree(temp_root)
    return return_code


if __name__ == "__main__":
    sys.exit(main())
