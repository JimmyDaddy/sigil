#!/usr/bin/env python3
"""Contract tests for the RFC-0034 real-provider aggregate runner."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("real-provider-dogfood-campaign.py")
SPEC = importlib.util.spec_from_file_location("real_provider_dogfood_campaign", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AdmissionTests(unittest.TestCase):
    def test_default_matrix_contains_edit_verification_safety_and_plan(self) -> None:
        self.assertEqual(
            MODULE.selected_cases(None),
            [
                "small-code-edit",
                "stale-after-write",
                "workspace-trust",
                "sandbox-denial",
                "plan-only",
            ],
        )

    def test_budget_is_partitioned_without_exceeding_total_admission(self) -> None:
        allocations = MODULE.budget_allocations(500_000, MODULE.CASE_ORDER, 1)
        plan = allocations["plan_run_budgets_microusd"]
        model = allocations["model_budget_microusd"]
        self.assertEqual(allocations["planned_runs"], 5)
        self.assertEqual(sum(plan) + model, 500_000)
        self.assertEqual(allocations["base_reservation_microusd"], 100_000)
        self.assertEqual(MODULE.format_microusd(plan[0]), "0.100000")

    def test_reported_plan_overrun_stops_later_provider_admission(self) -> None:
        self.assertEqual(
            MODULE.admission_failure(
                accounting_charged_microusd=150_000,
                next_reservation_microusd=400_000,
                max_cost_microusd=500_000,
                remaining_seconds=500,
            ),
            "budget_exhausted_before_admission",
        )
        self.assertIsNone(
            MODULE.admission_failure(
                accounting_charged_microusd=100_000,
                next_reservation_microusd=400_000,
                max_cost_microusd=500_000,
                remaining_seconds=500,
            )
        )

    def test_unknown_plan_cost_consumes_remaining_local_admission(self) -> None:
        charged, confidence = MODULE.account_plan_result(
            accounting_charged_microusd=100_000,
            max_cost_microusd=500_000,
            reservation_microusd=100_000,
            result={"charged_microusd": 100_000, "checks": {}},
        )
        self.assertEqual(charged, 500_000)
        self.assertEqual(confidence, "unknown")
        self.assertEqual(
            MODULE.admission_failure(
                accounting_charged_microusd=charged,
                next_reservation_microusd=400_000,
                max_cost_microusd=500_000,
                remaining_seconds=500,
            ),
            "budget_exhausted_before_admission",
        )

    def test_environment_keeps_provider_inputs_but_drops_ambient_sigil_overrides(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.dict(
                os.environ,
                {
                    "SIGIL_API_KEY": "provider-key",
                    "SIGIL_ANTHROPIC_API_KEY": "other-provider-key",
                    "SIGIL_STATE_HOME": "/private/state",
                    "SIGIL_CONFIG": "/private/config",
                    "UNRELATED_SECRET": "drop-me",
                },
                clear=False,
            ):
                environment = MODULE.child_environment(Path(temporary), "deepseek")
            self.assertEqual(environment["SIGIL_API_KEY"], "provider-key")
            self.assertNotIn("SIGIL_ANTHROPIC_API_KEY", environment)
            self.assertNotIn("SIGIL_STATE_HOME", environment)
            self.assertNotIn("SIGIL_CONFIG", environment)
            self.assertNotIn("UNRELATED_SECRET", environment)


class EvidenceTests(unittest.TestCase):
    def test_model_result_projection_drops_provider_and_session_content(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            record = {
                "report_schema_version": 3,
                "repetition": 1,
                "acceptance_passed": True,
                "execution_status": "completed",
                "result": {
                    "metadata": {
                        "case_id": "small-code-edit",
                        "provider": "private-provider",
                    },
                    "session_log_path": "/private/session.jsonl",
                },
            }
            (output / "results.jsonl").write_text(json.dumps(record) + "\n", encoding="utf-8")
            (output / "manifest.json").write_text(
                json.dumps(
                    {
                        "report_schema_version": 3,
                        "requested_repetitions": 1,
                        "charged_microusd": 1234,
                    }
                ),
                encoding="utf-8",
            )
            results, charged = MODULE.parse_model_results(output, ["small-code-edit"], 1)
            serialized = json.dumps(results)
            self.assertEqual(charged, 1234)
            self.assertNotIn("private-provider", serialized)
            self.assertNotIn("/private/session", serialized)
            self.assertEqual(results[0]["status"], "passed")
            self.assertRegex(results[0]["manifest_sha256"], r"^[0-9a-f]{64}$")
            self.assertRegex(results[0]["results_sha256"], r"^[0-9a-f]{64}$")

    def test_missing_model_terminal_evidence_is_failed_not_omitted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            results, charged = MODULE.parse_model_results(
                Path(temporary),
                ["workspace-trust", "sandbox-denial"],
                2,
            )
            self.assertEqual(charged, 0)
            self.assertEqual(len(results), 4)
            self.assertTrue(all(result["status"] == "failed" for result in results))
            self.assertTrue(
                all(result["execution_status"] == "missing_or_invalid_evidence" for result in results)
            )

    def test_duplicate_or_corrupt_model_evidence_fails_the_whole_projection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            record = {
                "report_schema_version": 3,
                "repetition": 1,
                "acceptance_passed": True,
                "execution_status": "completed",
                "result": {"metadata": {"case_id": "small-code-edit"}},
            }
            (output / "results.jsonl").write_text(
                json.dumps(record) + "\n" + json.dumps(record) + "\n",
                encoding="utf-8",
            )
            (output / "manifest.json").write_text(
                json.dumps(
                    {
                        "report_schema_version": 3,
                        "requested_repetitions": 1,
                        "charged_microusd": 1234,
                    }
                ),
                encoding="utf-8",
            )
            results, charged = MODULE.parse_model_results(output, ["small-code-edit"], 1)
            self.assertEqual(charged, 0)
            self.assertEqual(results[0]["status"], "failed")
            self.assertEqual(results[0]["failure_class"], "invalid_model_evidence")
            self.assertRegex(results[0]["manifest_sha256"], r"^[0-9a-f]{64}$")
            self.assertRegex(results[0]["results_sha256"], r"^[0-9a-f]{64}$")

    def test_safe_manifest_uses_only_public_identity_and_relative_evidence(self) -> None:
        identity = MODULE.SUPPORT.BinaryIdentity(
            "sigil",
            "a" * 64,
            "0.0.1-alpha.4",
            "b" * 12,
            "aarch64-apple-darwin",
            "release",
        )
        results = [
            {
                "case_id": "plan-only",
                "repetition": 1,
                "status": "passed",
                "evidence_dir": "plan-only/repetition-1",
            }
        ]
        manifest = MODULE.safe_manifest(
            status="passed",
            started_at="2026-07-16T00:00:00+00:00",
            finished_at="2026-07-16T00:00:01+00:00",
            duration_ms=1000,
            identity=identity,
            selected=["plan-only"],
            repetitions=1,
            timeout_secs=600,
            max_cost_microusd=500_000,
            accounting_charged_microusd=100_000,
            accounting_confidence="reported_or_reserved",
            campaign_failure_class=None,
            results=results,
            artifact_policy="local_only_under_git_ignored_output",
        )
        serialized = json.dumps(manifest)
        self.assertNotIn("/Users/", serialized)
        self.assertFalse(manifest["budget"]["provider_side_cap"])
        self.assertEqual(manifest["budget"]["accounting_confidence"], "reported_or_reserved")
        self.assertIsNone(manifest["failure_class"])
        self.assertEqual(manifest["results"][0]["evidence_dir"], "plan-only/repetition-1")

    def test_terminal_campaign_error_fails_even_when_all_case_results_passed(self) -> None:
        results = [{"case_id": "plan-only", "repetition": 1, "status": "passed"}]
        self.assertEqual(MODULE.terminal_status(results, 1, None), "passed")
        self.assertEqual(
            MODULE.terminal_status(results, 1, "campaign_internal_error"),
            "failed",
        )

    def test_orchestration_error_after_admission_writes_terminal_manifest(self) -> None:
        identity = MODULE.SUPPORT.BinaryIdentity(
            "sigil",
            "a" * 64,
            "0.0.1-alpha.4",
            "b" * 12,
            "aarch64-apple-darwin",
            "release",
        )
        source_config = mock.Mock(path=Path("/tmp/config.toml"), provider="deepseek")
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "campaign"
            with (
                mock.patch.object(MODULE.PLAN, "validate_source_config", return_value=source_config),
                mock.patch.object(MODULE.PLAN, "load_fixture", return_value={}),
                mock.patch.object(
                    MODULE.SUPPORT,
                    "inspect_binary",
                    return_value=(Path("/tmp/sigil"), identity),
                ),
                mock.patch.object(MODULE.SUPPORT, "assert_expected_identity"),
                mock.patch.object(
                    MODULE.SUPPORT,
                    "raw_artifact_policy",
                    return_value="local_only_under_git_ignored_output",
                ),
                mock.patch.object(
                    MODULE.SUPPORT,
                    "freeze_binary",
                    side_effect=OSError("private path must not enter the manifest"),
                ),
            ):
                exit_code = MODULE.main(
                    [
                        "--binary",
                        "/tmp/sigil",
                        "--config",
                        "/tmp/config.toml",
                        "--case",
                        "plan-only",
                        "--repetitions",
                        "1",
                        "--max-cost-usd",
                        "0.100000",
                        "--timeout-secs",
                        "60",
                        "--output-dir",
                        str(output),
                    ]
                )

            self.assertEqual(exit_code, 1)
            manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["status"], "failed")
            self.assertEqual(manifest["budget"]["accounting_confidence"], "unknown")
            self.assertEqual(manifest["results"][0]["failure_class"], "campaign_internal_error")
            self.assertNotIn("private path", json.dumps(manifest))


if __name__ == "__main__":
    unittest.main()
