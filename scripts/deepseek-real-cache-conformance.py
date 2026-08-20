#!/usr/bin/env python3
"""Run the explicitly authorized six-request DeepSeek cache campaign."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


TEST_NAME = (
    "provider::tests::"
    "real_agent_tool_turn_user_turn_and_resume_preserve_deepseek_cache_prefix"
)
REQUEST_CEILING = 6
SOURCE_PATHS = (
    "Cargo.toml",
    "Cargo.lock",
    "crates/sigil-kernel",
    "crates/sigil-provider-deepseek",
)
METRICS_PATTERN = re.compile(
    r"DeepSeek cache conformance: "
    r"requests=(?P<requests>\d+), "
    r"reservation_usd=(?P<reservation>\d+(?:\.\d+)?), "
    r"pricing_snapshot=(?P<pricing>[A-Za-z0-9._:-]+), "
    r"within_tool_turn=(?P<within>\d+(?:\.\d+)?), "
    r"cross_turn_resume=(?P<resume>\d+(?:\.\d+)?), "
    r"warm_pre_compaction=(?P<warm>\d+(?:\.\d+)?), "
    r"second_resume=(?P<second_resume>\d+(?:\.\d+)?), "
    r"cumulative_pre_compaction=(?P<cumulative>\d+(?:\.\d+)?), "
    r"first_post_compaction=(?P<post>\d+(?:\.\d+)?)"
)


class AdmissionError(RuntimeError):
    """Raised before any provider-capable process is started."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_checked(command: list[str], root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def git_head(root: Path) -> str:
    value = run_checked(["git", "rev-parse", "HEAD"], root).stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise AdmissionError("git HEAD is not a full commit identity")
    return value


def source_tree_digest(root: Path) -> tuple[str, int]:
    listing = run_checked(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            *SOURCE_PATHS,
        ],
        root,
    ).stdout.splitlines()
    paths = sorted(set(listing))
    if not paths:
        raise AdmissionError("cache conformance source inventory is empty")
    digest = hashlib.sha256()
    for relative in paths:
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise AdmissionError("cache conformance source inventory is not regular")
        encoded_path = relative.encode("utf-8")
        body = path.read_bytes()
        digest.update(len(encoded_path).to_bytes(8, "big"))
        digest.update(encoded_path)
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
    return digest.hexdigest(), len(paths)


def resolve_output_dir(root: Path, candidate: Path) -> Path:
    output_dir = candidate if candidate.is_absolute() else root / candidate
    output_dir = output_dir.resolve()
    allowed_root = (root / ".repo-local-dev").resolve()
    if output_dir == allowed_root or allowed_root not in output_dir.parents:
        raise AdmissionError("output directory must be below .repo-local-dev")
    cursor = allowed_root
    for part in output_dir.relative_to(allowed_root).parts:
        cursor = cursor / part
        if cursor.exists() and cursor.is_symlink():
            raise AdmissionError("output directory must not traverse a symbolic link")
    return output_dir


def build_test_binary(root: Path) -> Path:
    result = run_checked(
        [
            "cargo",
            "test",
            "-p",
            "sigil-provider-deepseek",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ],
        root,
    )
    executables: list[Path] = []
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == "sigil_provider_deepseek"
            and message.get("profile", {}).get("test") is True
            and isinstance(message.get("executable"), str)
        ):
            executables.append(Path(message["executable"]).resolve())
    unique = sorted(set(executables))
    if len(unique) != 1 or not unique[0].is_file():
        raise AdmissionError("could not resolve one exact DeepSeek test binary")
    return unique[0]


def package_version(root: Path) -> str:
    metadata = json.loads(
        run_checked(
            ["cargo", "metadata", "--no-deps", "--format-version=1"],
            root,
        ).stdout
    )
    versions = {
        package["version"]
        for package in metadata.get("packages", [])
        if package.get("name") == "sigil-provider-deepseek"
        and isinstance(package.get("version"), str)
    }
    if len(versions) != 1:
        raise AdmissionError("could not resolve the DeepSeek package version")
    return versions.pop()


def parse_metrics(output: str) -> dict[str, object]:
    matches = list(METRICS_PATTERN.finditer(output))
    if len(matches) != 1:
        raise AdmissionError("real-provider output did not contain one metrics record")
    values = matches[0].groupdict()
    return {
        "request_count": int(values["requests"]),
        "conservative_reservation_usd": values["reservation"],
        "pricing_snapshot": values["pricing"],
        "within_tool_turn_ratio": float(values["within"]),
        "cross_turn_resume_ratio": float(values["resume"]),
        "warm_pre_compaction_ratio": float(values["warm"]),
        "second_resume_ratio": float(values["second_resume"]),
        "cumulative_pre_compaction_ratio": float(values["cumulative"]),
        "first_post_compaction_ratio": float(values["post"]),
    }


