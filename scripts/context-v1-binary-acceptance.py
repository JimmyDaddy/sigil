#!/usr/bin/env python3
"""Verify Context V1 through the production Sigil binary and a loopback provider.

The acceptance workspace contains Rust, Python, JavaScript/JSX, TypeScript and Go
sources plus adversarial ignored, generated, secret-like, symlink and oversized
files. Sigil runs only through its public headless entrypoint; no test-only Rust
path is enabled. The loopback server records the exact provider request and
returns one deterministic SSE answer per run.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from dataclasses import asdict, dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


MAX_REQUEST_BYTES = 2 * 1024 * 1024
EXPECTED_POLICY = "warm_lsp_then_request_local_tree_sitter"
CONTEXT_ID_RE = re.compile(r"context:v1:[0-9a-f]{64}")
FORBIDDEN_SENTINELS = (
    "IGNORED_CONTEXT_SENTINEL",
    "GENERATED_CONTEXT_SENTINEL",
    "SECRET_CONTEXT_SENTINEL",
    "SYMLINK_CONTEXT_SENTINEL",
    "OVERSIZE_CONTEXT_SENTINEL",
    "context-v1-fixture-key",
)


@dataclass(frozen=True)
class AcceptanceCase:
    label: str
    prompt: str
    expected_path: str
    expected_symbol: str


CASES = (
    AcceptanceCase(
        "rust",
        "Explain the source implementation of rust_anchor without using tools.",
        "src/rust_service.rs",
        "rust_anchor",
    ),
    AcceptanceCase(
        "python",
        "Explain the source implementation of python_anchor without using tools.",
        "python/service.py",
        "python_anchor",
    ),
    AcceptanceCase(
        "javascript_jsx",
        "Explain the source implementation of JavascriptAnchor without using tools.",
        "web/client.jsx",
        "JavascriptAnchor",
    ),
    AcceptanceCase(
        "typescript",
        "Explain the source implementation of TypeScriptAnchor without using tools.",
        "src/typescript_service.ts",
        "TypeScriptAnchor",
    ),
    AcceptanceCase(
        "go",
        "Explain the source implementation of GoAnchor without using tools.",
        "go/service.go",
        "GoAnchor",
    ),
)


@dataclass
class RecordedRequest:
    path: str
    payload: dict[str, Any]


@dataclass
class FixtureState:
    requests: list[RecordedRequest] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record(self, path: str, payload: object) -> int:
        if not isinstance(payload, dict):
            raise ValueError("provider request must be a JSON object")
        with self.lock:
            self.requests.append(RecordedRequest(path, payload))
            return len(self.requests)

    def record_error(self, error: Exception) -> None:
        with self.lock:
            self.errors.append(f"{type(error).__name__}: {error}")

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

    def do_POST(self) -> None:  # noqa: N802 - required BaseHTTPRequestHandler hook.
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_REQUEST_BYTES:
                raise ValueError("provider request length is outside the acceptance bound")
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
            request_index = self.fixture.record(self.path, payload)
            if not self.path.endswith("/chat/completions"):
                self._send_json(404, {"error": "unexpected fixture route"})
                return
            answer = f"Context V1 fixture response {request_index}."
            body = (
                "data: "
                + json.dumps(
                    {
                        "choices": [
                            {
                                "delta": {"content": answer},
                                "finish_reason": "stop",
                            }
                        ]
                    },
                    separators=(",", ":"),
                )
                + "\n\ndata: [DONE]\n\n"
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:  # noqa: BLE001 - retain bounded fixture diagnostics.
            self.fixture.record_error(error)
            self._send_json(500, {"error": "fixture request failed"})

    def _send_json(self, status: int, payload: object) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


@dataclass
class CaseEvidence:
    label: str
    expected_path: str
    expected_symbol: str
    provider_request_sha256: str
    prefix_record_sha256: str
    context_id: str
    fallback_canary: bool


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run mixed-language Context V1 acceptance through the real Sigil binary."
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
        default=Path(".repo-local-dev/context-v1-acceptance"),
        help="Directory for the bounded JSON evidence report.",
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument(
        "--keep-temp",
        action="store_true",
        help="Keep the temporary workspace for local debugging.",
    )
    return parser.parse_args()


def write_workspace(workspace: Path) -> bool:
    sources = {
        "src/rust_service.rs": (
            "pub fn rust_anchor(value: &str) -> String {\n"
            "    format!(\"rust:{value}\")\n"
            "}\n"
        ),
        "python/service.py": (
            "def python_anchor(value: str) -> str:\n"
            "    return f\"python:{value}\"\n"
        ),
        "web/client.jsx": (
            "export function JavascriptAnchor({ value }) {\n"
            "  return <span>{`javascript:${value}`}</span>;\n"
            "}\n"
        ),
        "src/typescript_service.ts": (
            "export function TypeScriptAnchor(value: string): string {\n"
            "  return `typescript:${value}`;\n"
            "}\n"
        ),
        "go/service.go": (
            "package service\n\n"
            "func GoAnchor(value string) string {\n"
            "    return \"go:\" + value\n"
            "}\n"
        ),
        "ignored/ignored.py": (
            "def ignored_anchor():\n"
            "    return \"IGNORED_CONTEXT_SENTINEL\"\n"
        ),
        "dist/generated.ts": (
            "export const generatedAnchor = \"GENERATED_CONTEXT_SENTINEL\";\n"
        ),
        ".env.py": "SECRET_CONTEXT_SENTINEL = 'must-not-leave-workspace'\n",
    }
    workspace.mkdir(parents=True, exist_ok=True)
    (workspace / ".gitignore").write_text("ignored/\n", encoding="utf-8")
    for relative, body in sources.items():
        path = workspace / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    oversized = workspace / "src/oversized.ts"
    oversized.write_text(
        "export const filler = `" + ("x" * (192 * 1024)) + "`;\n"
        "export const hiddenAfterCap = \"OVERSIZE_CONTEXT_SENTINEL\";\n",
        encoding="utf-8",
    )

    symlink_created = False
    symlink_target = workspace.parent / "outside-symlink-target.py"
    symlink_target.write_text(
        "def linked_anchor():\n    return \"SYMLINK_CONTEXT_SENTINEL\"\n",
        encoding="utf-8",
    )
    symlink = workspace / "linked_service.py"
    try:
        symlink.symlink_to(symlink_target)
        symlink_created = True
    except OSError:
        pass
    return symlink_created


def write_config(path: Path, state_root: Path, cache_root: Path, port: int) -> None:
    path.write_text(
        "\n".join(
            (
                "[workspace]",
                'root = "."',
                "",
                "[storage]",
                f'state_root = "{toml_string(state_root)}"',
                f'cache_root = "{toml_string(cache_root)}"',
                "",
                "[agent]",
                'provider = "deepseek"',
                'model = "deepseek-v4-flash"',
                "tool_timeout_secs = 5",
                "",
                "[model_request]",
                "request_timeout_secs = 10",
                "",
                "[providers.deepseek]",
                f'base_url = "http://127.0.0.1:{port}"',
                f'beta_base_url = "http://127.0.0.1:{port}"',
                f'anthropic_base_url = "http://127.0.0.1:{port}"',
                'fim_model = "deepseek-v4-pro"',
                'api_key = "context-v1-fixture-key"',
                'strict_tools_mode = "auto"',
                "",
            )
        ),
        encoding="utf-8",
    )


def toml_string(path: Path) -> str:
    return str(path).replace("\\", "\\\\").replace('"', '\\"')


def context_message(payload: dict[str, Any]) -> str:
    messages = payload.get("messages")
    if not isinstance(messages, list):
        raise AssertionError("provider request did not contain messages")
    contexts = []
    for message in messages:
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if isinstance(content, str) and content.startswith("Sigil Context V1"):
            contexts.append(content)
    if len(contexts) != 1:
        raise AssertionError(
            f"expected one Context V1 provider message, observed {len(contexts)}"
        )
    return contexts[0]


def newest_session(before: set[Path], state_root: Path) -> Path:
    after = set(state_root.rglob("session-*.jsonl"))
    created = sorted(after - before)
    if len(created) != 1:
        raise AssertionError(f"expected one new session log, observed {len(created)}")
    return created[0]


def prefix_record(session_path: Path) -> tuple[str, str]:
    matches: list[tuple[str, dict[str, Any]]] = []
    for line in session_path.read_text(encoding="utf-8").splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if contains_key(record, "prefix_snapshot_captured"):
            matches.append((line, record))
    if len(matches) != 1:
        raise AssertionError(
            f"expected one durable prefix snapshot, observed {len(matches)}"
        )
    raw, record = matches[0]
    rendered = json.dumps(record, sort_keys=True, ensure_ascii=False)
    if "sigil_context_v1" not in rendered or "Sigil Context V1" not in rendered:
        raise AssertionError("durable prefix snapshot did not contain Context V1")
    if "sigil_context_v0" in rendered:
        raise AssertionError("new request unexpectedly persisted Context V0")
    context_id = find_context_id(record)
    return raw, context_id


def contains_key(value: object, expected: str) -> bool:
    if isinstance(value, dict):
        return expected in value or any(
            contains_key(child, expected) for child in value.values()
        )
    if isinstance(value, list):
        return any(contains_key(child, expected) for child in value)
    return False


def find_context_id(value: object) -> str:
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "id" and isinstance(child, str) and child.startswith("context:v1:"):
                return child
            found = find_context_id(child)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = find_context_id(child)
            if found:
                return found
    elif isinstance(value, str):
        matched = CONTEXT_ID_RE.search(value)
        if matched is not None:
            return matched.group(0)
    return ""


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def assert_forbidden_absent(*texts: str) -> None:
    combined = "\n".join(texts)
    leaked = [sentinel for sentinel in FORBIDDEN_SENTINELS if sentinel in combined]
    if leaked:
        raise AssertionError(f"forbidden fixture content reached request/session: {leaked}")


def run_case(
    binary: Path,
    config_path: Path,
    workspace: Path,
    state_root: Path,
    fixture: FixtureState,
    case: AcceptanceCase,
    timeout: float,
) -> CaseEvidence:
    request_index = len(fixture.requests)
    before_sessions = set(state_root.rglob("session-*.jsonl"))
    env = os.environ.copy()
    for key in (
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ):
        env.pop(key, None)
    env.update(
        {
            "SIGIL_API_KEY": "context-v1-fixture-key",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
    )
    completed = subprocess.run(
        [
            str(binary),
            "--config",
            str(config_path),
            "run",
            case.prompt,
            "--output",
            "json",
        ],
        cwd=workspace,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(
            f"{case.label} run failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    request = fixture.request_at(request_index)
    if request.path.endswith("/chat/completions") is False:
        raise AssertionError(f"unexpected provider path: {request.path}")
    serialized_request = json.dumps(request.payload, sort_keys=True, ensure_ascii=False)
    context = context_message(request.payload)
    for expected in (
        "sigil_context_v1",
        EXPECTED_POLICY,
        case.expected_path,
        case.expected_symbol,
        "repository_file",
        "lsp-context:unavailable",
    ):
        if expected not in context:
            raise AssertionError(f"{case.label} Context V1 omitted {expected!r}")

    session_path = newest_session(before_sessions, state_root)
    session_text = session_path.read_text(encoding="utf-8")
    prefix_raw, context_id = prefix_record(session_path)
    if not context_id.startswith("context:v1:"):
        raise AssertionError(f"{case.label} durable prefix omitted a Context V1 id")
    for expected in (case.expected_path, case.expected_symbol, context_id):
        if expected not in session_text:
            raise AssertionError(f"{case.label} durable session omitted {expected!r}")
    assert_forbidden_absent(serialized_request, session_text)

    return CaseEvidence(
        label=case.label,
        expected_path=case.expected_path,
        expected_symbol=case.expected_symbol,
        provider_request_sha256=sha256_text(serialized_request),
        prefix_record_sha256=sha256_text(prefix_raw),
        context_id=context_id,
        fallback_canary=True,
    )


def binary_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    args = parse_args()
    root = Path(__file__).resolve().parent.parent
    binary = args.binary if args.binary.is_absolute() else root / args.binary
    binary = binary.resolve()
    if not binary.is_file():
        print(f"sigil binary is missing: {binary}", file=sys.stderr)
        return 2
    output_dir = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    report_path = output_dir / "context-v1-binary-acceptance.json"

    fixture = FixtureState()
    server = FixtureServer(("127.0.0.1", 0), FixtureHandler)
    server.fixture = fixture
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    temp_root = Path(tempfile.mkdtemp(prefix="sigil-context-v1-acceptance-"))
    workspace = temp_root / "workspace"
    state_root = temp_root / "state"
    cache_root = temp_root / "cache"
    state_root.mkdir()
    cache_root.mkdir()
    symlink_created = write_workspace(workspace)
    config_path = workspace / "sigil.toml"
    write_config(config_path, state_root, cache_root, int(server.server_address[1]))

    status = "failed"
    evidence: list[CaseEvidence] = []
    error_message = ""
    try:
        for case in CASES:
            evidence.append(
                run_case(
                    binary,
                    config_path,
                    workspace,
                    state_root,
                    fixture,
                    case,
                    args.timeout,
                )
            )
        if fixture.errors:
            raise AssertionError(f"loopback fixture errors: {fixture.errors}")
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - write actionable local evidence on failure.
        error_message = f"{type(error).__name__}: {error}"
        return_code = 1
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        report = {
            "schema": "sigil_context_v1_binary_acceptance_v1",
            "status": status,
            "binary": str(binary),
            "binary_sha256": binary_sha256(binary),
            "binary_size_bytes": binary.stat().st_size,
            "production_entrypoint": "sigil run --output json",
            "loopback_only": True,
            "mixed_language_cases": [asdict(item) for item in evidence],
            "request_count": len(fixture.requests),
            "fallback_canary": all(item.fallback_canary for item in evidence),
            "forbidden_sentinels": list(FORBIDDEN_SENTINELS),
            "symlink_fixture_created": symlink_created,
            "fixture_errors": fixture.errors,
            "error": error_message or None,
        }
        report_path.write_text(
            json.dumps(report, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        if args.keep_temp:
            print(f"temporary acceptance workspace: {temp_root}")
        else:
            shutil.rmtree(temp_root, ignore_errors=True)

    print(f"Context V1 binary acceptance: {status}")
    print(f"report: {report_path}")
    if error_message:
        print(error_message, file=sys.stderr)
    return return_code


if __name__ == "__main__":
    raise SystemExit(main())
