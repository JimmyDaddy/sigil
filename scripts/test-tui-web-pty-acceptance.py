#!/usr/bin/env python3
"""Unit tests for the deterministic Web V1 PTY campaign contract."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("tui-web-pty-acceptance.py")
SPEC = importlib.util.spec_from_file_location("tui_web_pty_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class WebPtyAcceptanceTests(unittest.TestCase):
    def test_semantic_title_request_is_routed_outside_conversation_turns(self) -> None:
        payload = {
            "messages": [
                {
                    "role": "system",
                    "content": (
                        "Generate a concise semantic title for a coding-agent "
                        "conversation. Return only one title."
                    ),
                }
            ],
            "tools": [],
        }

        self.assertTrue(MODULE.is_semantic_title_request(payload))

        fixture = MODULE.FixtureState()
        fixture.record_semantic_title()
        _, _, _, provider_requests, title_requests = fixture.snapshot()
        self.assertEqual(provider_requests, [])
        self.assertEqual(title_requests, 1)

    def test_provider_request_without_tools_is_safe(self) -> None:
        fixture = MODULE.FixtureState()

        request_index = fixture.record_provider({"messages": []})

        self.assertEqual(request_index, 1)
        _, _, protocol_errors, provider_requests, title_requests = fixture.snapshot()
        self.assertEqual(protocol_errors, [])
        self.assertEqual(
            provider_requests,
            [
                {
                    "has_direct_route_tool": False,
                    "has_websearch_tool": False,
                    "has_tool_result": False,
                }
            ],
        )
        self.assertEqual(title_requests, 0)

    def test_provider_request_classifies_routing_and_ordinary_tool_turns(self) -> None:
        route_payload = {
            "messages": [],
            "tools": [
                {
                    "type": "function",
                    "function": {"name": "continue_without_task_planning"},
                }
            ],
        }
        ordinary_payload = {
            "messages": [],
            "tools": [
                {"type": "function", "function": {"name": "websearch"}}
            ],
        }
        continuation_payload = {
            "messages": [
                {
                    "role": "tool",
                    "tool_call_id": MODULE.TOOL_CALL_ID,
                    "content": "fixture result",
                }
            ],
            "tools": ordinary_payload["tools"],
        }

        self.assertTrue(
            MODULE.provider_request_exposes_tool(
                route_payload, "continue_without_task_planning"
            )
        )
        self.assertFalse(MODULE.provider_request_exposes_tool(route_payload, "websearch"))
        self.assertTrue(MODULE.provider_request_exposes_tool(ordinary_payload, "websearch"))
        self.assertFalse(
            MODULE.provider_request_has_result(ordinary_payload, MODULE.TOOL_CALL_ID)
        )
        self.assertTrue(
            MODULE.provider_request_has_result(continuation_payload, MODULE.TOOL_CALL_ID)
        )

    def test_write_config_uses_v2_unauthenticated_loopback_connection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "sigil.toml"
            MODULE.write_config(config, 43123)

            text = config.read_text(encoding="utf-8")
            self.assertIn("config_version = 2", text)
            self.assertIn('connection = "web-fixture"', text)
            self.assertIn('network_mode = "ask"', text)
            self.assertIn("[connections.web-fixture]", text)
            self.assertIn('provider = "custom"', text)
            self.assertIn('protocol = "chat_completions"', text)
            self.assertIn(
                'base_url = "http://127.0.0.1:43123/provider"',
                text,
            )
            self.assertIn('credential = { source = "none" }', text)
            self.assertNotIn("[providers.", text)
            self.assertNotIn("api_key", text)


if __name__ == "__main__":
    unittest.main()
