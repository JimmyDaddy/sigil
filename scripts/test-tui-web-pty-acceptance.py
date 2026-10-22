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
    def test_main_probe_recognizes_the_current_composer_without_agent_metadata(self) -> None:
        for header in (
            "▌ Build · danger-full-access     fixture-model",
            "▌ Plan · manual     fixture-model",
            "▌ Build · reviewer · manual     fixture-model",
        ):
            with self.subTest(header=header):
                self.assertTrue(MODULE.looks_like_main_tui_ready(
                    f"{header}\n▌\nEnter send · Ctrl-J newline · F4 details"
                ))

    def test_main_probe_rejects_trust_setup_and_startup_recovery(self) -> None:
        for screen in (
            "Workspace trust\ntrust this workspace\n▌ Build · manual     fixture-model",
            "Trust workspace\n▌ Build · manual     fixture-model",
            "Set up a model connection\nStep 1 of 4 · Choose the provider\n"
            "> DeepSeek · Add an API key next",
            "Startup diagnostics\nConfiguration is saved. Sigil has not started.\n"
            "agent: main could not build the session\n[Esc] Back to startup",
            "Build · agent: main\nstate: ready",
        ):
            with self.subTest(screen=screen):
                self.assertFalse(MODULE.looks_like_main_tui_ready(screen))

    def test_startup_wait_uses_the_current_vt_frame_instead_of_prior_trust_text(self) -> None:
        runner = MODULE.PtyRunner(["sigil"], Path("."), {}, Path("unused.log"))
        runner.output.extend(
            b"\x1b[?1049h\x1b[2J\x1b[HWorkspace trust\r\ntrust this workspace"
            b"\x1b[2J\x1b[H\x1b[36;2H"
            + "▌".encode()
            + b"\x1b[36;4HBuild"
            + " · danger-full-access     fixture-model".encode()
            + b"\x1b[40;3HEnter send"
            + " · Ctrl-J newline · F4 details".encode()
        )
        self.assertTrue(MODULE.looks_like_trust_gate(runner.rendered()))
        screen = runner.wait_until(
            MODULE.looks_like_main_tui_ready, 0.1, "current main TUI", final_screen=True,
        )
        self.assertFalse(MODULE.looks_like_trust_gate(screen))
        self.assertIn("▌ Build · danger-full-access", screen)

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
                    "has_optional_plan_tool": False,
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
                    "function": {"name": "request_plan_review"},
                }
            ],
        }
        ordinary_payload = {
            "messages": [],
            "tools": [
                {"type": "function", "function": {"name": "websearch"}},
                {"type": "function", "function": {"name": "request_plan_review"}},
            ],
        }
        fixture = MODULE.FixtureState()
        fixture.record_provider(ordinary_payload)
        self.assertEqual(fixture.snapshot()[3], [{
            "has_optional_plan_tool": True,
            "has_websearch_tool": True,
            "has_tool_result": False,
        }])
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
                route_payload, "request_plan_review"
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
