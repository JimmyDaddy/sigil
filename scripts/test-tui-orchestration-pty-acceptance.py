#!/usr/bin/env python3
"""Unit tests for the deterministic orchestration PTY campaign contract."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
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
                "task_handoff_requested": 1,
                "task_handoff_resolved": 1,
            },
            completed_steps=MODULE.READ_STEP_IDS,
            final_answer_count=1,
            approval_final_answer_count=0,
            continue_final_answer_count=0,
            integration_final_answer_count=0,
            task_final_count=1,
            approval_route_resolved_count=0,
            approved_tool_call_count=0,
            paused_task_run_count=0,
            interrupted_step_count=0,
            cancelled_task_run_count=0,
            promotion_preview_count=0,
            promotion_authority_count=0,
            promoted_integration_count=0,
            parent_verification_count=0,
            failed_run_count=0,
        )
        fixture = MODULE.FixtureState(
            request_counts={
                "conversation": 1,
                "planner": 1,
                f"read:{MODULE.READ_STEP_IDS[0]}": 1,
                f"read:{MODULE.READ_STEP_IDS[1]}": 1,
                "synthesis": 1,
            },
            max_concurrent_reads=2,
        )

        MODULE.validate_audit(audit, fixture)
        fixture.max_concurrent_reads = 1
        with self.assertRaises(MODULE.AcceptanceError):
            MODULE.validate_audit(audit, fixture)


if __name__ == "__main__":
    unittest.main()
