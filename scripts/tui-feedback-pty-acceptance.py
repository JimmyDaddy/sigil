#!/usr/bin/env python3
"""Exercise `/feedback` through the production TUI binary in an isolated PTY."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pty
import re
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path
from typing import Callable


ANSI_RE = re.compile(
    rb"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>])"
)
CANARIES = (
    "FEEDBACK-CREDENTIAL-CANARY-7421",
    "FEEDBACK-CONFIG-CANARY-9538",
    "FEEDBACK-WORKSPACE-CONTENT-CANARY-1864",
    "FEEDBACK-TERMINAL-PROGRAM-CANARY-3370",
    "private-feedback-endpoint-canary.invalid",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the real Sigil TUI /feedback privacy and export acceptance.",
    )
    parser.add_argument(
        "--binary",
        type=Path,
        help="Existing sigil binary. The script builds target/debug/sigil when omitted.",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-feedback-acceptance"),
        help="Directory for the bounded report and raw PTY log.",
    )
    parser.add_argument("--timeout", type=float, default=45.0)
    parser.add_argument(
        "--keep-fixture",
        action="store_true",
        help="Keep the isolated HOME, state, cache, and workspace after success.",
    )
    return parser.parse_args()


def repo_root() -> Path:
    output = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True
    )
    return Path(output.strip()).resolve()


def build_binary(root: Path) -> Path:
    git_hash = subprocess.check_output(
        ["git", "rev-parse", "--short=12", "HEAD"], cwd=root, text=True
    ).strip()
    target = next(
        line.removeprefix("host: ")
        for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
        if line.startswith("host: ")
    )
    env = os.environ.copy()
    env.update(
        {
            "SIGIL_BUILD_GIT_HASH": git_hash,
            "SIGIL_BUILD_TARGET": target,
            "SIGIL_BUILD_PROFILE": "debug",
        }
    )
    subprocess.run(
        ["cargo", "build", "--locked", "-p", "sigil"],
        cwd=root,
        env=env,
        check=True,
    )
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

    def settle(self, timeout: float = 2.0, quiet_period: float = 0.2) -> None:
        deadline = time.monotonic() + timeout
        quiet_since = time.monotonic()
        observed_size = len(self.output)
        while time.monotonic() < deadline:
            self.read_available(0.05)
            current_size = len(self.output)
            if current_size != observed_size:
                observed_size = current_size
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= quiet_period:
                return
        raise TimeoutError("PTY output did not settle")

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
            code = self.process.poll()
            if code is not None:
                return code
        raise TimeoutError("TUI did not exit after /quit; modal input may have leaked")

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
        if self.master_fd is not None:
            os.close(self.master_fd)
            self.master_fd = None
        self.raw_log.write_bytes(bytes(self.output))


def write_config(path: Path, workspace: Path, state_dir: Path, cache_dir: Path) -> None:
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
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[providers.deepseek]
base_url = "https://{CANARIES[4]}/v1"
beta_base_url = "https://{CANARIES[4]}/beta"
anthropic_base_url = "https://{CANARIES[4]}/anthropic"
api_key = "{CANARIES[1]}"
strict_tools_mode = "auto"
''',
        encoding="utf-8",
    )


def tree_snapshot(root: Path) -> dict[str, str]:
    snapshot: dict[str, str] = {}
    for path in [root, *sorted(root.rglob("*"))]:
        relative = "." if path == root else path.relative_to(root).as_posix()
        metadata = path.lstat()
        mode = stat.S_IMODE(metadata.st_mode)
        if path.is_symlink():
            snapshot[relative] = f"symlink:{mode:o}:{os.readlink(path)}"
        elif path.is_dir():
            snapshot[relative] = f"directory:{mode:o}"
        elif path.is_file():
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            snapshot[relative] = f"file:{mode:o}:{metadata.st_size}:{digest}"
        else:
            snapshot[relative] = f"other:{mode:o}"
    return snapshot


def wait_for_bundle(support_dir: Path, timeout: float) -> Path:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        bundles = sorted(support_dir.glob("sigil-support-*.json"))
        if len(bundles) == 1:
            return bundles[0]
        if len(bundles) > 1:
            raise RuntimeError("feedback export created more than one support bundle")
        time.sleep(0.05)
    raise TimeoutError("timed out waiting for the private support bundle")


def validate_bundle(bundle_path: Path, support_dir: Path, private_paths: list[Path]) -> dict:
    raw = bundle_path.read_text(encoding="utf-8")
    for canary in CANARIES:
        if canary in raw:
            raise RuntimeError(f"private canary leaked into support bundle: {canary}")
    for path in private_paths:
        if str(path) in raw:
            raise RuntimeError("private absolute path leaked into support bundle")
    bundle = json.loads(raw)
    if bundle.get("schema_version") != 1:
        raise RuntimeError("unexpected support bundle schema")
    doctor = bundle.get("doctor")
    if not isinstance(doctor, dict) or doctor.get("schema_version") != 1:
        raise RuntimeError("support bundle doctor projection is missing")
    checks = doctor.get("checks")
    if not isinstance(checks, list) or not checks:
        raise RuntimeError("support bundle doctor checks are missing")
    if any(check.get("name") == "other" for check in checks):
        raise RuntimeError("a production doctor check fell outside the support allowlist")
    build = doctor.get("build")
    if not isinstance(build, dict) or build.get("commit") in (None, "", "unknown"):
        raise RuntimeError("TUI did not receive exact binary build metadata")
    session = bundle.get("session")
    if not isinstance(session, dict) or "durable_entry_count" not in session:
        raise RuntimeError("coarse session summary is missing")
    if stat.S_IMODE(support_dir.stat().st_mode) != 0o700:
        raise RuntimeError("support bundle directory mode is not 0700")
    if stat.S_IMODE(bundle_path.stat().st_mode) != 0o600:
        raise RuntimeError("support bundle file mode is not 0600")
    return bundle


def write_report(
    path: Path,
    status: str,
    bundle: dict | None,
    error: str | None,
    checks: dict[str, bool],
) -> None:
    doctor = bundle.get("doctor", {}) if isinstance(bundle, dict) else {}
    report = {
        "status": status,
        "schema_version": bundle.get("schema_version") if bundle else None,
        "doctor_check_count": len(doctor.get("checks", [])),
        **checks,
        "error": error,
    }
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    if os.name != "posix":
        print("tui feedback PTY acceptance requires a POSIX host", file=sys.stderr)
        return 2
    args = parse_args()
    root = repo_root()
    output_dir = args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%d-%H%M%S")
    raw_log = output_dir / f"tui-feedback-pty-{timestamp}.log"
    report_path = output_dir / f"tui-feedback-pty-{timestamp}.json"
    fixture_root = Path(tempfile.mkdtemp(prefix="sigil-feedback-pty-"))
    workspace = fixture_root / "workspace"
    state_dir = fixture_root / "state"
    cache_dir = fixture_root / "cache"
    home_dir = fixture_root / "home"
    for directory in (workspace, state_dir, cache_dir, home_dir):
        directory.mkdir(parents=True, exist_ok=True)
    (workspace / "private.txt").write_text(CANARIES[2], encoding="utf-8")
    config_path = home_dir / "sigil.toml"
    write_config(config_path, workspace, state_dir, cache_dir)

    binary = (
        args.binary.resolve()
        if args.binary is not None
        else build_binary(root)
    )
    if not binary.is_file():
        raise RuntimeError(f"sigil binary is missing: {binary}")
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home_dir),
            "SIGIL_API_KEY": CANARIES[0],
            "SIGIL_CACHE_HOME": str(cache_dir),
            "SIGIL_STATE_HOME": str(state_dir),
            "TERM": "xterm-256color",
            "TERM_PROGRAM": CANARIES[3],
        }
    )
    runner = PtyRunner(
        [str(binary), "--config", str(config_path)], workspace, env, raw_log
    )
    bundle: dict | None = None
    error_text: str | None = None
    status = "failed"
    checks = {
        "preview_wrote_nothing": False,
        "state_and_cache_preview_unchanged": False,
        "state_tree_unchanged_after_export": False,
        "modal_input_exclusive": False,
        "privacy_canaries_absent": False,
    }
    try:
        runner.start()
        rendered = runner.wait_until(
            lambda text: "Workspace trust" in text or "agent:" in text,
            args.timeout,
            "initial TUI screen",
        )
        if "Workspace trust" in rendered:
            runner.send("\r")
            runner.wait_until(
                lambda text: "agent:" in text and "Build" in text,
                args.timeout,
                "main TUI after workspace trust",
            )
        runner.settle()
        state_before = tree_snapshot(state_dir)
        cache_before = tree_snapshot(cache_dir)

        runner.type_text("/feedback")
        runner.send("\r")
        runner.wait_until(
            lambda text: (
                "Feedback Report" in text
                and "Nothing has been written or uploaded" in text
                and "Enter export locally" in text
            ),
            args.timeout,
            "feedback privacy preview",
        )
        runner.settle()
        support_dir = cache_dir / "support-bundles"
        if support_dir.exists():
            raise RuntimeError("feedback preview wrote a support directory before Enter")
        if tree_snapshot(state_dir) != state_before:
            raise RuntimeError("feedback preview changed the isolated state tree")
        if tree_snapshot(cache_dir) != cache_before:
            raise RuntimeError("feedback preview changed the isolated cache tree")
        checks["preview_wrote_nothing"] = True
        checks["state_and_cache_preview_unchanged"] = True

        runner.send("\r")
        runner.wait_until(
            lambda text: (
                "Saved locally. Nothing was uploaded" in text
                and "Enter review JSON" in text
                and "B open bug form" in text
            ),
            args.timeout,
            "feedback export result",
        )
        runner.settle()
        bundle_path = wait_for_bundle(support_dir, args.timeout)
        if tree_snapshot(state_dir) != state_before:
            raise RuntimeError("feedback export changed the isolated state tree")
        checks["state_tree_unchanged_after_export"] = True
        bundle = validate_bundle(
            bundle_path,
            support_dir,
            [fixture_root, workspace, state_dir, cache_dir, home_dir, config_path],
        )
        if sorted(support_dir.iterdir()) != [bundle_path]:
            raise RuntimeError("feedback export left unexpected support files")
        checks["privacy_canaries_absent"] = True

        runner.send("\r")
        runner.wait_until(
            lambda text: (
                "Reviewing the exact redacted JSON saved locally" in text
                and '"schema_version": 1' in text
            ),
            args.timeout,
            "feedback JSON review",
        )
        runner.type_text("fi")
        runner.settle()
        runner.send("\x1b[27u")
        runner.settle()
        runner.send("\x1b[27u")
        runner.settle()
        runner.type_text("/quit")
        runner.send("\r")
        if runner.wait_for_exit(args.timeout) != 0:
            raise RuntimeError("TUI exited unsuccessfully after feedback acceptance")
        checks["modal_input_exclusive"] = True
        status = "passed"
        return_code = 0
    except Exception as error:  # noqa: BLE001 - acceptance reports bounded failures.
        error_text = str(error)
        return_code = 1
    finally:
        runner.close()
        write_report(report_path, status, bundle, error_text, checks)
        print(f"feedback PTY acceptance: {status}")
        print(f"report: {report_path}")
        print(f"raw log: {raw_log}")
        if status == "passed" and not args.keep_fixture:
            shutil.rmtree(fixture_root, ignore_errors=True)
        else:
            print(f"fixture kept: {fixture_root}")
    return return_code


if __name__ == "__main__":
    sys.exit(main())
