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
    def test_write_config_uses_v2_unauthenticated_loopback_connection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "sigil.toml"
            MODULE.write_config(config, 43123)

            text = config.read_text(encoding="utf-8")
            self.assertIn("config_version = 2", text)
            self.assertIn('connection = "web-fixture"', text)
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
