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
        "provider_request_count": 5,
        "plan_request_count": 1,
        "queued_follow_up_observed": True,
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

    def test_scrolling_region_scrolls_up_and_down_without_touching_outer_rows(self) -> None:
        screen = MODULE.VtScreen(rows=5, cols=5)
        screen.feed(
            b"\x1b[1;1HAAAAA"
            b"\x1b[2;1HBBBBB"
            b"\x1b[3;1HCCCCC"
            b"\x1b[4;1HDDDDD"
            b"\x1b[5;1HEEEEE"
            b"\x1b[2;4r\x1b[1S"
        )
        self.assertEqual(["".join(row) for row in screen.cells], [
            "AAAAA",
            "CCCCC",
            "DDDDD",
            "     ",
            "EEEEE",
        ])

        screen.feed(b"\x1b[1T")
        self.assertEqual(["".join(row) for row in screen.cells], [
            "AAAAA",
            "     ",
            "CCCCC",
            "DDDDD",
            "EEEEE",
        ])
        self.assertEqual(screen.history_text(), "")

    def test_top_anchored_scroll_region_records_native_scrollback_history(self) -> None:
        screen = MODULE.VtScreen(rows=3, cols=5)
        screen.feed(
            b"\x1b[1;1HAAAAA"
            b"\x1b[2;1HBBBBB"
            b"\x1b[3;1HCCCCC"
            b"\x1b[1;1r\x1b[1S\x1b[r"
        )

        self.assertEqual(screen.history_text(), "AAAAA")
        self.assertEqual(screen.full_text().splitlines(), ["AAAAA", "", "BBBBB", "CCCCC"])

    def test_erase_saved_lines_clears_native_scrollback_history(self) -> None:
        screen = MODULE.VtScreen(rows=2, cols=4)
        screen.feed(b"\x1b[1;1HKEEP\x1b[1;1r\x1b[1S\x1b[r")
        self.assertEqual(screen.history_text(), "KEEP")

        screen.feed(b"\x1b[3J")

        self.assertEqual(screen.history_text(), "")
        self.assertEqual(screen.text(), "\n")

    def test_runtime_resize_updates_geometry_without_inventing_history(self) -> None:
        screen = MODULE.VtScreen(rows=3, cols=5)
        screen.feed(b"\x1b[1;1HABCDE\x1b[2;1HFGHIJ\x1b[3;1HKLMNO")

        screen.resize(2, 3)
        self.assertEqual(["".join(row) for row in screen.cells], ["ABC", "FGH"])
        self.assertEqual(screen.history_text(), "")
        self.assertEqual((screen.scroll_top, screen.scroll_bottom), (0, 2))

        screen.resize(4, 6)
        self.assertEqual(["".join(row) for row in screen.cells], [
            "ABC   ",
            "FGH   ",
            "      ",
            "      ",
        ])
        self.assertEqual((screen.rows, screen.cols), (4, 6))

    def test_plan_queue_busy_idle_resize_sequence_preserves_only_native_history(self) -> None:
        screen = MODULE.VtScreen(rows=4, cols=20)
        screen.feed(
            b"\x1b[1;1HSTABLE-TRANSCRIPT"
            b"\x1b[2;1HPLAN-PREVIEW"
            b"\x1b[3;1HQUEUE-PENDING"
            b"\x1b[4;1HReplying..."
            b"\x1b[1;1r\x1b[1S\x1b[r"
        )

        screen.resize(3, 16)
        screen.feed(b"\x1b[3;1HIdle\x1b[K")
        screen.resize(6, 28)

        self.assertEqual(screen.history_text(), "STABLE-TRANSCRIPT")
        self.assertNotIn("PLAN-PREVIEW", screen.history_text())
        self.assertNotIn("QUEUE-PENDING", screen.history_text())
        self.assertIn("Idle", screen.text())

    def test_empty_decstbm_resets_scrolling_region_to_the_full_screen(self) -> None:
        screen = MODULE.VtScreen(rows=4, cols=4)
        screen.feed(
            b"\x1b[1;1HAAAA"
            b"\x1b[2;1HBBBB"
            b"\x1b[3;1HCCCC"
            b"\x1b[4;1HDDDD"
            b"\x1b[2;3r\x1b[r\x1b[1S"
        )
        self.assertEqual(["".join(row) for row in screen.cells], [
            "BBBB",
            "CCCC",
            "DDDD",
            "    ",
        ])

    def test_inline_scroll_exposes_multi_character_cell_overwriting_the_next_row(self) -> None:
        def next_row_after_insert(draw: bytes) -> str:
            screen = MODULE.VtScreen(rows=2, cols=5)
            screen.feed(
                b"\x1b[2;1HGUARD\x1b[1;1H"
                + draw
                + b"\x1b[1;1r\x1b[1S\x1b[r"
            )
            return "".join(screen.cells[1])

        self.assertEqual(next_row_after_insert(b"ABCDE"), "GUARD")
        self.assertEqual(next_row_after_insert(b"ABCDE    "), "    D")

    def test_decawm_private_mode_controls_pending_wrap(self) -> None:
        screen = MODULE.VtScreen(rows=2, cols=4)
        screen.feed(b"\x1b[2;1HKEEP\x1b[?7l\x1b[1;1HABCDX")
        self.assertEqual(["".join(row) for row in screen.cells], ["ABCX", "KEEP"])

        screen.feed(b"\x1b[?7h\x1b[1;1HWXYZ!")
        self.assertEqual(["".join(row) for row in screen.cells], ["WXYZ", "!EEP"])

    def test_feed_preserves_utf8_and_csi_state_across_chunks(self) -> None:
        screen = MODULE.VtScreen(rows=2, cols=8)
        encoded = "你".encode("utf-8")

        screen.feed(b"\x1b[2;1HGUARD\x1b[")
        screen.feed(b"1;1H" + encoded[:2])
        screen.feed(encoded[2:] + b"OK")

        self.assertEqual(screen.text().splitlines(), ["你 OK", "GUARD"])