def write_json_atomic(path: Path, value: object) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run and attest the paid six-request DeepSeek cache campaign.",
    )
    parser.add_argument(
        "--confirm-paid-provider",
        action="store_true",
        help="required acknowledgement that the command may spend provider credit",
    )
    parser.add_argument("--max-cost-usd", type=float, default=0.10)
    parser.add_argument("--min-hit-ratio", type=float, default=0.90)
    parser.add_argument("--timeout", type=float, default=720.0)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/deepseek-real-cache-conformance"),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = repo_root()
    started_at = utc_now()
    started = time.monotonic()
    try:
        if not args.confirm_paid_provider:
            raise AdmissionError("--confirm-paid-provider is required")
        if not (0.0 < args.max_cost_usd <= 1.0):
            raise AdmissionError("max cost must be in (0, 1]")
        if not (0.0 < args.min_hit_ratio <= 1.0):
            raise AdmissionError("minimum hit ratio must be in (0, 1]")
        if not (60.0 <= args.timeout <= 900.0):
            raise AdmissionError("timeout must be between 60 and 900 seconds")
        if not os.environ.get("SIGIL_API_KEY", "").strip():
            raise AdmissionError("SIGIL_API_KEY is required")
        output_dir = resolve_output_dir(root, args.output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)
        source_digest, source_file_count = source_tree_digest(root)
        revision = git_head(root)
        binary = build_test_binary(root)
        binary_digest = sha256_file(binary)
        version = package_version(root)
    except Exception as error:  # noqa: BLE001 - admission must remain pre-provider.
        print(f"DeepSeek cache campaign admission failed: {error}", file=sys.stderr)
        return 2

    environment = os.environ.copy()
    environment.update(
        {
            "SIGIL_REAL_PROVIDER_CACHE_CONFORMANCE": "1",
            "SIGIL_REAL_PROVIDER_MAX_COST_USD": f"{args.max_cost_usd:.8f}",
            "SIGIL_REAL_PROVIDER_MIN_CACHE_HIT_RATIO": f"{args.min_hit_ratio:.8f}",
        }
    )
    timed_out = False
    try:
        result = subprocess.run(
            [str(binary), "--ignored", "--exact", TEST_NAME, "--nocapture"],
            cwd=root,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
            timeout=args.timeout,
        )
        process_output = result.stdout
        return_code = result.returncode
    except subprocess.TimeoutExpired as error:
        timed_out = True
        return_code = None
        process_output = error.stdout or ""
        if isinstance(process_output, bytes):
            process_output = process_output.decode("utf-8", errors="replace")
    process_log = output_dir / "process.log"
    process_log.write_text(process_output, encoding="utf-8")
    status = "failed"
    metrics: dict[str, object] | None = None
    notes: list[str] = []
    if return_code == 0:
        try:
            metrics = parse_metrics(process_output)
            if metrics["request_count"] != REQUEST_CEILING:
                raise AdmissionError(
                    "passed campaign did not report exactly six requests"
                )
            status = "passed"
        except Exception as error:  # noqa: BLE001 - preserve a bounded failure class.
            notes.append(type(error).__name__)
    elif timed_out:
        notes.append("test_process_timed_out")
    else:
        notes.append("test_process_failed")

    configuration = {
        "provider": "deepseek",
        "route": "official_https",
        "model": "deepseek-v4-flash",
        "request_ceiling": REQUEST_CEILING,
        "max_cost_usd": f"{args.max_cost_usd:.8f}",
        "minimum_pre_compaction_hit_ratio": f"{args.min_hit_ratio:.8f}",
    }
    report = {
        "schema_version": 2,
        "campaign": "sigil-deepseek-real-agent-cache-conformance-v2",
        "status": status,
        "started_at": started_at,
        "finished_at": utc_now(),
        "duration_ms": int((time.monotonic() - started) * 1000),
        "source": {
            "git_head": revision,
            "tree_sha256": source_digest,
            "file_count": source_file_count,
        },
        "build": {
            "package_version": version,
            "test_binary_sha256": binary_digest,
        },
        "configuration": configuration,
        "configuration_sha256": sha256_bytes(
            json.dumps(configuration, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ),
        "scenario_source_sha256": sha256_file(
            root / "crates/sigil-provider-deepseek/src/tests/provider_tests.rs"
        ),
        "observations": metrics,
        "checks": {
            "hard_request_ceiling": "2+1+1+1+1",
            "durable_usage_snapshot_count": REQUEST_CEILING if status == "passed" else None,
            "exact_request_reconstruction": status == "passed",
            "tool_result_world_state_verified": status == "passed",
            "pre_compaction_prefix_preserved": status == "passed",
            "post_compaction_epoch_reset": status == "passed",
        },
        "evidence": {
            "process_log": process_log.name,
            "process_log_sha256": sha256_file(process_log),
        },
        "privacy": {
            "automatic_upload": False,
            "credentials_recorded": False,
            "raw_artifacts_local_only": True,
        },
        "notes": notes,
    }
    manifest = output_dir / "manifest.json"
    write_json_atomic(manifest, report)
    (output_dir / "manifest.sha256").write_text(
        f"{sha256_file(manifest)}  manifest.json\n",
        encoding="utf-8",
    )
    print(f"DeepSeek cache campaign {status}: {manifest}")
    return 0 if status == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
