#!/usr/bin/env python3
"""Contract tests for the alpha dogfood campaign orchestrator."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts" / "alpha-dogfood-campaign.py"
SPEC = importlib.util.spec_from_file_location("alpha_dogfood_campaign", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("failed to load alpha dogfood campaign module")
campaign = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = campaign
SPEC.loader.exec_module(campaign)

FEEDBACK_MODULE_PATH = ROOT / "scripts" / "tui-feedback-pty-acceptance.py"
FEEDBACK_SPEC = importlib.util.spec_from_file_location(
    "tui_feedback_pty_acceptance", FEEDBACK_MODULE_PATH
)
if FEEDBACK_SPEC is None or FEEDBACK_SPEC.loader is None:
    raise RuntimeError("failed to load feedback PTY acceptance module")
feedback_acceptance = importlib.util.module_from_spec(FEEDBACK_SPEC)
FEEDBACK_SPEC.loader.exec_module(feedback_acceptance)


class AlphaDogfoodCampaignTests(unittest.TestCase):
    def identity(self) -> campaign.BinaryIdentity:
        return campaign.BinaryIdentity(
            label="sigil",
            sha256="a" * 64,
            version="0.0.1-alpha.4",
            commit="f4e6c5aeea86",
            target="aarch64-apple-darwin",
            profile="release",
        )

    def test_version_parser_requires_complete_build_identity(self) -> None:
        identity = campaign.parse_version_output(
            "sigil 0.0.1-alpha.4\ncommit: f4e6c5aeea86\n"
            "target: aarch64-apple-darwin\nprofile: release\n",
            label="sigil",
            sha256="a" * 64,
        )
        self.assertEqual(identity, self.identity())
        with self.assertRaisesRegex(campaign.CampaignError, "missing fields"):
            campaign.parse_version_output(
                "sigil 0.0.1-alpha.4\ncommit: f4e6c5aeea86\n",
                label="sigil",
                sha256="a" * 64,
            )

    def test_expected_identity_accepts_full_commit_and_rejects_digest_drift(self) -> None:
        campaign.assert_expected_identity(
            self.identity(),
            expected_version="0.0.1-alpha.4",
            expected_commit="f4e6c5aeea86b3283988efe20db44a0f97454f97",
            expected_sha256="a" * 64,
        )
        with self.assertRaisesRegex(campaign.CampaignError, "SHA-256 mismatch"):
            campaign.assert_expected_identity(
                self.identity(),
                expected_version=None,
                expected_commit=None,
                expected_sha256="b" * 64,
            )

    def test_case_environment_strips_credentials_config_and_ambient_proxy(self) -> None:
        source = {
            "PATH": os.defpath,
            "LANG": "en_US.UTF-8",
            "SIGIL_API_KEY": "secret",
            "OPENAI_API_KEY": "secret",
            "SIGIL_CONFIG": "/private/config.toml",
            "HTTPS_PROXY": "https://private.proxy.example",
            "GH_TOKEN": "secret",
            "CARGO_HOME": "/private/cargo",
            "RUSTUP_HOME": "/private/rustup",
        }
        with tempfile.TemporaryDirectory() as temporary:
            case_root = Path(temporary) / "case"
            environment = campaign.case_environment(source, case_root)
            self.assertNotIn("SIGIL_API_KEY", environment)
            self.assertNotIn("OPENAI_API_KEY", environment)
            self.assertNotIn("SIGIL_CONFIG", environment)
            self.assertNotIn("GH_TOKEN", environment)
            self.assertNotIn("CARGO_HOME", environment)
            self.assertNotIn("RUSTUP_HOME", environment)
            self.assertEqual(environment["HTTPS_PROXY"], "http://127.0.0.1:1")
            self.assertEqual(environment["NO_PROXY"], "127.0.0.1,localhost,::1")
            self.assertTrue(environment["HOME"].startswith(str(case_root)))
            self.assertTrue(environment["XDG_STATE_HOME"].startswith(str(case_root)))
            self.assertTrue(environment["XDG_CACHE_HOME"].startswith(str(case_root)))
            self.assertNotIn("SIGIL_STATE_HOME", environment)
            self.assertNotIn("SIGIL_CACHE_HOME", environment)

    def test_feedback_export_marker_tolerates_absolute_cursor_repaint(self) -> None:
        rendered = (
            "Feedback Report Saved locally. Nothing was uploded. "
            "Enter review JSON O reveal file B open bug form"
        )
        self.assertTrue(feedback_acceptance.feedback_export_result_visible(rendered))
        self.assertFalse(
            feedback_acceptance.feedback_export_result_visible(
                "Feedback Report Enter review JSON B open bug form"
            )
        )

    def test_case_selection_is_canonical_and_deduplicated(self) -> None:
        self.assertEqual(
            campaign.selected_cases(["image", "context", "image"]),
            ["context", "image"],
        )
        self.assertEqual(campaign.selected_cases(None), list(campaign.CASE_ORDER))

    def test_repository_output_must_be_git_ignored(self) -> None:
        with self.assertRaisesRegex(campaign.CampaignError, "must be git-ignored"):
            campaign.raw_artifact_policy(ROOT, ROOT / "dogfood-output-contract-test")
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(
                campaign.raw_artifact_policy(ROOT, Path(temporary) / "evidence"),
                "local_only_under_selected_output",
            )

    def test_mismatched_digest_fails_before_output_creation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            binary = temporary_root / "source-sigil"
            frozen = temporary_root / "frozen-sigil"
            output = temporary_root / "campaign-output"
            with mock.patch.object(
                campaign, "freeze_binary", return_value=(frozen, self.identity())
            ):
                return_code = campaign.main(
                    [
                        "--binary",
                        str(binary),
                        "--expected-sha256",
                        "b" * 64,
                        "--output-dir",
                        str(output),
                    ]
                )
            self.assertEqual(return_code, 2)
            self.assertFalse(output.exists())

    def test_script_launchers_are_rejected_before_output_creation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            for name, shebang in (
                ("sigil.js", "#!/usr/bin/env node\n"),
                ("sigil.sh", "#!/bin/sh\n"),
            ):
                launcher = temporary_root / name
                launcher.write_text(shebang, encoding="utf-8")
                launcher.chmod(0o700)
                output = temporary_root / f"{name}-output"
                return_code = campaign.main(
                    ["--binary", str(launcher), "--output-dir", str(output)]
                )
                self.assertEqual(return_code, 2)
                self.assertFalse(output.exists())

    def test_native_executable_format_recognizes_supported_magics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            elf = temporary_root / "elf"
            elf.write_bytes(b"\x7fELF" + b"\x00" * 60)
            mach_o = temporary_root / "mach-o"
            mach_o.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 60)
            pe = temporary_root / "pe.exe"
            pe.write_bytes(
                b"MZ" + b"\x00" * 58 + (64).to_bytes(4, "little") + b"PE\x00\x00"
            )
            script = temporary_root / "script"
            script.write_text("#!/bin/sh\n", encoding="utf-8")
            self.assertEqual(campaign.native_executable_format(elf), "elf")
            self.assertEqual(campaign.native_executable_format(mach_o), "mach-o")
            self.assertEqual(campaign.native_executable_format(pe), "pe")
            self.assertIsNone(campaign.native_executable_format(script))

    def test_frozen_copy_format_is_authoritative(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            source = temporary_root / "source-sigil"
            source.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 60)
            source.chmod(0o700)
            frozen_root = temporary_root / "frozen"
            frozen_root.mkdir()
            with mock.patch.object(
                campaign,
                "native_executable_format",
                side_effect=("mach-o", None),
            ):
                with self.assertRaisesRegex(campaign.CampaignError, "changed during admission"):
                    campaign.freeze_binary(source, frozen_root)

    def test_failed_case_continues_and_writes_terminal_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            binary = temporary_root / "source-sigil"
            frozen = temporary_root / "frozen-sigil"
            failure = temporary_root / "failure.py"
            failure.write_text("raise SystemExit(1)\n", encoding="utf-8")
            success = temporary_root / "success.py"
            success.write_text(
                "import sys\n"
                "from pathlib import Path\n"
                "binary = Path(sys.argv[sys.argv.index('--binary') + 1])\n"
                f"raise SystemExit(0 if binary == Path({str(frozen)!r}) else 3)\n",
                encoding="utf-8",
            )
            output = temporary_root / "campaign-output"
            with (
                mock.patch.object(campaign, "CASE_ORDER", ("failure", "success")),
                mock.patch.object(
                    campaign,
                    "CASE_SCRIPTS",
                    {"failure": str(failure), "success": str(success)},
                ),
                mock.patch.object(
                    campaign,
                    "freeze_binary",
                    return_value=(frozen, self.identity()),
                ),
            ):
                return_code = campaign.main(
                    [
                        "--binary",
                        str(binary),
                        "--expected-sha256",
                        "a" * 64,
                        "--case",
                        "failure",
                        "--case",
                        "success",
                        "--output-dir",
                        str(output),
                        "--timeout",
                        "30",
                    ]
                )
            self.assertEqual(return_code, 1)
            manifest = json.loads((output / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["status"], "failed")
            self.assertEqual(
                [(item["id"], item["status"]) for item in manifest["cases"]],
                [("failure", "failed"), ("success", "passed")],
            )

    @unittest.skipUnless(os.name == "posix", "detached process cleanup requires POSIX")
    def test_terminate_process_reaps_detached_descendant(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_root = Path(temporary)
            child_pid_file = temporary_root / "child.pid"
            outer_script = temporary_root / "outer.py"
            outer_script.write_text(
                "import subprocess, sys, time\n"
                "from pathlib import Path\n"
                "child = subprocess.Popen(\n"
                "    [sys.executable, '-c', 'import time; time.sleep(60)'],\n"
                "    start_new_session=True,\n"
                ")\n"
                "Path(sys.argv[1]).write_text(str(child.pid), encoding='utf-8')\n"
                "time.sleep(60)\n",
                encoding="utf-8",
            )
            outer = subprocess.Popen(
                [sys.executable, str(outer_script), str(child_pid_file)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            child_pid: int | None = None
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and not child_pid_file.exists():
                    time.sleep(0.05)
                self.assertTrue(child_pid_file.exists())
                child_pid = int(child_pid_file.read_text(encoding="utf-8"))
                self.assertTrue(campaign.process_is_running(child_pid))
                campaign.terminate_process(outer)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and campaign.process_is_running(child_pid):
                    time.sleep(0.05)
                self.assertFalse(campaign.process_is_running(child_pid))
            finally:
                if outer.poll() is None:
                    os.killpg(outer.pid, signal.SIGKILL)
                    outer.wait(timeout=5)
                if child_pid is not None and campaign.process_is_running(child_pid):
                    campaign.signal_processes({child_pid}, signal.SIGKILL)

    def test_every_case_uses_an_existing_script_and_explicit_binary(self) -> None:
        binary = ROOT / "target" / "release" / "sigil"
        evidence = ROOT / ".repo-local-dev" / "dogfood" / "evidence"
        for case_id in campaign.CASE_ORDER:
            command = campaign.case_command(
                ROOT,
                case_id,
                binary,
                evidence,
                180,
                skip_clipboard=True,
            )
            self.assertEqual(command[0], sys.executable)
            self.assertIn(str(binary), command)
            self.assertIn(str(evidence), command)
            self.assertTrue((ROOT / campaign.CASE_SCRIPTS[case_id]).is_file())

    def test_aggregate_manifest_contains_only_safe_relative_evidence(self) -> None:
        result = campaign.CaseResult(
            case_id="context",
            status="passed",
            duration_ms=42,
            evidence_dir="cases/context/evidence",
            runner_log="cases/context/runner.log",
        )
        payload = campaign.manifest_payload(
            status="passed",
            started_at="2026-07-16T00:00:00+00:00",
            finished_at="2026-07-16T00:00:01+00:00",
            identity=self.identity(),
            cases=[result],
        )
        serialized = json.dumps(payload, sort_keys=True)
        self.assertNotIn("/Users/private", serialized)
        self.assertNotIn("prompt", serialized.lower())
        self.assertNotIn("provider_response", serialized.lower())
        self.assertIn('"absolute_paths_in_aggregate": false', serialized)
        self.assertIn('"os_network_sandbox": false', serialized)



if __name__ == "__main__":
    unittest.main()
