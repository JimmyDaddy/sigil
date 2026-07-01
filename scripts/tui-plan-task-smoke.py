#!/usr/bin/env python3
"""Run an opt-in real-terminal smoke for `/plan -> task`.

This script intentionally drives the actual Sigil TUI in a pseudo-terminal and
uses the user's real provider configuration. It is not part of the default test
gate because it can spend provider tokens and depends on terminal/provider
availability.
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
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)


@dataclass
class SessionAudit:
    session_path: Path | None
    has_plan_draft: bool
    has_task_created_from_plan: bool
    has_completed_task_run: bool
    forbidden_markers: list[str]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a real TUI smoke for /plan -> task handoff.",
    )
    parser.add_argument(
        "--binary",
        type=Path,
        help="Run this sigil binary instead of `cargo run -p sigil --`.",
    )
    parser.add_argument(
        "--config",
        type=Path,
        help="Optional config path passed to sigil as `--config <path>`.",
    )
    parser.add_argument(
        "--workspace",
        type=Path,
        help="Workspace directory to use. Defaults to a fresh temporary workspace.",
    )
    parser.add_argument(
        "--state-dir",
        type=Path,
        help="SIGIL_STATE_HOME to use. Defaults to a temporary state directory.",
    )
    parser.add_argument(
        "--cache-dir",
        type=Path,
        help="SIGIL_CACHE_HOME to use. Defaults to a temporary cache directory.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-smoke"),
        help="Directory for raw log and Markdown report.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=240.0,
        help="Overall smoke timeout in seconds.",
    )
    parser.add_argument(
        "--prompt",
        default="/plan Fix README.md typo. Only change typoo to typo.",
        help="Prompt to submit to the TUI.",
    )
    parser.add_argument(
        "--keep-workspace",
        action="store_true",
        help="Keep the temporary workspace/state/cache directories after success.",
    )
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"],
        text=True,
    )
    return Path(output.strip()).resolve()


def command_for(args: argparse.Namespace, root: Path) -> list[str]:
    if args.binary is not None:
        binary = args.binary if args.binary.is_absolute() else root / args.binary
        command = [str(binary.resolve())]
    else:
        command = [
            "cargo",
            "run",
            "--manifest-path",
            str(root / "Cargo.toml"),
            "-p",
            "sigil",
            "--",
        ]
    if args.config is not None:
        command.extend(["--config", str(args.config)])
    return command


def strip_control(data: bytes) -> str:
    without_ansi = ANSI_RE.sub(b"", data)
    without_controls = bytes(
        byte for byte in without_ansi if byte in (9, 10, 13) or byte >= 32
    )
    return without_controls.decode("utf-8", errors="replace")


def find_session_logs(state_dir: Path) -> list[Path]:
    if not state_dir.exists():
        return []
    return sorted(
        state_dir.glob("workspaces/*/sessions/session-*.jsonl"),
        key=lambda path: path.stat().st_mtime,
        reverse=True,
    )


def read_jsonl(path: Path) -> list[dict]:
    records: list[dict] = []
    try:
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    except OSError:
        return []
    return records


def control_payload(record: dict, name: str) -> dict | None:
    control = (
        record.get("payload", {})
        .get("session_log_entry", {})
        .get("control", {})
    )
    value = control.get(name)
    return value if isinstance(value, dict) else None


def inspect_session(state_dir: Path) -> SessionAudit:
    session_path = next(iter(find_session_logs(state_dir)), None)
    if session_path is None:
        return SessionAudit(None, False, False, False, [])

    has_plan_draft = False
    has_task_created_from_plan = False
    has_completed_task_run = False
    forbidden_markers: list[str] = []
    raw_text = session_path.read_text(encoding="utf-8", errors="replace")
    for marker in (
        "workspace_mutation_detected",
        "unknown_dirty",
        "inconclusive",
        "resolve_unknown_dirty",
    ):
        if marker in raw_text:
            forbidden_markers.append(marker)

    for record in read_jsonl(session_path):
        event_type = record.get("event_type")
        if event_type == "plan_draft_created":
            has_plan_draft = True
        if event_type == "task_created_from_plan":
            has_task_created_from_plan = True
        task_run = control_payload(record, "task_run")
        if task_run and task_run.get("status") == "completed":
            has_completed_task_run = True

    return SessionAudit(
        session_path=session_path,
        has_plan_draft=has_plan_draft,
        has_task_created_from_plan=has_task_created_from_plan,
        has_completed_task_run=has_completed_task_run,
        forbidden_markers=forbidden_markers,
    )


def looks_like_trust_gate(text: str) -> bool:
    lower = text.lower()
    return (
        "trust this workspace" in lower
        or "workspace trust" in lower
        or "trust workspace" in lower
    )


def looks_like_main_tui_ready(text: str) -> bool:
    lower = text.lower()
    return "agent:" in lower and ("build" in lower or "session" in lower)


def looks_like_plan_ready(text: str) -> bool:
    lower = text.lower()
    return "plan ready" in lower and "create and run task" in lower


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

    def read_available(self, timeout: float = 0.05) -> bytes:
        if self.master_fd is None:
            return b""
        chunks = bytearray()
        while True:
            ready, _, _ = select.select([self.master_fd], [], [], timeout)
            if not ready:
                break
            try:
                chunk = os.read(self.master_fd, 8192)
            except OSError:
                break
            if not chunk:
                break
            chunks.extend(chunk)
            self.output.extend(chunk)
            timeout = 0.0
        return bytes(chunks)

    def send(self, text: str) -> None:
        if self.master_fd is None:
            raise RuntimeError("pty not started")
        os.write(self.master_fd, text.encode("utf-8"))

    def type_text(self, text: str) -> None:
        for char in text:
            self.send(char)
            time.sleep(0.002)

    def wait_until(
        self,
        predicate: Callable[[str], bool],
        timeout: float,
        description: str,
    ) -> str:
        deadline = time.monotonic() + timeout
        rendered = strip_control(bytes(self.output))
        while time.monotonic() < deadline:
            self.read_available(0.1)
            rendered = strip_control(bytes(self.output))
            if predicate(rendered):
                return rendered
            if self.process and self.process.poll() is not None:
                raise RuntimeError(
                    f"process exited while waiting for {description}: {self.process.returncode}"
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
                try:
                    os.killpg(self.process.pid, signal.SIGTERM)
                except OSError:
                    pass
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.process.pid, signal.SIGKILL)
                    except OSError:
                        pass
        if self.master_fd is not None:
            os.close(self.master_fd)
            self.master_fd = None
        self.raw_log_path.write_bytes(bytes(self.output))


def wait_for_audit(
    state_dir: Path,
    predicate: Callable[[SessionAudit], bool],
    timeout: float,
    description: str,
    tick: Callable[[], None] | None = None,
) -> SessionAudit:
    deadline = time.monotonic() + timeout
    audit = inspect_session(state_dir)
    while time.monotonic() < deadline:
        if tick is not None:
            tick()
        audit = inspect_session(state_dir)
        if predicate(audit):
            return audit
        time.sleep(0.5)
    raise TimeoutError(f"timed out waiting for {description}")


def write_report(
    report_path: Path,
    *,
    status: str,
    command: list[str],
    workspace: Path,
    state_dir: Path,
    cache_dir: Path,
    raw_log_path: Path,
    audit: SessionAudit,
    readme_text: str,
    notes: list[str],
) -> None:
    report_path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Sigil TUI Plan Task Smoke",
        "",
        f"Status: `{status}`",
        f"Workspace: `{workspace}`",
        f"State dir: `{state_dir}`",
        f"Cache dir: `{cache_dir}`",
        f"Raw log: `{raw_log_path}`",
        f"Session: `{audit.session_path or '-'}`",
        "",
        "## Checks",
        "",
        f"- Plan draft: `{audit.has_plan_draft}`",
        f"- Task created from plan: `{audit.has_task_created_from_plan}`",
        f"- Completed task run: `{audit.has_completed_task_run}`",
        f"- Forbidden mutation/readiness markers: `{', '.join(audit.forbidden_markers) or '-'}`",
        f"- README fixed: `{'typoo' not in readme_text and 'typo' in readme_text}`",
        "",
        "## Command",
        "",
        "```text",
        " ".join(command),
        "```",
        "",
        "## README",
        "",
        "```text",
        readme_text.rstrip(),
        "```",
    ]
    if notes:
        lines.extend(["", "## Notes", ""])
        lines.extend(f"- {note}" for note in notes)
    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    root = repo_root()
    timestamp = time.strftime("%Y%m%d-%H%M%S")
    output_dir = (root / args.output_dir).resolve() if not args.output_dir.is_absolute() else args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    raw_log_path = output_dir / f"tui-plan-task-smoke-{timestamp}.log"
    report_path = output_dir / f"tui-plan-task-smoke-{timestamp}.md"

    temp_root = Path(tempfile.mkdtemp(prefix="sigil-tui-plan-smoke-"))
    workspace = args.workspace.resolve() if args.workspace else temp_root / "workspace"
    state_dir = args.state_dir.resolve() if args.state_dir else temp_root / "state"
    cache_dir = args.cache_dir.resolve() if args.cache_dir else temp_root / "cache"
    workspace.mkdir(parents=True, exist_ok=True)
    state_dir.mkdir(parents=True, exist_ok=True)
    cache_dir.mkdir(parents=True, exist_ok=True)

    readme = workspace / "README.md"
    if not readme.exists():
        readme.write_text("# Demo\n\nThis line has typoo.\n", encoding="utf-8")

    command = command_for(args, root)
    env = os.environ.copy()
    env.update(
        {
            "SIGIL_STATE_HOME": str(state_dir),
            "SIGIL_CACHE_HOME": str(cache_dir),
            "TERM": env.get("TERM", "xterm-256color"),
        }
    )

    runner = PtyRunner(command, workspace, env, raw_log_path)
    audit = SessionAudit(None, False, False, False, [])
    notes: list[str] = []
    status = "failed"
    try:
        runner.start()
        runner.wait_until(
            lambda text: looks_like_trust_gate(text) or looks_like_main_tui_ready(text),
            args.timeout,
            "initial TUI screen",
        )
        if looks_like_trust_gate(strip_control(bytes(runner.output))):
            runner.send("\r")
            runner.wait_until(
                looks_like_main_tui_ready,
                min(30.0, args.timeout),
                "main TUI screen after workspace trust",
            )

        runner.type_text(args.prompt)
        runner.send("\r")

        audit = wait_for_audit(
            state_dir,
            lambda value: value.has_plan_draft,
            min(90.0, args.timeout),
            "PlanDraftCreated",
            tick=lambda: runner.read_available(0.01),
        )
        runner.wait_until(
            looks_like_plan_ready,
            min(30.0, args.timeout),
            "visible plan handoff prompt",
        )
        runner.send("\r")

        audit = wait_for_audit(
            state_dir,
            lambda value: value.has_task_created_from_plan and value.has_completed_task_run,
            args.timeout,
            "completed plan-derived task",
            tick=lambda: runner.read_available(0.01),
        )

        readme_text = readme.read_text(encoding="utf-8")
        if "typoo" in readme_text or "typo" not in readme_text:
            raise RuntimeError("README.md did not contain the expected typo fix")
        if audit.forbidden_markers:
            raise RuntimeError(
                "session contains forbidden mutation/readiness markers: "
                + ", ".join(audit.forbidden_markers)
            )
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - script reports all smoke failures.
        notes.append(str(error))
        readme_text = readme.read_text(encoding="utf-8", errors="replace") if readme.exists() else ""
        return_code = 1
    finally:
        runner.stop()
        audit = inspect_session(state_dir)
        readme_text = readme.read_text(encoding="utf-8", errors="replace") if readme.exists() else ""
        write_report(
            report_path,
            status=status,
            command=command,
            workspace=workspace,
            state_dir=state_dir,
            cache_dir=cache_dir,
            raw_log_path=raw_log_path,
            audit=audit,
            readme_text=readme_text,
            notes=notes,
        )
        print(f"wrote {report_path}")
        print(f"raw log {raw_log_path}")
        if status != "passed" or args.keep_workspace or args.workspace:
            print(f"workspace {workspace}")
            print(f"state {state_dir}")
        elif temp_root.exists():
            shutil.rmtree(temp_root)
    return return_code


if __name__ == "__main__":
    sys.exit(main())
