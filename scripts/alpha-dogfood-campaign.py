#!/usr/bin/env python3
"""Run privacy-bounded offline dogfood cases through an explicit Sigil binary."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from typing import Mapping, Sequence


SCHEMA_VERSION = 1
CASE_ORDER = ("context", "web", "feedback", "attention", "image")
CASE_SCRIPTS = {
    "context": "scripts/context-v1-binary-acceptance.py",
    "web": "scripts/tui-web-pty-acceptance.py",
    "feedback": "scripts/tui-feedback-pty-acceptance.py",
    "attention": "scripts/tui-attention-signals-pty-acceptance.py",
    "image": "scripts/image-attachment-v1-acceptance.py",
}
PASS_THROUGH_ENV = {
    "COLORTERM",
    "COMSPEC",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "PATH",
    "PATHEXT",
    "PYTHONIOENCODING",
    "SHELL",
    "SYSTEMROOT",
    "TERM",
    "TERMINFO",
    "TERMINFO_DIRS",
    "USER",
    "WINDIR",
}
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{12,40}$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
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


class CampaignError(RuntimeError):
    """Raised when campaign admission or evidence construction fails."""


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
class CaseResult:
    case_id: str
    status: str
    duration_ms: int
    evidence_dir: str
    runner_log: str
    reason: str | None = None

    def as_dict(self) -> dict[str, object]:
        payload: dict[str, object] = {
            "id": self.case_id,
            "status": self.status,
            "duration_ms": self.duration_ms,
            "evidence_dir": self.evidence_dir,
            "runner_log": self.runner_log,
        }
        if self.reason is not None:
            payload["reason"] = self.reason
        return payload


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_version_output(output: str, *, label: str, sha256: str) -> BinaryIdentity:
    fields: dict[str, str] = {}
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if not lines or not lines[0].startswith("sigil "):
        raise CampaignError("binary --version output is missing the sigil version line")
    fields["version"] = lines[0].removeprefix("sigil ").strip()
    for line in lines[1:]:
        key, separator, value = line.partition(":")
        if separator:
            fields[key.strip()] = value.strip()
    missing = [key for key in ("version", "commit", "target", "profile") if not fields.get(key)]
    if missing:
        raise CampaignError(f"binary --version output is missing fields: {', '.join(missing)}")
    commit = fields["commit"].lower()
    if not COMMIT_PATTERN.fullmatch(commit):
        raise CampaignError("binary --version commit is not a supported hexadecimal build id")
    if not SHA256_PATTERN.fullmatch(sha256):
        raise CampaignError("binary SHA-256 is not a lowercase hexadecimal digest")
    return BinaryIdentity(
        label=label,
        sha256=sha256,
        version=fields["version"],
        commit=commit,
        target=fields["target"],
        profile=fields["profile"],
    )


def assert_expected_identity(
    identity: BinaryIdentity,
    *,
    expected_version: str | None,
    expected_commit: str | None,
    expected_sha256: str | None,
) -> None:
    if expected_version is not None and identity.version != expected_version:
        raise CampaignError(
            f"binary version mismatch: expected {expected_version}, got {identity.version}"
        )
    if expected_commit is not None:
        normalized = expected_commit.lower()
        if not COMMIT_PATTERN.fullmatch(normalized):
            raise CampaignError("expected commit must contain 12 to 40 hexadecimal characters")
        if not normalized.startswith(identity.commit) and not identity.commit.startswith(normalized):
            raise CampaignError(
                f"binary commit mismatch: expected {normalized}, got {identity.commit}"
            )
    if expected_sha256 is not None:
        normalized = expected_sha256.lower()
        if not SHA256_PATTERN.fullmatch(normalized):
            raise CampaignError("expected SHA-256 must contain 64 hexadecimal characters")
        if identity.sha256 != normalized:
            raise CampaignError("binary SHA-256 mismatch")


def inspect_binary(path: Path) -> tuple[Path, BinaryIdentity]:
    try:
        resolved = path.expanduser().resolve(strict=True)
    except OSError as error:
        raise CampaignError(f"binary does not exist: {path.name}") from error
    if not resolved.is_file():
        raise CampaignError(f"binary is not a regular file: {resolved.name}")
    if not os.access(resolved, os.X_OK):
        raise CampaignError(f"binary is not executable: {resolved.name}")
    digest = sha256_file(resolved)
    try:
        completed = subprocess.run(
            [str(resolved), "--version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
            env=identity_environment(os.environ),
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise CampaignError("failed to execute binary --version") from error
    if completed.returncode != 0:
        raise CampaignError(f"binary --version failed with exit code {completed.returncode}")
    return resolved, parse_version_output(completed.stdout, label=resolved.name, sha256=digest)


def native_executable_format(path: Path) -> str | None:
    try:
        with path.open("rb") as executable:
            header = executable.read(64)
            if header.startswith(b"\x7fELF"):
                return "elf"
            if header[:4] in MACH_O_MAGICS:
                return "mach-o"
            if len(header) < 64 or not header.startswith(b"MZ"):
                return None
            pe_offset = int.from_bytes(header[60:64], byteorder="little")
            executable.seek(pe_offset)
            if executable.read(4) == b"PE\x00\x00":
                return "pe"
    except OSError as error:
        raise CampaignError("failed to inspect the executable format") from error
    return None


def freeze_binary(source: Path, destination_root: Path) -> tuple[Path, BinaryIdentity]:
    try:
        resolved = source.expanduser().resolve(strict=True)
    except OSError as error:
        raise CampaignError(f"binary does not exist: {source.name}") from error
    if not resolved.is_file():
        raise CampaignError(f"binary is not a regular file: {resolved.name}")
    if not os.access(resolved, os.X_OK):
        raise CampaignError(f"binary is not executable: {resolved.name}")
    if native_executable_format(resolved) is None:
        raise CampaignError(
            "binary is not a supported native Mach-O, ELF, or PE executable; select a "
            "standalone release, Homebrew, or platform-package binary"
        )
    frozen = destination_root / resolved.name
    try:
        shutil.copyfile(resolved, frozen)
        frozen.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    except OSError as error:
        raise CampaignError("failed to freeze the admitted binary") from error
    if native_executable_format(frozen) is None:
        raise CampaignError("frozen executable format changed during admission")
    return inspect_binary(frozen)


def identity_environment(source: Mapping[str, str]) -> dict[str, str]:
    environment = {
        key: value
        for key, value in source.items()
        if key in PASS_THROUGH_ENV or key.startswith("LC_")
    }
    environment.setdefault("PATH", os.defpath)
    return environment


def case_environment(source: Mapping[str, str], case_root: Path) -> dict[str, str]:
    environment = identity_environment(source)
    home = case_root / "home"
    config = case_root / "config"
    state = case_root / "state"
    cache = case_root / "cache"
    temp = case_root / "tmp"
    for directory in (home, config, state, cache, temp):
        directory.mkdir(parents=True, exist_ok=True)

    environment.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config),
            "XDG_STATE_HOME": str(state),
            "XDG_CACHE_HOME": str(cache),
            "TMPDIR": str(temp),
            "TMP": str(temp),
            "TEMP": str(temp),
            "HTTP_PROXY": "http://127.0.0.1:1",
            "HTTPS_PROXY": "http://127.0.0.1:1",
            "ALL_PROXY": "http://127.0.0.1:1",
            "http_proxy": "http://127.0.0.1:1",
            "https_proxy": "http://127.0.0.1:1",
            "all_proxy": "http://127.0.0.1:1",
            "NO_PROXY": "127.0.0.1,localhost,::1",
            "no_proxy": "127.0.0.1,localhost,::1",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
        }
    )
    return environment


def selected_cases(requested: Sequence[str] | None) -> list[str]:
    if not requested:
        return list(CASE_ORDER)
    unknown = sorted(set(requested) - set(CASE_ORDER))
    if unknown:
        raise CampaignError(f"unknown case ids: {', '.join(unknown)}")
    return [case_id for case_id in CASE_ORDER if case_id in requested]


def case_command(
    root: Path,
    case_id: str,
    binary: Path,
    evidence_dir: Path,
    timeout_seconds: int,
    *,
    skip_clipboard: bool,
) -> list[str]:
    script = root / CASE_SCRIPTS[case_id]
    if not script.is_file():
        raise CampaignError(f"case script is missing: {CASE_SCRIPTS[case_id]}")
    command = [
        sys.executable,
        str(script),
        "--binary",
        str(binary),
        "--output-dir",
        str(evidence_dir),
        "--timeout",
        str(timeout_seconds),
    ]
    if case_id == "image" and skip_clipboard:
        command.append("--skip-clipboard")
    return command


def posix_descendant_pids(root_pid: int) -> set[int]:
    if os.name != "posix":
        return set()
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
    if os.name != "posix":
        return False
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
        except ProcessLookupError:
            continue


def wait_for_processes(process_ids: set[int], timeout: float) -> set[int]:
    deadline = time.monotonic() + timeout
    remaining = set(process_ids)
    while remaining and time.monotonic() < deadline:
        remaining = {pid for pid in remaining if process_is_running(pid)}
        if remaining:
            time.sleep(0.05)
    return remaining


def terminate_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "posix":
        descendants = posix_descendant_pids(process.pid)
        try:
            os.killpg(process.pid, signal.SIGINT)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            descendants.update(posix_descendant_pids(process.pid))
            signal_processes(descendants, signal.SIGTERM)
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                descendants.update(posix_descendant_pids(process.pid))
                signal_processes(descendants, signal.SIGKILL)
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
        remaining = wait_for_processes(descendants, 2)
        if remaining:
            signal_processes(remaining, signal.SIGKILL)
            wait_for_processes(remaining, 2)
        return
    else:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


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
        raise CampaignError("output directory inside the repository must be git-ignored")
    raise CampaignError("failed to verify the repository output ignore policy")


def run_case(
    *,
    root: Path,
    output_dir: Path,
    case_id: str,
    binary: Path,
    timeout_seconds: int,
    skip_clipboard: bool,
    source_environment: Mapping[str, str],
) -> CaseResult:
    relative_dir = Path("cases") / case_id
    case_root = output_dir / relative_dir
    evidence_dir = case_root / "evidence"
    evidence_dir.mkdir(parents=True, exist_ok=False)
    log_path = case_root / "runner.log"
    command = case_command(
        root,
        case_id,
        binary,
        evidence_dir,
        timeout_seconds,
        skip_clipboard=skip_clipboard,
    )
    environment = case_environment(source_environment, case_root / "isolation")
    started = time.monotonic()
    timed_out = False
    with log_path.open("wb") as log:
        process = subprocess.Popen(
            command,
            cwd=root,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=os.name == "posix",
            creationflags=(
                subprocess.CREATE_NEW_PROCESS_GROUP
                if os.name == "nt" and hasattr(subprocess, "CREATE_NEW_PROCESS_GROUP")
                else 0
            ),
        )
        try:
            return_code = process.wait(timeout=timeout_seconds + 45)
        except subprocess.TimeoutExpired:
            timed_out = True
            terminate_process(process)
            return_code = process.returncode if process.returncode is not None else -1
    duration_ms = round((time.monotonic() - started) * 1000)
    if timed_out:
        status = "failed"
        reason = "outer campaign deadline exceeded"
    elif return_code == 0:
        status = "passed"
        reason = "clipboard explicitly skipped" if case_id == "image" and skip_clipboard else None
    else:
        status = "failed"
        reason = f"case exited with code {return_code}"
    return CaseResult(
        case_id=case_id,
        status=status,
        duration_ms=duration_ms,
        evidence_dir=relative_dir.joinpath("evidence").as_posix(),
        runner_log=relative_dir.joinpath("runner.log").as_posix(),
        reason=reason,
    )


def manifest_payload(
    *,
    status: str,
    started_at: str,
    finished_at: str,
    identity: BinaryIdentity,
    cases: Sequence[CaseResult],
    raw_artifacts: str = "local_only_under_selected_output",
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "campaign": "offline_alpha_dogfood",
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "binary": identity.as_dict(),
        "cases": [result.as_dict() for result in cases],
        "privacy": {
            "provider_endpoints": "case_owned_loopback_only",
            "ambient_proxy_routes": "redirected_to_closed_loopback_endpoint",
            "os_network_sandbox": False,
            "credentials_inherited": False,
            "absolute_paths_in_aggregate": False,
            "raw_case_artifacts": raw_artifacts,
        },
    }


def atomic_write(path: Path, content: str) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("x", encoding="utf-8") as destination:
        destination.write(content)
        destination.flush()
        os.fsync(destination.fileno())
    os.replace(temporary, path)


def render_summary(payload: Mapping[str, object]) -> str:
    binary = payload["binary"]
    assert isinstance(binary, dict)
    cases = payload["cases"]
    assert isinstance(cases, list)
    privacy = payload["privacy"]
    assert isinstance(privacy, dict)
    raw_artifacts = privacy["raw_case_artifacts"]
    raw_location = (
        "the git-ignored campaign output directory"
        if raw_artifacts == "local_only_under_git_ignored_output"
        else "the explicitly selected local output directory"
    )
    lines = [
        "# Alpha Dogfood Offline Campaign",
        "",
        f"Status: `{payload['status']}`",
        f"Version: `{binary['version']}`",
        f"Commit: `{binary['commit']}`",
        f"Target: `{binary['target']}`",
        f"Profile: `{binary['profile']}`",
        f"Binary SHA-256: `{binary['sha256']}`",
        "",
        "| Case | Status | Duration (ms) | Evidence |",
        "| --- | --- | ---: | --- |",
    ]
    for item in cases:
        assert isinstance(item, dict)
        lines.append(
            f"| {item['id']} | {item['status']} | {item['duration_ms']} | "
            f"`{item['evidence_dir']}` |"
        )
    lines.extend(
        [
            "",
            "The aggregate excludes absolute paths, prompts, provider material, credentials, "
            f"and session content. Raw case artifacts remain local under {raw_location}. "
            "Offline status comes from case-owned loopback endpoints and "
            "credential/config isolation; this runner does not claim an OS socket sandbox.",
            "",
        ]
    )
    return "\n".join(lines)


def write_evidence(output_dir: Path, payload: Mapping[str, object]) -> str:
    manifest_path = output_dir / "manifest.json"
    serialized = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    atomic_write(manifest_path, serialized)
    atomic_write(output_dir / "summary.md", render_summary(payload))
    digest = hashlib.sha256(serialized.encode("utf-8")).hexdigest()
    atomic_write(output_dir / "manifest.sha256", f"{digest}  manifest.json\n")
    return digest


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run isolated offline alpha dogfood cases through an explicit Sigil binary."
    )
    parser.add_argument(
        "--binary",
        type=Path,
        help="Standalone native Sigil executable from a release or installed package.",
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-commit")
    parser.add_argument("--expected-sha256")
    parser.add_argument("--case", action="append", choices=CASE_ORDER)
    parser.add_argument("--timeout", type=int, default=180, help="Per-case inner deadline.")
    parser.add_argument(
        "--skip-clipboard",
        action="store_true",
        help="Explicitly skip the image clipboard case on a headless/non-macOS host.",
    )
    parser.add_argument("--list-cases", action="store_true")
    args = parser.parse_args(argv)
    if args.list_cases:
        return args
    if args.binary is None:
        parser.error("--binary is required")
    if not 30 <= args.timeout <= 900:
        parser.error("--timeout must be between 30 and 900 seconds")
    chosen = selected_cases(args.case)
    if "image" in chosen and platform.system() != "Darwin" and not args.skip_clipboard:
        parser.error("non-macOS image dogfood requires explicit --skip-clipboard")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.list_cases:
        print("\n".join(CASE_ORDER))
        return 0

    root = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix="sigil-dogfood-binary-") as temporary:
        try:
            binary, identity = freeze_binary(args.binary, Path(temporary))
            assert_expected_identity(
                identity,
                expected_version=args.expected_version,
                expected_commit=args.expected_commit,
                expected_sha256=args.expected_sha256,
            )
            chosen = selected_cases(args.case)
            timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
            output_dir = args.output_dir or root / ".repo-local-dev" / "dogfood" / f"offline-{timestamp}"
            if not output_dir.is_absolute():
                output_dir = root / output_dir
            output_dir = output_dir.resolve(strict=False)
            artifacts_policy = raw_artifact_policy(root, output_dir)
            output_dir.mkdir(parents=True, exist_ok=False)
        except (CampaignError, OSError, subprocess.SubprocessError) as error:
            print(f"alpha dogfood admission failed: {error}", file=sys.stderr)
            return 2

        started_at = utc_now()
        results: list[CaseResult] = []
        for case_id in chosen:
            print(f"running alpha dogfood case: {case_id}", flush=True)
            try:
                result = run_case(
                    root=root,
                    output_dir=output_dir,
                    case_id=case_id,
                    binary=binary,
                    timeout_seconds=args.timeout,
                    skip_clipboard=args.skip_clipboard,
                    source_environment=os.environ,
                )
            except (CampaignError, OSError, subprocess.SubprocessError) as error:
                relative_dir = Path("cases") / case_id
                result = CaseResult(
                    case_id=case_id,
                    status="failed",
                    duration_ms=0,
                    evidence_dir=relative_dir.joinpath("evidence").as_posix(),
                    runner_log=relative_dir.joinpath("runner.log").as_posix(),
                    reason=f"runner failure: {type(error).__name__}",
                )
            results.append(result)
            print(f"alpha dogfood case {case_id}: {result.status}", flush=True)

        status = "passed" if all(result.status == "passed" for result in results) else "failed"
        payload = manifest_payload(
            status=status,
            started_at=started_at,
            finished_at=utc_now(),
            identity=identity,
            cases=results,
            raw_artifacts=artifacts_policy,
        )
        try:
            digest = write_evidence(output_dir, payload)
        except OSError as error:
            print(f"failed to write alpha dogfood evidence: {error}", file=sys.stderr)
            return 1
        print(f"alpha dogfood campaign: {status}")
        print(f"manifest SHA-256: {digest}")
        print(f"evidence: {output_dir}")
        return 0 if status == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
