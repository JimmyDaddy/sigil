#!/usr/bin/env python3
"""Deterministic tests for the R71.8 qualification contract."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

import r71_qualification_common as common


class QualificationContractTests(unittest.TestCase):
    def test_frozen_manifests_have_exact_required_sets(self) -> None:
        conformance = common.load_conformance_manifest()
        self.assertEqual(len(conformance["cases"]), 228)
        platform = common.load_platform_manifest()
        self.assertEqual(
            {job["job_id"] for job in platform["jobs"]}, common.REQUIRED_PLATFORM_JOBS
        )

    def test_sha_validation_rejects_short_or_non_hex_values(self) -> None:
        with self.assertRaises(ValueError):
            common.validate_sha("abc", "candidate_sha")
        with self.assertRaises(ValueError):
            common.validate_sha("z" * 40, "candidate_sha")

    def test_manifest_digest_is_stable_across_checkout_line_endings(self) -> None:
        with tempfile.TemporaryDirectory(prefix="r71-manifest-digest-") as directory:
            path = Path(directory) / "manifest.toml"
            path.write_bytes(b"[manifest]\ncase = 1\n")
            lf_digest = common.sha256_canonical_text_file(path)
            path.write_bytes(b"[manifest]\r\ncase = 1\r\n")
            self.assertEqual(lf_digest, common.sha256_canonical_text_file(path))

    def test_clean_candidate_and_ancestor_are_required(self) -> None:
        with tempfile.TemporaryDirectory(prefix="r71-qualification-meta-") as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", "-b", "main"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "r71@example.invalid"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "R71 Test"], cwd=root, check=True)
            (root / "fixture.txt").write_text("one\n", encoding="utf-8")
            subprocess.run(["git", "add", "fixture.txt"], cwd=root, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "fixture"], cwd=root, check=True)
            base = common.git(root, "rev-parse", "HEAD")
            (root / "fixture.txt").write_text("two\n", encoding="utf-8")
            subprocess.run(["git", "add", "fixture.txt"], cwd=root, check=True)
            subprocess.run(["git", "commit", "-q", "-m", "candidate"], cwd=root, check=True)
            candidate = common.git(root, "rev-parse", "HEAD")
            identity = common.validate_git_identity(root, candidate, base)
            self.assertEqual(identity["candidate_sha"], candidate)
            (root / "dirty.txt").write_text("must fail\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "clean checkout"):
                common.validate_git_identity(root, candidate, base)

    def test_fixed_workflow_job_ids_and_dispatch_contract_are_present(self) -> None:
        workflow = (common.ROOT / ".github/workflows/sandbox-conformance.yml").read_text(
            encoding="utf-8"
        )
        dispatch = (common.ROOT / "scripts/dispatch-r71-platform-qualification.sh").read_text(
            encoding="utf-8"
        )
        for job_id in sorted(common.REQUIRED_PLATFORM_JOBS):
            self.assertIn(f"  {job_id}:", workflow)
        self.assertIn("--ref r71-release-candidate", dispatch)
        self.assertIn("-f require_conformance=true", dispatch)
        self.assertIn('run.get("headSha") != candidate', dispatch)

    def test_shipping_tui_gate_is_included_in_full_release_wrapper(self) -> None:
        wrapper = (common.ROOT / "scripts/run-r71-release-qualification.sh").read_text(
            encoding="utf-8"
        )
        gate = common.ROOT / "scripts/run-r71-tui-shipping-e2e.sh"
        self.assertTrue(gate.is_file())
        self.assertIn(
            "run_step tui-shipping-e2e bash scripts/run-r71-tui-shipping-e2e.sh",
            wrapper,
        )

    def test_bootstrap_namespace_isolated_and_marker_bound_cleanup_is_required(self) -> None:
        wrapper = (common.ROOT / "scripts/run-r71-release-qualification.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn('export HOME="$qualification_home"', wrapper)
        self.assertIn('export SIGIL_R71_BOOTSTRAP_ISOLATED=1', wrapper)
        self.assertIn('qualification_corepack_home="${COREPACK_HOME:-$qualification_original_home/.cache/node/corepack}"', wrapper)
        self.assertIn('export COREPACK_HOME="$qualification_corepack_home"', wrapper)
        self.assertIn('export VSCMD_SKIP_SENDTELEMETRY=1', wrapper)
        self.assertIn('qualification_windows_vctip_baseline=""', wrapper)
        self.assertIn('export SIGIL_R71_QUALIFICATION_VCTIP_BASELINE=', wrapper)
        self.assertIn('settle_qualification_windows_helpers()', wrapper)
        self.assertIn('$identity = "{0}:{1}" -f $_.Id, $_.StartTime.ToUniversalTime().Ticks', wrapper)
        self.assertIn('$baseline -notcontains $identity', wrapper)
        self.assertIn('$ErrorActionPreference = "Stop"', wrapper)
        self.assertIn('export HOME="$qualification_original_home"', wrapper)
        self.assertIn('export USERPROFILE="$qualification_original_userprofile"', wrapper)
        self.assertIn('export RUST_TEST_THREADS=1', wrapper)
        self.assertIn('qualification_home_marker="$qualification_home/.sigil-r71-qualification-home"', wrapper)
        self.assertIn('refusing qualification HOME cleanup without ownership marker', wrapper)
        self.assertIn('-L "$qualification_home_marker"', wrapper)
        self.assertIn('return 1', wrapper)
        self.assertIn('for _qualification_cleanup_pass in 1 2 3', wrapper)
        self.assertIn('-delete 2>/dev/null || true', wrapper)
        self.assertIn('qualification HOME cleanup left unexpected residue', wrapper)
        self.assertIn('find "$qualification_home" -mindepth 1 -maxdepth 8 -print', wrapper)
        self.assertIn('"bootstrap_root_residue_count_before_cleanup"', wrapper)


if __name__ == "__main__":
    unittest.main()
