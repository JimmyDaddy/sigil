#!/usr/bin/env python3
"""Prove provider cache telemetry through a real Sigil TUI binary and resume."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import ssl
import sys
import tempfile
import threading
import time


SUPPORT_SCRIPT = Path(__file__).with_name("tui-stateful-pty-acceptance.py")
SUPPORT_SPEC = importlib.util.spec_from_file_location("tui_cache_pty_support", SUPPORT_SCRIPT)
assert SUPPORT_SPEC is not None and SUPPORT_SPEC.loader is not None
SUPPORT = importlib.util.module_from_spec(SUPPORT_SPEC)
sys.modules[SUPPORT_SPEC.name] = SUPPORT
SUPPORT_SPEC.loader.exec_module(SUPPORT)

CACHE_RATIO_LABEL = "cache=5%"
FIRST_REPLY = "STATEFUL-HISTORY-1"
SECOND_REPLY = "STATEFUL-HISTORY-2"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the real TUI against a loopback DeepSeek cache-usage fixture.",
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--tokenizer-json", type=Path, required=True)
    parser.add_argument(
        "--expected-tokenizer-sha256",
        default=SUPPORT.TOKENIZER_SHA256,
    )
    parser.add_argument(
        "--tokenizer-snapshot",
        default=SUPPORT.TOKENIZER_SNAPSHOT,
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(".repo-local-dev/tui-cache-pty-acceptance"),
    )
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--keep-fixture", action="store_true")
    return parser.parse_args()


def usage_totals(session_path: Path) -> tuple[int, int, int]:
    usage_events = 0
    cache_hits = 0
    cache_misses = 0
    for line in session_path.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        payload = record.get("payload")
        entry = payload.get("session_log_entry") if isinstance(payload, dict) else None
        control = entry.get("control") if isinstance(entry, dict) else None
        usage = control.get("usage_snapshot") if isinstance(control, dict) else None
        if not isinstance(usage, dict):
            continue
        usage_events += 1
        cache_hits += int(usage.get("cache_hit_tokens", 0) or 0)
        cache_misses += int(usage.get("cache_miss_tokens", 0) or 0)
    return usage_events, cache_hits, cache_misses


def wait_for_settled_reply(
    runner: SUPPORT.PtyRunner,
    reply: str,
    cache_label: str,
    timeout: float,
) -> str:
    return runner.wait_until(
        lambda text: reply in text
        and cache_label in "".join(text.split())
        and not SUPPORT.looks_like_busy_tui(text),
        timeout,
        f"settled reply with {cache_label}",
        final_screen=True,
    )


def write_report(
    output_dir: Path,
    *,
    status: str,
    started_at: str,
    duration_ms: int,
    identity: SUPPORT.BinaryIdentity,
    artifact_policy: str,
    checks: dict[str, object],
    notes: list[str],
) -> Path:
    report = {
        "schema_version": 1,
        "campaign": "sigil-tui-cache-telemetry-v1",
        "status": status,
        "started_at": started_at,
        "finished_at": SUPPORT.utc_now(),
        "duration_ms": duration_ms,
        "binary": identity.as_dict(),
        "checks": checks,
        "evidence": {
            "live_pty_log": "live-process.log",
            "resume_pty_log": "resume-process.log",
            "session": "session.jsonl" if (output_dir / "session.jsonl").is_file() else None,
        },
        "privacy": {
            "automatic_upload": False,
            "raw_artifact_policy": artifact_policy,
            "raw_artifacts_local_only": True,
        },
        "notes": notes,
    }
    path = output_dir / "manifest.json"
    path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output_dir / "manifest.sha256").write_text(
        f"{SUPPORT.sha256_file(path)}  manifest.json\n",
        encoding="utf-8",
    )
    return path


def main() -> int:
    args = parse_args()
    root = SUPPORT.repo_root()
    output_dir = (
        args.output_dir if args.output_dir.is_absolute() else root / args.output_dir
    ).expanduser().resolve()
    try:
        artifact_policy = SUPPORT.raw_artifact_policy(root, output_dir)
        deadline = SUPPORT.CampaignDeadline(args.timeout)
    except Exception as error:  # noqa: BLE001 - bounded admission report.
        print(f"TUI cache campaign admission failed: {error}", file=sys.stderr)
        return 2
    output_dir.mkdir(parents=True, exist_ok=True)
    started_at = SUPPORT.utc_now()
    started = time.monotonic()
    fixture_root: Path | None = None
    server: SUPPORT.FixtureServer | None = None
    server_thread: threading.Thread | None = None
    live_runner: SUPPORT.PtyRunner | None = None
    resume_runner: SUPPORT.PtyRunner | None = None
    identity = SUPPORT.BinaryIdentity("unadmitted", "", "", "", "", "")
    checks: dict[str, object] = {}
    notes: list[str] = []
    status = "failed"
    try:
        binary, identity = SUPPORT.inspect_binary(
            args.binary,
            timeout=deadline.remaining(15.0),
        )
        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-tui-cache-pty-"))
        frozen_binary = SUPPORT.freeze_binary(binary, fixture_root, identity)
        workspace = fixture_root / "workspace"
        state_root = fixture_root / "state"
        cache_root = fixture_root / "cache"
        session_dir = fixture_root / "sessions"
        for directory in (workspace, state_root, cache_root, session_dir):
            directory.mkdir()
        SUPPORT.install_tokenizer(
            args.tokenizer_json,
            cache_root,
            snapshot=args.tokenizer_snapshot,
            expected_sha256=args.expected_tokenizer_sha256,
        )
        certificate, private_key = SUPPORT.generate_fixture_tls_identity(fixture_root)
        fixture = SUPPORT.FixtureState(cache_usage_enabled=True)
        server = SUPPORT.FixtureServer(("127.0.0.1", 0), SUPPORT.FixtureHandler)
        server.fixture = fixture
        tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_context.load_cert_chain(certfile=str(certificate), keyfile=str(private_key))
        server.tls_context = tls_context
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        port = int(server.server_address[1])
        environment = SUPPORT.isolated_environment(fixture_root, port)
        config_path = fixture_root / "sigil.toml"
        SUPPORT.write_config(
            config_path,
            workspace=workspace,
            state_root=state_root,
            cache_root=cache_root,
            session_dir=session_dir,
            port=port,
        )

        live_runner = SUPPORT.PtyRunner(
            [str(frozen_binary), "--config", str(config_path)],
            workspace,
            environment,
            output_dir / "live-process.log",
        )
        live_runner.start()
        SUPPORT.wait_for_main_tui(live_runner, deadline.remaining())
        fixture.release_response(1)
        live_runner.type_text("cache telemetry cold turn")
        live_runner.send("\r")
        wait_for_settled_reply(live_runner, FIRST_REPLY, "cache=0%", deadline.remaining())
        live_runner.type_text("cache telemetry warm turn")
        live_runner.send("\r")
        wait_for_settled_reply(live_runner, SECOND_REPLY, CACHE_RATIO_LABEL, deadline.remaining())

        sessions = SUPPORT.session_files(session_dir)
        if len(sessions) != 1:
            raise SUPPORT.AcceptanceError("cache campaign did not create exactly one session")
        session_path = sessions[0]
        usage_events, hits, misses = usage_totals(session_path)
        if (usage_events, hits, misses) != (2, 50_000, 950_000):
            raise SUPPORT.AcceptanceError("durable cache usage totals do not match the fixture")
        shutil.copyfile(session_path, output_dir / "session.jsonl")

        live_runner.quit(timeout=deadline.remaining(10.0))
        live_runner.stop()
        live_runner = None
        resume_runner = SUPPORT.PtyRunner(
            [
                str(frozen_binary),
                "--config",
                str(config_path),
                "resume",
                str(session_path),
            ],
            workspace,
            environment,
            output_dir / "resume-process.log",
        )
        resume_runner.start()
        SUPPORT.wait_for_main_tui(resume_runner, deadline.remaining())
        wait_for_settled_reply(
            resume_runner,
            SECOND_REPLY,
            CACHE_RATIO_LABEL,
            deadline.remaining(),
        )
        resume_runner.quit(timeout=deadline.remaining(10.0))
        resume_runner.stop()
        resume_runner = None
        with fixture.lock:
            if fixture.protocol_errors or len(fixture.provider_requests) != 2:
                raise SUPPORT.AcceptanceError("provider fixture lifecycle is invalid")
        checks = {
            "provider_request_count": 2,
            "usage_event_count": usage_events,
            "cache_hit_tokens": hits,
            "cache_miss_tokens": misses,
            "live_cache_ratio": CACHE_RATIO_LABEL,
            "resumed_cache_ratio": CACHE_RATIO_LABEL,
            "session_sha256": SUPPORT.sha256_file(output_dir / "session.jsonl"),
        }
        status = "passed"
    except Exception as error:  # noqa: BLE001 - preserve safe failure class only.
        notes.append(type(error).__name__)
        print(f"TUI cache campaign failed: {type(error).__name__}: {error}", file=sys.stderr)
    finally:
        for runner in (live_runner, resume_runner):
            if runner is not None:
                runner.stop()
        if server is not None:
            server.shutdown()
            server.server_close()
        if server_thread is not None:
            server_thread.join(timeout=5)
        if fixture_root is not None and not args.keep_fixture:
            shutil.rmtree(fixture_root, ignore_errors=True)
    report = write_report(
        output_dir,
        status=status,
        started_at=started_at,
        duration_ms=int((time.monotonic() - started) * 1000),
        identity=identity,
        artifact_policy=artifact_policy,
        checks=checks,
        notes=notes,
    )
    print(f"TUI cache campaign {status}: {report}")
    return 0 if status == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
