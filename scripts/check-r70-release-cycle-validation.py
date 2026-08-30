#!/usr/bin/env python3
"""Validate the external evidence required before final R70.8 deletion.

The repository can prove the public package boundary locally, but it cannot infer that a package
was published or that a real user exercised it. This checker accepts only an explicit, immutable
release-cycle evidence record and fails closed for missing or synthetic-looking fields.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


EXPECTED_PACKAGES = ["sigil-tui-core", "sigil-tui-ratatui", "sigil-tui"]
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def validate_evidence(document: object) -> list[str]:
    if not isinstance(document, dict):
        return ["evidence must be a JSON object"]
    errors: list[str] = []
    required_strings = (
        "release_tag",
        "published_at",
        "release_cycle_id",
        "user_validation_id",
        "implementation_commit",
    )
    for key in required_strings:
        value = document.get(key)
        if not isinstance(value, str) or not value.strip():
            errors.append(f"{key} must be a non-empty string")
    if document.get("schema_version") != "r70-release-cycle-validation-v1":
        errors.append("schema_version must be r70-release-cycle-validation-v1")
    if document.get("preview_version") != "0.1.0":
        errors.append("preview_version must be 0.1.0")
    if document.get("published") is not True:
        errors.append("published must be true")
    if document.get("release_cycle_completed") is not True:
        errors.append("release_cycle_completed must be true")
    if document.get("user_validation_status") != "passed":
        errors.append("user_validation_status must be passed")
    if document.get("published_packages") != EXPECTED_PACKAGES:
        errors.append(f"published_packages must be exactly {EXPECTED_PACKAGES!r} in order")
    implementation_commit = document.get("implementation_commit")
    if isinstance(implementation_commit, str) and not SHA_RE.fullmatch(implementation_commit):
        errors.append("implementation_commit must be a 40-character lowercase git SHA")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        document = json.loads(args.evidence.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"r70 release-cycle validation: cannot load evidence: {error}", file=sys.stderr)
        return 2
    errors = validate_evidence(document)
    if errors:
        for error in errors:
            print(f"r70 release-cycle validation: {error}", file=sys.stderr)
        return 2
    print(json.dumps({"status": "pass", "release_tag": document["release_tag"]}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
