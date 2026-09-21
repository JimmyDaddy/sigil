#!/usr/bin/env python3
"""Unit tests for the deterministic Direct Task PTY campaign contract."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("tui-orchestration-pty-acceptance.py")
SPEC = importlib.util.spec_from_file_location("tui_direct_task_pty_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def tool(name: str) -> dict[str, object]:
    return {"type": "function", "function": {"name": name}}


class DirectTaskPtyAcceptanceTests(unittest.TestCase):
    def test_cancellation_requires_exact_task_scope_and_confirmed_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.jsonl"
            request = {"record": "requested", "request_id": "cancel-one",
                       "run_scope_id": "scope-one", "target": {"kind": "task", "task_id": "task-one"}}
            terminal = {"record": "finalized", "request_id": "cancel-one",
                        "run_scope_id": "scope-one", "outcome": "cancelled",
                        "cleanup_complete": True, "active_effects": 0, "active_tasks": 0}
            def write() -> None:
                path.write_text("".join(json.dumps({"payload": payload}) + "\n"
                                        for payload in [request, terminal]), encoding="utf-8")
            write()
            self.assertTrue(MODULE.task_cancellation_settled(path, "task-one"))
            self.assertFalse(MODULE.task_cancellation_settled(path, "old-task"))
            terminal["run_scope_id"] = "other-scope"
            write()
            self.assertFalse(MODULE.task_cancellation_settled(path, "task-one"))
            terminal["run_scope_id"] = "scope-one"
            terminal["outcome"] = "interrupted"
            terminal["cleanup_complete"] = False
            write()
            with self.assertRaises(MODULE.AcceptanceError):
                MODULE.task_cancellation_settled(path, "task-one")

    def test_workbench_identity_keeps_the_complete_wrapped_hash(self) -> None:
        screen = "plan plan-one · sha256:" + "a" * 48 + "\n" + "a" * 16
        self.assertEqual(MODULE.plan_workbench_identity(screen), ("plan-one", "a" * 64))
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.plan_workbench_identity("plan plan-one · sha256:abc")

    def test_stale_retry_requires_the_same_visible_plan_and_hash(self) -> None:
        screen = (
            "Plan Review · ready\nReview scenario 4 before execution\n"
            "plan plan-one · sha256:" + "a" * 64 + "\n"
            "Run failed: the session changed while running; retry the same command\n"
            "Run [R] Reject [X]"
        )
        identity = ("plan-one", "a" * 64)
        self.assertTrue(MODULE.retryable_plan_start(screen, 4, identity))
        self.assertFalse(MODULE.retryable_plan_start(screen, 3, identity))
        self.assertFalse(MODULE.retryable_plan_start(screen, 4, ("plan-two", "a" * 64)))
        self.assertFalse(MODULE.retryable_plan_start(screen, 4, ("plan-one", "b" * 64)))
        self.assertFalse(MODULE.retryable_plan_start(
            screen.replace("the session changed while running", "permission denied"), 4, identity,
        ))

    def test_resumed_direct_identity_requires_same_admission_two_attempts_and_no_plan(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.jsonl"
            controls = [
                {"task_direct_execution_admitted_v1": {"task_id": "task", "admission_id": "admission"}},
                {"task_direct_execution_attempt_v1": {"task_id": "task", "attempt_id": "first"}},
                {"task_direct_execution_attempt_v1": {"task_id": "task", "attempt_id": "second"}},
            ]
            def write() -> None:
                path.write_text("\n".join(json.dumps({"payload": {"session_log_entry": {"control": control}}}) for control in controls), encoding="utf-8")
            write()
            MODULE.validate_resumed_direct_identity(path, "task", "admission")
            controls.append({"task_direct_execution_attempt_v1": {"task_id": "task", "attempt_id": "third"}})
            write()
            with self.assertRaises(MODULE.AcceptanceError):
                MODULE.validate_resumed_direct_identity(path, "task", "admission")
            controls.pop()
            controls.append({"task_plan": {"task_id": "task"}})
            write()
            with self.assertRaises(MODULE.AcceptanceError):
                MODULE.validate_resumed_direct_identity(path, "task", "admission")

    def test_conversation_terminal_status_uses_current_typed_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.jsonl"
            for status, expected in (("succeeded", 0), ("failed", 1), ("unknown", 1)):
                path.write_text(json.dumps({"event_type": "run_finalized", "payload": {
                    "record": "conversation_run_finalized_v1", "status": status,
                }}) + "\n", encoding="utf-8")
                self.assertEqual(MODULE.read_session_audit(path).failed_run_count, expected)

    def test_session_audit_distinguishes_cancelled_from_hard_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            session = Path(directory) / "session.jsonl"
            records = [
                {
                    "event_type": "run_finalized",
                    "payload": {
                        "outcome": outcome,
                        "cleanup_complete": cleanup_complete,
                    },
                }
                for outcome, cleanup_complete in (
                    ("cancelled", True),
                    ("interrupted", False),
                    ("cancelled", False),
                )
            ]
            session.write_text(
                "".join(json.dumps(record) + "\n" for record in records),
                encoding="utf-8",
            )

            audit = MODULE.read_session_audit(session)

            self.assertEqual(audit.cancelled_run_count, 1)
            self.assertEqual(audit.failed_run_count, 2)

    def test_session_audit_does_not_call_controlled_paused_finalization_a_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            session = Path(directory) / "session.jsonl"
            session.write_text(
                json.dumps(
                    {
                        "event_type": "run_finalized",
                        "payload": {
                            "run_status": "paused",
                            "terminal_reason": "provider_recovery_cancelled",
                        },
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            audit = MODULE.read_session_audit(session)

            self.assertEqual(audit.failed_run_count, 0)

    def test_write_config_uses_v2_unauthenticated_loopback_connection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "sigil.toml"
            MODULE.write_config(
                config,
                workspace=root / "workspace",
                state_root=root / "state",
                cache_root=root / "cache",
                session_dir=root / "sessions",
                port=43123,
            )

            text = config.read_text(encoding="utf-8")
            self.assertIn("config_version = 2", text)
            self.assertIn('connection = "direct-task-fixture"', text)
            self.assertIn("[connections.direct-task-fixture]", text)
            self.assertIn('provider = "custom"', text)
            self.assertIn('protocol = "chat_completions"', text)
            self.assertIn('base_url = "http://127.0.0.1:43123/v1"', text)
            self.assertIn('credential = { source = "none" }', text)
            self.assertIn('[permission.tools]\nterminal_cancel = "allow"', text)
            self.assertNotIn("[providers.", text)
            self.assertNotIn("api_key", text)

    def test_screen_text_click_uses_one_based_sgr_coordinates(self) -> None:
        class Runner:
            def __init__(self) -> None:
                self.sent: list[str] = []

            def send(self, value: str) -> None:
                self.sent.append(value)

        runner = Runner()
        MODULE.click_screen_text(
            runner,
            "first row\n   Integration review\nlast row",
            "Integration review",
        )
        self.assertEqual(
            runner.sent,
            ["\x1b[<0;4;2M", "\x1b[<0;4;2m"],
        )

    def test_plan_approval_accepts_workbench_that_already_replaced_card(self) -> None:
        self.assertTrue(
            MODULE.plan_review_workbench_visible(
                "Plan Review · 1 steps\nRun [R]  Save [S]  Reject [X]"
            )
        )
        self.assertFalse(MODULE.plan_review_workbench_visible("Plan ready"))

    def test_latest_plan_surface_reads_only_the_current_plan_notice_suffix(self) -> None:
        raw = (
            b"validated plan draft recorded old"
            b"\x1b[1mPlan\x1b[22m ready old\n"
            b"validated plan draft recorded current"
            b"\x1b[1mPlan\x1b[22m ready current\n"
            b"\x1b[1mPlan Review\x1b[22m\nRun [R]  Reject [X]"
        )
        surface = MODULE.latest_plan_surface(raw)
        self.assertNotIn("old", surface)
        self.assertIn("Plan ready current", surface)
        self.assertTrue(MODULE.plan_review_workbench_visible(surface))

    def test_request_classification_uses_current_typed_review_actions(self) -> None:
        self.assertEqual(
            MODULE.classify_request({
                "messages": [{"role": "user", "content": "Review scenario 1 before execution"}],
                "tools": [tool("request_plan_review")],
            }),
            "routing:plan_review",
        )
        self.assertEqual(
            MODULE.classify_request({
                "messages": [{"role": "user", "content": "Review scenario 1 before execution"}],
                "tools": [tool("submit_plan_review_result")],
            }),
            "plan_review_research",
        )
        for retired_tool in ("request_task_planning", "submit_plan_draft"):
            with self.subTest(retired_tool=retired_tool):
                with self.assertRaises(MODULE.AcceptanceError):
                    MODULE.classify_request({
                        "messages": [{"role": "user", "content": "opaque"}],
                        "tools": [tool(retired_tool)],
                    })

    def test_managed_parent_session_files_excludes_child_sessions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            parent = root / "session-parent" / "records.jsonl"
            child = root / "pr-child" / "records.jsonl"
            parent.parent.mkdir()
            child.parent.mkdir()
            parent.write_text(
                json.dumps({"event_type": "user_message_recorded"}) + "\n",
                encoding="utf-8",
            )
            child.write_text("{}\n", encoding="utf-8")
            self.assertEqual(MODULE.managed_parent_session_files(root), [parent])

    def test_managed_parent_session_files_ignores_resume_boot_identity_stream(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            parent = root / "session-parent" / "records.jsonl"
            boot = root / "session-boot" / "records.jsonl"
            parent.parent.mkdir()
            boot.parent.mkdir()
            parent.write_text(
                json.dumps({"event_type": "task_created_from_plan"}) + "\n",
                encoding="utf-8",
            )
            boot.write_text(
                "\n".join(
                    json.dumps({"event_type": event_type})
                    for event_type in (
                        "workspace_trust_decision",
                        "session_entry_recorded",
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            self.assertEqual(MODULE.managed_parent_session_files(root), [parent])

    def test_plan_ready_screen_without_legacy_host_notice_is_detected(self) -> None:
        class Runner:
            output = b""
            process = None

            def read_available(self, _timeout: float) -> None:
                pass

            def screen(self) -> str:
                return "▌ Plan ready  ·  Review scenario 1 before execution"

        self.assertIn("Plan ready", MODULE.wait_for_plan_surface(Runner(), 0.1, scenario=1))

    def test_plan_surface_requires_current_scenario_and_active_card(self) -> None:
        visible = MODULE.current_plan_surface_visible
        old_card = "▌ Plan ready  ·  Review scenario 1 before execution"
        new_message = "Plan ready: Review scenario 2 before execution"
        self.assertFalse(visible(old_card + "\n" + new_message, 2, require_workbench=False))
        new_card = "▌ Plan ready  ·  Review scenario 2 before execution"
        self.assertTrue(visible(old_card + "\n" + new_card, 2, require_workbench=False))
        self.assertFalse(visible(new_card, 2, require_workbench=True))
        old_workbench = "Plan Review · draft_ready\nReview scenario 1 before execution\nRun Reject"
        self.assertFalse(visible(old_workbench, 2, require_workbench=False))
        new_workbench = old_workbench.replace("scenario 1", "scenario 2")
        self.assertTrue(visible(new_workbench, 2, require_workbench=True))

    def test_historical_raw_workbench_does_not_authorize_plan_keys(self) -> None:
        class Runner:
            output = (b"validated plan draft recorded\nPlan Review\n"
                      b"Review scenario 2 before execution\nRun Reject")
            process = None

            def read_available(self, _timeout: float) -> None:
                pass

            def screen(self) -> str:
                return "▌ Plan ready · Review scenario 1 before execution"

        with self.assertRaises(TimeoutError):
            MODULE.wait_for_plan_surface(Runner(), 0.01, scenario=2)

    def test_bound_approval_rejects_changed_identity_and_stops_after_resolution(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.jsonl"
            controls = [{"call_id": "call", "action": "requested",
                         "identity": {"approval_request_id": "request"}}]

            def write() -> None:
                path.write_text("".join(json.dumps({"payload": {"session_log_entry": {
                    "control": {"tool_approval": value}}}}) + "\n" for value in controls),
                    encoding="utf-8")

            write()
            self.assertEqual(MODULE.bound_approval_state(path, "call", "request"), "requested")
            controls.append({"call_id": "other", "action": "requested",
                             "identity": {"approval_request_id": "other-request"}})
            write()
            with self.assertRaises(MODULE.AcceptanceError):
                MODULE.bound_approval_state(path, "call", "request")
            controls.insert(1, {"call_id": "call", "action": "resolved", "user_decision": "approved",
                                "identity": {"approval_request_id": "request"}})
            write()
            self.assertEqual(MODULE.bound_approval_state(path, "call", "request"), "resolved")
            with self.assertRaises(MODULE.AcceptanceError):
                MODULE.bound_approval_state(path, "call", "stale-request")

    def test_direct_request_uses_latest_user_scenario_not_history_order(self) -> None:
        for current, expected in ((2, "direct:write:request"), (3, "direct:continue"),
                                  (1, "direct:read")):
            payload = {"messages": [
                {"role": "user", "content": "Review scenario 1 before execution"},
                {"role": "assistant", "content": "Review scenario 3 before execution"},
                {"role": "user", "content": f"Review scenario {current} before execution"},
                {"role": "tool", "content": "Review scenario 6 before execution"},
            ], "tools": []}
            self.assertEqual(MODULE.classify_request(payload), expected)
        payload["messages"][-2]["content"] = "Review scenario 2 before execution"
        payload["messages"].append({"role": "tool", "tool_call_id": MODULE.APPROVAL_TOOL_CALL_ID})
        self.assertEqual(MODULE.classify_request(payload), "direct:write:after_tool")

    def test_direct_request_uses_latest_objective_within_user_context(self) -> None:
        payload = {"messages": [{"role": "user", "content":
            "Previous: Review scenario 3 before execution\n"
            "Current: Review scenario 2 before execution"}], "tools": []}
        self.assertEqual(MODULE.classify_request(payload), "direct:write:request")
        payload["messages"][0]["role"] = "assistant"
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.classify_request(payload)

    def test_current_plan_review_result_tool_is_classified(self) -> None:
        self.assertEqual(MODULE.classify_request({
            "tools": [{"type": "function", "function": {"name": "submit_plan_review_result"}}],
            "messages": [],
        }), "plan_review_research")

    def test_hashed_managed_parent_excludes_active_children(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            parent = root / ("a" * 64) / "records.jsonl"
            child = root / ("b" * 64) / "records.jsonl"
            for path, events in ((parent, ["plan_review_attempt"]),
                                 (child, ["user_message_recorded", "assistant_message_recorded",
                                          "run_status_changed", "run_finalized"])):
                path.parent.mkdir()
                path.write_text("".join(json.dumps({"event_type": event}) + "\n"
                                        for event in events), encoding="utf-8")
            self.assertEqual(MODULE.managed_parent_session_files(root), [parent])

    def test_fixture_protocol_error_is_persisted_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "errors.json"
            fixture = MODULE.FixtureState(diagnostics_path=path)
            fixture.start_request("plan_review_research")
            fixture.record_error(ValueError("wrong contract"))
            data = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(data["protocol_errors"], ["ValueError: wrong contract"])
            self.assertEqual(data["request_counts"], {"plan_review_research": 1})

    def test_unknown_request_fails_closed(self) -> None:
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.classify_request(
                {
                    "messages": [{"role": "user", "content": "ordinary answer"}],
                    "tools": [],
                }
            )

    def test_valid_terminal_audit_requires_approval_readiness_progress_and_cancel(self) -> None:
        audit = MODULE.SessionAudit(
            event_counts={
                "plan_draft_created": 6,
            },
            final_answer_count=1,
            approval_final_answer_count=1,
            continue_final_answer_count=1,
            integration_final_answer_count=1,
            terminal_final_answer_count=1,
            task_final_count=5,
            approved_tool_call_count=1,
            approved_terminal_start_count=1,
            terminal_start_completed_count=1,
            terminal_cancel_completed_count=1,
            terminal_task_statuses=("cancelled", "running"),
            terminal_readiness_states=("ready", "waiting"),
            terminal_max_output_bytes=(
                len(MODULE.TERMINAL_READY_CANARY)
                + len(MODULE.TERMINAL_PROGRESS_CANARY)
                + 2
            ),
            paused_task_run_count=1,
            interrupted_task_run_count=1,
            promotion_preview_count=1,
            promotion_authority_count=1,
            promoted_integration_count=1,
            parent_verification_count=1,
            failed_run_count=0,
            cancelled_run_count=1,
        )
        fixture = MODULE.FixtureState(
            request_counts={
                "routing:plan_review": 6,
                "plan_review_research": 6,
                "direct:read": 1,
                "direct:write:request": 1,
                "direct:write:after_tool": 1,
                "direct:continue": 2,
                "direct:cancel": 1,
                "direct:integration:a": 1,
                "direct:integration:b": 1,
                "direct:integration:final": 1,
                "direct:terminal:start": 1,
                "direct:terminal:after_start": 1,
                "direct:terminal:after_cancel": 1,
                "title": 1,
            }
        )

        MODULE.validate_terminal_audit(audit, fixture)
        audit = MODULE.dataclasses.replace(audit, terminal_max_output_bytes=1)
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.validate_terminal_audit(audit, fixture)


if __name__ == "__main__":
    unittest.main()
