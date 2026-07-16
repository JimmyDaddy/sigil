#!/usr/bin/env python3
"""Contract tests for the bounded real-provider Plan TUI acceptance."""

from __future__ import annotations

import dataclasses
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("tui-plan-provider-acceptance.py")
SPEC = importlib.util.spec_from_file_location("tui_plan_provider_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def write_config(path: Path, *, workspace: str = ".", session_log_dir: str | None = None) -> None:
    session = f'\n[session]\nlog_dir = "{session_log_dir}"\n' if session_log_dir else ""
    path.write_text(
        f'''[workspace]
root = "{workspace}"

[agent]
provider = "deepseek"
model = "deepseek-chat"
{session}
[providers.deepseek]
''',
        encoding="utf-8",
    )


def write_audit(path: Path, *, tool_name: str = "read_file", target: str = "README.md") -> None:
    records = [
        {
            "event_type": "tool_execution_completed",
            "payload": {
                "session_log_entry": {
                    "control": {
                        "tool_execution": {
                            "call_id": "call-1",
                            "tool_name": tool_name,
                            "status": "completed",
                        }
                    }
                }
            },
        },
        {
            "event_type": "session_entry_recorded",
            "payload": {
                "session_log_entry": {
                    "control": {
                        "usage_snapshot": {
                            "prompt_tokens": 100,
                            "completion_tokens": 50,
                            "input_cost": 0.001,
                            "output_cost": 0.002,
                        }
                    }
                }
            },
        },
        {
            "event_type": "plan_draft_created",
            "payload": {
                "session_log_entry": {
                    "control": {
                        "plan_draft_created": {
                            "target_paths": [target],
                            "steps": [{"step_id": "step-1", "target_paths": [target]}],
                        }
                    }
                }
            },
        },
        {"event_type": "run_finalized", "payload": {"run_status": "completed"}},
    ]
    path.write_text("".join(json.dumps(record) + "\n" for record in records), encoding="utf-8")


class AdmissionTests(unittest.TestCase):
    def test_long_plan_prompt_drains_pty_redraws_while_typing(self) -> None:
        class FakeRunner:
            def __init__(self) -> None:
                self.sent: list[str] = []
                self.reads: list[float] = []

            def send(self, value: str) -> None:
                self.sent.append(value)

            def read_available(self, timeout: float) -> None:
                self.reads.append(timeout)

        runner = FakeRunner()
        prompt = "/plan " + ("inspect README.md " * 32)
        MODULE.type_text_while_draining(runner, prompt)
        self.assertEqual("".join(runner.sent), prompt)
        self.assertEqual(len(runner.reads), len(prompt))
        self.assertTrue(all(timeout == 0.002 for timeout in runner.reads))

    def test_committed_fixture_is_checksum_pinned(self) -> None:
        fixture = MODULE.load_fixture(MODULE.repo_root() / MODULE.DEFAULT_FIXTURE)
        self.assertEqual(fixture.fixture_id, "plan-only")
        self.assertEqual(fixture.expected_target_path, "README.md")
        self.assertIn("read_file", fixture.allowed_tools)
        self.assertEqual(len(fixture.files), 1)

    def test_prompt_path_cannot_escape_or_symlink_out_of_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / "outside.txt"
            outside.write_text("private", encoding="utf-8")
            fixture = root / "fixture"
            fixture.mkdir()
            (fixture / "fixture.toml").write_text(
                '''schema_version = 1
id = "plan-only"
prompt_file = "../outside.txt"
prompt_sha256 = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
expected_target_path = "README.md"
allowed_tools = ["read_file"]
''',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(MODULE.PlanAcceptanceError, "prompt path"):
                MODULE.load_fixture(fixture)
            (fixture / "prompt.txt").symlink_to(outside)
            (fixture / "fixture.toml").write_text(
                f'''schema_version = 1
id = "plan-only"
prompt_file = "prompt.txt"
prompt_sha256 = "sha256:{MODULE.sha256_file(outside)}"
expected_target_path = "README.md"
allowed_tools = ["read_file"]
''',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(MODULE.PlanAcceptanceError, "must not traverse a symlink"):
                MODULE.load_fixture(fixture)
            (fixture / "prompt.txt").unlink()
            internal = fixture / "internal.txt"
            internal.write_text("internal", encoding="utf-8")
            (fixture / "prompt.txt").symlink_to(internal)
            (fixture / "fixture.toml").write_text(
                f'''schema_version = 1
id = "plan-only"
prompt_file = "prompt.txt"
prompt_sha256 = "sha256:{MODULE.sha256_file(internal)}"
expected_target_path = "README.md"
allowed_tools = ["read_file"]
''',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(MODULE.PlanAcceptanceError, "must not traverse a symlink"):
                MODULE.load_fixture(fixture)

    def test_cost_is_positive_and_locally_bounded(self) -> None:
        self.assertEqual(MODULE.parse_cost_microusd("0.05"), 50_000)
        self.assertGreaterEqual(MODULE.PLAN_RUN_WAIT_CAP_SECS, 90.0)
        for value in ("0", "-1", "nan", "inf", "1.01", "not-a-number"):
            with self.subTest(value=value):
                with self.assertRaises(MODULE.PlanAcceptanceError):
                    MODULE.parse_cost_microusd(value)

    def test_source_config_is_scrubbed_into_a_bounded_case_config(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            accepted = root / "accepted.toml"
            write_config(accepted, workspace="/private/project", session_log_dir="../sessions")
            accepted.write_text(
                accepted.read_text(encoding="utf-8")
                + 'api_key = "must-not-be-copied"\nbase_url = "https://api.example.test"\n',
                encoding="utf-8",
            )
            with mock.patch.dict(os.environ, {"SIGIL_API_KEY": "environment-key"}, clear=False):
                source = MODULE.validate_source_config(accepted)
                isolated = MODULE.write_isolated_config(source, root / "case")
            rendered = isolated.read_text(encoding="utf-8")
            self.assertNotIn("must-not-be-copied", rendered)
            self.assertNotIn("api_key", rendered)
            self.assertIn("max_turns = 4", rendered)
            self.assertIn('mode = "read-only"', rendered)
            self.assertIn('root = "."', rendered)

    def test_source_config_requires_documented_environment_credential(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            config = Path(temporary) / "sigil.toml"
            write_config(config)
            with mock.patch.dict(os.environ, {}, clear=True):
                with self.assertRaisesRegex(MODULE.PlanAcceptanceError, "credential"):
                    MODULE.validate_source_config(config)

    def test_environment_keeps_only_explicit_provider_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.dict(
                os.environ,
                {
                    "SIGIL_API_KEY": "test-key",
                    "SIGIL_ANTHROPIC_API_KEY": "other-provider-key",
                    "SIGIL_CONFIG": "/private/config",
                    "UNRELATED_SECRET": "drop-me",
                },
                clear=False,
            ):
                environment = MODULE.provider_environment(Path(temporary), "deepseek")
            self.assertEqual(environment["SIGIL_API_KEY"], "test-key")
            self.assertNotIn("SIGIL_ANTHROPIC_API_KEY", environment)
            self.assertNotIn("SIGIL_CONFIG", environment)
            self.assertNotIn("UNRELATED_SECRET", environment)
            self.assertTrue(environment["SIGIL_STATE_HOME"].startswith(temporary))

    def test_repository_output_must_be_git_ignored(self) -> None:
        root = MODULE.repo_root()
        ignored = (root / ".repo-local-dev/dogfood/plan-contract").resolve()
        self.assertEqual(
            MODULE.SUPPORT.raw_artifact_policy(root, ignored),
            "local_only_under_git_ignored_output",
        )
        unignored = (root / "plan-provider-unignored-contract-output").resolve()
        self.assertFalse(unignored.exists())
        with self.assertRaisesRegex(MODULE.SUPPORT.AcceptanceError, "must be git-ignored"):
            MODULE.SUPPORT.raw_artifact_policy(root, unignored)


class DurableEvidenceTests(unittest.TestCase):
    def test_session_discovery_ignores_non_session_jsonl_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary)
            history = state / "workspaces" / "fixture" / "input-history.jsonl"
            history.parent.mkdir(parents=True)
            history.write_text('{"input":"private prompt"}\n', encoding="utf-8")
            session = history.parent / "sessions" / "session-fixture.jsonl"
            session.parent.mkdir()
            session.write_text("{}\n", encoding="utf-8")
            unrelated = state / "sessions" / "other.jsonl"
            unrelated.parent.mkdir()
            unrelated.write_text("{}\n", encoding="utf-8")
            self.assertEqual(MODULE.session_files(state), [session])

    def test_audit_requires_structured_plan_usage_terminal_and_read_only_tools(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary) / "session.jsonl"
            write_audit(session)
            audit = MODULE.read_plan_audit(session)
            fixture = MODULE.load_fixture(MODULE.repo_root() / MODULE.DEFAULT_FIXTURE)
            MODULE.validate_audit(audit, fixture, 10_000)
            self.assertEqual(audit.event_counts["plan_draft_created"], 1)
            self.assertEqual(audit.plan_step_count, 1)
            self.assertEqual(audit.observed_cost_usd, 0.003)

    def test_write_tool_and_task_handoff_fail_plan_only_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary) / "session.jsonl"
            write_audit(session, tool_name="write_file")
            records = session.read_text(encoding="utf-8")
            records += json.dumps(
                {
                    "event_type": "task_created_from_plan",
                    "payload": {
                        "session_log_entry": {
                            "control": {"task_created_from_plan": {"task_id": "task-1"}}
                        }
                    },
                }
            ) + "\n"
            session.write_text(records, encoding="utf-8")
            audit = MODULE.read_plan_audit(session)
            fixture = MODULE.load_fixture(MODULE.repo_root() / MODULE.DEFAULT_FIXTURE)
            with self.assertRaises(MODULE.PlanAcceptanceError):
                MODULE.validate_audit(audit, fixture, 10_000)

    def test_unexecuted_write_request_still_fails_read_only_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary) / "session.jsonl"
            write_audit(session)
            records = session.read_text(encoding="utf-8")
            records += json.dumps(
                {
                    "event_type": "assistant_message_recorded",
                    "payload": {
                        "session_log_entry": {
                            "assistant": {
                                "role": "assistant",
                                "tool_calls": [
                                    {"id": "call-write", "name": "write_file", "args_json": "{}"}
                                ],
                            }
                        }
                    },
                }
            ) + "\n"
            session.write_text(records, encoding="utf-8")
            audit = MODULE.read_plan_audit(session)
            fixture = MODULE.load_fixture(MODULE.repo_root() / MODULE.DEFAULT_FIXTURE)
            with self.assertRaisesRegex(MODULE.PlanAcceptanceError, "read-only"):
                MODULE.validate_audit(audit, fixture, 10_000)

    def test_workspace_digest_detects_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            target = workspace / "README.md"
            target.write_text("before", encoding="utf-8")
            before = MODULE.workspace_digest(workspace)
            target.write_text("after", encoding="utf-8")
            self.assertNotEqual(before, MODULE.workspace_digest(workspace))

    def test_safe_manifest_contains_only_relative_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            session = Path(temporary) / "session.jsonl"
            write_audit(session)
            audit = MODULE.read_plan_audit(session)
            fixture = MODULE.load_fixture(MODULE.repo_root() / MODULE.DEFAULT_FIXTURE)
            identity = MODULE.SUPPORT.BinaryIdentity(
                "sigil",
                "a" * 64,
                "0.0.1-alpha.4",
                "b" * 12,
                "aarch64-apple-darwin",
                "release",
            )
            manifest = MODULE.safe_manifest(
                status="passed",
                identity=identity,
                fixture=fixture,
                started_at="2026-07-16T00:00:00+00:00",
                finished_at="2026-07-16T00:00:01+00:00",
                duration_ms=1000,
                max_cost_microusd=10_000,
                charged_microusd=10_000,
                audit=audit,
                workspace_unchanged=True,
                artifact_policy="local_only_under_git_ignored_output",
                failure_class=None,
            )
            serialized = json.dumps(manifest)
            self.assertNotIn(temporary, serialized)
            self.assertNotIn(fixture.prompt, serialized)
            self.assertEqual(manifest["evidence"]["session"], "session.jsonl")


if __name__ == "__main__":
    unittest.main()
