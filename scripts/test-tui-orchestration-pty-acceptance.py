#!/usr/bin/env python3
"""Unit tests for the deterministic orchestration PTY campaign contract."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("tui-orchestration-pty-acceptance.py")
SPEC = importlib.util.spec_from_file_location("tui_orchestration_pty_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def tool(name: str) -> dict[str, object]:
    return {"type": "function", "function": {"name": name}}


class OrchestrationPtyAcceptanceTests(unittest.TestCase):
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
            self.assertIn('connection = "orchestration-fixture"', text)
            self.assertIn("[connections.orchestration-fixture]", text)
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

    def test_request_classification_uses_typed_tools_and_role_contracts(self) -> None:
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [{"role": "user", "content": "opaque"}],
                    "tools": [tool("request_task_planning")],
                }
            ),
            "conversation",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [{"role": "user", "content": "opaque"}],
                    "tools": [tool("task_plan_update")],
                }
            ),
            "planner",
        )
        self.assertEqual(
            MODULE.classify_request(
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
            ),
            "title",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": (
                                "Execute this delegated subagent step.\n"
                                f"Step: {MODULE.READ_STEP_IDS[1]}\n"
                                "Role: subagent_read"
                            ),
                        }
                    ],
                    "tools": [],
                }
            ),
            f"read:{MODULE.READ_STEP_IDS[1]}",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": "Produce the single user-visible final answer",
                        }
                    ],
                    "tools": [],
                }
            ),
            "synthesis",
        )
        write_messages = [
            {
                "role": "user",
                "content": "Execute task step.\nStep: write_note\nRole: executor",
            }
        ]
        self.assertEqual(
            MODULE.classify_request({"messages": write_messages, "tools": []}),
            "write:request",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": write_messages
                    + [
                        {
                            "role": "tool",
                            "tool_call_id": MODULE.APPROVAL_TOOL_CALL_ID,
                            "content": "ok",
                        }
                    ],
                    "tools": [],
                }
            ),
            "write:after_tool",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": (
                                "Execute task step.\n"
                                "Step: continue_after_crash\nRole: executor"
                            ),
                        }
                    ],
                    "tools": [],
                }
            ),
            "continue:step",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": (
                                "Execute task step.\n"
                                "Step: cancel_in_flight\nRole: executor"
                            ),
                        }
                    ],
                    "tools": [],
                }
            ),
            "cancel:step",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": (
                                "Execute task step.\n"
                                "Step: integrate_a\nRole: subagent_write"
                            ),
                        }
                    ],
                    "tools": [],
                }
            ),
            "integration:step:a",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": [
                        {
                            "role": "user",
                            "content": (
                                "Execute task step.\n"
                                "Step: integrate_b\nRole: subagent_write"
                            ),
                        }
                    ],
                    "tools": [],
                }
            ),
            "integration:step:b",
        )
        terminal_messages = [
            {
                "role": "user",
                "content": (
                    "Execute task step.\nStep: terminal_lifecycle\nRole: executor"
                ),
            }
        ]
        self.assertEqual(
            MODULE.classify_request({"messages": terminal_messages, "tools": []}),
            "terminal:start",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": terminal_messages
                    + [
                        {
                            "role": "tool",
                            "tool_call_id": MODULE.TERMINAL_START_TOOL_CALL_ID,
                            "content": "started",
                        }
                    ],
                    "tools": [],
                }
            ),
            "terminal:after_start",
        )
        self.assertEqual(
            MODULE.classify_request(
                {
                    "messages": terminal_messages
                    + [
                        {
                            "role": "tool",
                            "tool_call_id": MODULE.TERMINAL_START_TOOL_CALL_ID,
                            "content": "started",
                        },
                        {
                            "role": "tool",
                            "tool_call_id": MODULE.TERMINAL_CANCEL_TOOL_CALL_ID,
                            "content": "cancelled",
                        },
                    ],
                    "tools": [],
                }
            ),
            "terminal:after_cancel",
        )

    def test_request_classification_selects_current_step_from_approved_plan(self) -> None:
        payload = {
            "messages": [
                {
                    "role": "user",
                    "content": (
                        "Execute the following user-approved Plan.\n"
                        "Steps:\n"
                        "1. Inspect kernel route\n"
                        "Step: inspect_kernel\n"
                        "Role: subagent_read\n"
                        "2. Inspect runtime route\n"
                        "Step: inspect_runtime\n"
                        "Role: subagent_read\n"
                        "Execute this delegated subagent step.\n"
                        "Step: inspect_kernel\n"
                        "Role: subagent_read"
                    ),
                }
            ],
            "tools": [],
        }
        self.assertEqual(MODULE.classify_request(payload), "read:inspect_kernel")

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

    def test_unknown_request_fails_closed(self) -> None:
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.classify_request(
                {
                    "messages": [{"role": "user", "content": "ordinary answer"}],
                    "tools": [],
                }
            )

    def test_valid_audit_requires_provider_overlap_and_unique_final(self) -> None:
        audit = MODULE.SessionAudit(
            event_counts={
                "plan_draft_created": 1,
            },
            completed_steps=MODULE.READ_STEP_IDS,
            final_answer_count=1,
            approval_final_answer_count=0,
            continue_final_answer_count=0,
            integration_final_answer_count=0,
            terminal_final_answer_count=0,
            task_final_count=1,
            approval_route_resolved_count=0,
            approved_tool_call_count=0,
            terminal_approval_route_resolved_count=0,
            approved_terminal_start_count=0,
            terminal_start_completed_count=0,
            terminal_cancel_completed_count=0,
            terminal_task_statuses=(),
            terminal_readiness_states=(),
            terminal_max_output_bytes=0,
            paused_task_run_count=0,
            interrupted_step_count=0,
            interrupted_task_run_count=0,
            cancelled_task_run_count=0,
            promotion_preview_count=0,
            promotion_authority_count=0,
            promoted_integration_count=0,
            parent_verification_count=0,
            failed_run_count=0,
            cancelled_run_count=0,
        )
        fixture = MODULE.FixtureState(
            request_counts={
                "routing:plan_review": 1,
                "plan_review": 1,
                f"read:{MODULE.READ_STEP_IDS[0]}": 1,
                f"read:{MODULE.READ_STEP_IDS[1]}": 1,
                "synthesis": 1,
                "title": 1,
            },
            max_concurrent_reads=2,
        )

        MODULE.validate_audit(audit, fixture)
        fixture.max_concurrent_reads = 1
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.validate_audit(audit, fixture)

    def test_valid_terminal_audit_requires_approval_readiness_progress_and_cancel(self) -> None:
        audit = MODULE.SessionAudit(
            event_counts={
                "plan_draft_created": 6,
            },
            completed_steps=("terminal_lifecycle",),
            final_answer_count=1,
            approval_final_answer_count=1,
            continue_final_answer_count=1,
            integration_final_answer_count=1,
            terminal_final_answer_count=1,
            task_final_count=5,
            approval_route_resolved_count=1,
            approved_tool_call_count=1,
            terminal_approval_route_resolved_count=1,
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
            interrupted_step_count=1,
            interrupted_task_run_count=1,
            cancelled_task_run_count=1,
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
                "plan_review": 6,
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
            }
        )

        MODULE.validate_terminal_audit(audit, fixture)
        audit = MODULE.dataclasses.replace(audit, terminal_max_output_bytes=1)
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.validate_terminal_audit(audit, fixture)


if __name__ == "__main__":
    unittest.main()