class PtyRunnerTerminalContractTests(unittest.TestCase):
    def test_cursor_position_query_is_answered_across_chunk_boundaries(self) -> None:
        runner = MODULE.PtyRunner(
            ["sigil"],
            Path("."),
            {},
            Path("unused.log"),
        )

        self.assertEqual(runner._terminal_query_responses(b"prefix\x1b["), [])
        self.assertEqual(
            runner._terminal_query_responses(b"6nsuffix"),
            [MODULE.CURSOR_POSITION_RESPONSE],
        )
        self.assertEqual(runner.cpr_request_count, 1)

    def test_native_inline_scrollback_requires_a_real_scroll_region_sequence(self) -> None:
        runner = MODULE.PtyRunner(
            ["sigil"],
            Path("."),
            {},
            Path("unused.log"),
        )
        runner.output.extend(b"\x1b[3;8r\x1b[2S\x1b[r")

        self.assertTrue(runner.observed_native_inline_scrollback())

        runner.output.clear()
        runner.output.extend(b"\x1b[3;8r\x1b[2S")
        self.assertFalse(runner.observed_native_inline_scrollback())

    def test_terminal_replay_applies_resize_at_the_recorded_output_boundary(self) -> None:
        runner = MODULE.PtyRunner(
            ["sigil"],
            Path("."),
            {},
            Path("unused.log"),
            rows=3,
            cols=5,
        )
        runner.output.extend(b"\x1b[1;1HAAAAA\x1b[1;1r\x1b[1S\x1b[r")
        runner._terminal_resizes.append(MODULE.TerminalResize(len(runner.output), 2, 4))
        runner.output.extend(b"\x1b[2;1HDONE")

        terminal = runner.terminal()

        self.assertEqual((terminal.rows, terminal.cols), (2, 4))
        self.assertEqual(terminal.history_text(), "AAAAA")
        self.assertEqual(terminal.text().splitlines(), ["", "DONE"])

    def test_terminal_replay_preserves_split_csi_and_utf8_across_resize_boundaries(self) -> None:
        runner = MODULE.PtyRunner(
            ["sigil"],
            Path("."),
            {},
            Path("unused.log"),
            rows=2,
            cols=8,
        )
        encoded = "你".encode("utf-8")
        runner.output.extend(b"\x1b[2;1HGUARD\x1b[")
        runner._terminal_resizes.append(MODULE.TerminalResize(len(runner.output), 2, 8))
        runner.output.extend(b"1;1H" + encoded[:2])
        runner._terminal_resizes.append(MODULE.TerminalResize(len(runner.output), 2, 8))
        runner.output.extend(encoded[2:] + b"OK")

        self.assertEqual(runner.screen().splitlines(), ["你 OK", "GUARD"])

    @unittest.skipUnless(os.name == "posix", "runtime PTY resize requires POSIX")
    def test_runtime_resize_updates_pty_and_delivers_sigwinch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            code = "\n".join(
                (
                    "import os, signal, time",
                    "resized = False",
                    "def on_resize(_signum, _frame):",
                    "    global resized",
                    "    resized = True",
                    "signal.signal(signal.SIGWINCH, on_resize)",
                    "size = os.get_terminal_size()",
                    "print(f'READY:{size.lines}x{size.columns}', flush=True)",
                    "deadline = time.monotonic() + 3",
                    "while time.monotonic() < deadline:",
                    "    if resized:",
                    "        size = os.get_terminal_size()",
                    "        print(f'RESIZED:{size.lines}x{size.columns}', flush=True)",
                    "        break",
                    "    time.sleep(0.01)",
                )
            )
            runner = MODULE.PtyRunner(
                [sys.executable, "-c", code],
                root,
                dict(os.environ),
                root / "resize.log",
            )
            try:
                runner.start()
                runner.wait_until(
                    lambda text: "READY:42x140" in text,
                    3,
                    "initial PTY geometry",
                )
                runner.resize(18, 72)
                runner.wait_until(
                    lambda text: "RESIZED:18x72" in text,
                    3,
                    "SIGWINCH redraw geometry",
                )
                self.assertEqual((runner.terminal().rows, runner.terminal().cols), (18, 72))
            finally:
                runner.stop()


