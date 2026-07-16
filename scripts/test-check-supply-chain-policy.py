#!/usr/bin/env python3
"""Unit tests for the supply-chain policy consistency gate."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("check-supply-chain-policy.py")
SPEC = importlib.util.spec_from_file_location("check_supply_chain_policy", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
check_policy = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = check_policy
SPEC.loader.exec_module(check_policy)


class SupplyChainPolicyTests(unittest.TestCase):
    def test_matching_policy_workflow_and_ledger_pass(self) -> None:
        ids = check_policy.validate_policy(
            '[advisories]\nignore = ["RUSTSEC-2025-0001"]\n',
            "cargo audit --ignore RUSTSEC-2025-0001",
            "Reviewed exception `RUSTSEC-2025-0001`.",
        )
        self.assertEqual(ids, {"RUSTSEC-2025-0001"})

    def test_workflow_drift_fails(self) -> None:
        with self.assertRaisesRegex(ValueError, "workflow exceptions differ"):
            check_policy.validate_policy(
                '[advisories]\nignore = ["RUSTSEC-2025-0001"]\n',
                "cargo audit --ignore RUSTSEC-2025-0002",
                "RUSTSEC-2025-0001",
            )

    def test_unexplained_or_invalid_policy_fails(self) -> None:
        with self.assertRaisesRegex(ValueError, "does not explain"):
            check_policy.validate_policy(
                '[advisories]\nignore = ["RUSTSEC-2025-0001"]\n',
                "RUSTSEC-2025-0001",
                "no reviewed exception",
            )
        with self.assertRaisesRegex(ValueError, "invalid advisory ids"):
            check_policy.validate_policy(
                '[advisories]\nignore = ["CVE-2025-0001"]\n',
                "CVE-2025-0001",
                "CVE-2025-0001",
            )


if __name__ == "__main__":
    unittest.main()
