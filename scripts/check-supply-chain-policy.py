#!/usr/bin/env python3
"""Keep RustSec advisory exceptions aligned across policy, CI, and governance."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ADVISORY_ID = re.compile(r"RUSTSEC-\d{4}-\d{4}")


def validate_policy(policy_text: str, workflow_text: str, ledger_text: str) -> set[str]:
    """Return reviewed advisory IDs or raise when any committed surface drifts."""
    policy = tomllib.loads(policy_text)
    configured = policy.get("advisories", {}).get("ignore", [])
    if not isinstance(configured, list) or not all(
        isinstance(value, str) for value in configured
    ):
        raise ValueError("deny.toml advisories.ignore must be a string list")
    if len(configured) != len(set(configured)):
        raise ValueError("deny.toml advisories.ignore contains duplicate entries")

    policy_ids = set(configured)
    invalid = sorted(value for value in policy_ids if ADVISORY_ID.fullmatch(value) is None)
    if invalid:
        raise ValueError(f"deny.toml contains invalid advisory ids: {', '.join(invalid)}")

    workflow_ids = set(ADVISORY_ID.findall(workflow_text))
    if workflow_ids != policy_ids:
        raise ValueError(
            "cargo-audit workflow exceptions differ from deny.toml: "
            f"policy={sorted(policy_ids)} workflow={sorted(workflow_ids)}"
        )

    ledger_ids = set(ADVISORY_ID.findall(ledger_text))
    missing = sorted(policy_ids - ledger_ids)
    if missing:
        raise ValueError(
            "dependency ledger does not explain advisory exceptions: " + ", ".join(missing)
        )
    return policy_ids


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    try:
        advisory_ids = validate_policy(
            (root / "deny.toml").read_text(encoding="utf-8"),
            (root / ".github/workflows/dependency-supply-chain.yml").read_text(
                encoding="utf-8"
            ),
            (root / "dev/governance/dependency-supply-chain.md").read_text(
                encoding="utf-8"
            ),
        )
    except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"supply-chain policy check failed: {error}", file=sys.stderr)
        return 1
    print(f"supply-chain advisory policy aligned: {', '.join(sorted(advisory_ids))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
