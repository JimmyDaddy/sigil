#!/usr/bin/env python3
"""Contract tests for the stateful real-PTY dogfood harness."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("tui-stateful-pty-acceptance.py")
SPEC = importlib.util.spec_from_file_location("tui_stateful_pty_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def passed_checks() -> dict[str, object]:
    return {
        "provider_request_count": 6,
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
        "final_file_sha256": "e" * 64,
    }


def session_evidence() -> dict[str, dict[str, str]]:
    return {
        "source_session": {"path": "sessions/source.jsonl", "sha256": "f" * 64},
        "fork_session": {"path": "sessions/fork.jsonl", "sha256": "a" * 64},
    }


class VtScreenTests(unittest.TestCase):
    def test_final_screen_replaces_prior_full_screen_paints(self) -> None:
        canary = MODULE.FINAL_CANARY
        stream = (
            b"\x1b[?1049h\x1b[2J\x1b[H"
            + canary.encode()
            + b"\x1b[2J\x1b[Hupdated\r\n"
            + canary.encode()
        )
        screen = MODULE.VtScreen(rows=8, cols=100)
        screen.feed(stream)
        self.assertEqual(screen.text().count(canary), 1)
        self.assertIn("updated", screen.text())

    def test_cursor_position_and_line_erase_match_ratatui_style_updates(self) -> None:
        screen = MODULE.VtScreen(rows=4, cols=20)
        screen.feed(b"\x1b[2;4Hbefore\x1b[2;4H\x1b[Kafter")
        self.assertIn("after", screen.text())
        self.assertNotIn("before", screen.text())


class DurableAuditTests(unittest.TestCase):
    def test_final_answer_count_is_structural_not_raw_substring_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "session.jsonl"
            records = [
                {
                    "event_type": "session_entry_recorded",
                    "session_id": "session-1",
                    "payload": {
                        "session_log_entry": {
                            "assistant": {
                                "assistant_kind": "final_answer",
                                "content": MODULE.FINAL_CANARY,
                            }
                        }
                    },
                },
                {
                    "event_type": "compaction_applied_v2",
                    "session_id": "session-1",
                    "payload": {"checkpoint": {"model_notes": [MODULE.FINAL_CANARY]}},
                },
            ]
            path.write_text(
                "".join(json.dumps(record) + "\n" for record in records),
                encoding="utf-8",
            )
            audit = MODULE.read_session_audit(path)
            self.assertEqual(audit.final_canary_count, 1)
            self.assertEqual(audit.failed_run_count, 0)
            self.assertEqual(audit.event_counts["compaction_applied_v2"], 1)

    def test_session_display_token_matches_tui_file_identity(self) -> None:
        path = Path("/private/sessions/session-fork-1784188731396-2ed6aea8.jsonl")
        self.assertEqual(MODULE.session_display_token(path), "fork-178")


class AdmissionTests(unittest.TestCase):
    def test_tokenizer_install_requires_exact_checksum_and_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "tokenizer.json"
            source.write_text('{"fixture":true}', encoding="utf-8")
            digest = MODULE.sha256_file(source)
            identity = MODULE.install_tokenizer(
                source,
                root / "cache",
                snapshot="a" * 40,
                expected_sha256=digest,
            )
            self.assertEqual(identity["sha256"], digest)
            installed = (
                root
                / "cache/provider-profiles"
                / MODULE.MODEL_NAME
                / ("a" * 40)
                / "tokenizer.json"
            )
            self.assertEqual(installed.read_bytes(), source.read_bytes())
            with self.assertRaisesRegex(MODULE.AcceptanceError, "SHA-256 mismatch"):
                MODULE.install_tokenizer(
                    source,
                    root / "other-cache",
                    snapshot="a" * 40,
                    expected_sha256="0" * 64,
                )

    def test_expected_binary_identity_rejects_mismatch(self) -> None:
        identity = MODULE.BinaryIdentity(
            "sigil",
            "a" * 64,
            "0.0.1-alpha.4",
            "b" * 12,
            "aarch64-apple-darwin",
            "release",
        )
        args = argparse.Namespace(
            expected_version="0.0.1-alpha.5",
            expected_commit=None,
            expected_binary_sha256=None,
        )
        with self.assertRaisesRegex(MODULE.AcceptanceError, "version mismatch"):
            MODULE.assert_expected_identity(identity, args)

    def test_script_launcher_is_not_a_native_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            launcher = Path(temporary) / "sigil"
            launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            launcher.chmod(0o755)
            self.assertIsNone(MODULE.native_executable_format(launcher))

    def test_repository_output_must_be_git_ignored(self) -> None:
        root = MODULE.repo_root()
        ignored = (root / ".repo-local-dev/tui-stateful-acceptance/contract-test").resolve()
        self.assertEqual(
            MODULE.raw_artifact_policy(root, ignored),
            "local_only_under_git_ignored_output",
        )
        unignored = (root / "stateful-acceptance-unignored-contract-output").resolve()
        self.assertFalse(unignored.exists())
        with self.assertRaisesRegex(MODULE.AcceptanceError, "must be git-ignored"):
            MODULE.raw_artifact_policy(root, unignored)
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(
                MODULE.raw_artifact_policy(root, Path(temporary).resolve()),
                "local_only_under_selected_output",
            )

    def test_campaign_deadline_is_global_and_step_capped(self) -> None:
        deadline = MODULE.CampaignDeadline(0.02)
        self.assertLessEqual(deadline.remaining(0.005), 0.005)
        time.sleep(0.03)
        with self.assertRaisesRegex(TimeoutError, "overall deadline"):
            deadline.remaining()


class IsolationTests(unittest.TestCase):
    def test_default_compaction_phase_removes_custom_provider_routes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / "sigil.toml"
            common = {
                "workspace": root / "workspace",
                "state_root": root / "state",
                "cache_root": root / "cache",
                "session_dir": root / "sessions",
            }
            MODULE.write_config(config, **common, port=43123)
            self.assertIn('base_url = "http://127.0.0.1:43123"', config.read_text())
            MODULE.write_config(config, **common, port=None)
            default_config = config.read_text(encoding="utf-8")
            self.assertNotIn("base_url", default_config)
            self.assertIn('mode = "auto-edit"', default_config)

    def test_isolated_environment_drops_credentials_and_closes_ambient_proxy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.dict(
                os.environ,
                {"SIGIL_API_KEY": "secret", "HTTPS_PROXY": "http://private.invalid"},
                clear=False,
            ):
                env = MODULE.isolated_environment(Path(temporary))
            self.assertNotIn("SIGIL_API_KEY", env)
            self.assertEqual(env["HTTPS_PROXY"], "http://127.0.0.1:9")
            self.assertEqual(env["NO_PROXY"], "127.0.0.1,localhost")


@unittest.skipUnless(os.name == "posix", "PTY descendant cleanup requires POSIX")
class PtyLifecycleTests(unittest.TestCase):
    def test_stop_reaps_detached_descendant_after_parent_exit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pid_path = root / "descendant.pid"
            code = (
                "import pathlib, subprocess, sys, time; "
                "child=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], "
                "start_new_session=True); "
                "pathlib.Path(sys.argv[1]).write_text(str(child.pid)); time.sleep(0.5)"
            )
            runner = MODULE.PtyRunner(
                [sys.executable, "-c", code, str(pid_path)],
                root,
                dict(os.environ),
                root / "runner.log",
            )
            descendant_pid: int | None = None
            try:
                runner.start()
                deadline = time.monotonic() + 3
                while not pid_path.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(pid_path.exists())
                descendant_pid = int(pid_path.read_text())
                assert runner.process is not None
                runner.process.wait(timeout=3)
                runner.stop()
                self.assertFalse(MODULE.process_is_running(descendant_pid))
            finally:
                runner.stop()
                if descendant_pid is not None and MODULE.process_is_running(descendant_pid):
                    os.kill(descendant_pid, signal.SIGKILL)


class ManifestTests(unittest.TestCase):
    def test_manifest_uses_relative_evidence_and_excludes_session_content(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = MODULE.BinaryIdentity(
                "sigil",
                "a" * 64,
                "0.0.1-alpha.4",
                "b" * 12,
                "aarch64-apple-darwin",
                "release",
            )
            path = MODULE.write_manifest(
                root,
                status="passed",
                started_at="2026-07-16T00:00:00+00:00",
                finished_at="2026-07-16T00:00:01+00:00",
                duration_ms=1000,
                binary=identity,
                tokenizer={
                    "model": MODULE.MODEL_NAME,
                    "snapshot": "c" * 40,
                    "sha256": "d" * 64,
                },
                checks=passed_checks(),
                session_evidence=session_evidence(),
                artifact_policy="local_only_under_git_ignored_output",
                notes=[],
            )
            payload = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(payload["evidence"]["turns_pty_log"], "turns-process.log")
            self.assertEqual(payload["evidence"]["stateful_pty_log"], "stateful-process.log")
            self.assertEqual(payload["evidence"]["source_session"]["path"], "sessions/source.jsonl")
            serialized = path.read_text(encoding="utf-8")
            self.assertNotIn(MODULE.FINAL_CANARY, serialized)
            self.assertNotIn(str(root), serialized)

    def test_manifest_rejects_private_failure_notes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = MODULE.BinaryIdentity("sigil", "a" * 64, "v", "b" * 12, "t", "p")
            with self.assertRaisesRegex(MODULE.AcceptanceError, "private path"):
                MODULE.write_manifest(
                    root,
                    status="failed",
                    started_at="start",
                    finished_at="finish",
                    duration_ms=1,
                    binary=identity,
                    tokenizer={"model": MODULE.MODEL_NAME, "snapshot": "", "sha256": ""},
                    checks={},
                    session_evidence={},
                    artifact_policy="local_only_under_git_ignored_output",
                    notes=["failed under /Users/example/private"],
                )

    def test_passed_manifest_rejects_invalid_terminal_counts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = MODULE.BinaryIdentity("sigil", "a" * 64, "v", "b" * 12, "t", "p")
            checks = passed_checks()
            checks["fork_final_answer_count"] = 2
            with self.assertRaisesRegex(MODULE.AcceptanceError, "fork_final_answer_count"):
                MODULE.write_manifest(
                    root,
                    status="passed",
                    started_at="start",
                    finished_at="finish",
                    duration_ms=1,
                    binary=identity,
                    tokenizer={"model": MODULE.MODEL_NAME, "snapshot": "c" * 40, "sha256": "d" * 64},
                    checks=checks,
                    session_evidence=session_evidence(),
                    artifact_policy="local_only_under_git_ignored_output",
                    notes=[],
                )

    def test_preserved_session_evidence_is_byte_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            output.mkdir()
            source = root / "source.jsonl"
            fork = root / "fork.jsonl"
            source.write_text('{"event_type":"source"}\n', encoding="utf-8")
            fork.write_text('{"event_type":"fork"}\n', encoding="utf-8")
            evidence = MODULE.preserve_session_evidence(
                output,
                source_path=source,
                fork_path=fork,
            )
            for label, original in (("source_session", source), ("fork_session", fork)):
                copy = output / evidence[label]["path"]
                self.assertEqual(copy.read_bytes(), original.read_bytes())
                self.assertEqual(evidence[label]["sha256"], MODULE.sha256_file(original))


if __name__ == "__main__":
    unittest.main()