class StatefulSurfaceCampaignTests(unittest.TestCase):
    def test_escape_key_uses_a_complete_unambiguous_terminal_sequence(self) -> None:
        self.assertNotEqual(MODULE.ESCAPE_KEY_SEQUENCE, b"\x1b")
        self.assertEqual(MODULE.ESCAPE_KEY_SEQUENCE, b"\x1b[27u")

    def test_plan_rejection_retries_the_complete_escape_sequence(self) -> None:
        runner = mock.Mock()
        runner.wait_until.side_effect = [TimeoutError("redraw race"), "dismissed"]

        self.assertEqual(MODULE.reject_visible_plan_preview(runner, 2), "dismissed")
        self.assertEqual(
            runner.send.call_args_list,
            [
                mock.call(MODULE.ESCAPE_KEY_SEQUENCE),
                mock.call(MODULE.ESCAPE_KEY_SEQUENCE),
            ],
        )
        self.assertEqual(runner.wait_until.call_count, 2)
        for call in runner.wait_until.call_args_list:
            self.assertTrue(call.kwargs["final_screen"])

    def test_plan_fixture_recognizes_legacy_plan_mode_and_builds_a_schema_v2_draft(
        self,
    ) -> None:
        payload = {
            "messages": [
                {
                    "role": "system",
                    "content": f"{MODULE.PLAN_MODE_MARKER} Inspect first.",
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {"name": "submit_plan_draft"},
                }
            ]
        }

        self.assertTrue(MODULE.is_plan_mode_request(payload))
        self.assertTrue(MODULE.advertises_tool(payload, "submit_plan_draft"))
        draft = json.loads(MODULE.PLAN_DRAFT_ARGUMENTS)
        self.assertEqual(draft["schema_version"], 2)
        self.assertEqual(draft["summary"], MODULE.PLAN_SUMMARY_CANARY)
        self.assertEqual(len(draft["steps"]), 1)
        self.assertIn("```sigil-plan-v2", MODULE.PLAN_DRAFT_TEXT)
        self.assertIn(MODULE.PLAN_SUMMARY_CANARY, MODULE.PLAN_DRAFT_TEXT)

    def test_plan_fixture_does_not_misclassify_an_ordinary_conversation(self) -> None:
        self.assertFalse(
            MODULE.is_plan_mode_request(
                {"messages": [{"role": "user", "content": MODULE.PLAN_PROMPT}]}
            )
        )

    def test_plan_preview_waits_for_the_run_to_be_idle_before_escape(self) -> None:
        preview = f"Plan ready · {MODULE.PLAN_SUMMARY_CANARY}"
        self.assertFalse(MODULE.looks_like_ready_plan_preview(f"{preview}\nReplying..."))
        self.assertFalse(MODULE.looks_like_ready_plan_preview(f"{preview}\nThinking..."))
        self.assertFalse(
            MODULE.looks_like_ready_plan_preview(f"{preview}\nreceiving response")
        )
        self.assertTrue(MODULE.looks_like_ready_plan_preview(preview))
        self.assertFalse(MODULE.looks_like_dismissed_plan_preview(preview))
        self.assertTrue(
            MODULE.looks_like_dismissed_plan_preview(
                f"{MODULE.PLAN_SUMMARY_CANARY}\nBuild · agent: main"
            )
        )

    def test_visible_queued_follow_up_accepts_the_rendered_item_detail(self) -> None:
        rendered = (
            f"{MODULE.QUEUED_FOLLOW_UP_PROMPT}\n"
            "pending · main · follow-up"
        )

        self.assertTrue(MODULE.looks_like_visible_queued_follow_up(rendered))
        self.assertFalse(
            MODULE.looks_like_visible_queued_follow_up(
                f"{MODULE.QUEUED_FOLLOW_UP_PROMPT}\npending"
            )
        )

    def test_screen_marker_count_ignores_physical_line_wrapping(self) -> None:
        marker = MODULE.QUEUED_FOLLOW_UP_REPLY
        split = len(marker) // 2
        wrapped = f"{marker[:split]}\n  {marker[split:]}"

        self.assertEqual(MODULE.count_on_screen(wrapped, marker), 1)

    def test_queued_follow_up_is_visible_in_the_second_provider_request(self) -> None:
        fixture = MODULE.FixtureState()

        self.assertEqual(
            fixture.record_request({"messages": [{"role": "user", "content": "first"}]}),
            1,
        )
        self.assertEqual(
            fixture.record_request(
                {
                    "messages": [
                        {"role": "user", "content": "first"},
                        {"role": "user", "content": MODULE.QUEUED_FOLLOW_UP_PROMPT},
                    ]
                }
            ),
            2,
        )

        self.assertFalse(fixture.provider_requests[0]["has_queued_follow_up"])
        self.assertTrue(fixture.provider_requests[1]["has_queued_follow_up"])

    def test_busy_marker_requires_a_rendered_runtime_status(self) -> None:
        self.assertTrue(MODULE.looks_like_busy_tui("agent: main · Thinking..."))
        self.assertTrue(MODULE.looks_like_busy_tui("agent: main · Replying..."))
        self.assertFalse(MODULE.looks_like_busy_tui("agent: main · session idle"))

    def test_busy_resize_waits_for_runtime_status_and_fails_closed(self) -> None:
        runner = mock.Mock()
        events: list[str] = []

        def wait_until(predicate, _timeout, _description, *, final_screen=False):
            self.assertTrue(final_screen)
            self.assertTrue(predicate("agent: main · Thinking..."))
            events.append("busy")
            return "agent: main · Thinking..."

        runner.wait_until.side_effect = wait_until
        runner.resize.side_effect = lambda _rows, _cols: events.append("resize") or 7

        self.assertEqual(
            MODULE.resize_while_busy(runner, 18, 72, 3, "busy resize"),
            7,
        )
        self.assertEqual(events, ["busy", "resize"])

        runner.reset_mock()
        runner.wait_until.side_effect = TimeoutError("busy marker missing")
        with self.assertRaisesRegex(TimeoutError, "busy marker missing"):
            MODULE.resize_while_busy(runner, 18, 72, 3, "busy resize")
        runner.resize.assert_not_called()


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
            expected_version="0.0.1-beta.2",
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
    def test_compaction_phase_can_preserve_the_exact_session_route(self) -> None:
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
            active_config = config.read_text(encoding="utf-8")
            self.assertIn("config_version = 2", active_config)
            self.assertIn('connection = "stateful-fixture"', active_config)
            self.assertIn('provider = "deepseek"', active_config)
            self.assertIn('protocol = "deepseek"', active_config)
            self.assertIn('base_url = "https://api.deepseek.com"', active_config)
            self.assertIn('[task]\nrouting_policy = "manual"', active_config)
            self.assertIn("[web]\nenabled = false", active_config)
            MODULE.write_config(config, **common, port=43123)
            resumed_config = config.read_text(encoding="utf-8")
            self.assertEqual(resumed_config, active_config)
            self.assertIn(
                'credential = { source = "environment", '
                'name = "SIGIL_API_KEY" }',
                resumed_config,
            )
            self.assertIn('mode = "auto-edit"', resumed_config)

    def test_semantic_title_request_is_not_counted_as_a_history_turn(self) -> None:
        self.assertTrue(
            MODULE.is_semantic_title_request(
                {
                    "messages": [
                        {
                            "role": "system",
                            "content": (
                                "Generate a concise semantic title for a coding-agent "
                                "conversation"
                            ),
                        }
                    ],
                    "tools": [],
                }
            )
        )
        self.assertFalse(
            MODULE.is_semantic_title_request(
                {
                    "messages": [{"role": "user", "content": "ordinary history turn"}],
                    "tools": [],
                }
            )
        )

    def test_semantic_compaction_output_is_grounded_in_the_source_index(self) -> None:
        output = MODULE.semantic_compaction_output(
            {
                "messages": [
                    {
                        "role": "user",
                        "content": (
                            "Return a semantic continuation summary.\n"
                            'SOURCE_INDEX=[{"event_id":"event-1"}]'
                        ),
                    }
                ]
            }
        )
        self.assertIsNotNone(output)
        payload = json.loads(output or "{}")
        self.assertEqual(
            payload["model_notes"][0]["source_event_ids"],
            ["event-1"],
        )
        self.assertIsNone(
            MODULE.semantic_compaction_output(
                {"messages": [{"role": "user", "content": "ordinary request"}]}
            )
        )

    def test_isolated_environment_drops_credentials_and_closes_ambient_proxy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.dict(
                os.environ,
                {"SIGIL_API_KEY": "secret", "HTTPS_PROXY": "http://private.invalid"},
                clear=False,
            ):
                env = MODULE.isolated_environment(Path(temporary))
            self.assertEqual(
                env["SIGIL_API_KEY"],
                "stateful-fixture-key",
            )
            self.assertEqual(env["HTTPS_PROXY"], "http://127.0.0.1:9")
            self.assertEqual(env["NO_PROXY"], "127.0.0.1,localhost")
            self.assertEqual(env["SSL_CERT_FILE"], str(Path(temporary) / "tls-ca.pem"))

    def test_local_tls_identity_is_private_and_trustable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            certificate, private_key = MODULE.generate_fixture_tls_identity(Path(temporary))
            self.assertTrue(certificate.read_text(encoding="utf-8").startswith("-----BEGIN CERTIFICATE-----"))
            self.assertTrue(
                (Path(temporary) / "tls-ca.pem")
                .read_text(encoding="utf-8")
                .startswith("-----BEGIN CERTIFICATE-----")
            )
            self.assertEqual(private_key.stat().st_mode & 0o777, 0o600)

    def test_isolated_environment_can_route_default_tls_origin_to_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            env = MODULE.isolated_environment(Path(temporary), 43123)
        self.assertEqual(env["HTTPS_PROXY"], "http://127.0.0.1:43123")
        self.assertEqual(env["https_proxy"], "http://127.0.0.1:43123")
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
