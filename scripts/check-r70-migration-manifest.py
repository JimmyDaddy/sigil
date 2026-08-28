#!/usr/bin/env python3
"""RFC-0070 R70.0 production command/event discovery and manifest gate.

The allowlist in this file is deliberately closed.  It names the production protocol enums
that R70.0 freezes; the checker does not walk every Rust enum and it never classifies an unknown
variant by a wildcard.  Every discovered ``(enum, variant)`` pair must have one explicit row in
the source-controlled manifest.
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "dev/governance/r70-command-event-migration-v1.toml"

# ``CommandSurface`` is a routing category, not a command/event protocol.  It is intentionally
# not in this list; adding a new protocol enum requires a code review change here and a manifest
# refresh, rather than silently expanding discovery.
ENUM_SPECS = (
    ("sigil-kernel", "crates/sigil-kernel/src/resource_recovery_surface.rs", "ResourceRecoveryActionV1"),
    ("sigil-tui", "crates/sigil-tui/src/runner/protocol.rs", "WorkerApprovalCommand"),
    ("sigil-tui", "crates/sigil-tui/src/runner/protocol.rs", "WorkerCommand"),
    ("sigil-tui", "crates/sigil-tui/src/runner/protocol.rs", "WorkerMessage"),
    ("sigil-tui", "crates/sigil-tui/src/app.rs", "AppAction"),
    ("sigil-tui", "crates/sigil-tui/src/commands.rs", "UiCommand"),
    ("sigil-http", "crates/sigil-http/src/dto.rs", "HttpApplicationClientAction"),
    ("sigil-desktop", "apps/desktop/src-tauri/src/commands.rs", "DesktopRecoveryAction"),
)

TRANSITIONAL_EDGE_SPECS = (
    ("crates/sigil-tui/src/app.rs", "sigil_runtime::application_host::"),
    ("crates/sigil-tui/src/launcher.rs", "sigil_runtime::application_host::"),
    ("crates/sigil-tui/src/runner/spawn.rs", "sigil_runtime::application_host::"),
    ("crates/sigil/src/main.rs", "sigil_runtime::application_host::"),
    ("crates/sigil-http/src/production_driver.rs", "sigil_runtime::application_host::"),
)

REQUIRED_MAPPING_FIELDS = {
    "source_enum",
    "source_variant",
    "classification",
    "disposition",
    "target",
    "typed_receipt",
    "effect_settlement",
    "replay_policy",
    "host_lane",
    "surface_exposure",
    "rationale",
    "test_id",
    "migration_phase",
}


def _strip_comments_preserving_lines(text: str) -> str:
    """Remove Rust comments while preserving line boundaries for enum scanning."""

    out: list[str] = []
    i = 0
    block_depth = 0
    string_delimiter: str | None = None
    escaped = False
    while i < len(text):
        char = text[i]
        next_char = text[i + 1] if i + 1 < len(text) else ""
        if block_depth:
            if char == "/" and next_char == "*":
                block_depth += 1
                out.extend((" ", " "))
                i += 2
                continue
            if char == "*" and next_char == "/":
                block_depth -= 1
                out.extend((" ", " "))
                i += 2
                continue
            out.append("\n" if char == "\n" else " ")
            i += 1
            continue
        if string_delimiter:
            out.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == string_delimiter:
                string_delimiter = None
            i += 1
            continue
        if char == "/" and next_char == "/":
            out.extend((" ", " "))
            i += 2
            while i < len(text) and text[i] != "\n":
                out.append(" ")
                i += 1
            continue
        if char == "/" and next_char == "*":
            block_depth = 1
            out.extend((" ", " "))
            i += 2
            continue
        if char in ('"', "'"):
            string_delimiter = char
        out.append(char)
        i += 1
    if block_depth:
        raise ValueError("unterminated Rust block comment")
    return "".join(out)


def _enum_body(text: str, enum_name: str) -> str:
    match = re.search(rf"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+{re.escape(enum_name)}\s*\{{", text)
    if not match:
        raise ValueError(f"enum not found: {enum_name}")
    start = match.end()
    depth = 1
    i = start
    quote: str | None = None
    escaped = False
    while i < len(text) and depth:
        char = text[i]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif char in ('"', "'", "`"):
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        i += 1
    if depth:
        raise ValueError(f"unterminated enum: {enum_name}")
    return text[start : i - 1]


def discover_variants(path: Path, enum_name: str) -> list[str]:
    return _discover_variants_from_text(
        _strip_comments_preserving_lines(path.read_text(encoding="utf-8")), enum_name
    )


def _discover_variants_from_text(clean: str, enum_name: str) -> list[str]:
    body = _enum_body(clean, enum_name)
    variants: list[str] = []
    depth = 0
    for raw_line in body.splitlines():
        line = raw_line.strip()
        if depth == 0 and line and not line.startswith("#"):
            match = re.match(r"([A-Za-z_][A-Za-z0-9_]*)\b", line)
            if match:
                variants.append(match.group(1))
        depth += line.count("{") - line.count("}")
        if depth < 0:
            raise ValueError(f"negative brace depth while scanning {enum_name}")
    if not variants:
        raise ValueError(f"empty production enum: {enum_name}")
    if len(variants) != len(set(variants)):
        raise ValueError(f"duplicate variant in {enum_name}")
    return variants


def discover() -> list[dict[str, object]]:
    rows: list[dict[str, object]] = []
    for package, relative_path, enum_name in ENUM_SPECS:
        path = ROOT / relative_path
        for variant in discover_variants(path, enum_name):
            rows.append(
                {
                    "package": package,
                    "source_file": relative_path,
                    "source_enum": enum_name,
                    "source_variant": variant,
                }
            )
    return rows


def _git_exists(revision: str) -> bool:
    result = subprocess.run(
        ["git", "cat-file", "-e", f"{revision}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
        check=False,
    )
    return result.returncode == 0


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    try:
        manifest = tomllib.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(f"r70 migration manifest cannot be loaded: {error}", file=sys.stderr)
        return 2

    errors: list[str] = []
    if manifest.get("schema_version") != "r70-command-event-migration-v1":
        errors.append("manifest schema_version is not r70-command-event-migration-v1")
    if manifest.get("phase") != "R70.0":
        errors.append("manifest phase is not R70.0")
    fixture_path = ROOT / str(manifest.get("transcript_fixture_path", ""))
    if not fixture_path.is_file():
        errors.append(f"missing deterministic transcript fixture: {fixture_path}")
    else:
        try:
            fixture = tomllib.loads(fixture_path.read_text(encoding="utf-8"))
            workloads = fixture.get("workloads", {})
            if workloads.get("transcript_10k") != 10000 or workloads.get("transcript_100k") != 100000:
                errors.append("transcript fixture must define fixed 10k and 100k workloads")
            mix = fixture.get("mix", {})
            if round(sum(float(value) for value in mix.values()), 6) != 1.0:
                errors.append("transcript fixture mix must sum to exactly 1.0")
        except (OSError, tomllib.TOMLDecodeError, TypeError, ValueError) as error:
            errors.append(f"invalid deterministic transcript fixture: {error}")
    for key in ("baseline_commit", "qualified_r71_candidate"):
        value = manifest.get(key)
        if not isinstance(value, str) or not _git_exists(value):
            errors.append(f"{key} does not resolve to a commit: {value!r}")

    handoff_path = ROOT / str(manifest.get("r71_handoff_path", ""))
    if not handoff_path.is_file():
        errors.append(f"missing R71 handoff manifest: {handoff_path}")
    else:
        handoff = handoff_path.read_text(encoding="utf-8")
        for marker in ("ec5459d829e086fbb73f090dcb3201f649d99d7b", "Implemented / Frozen"):
            if marker not in handoff:
                errors.append(f"R71 handoff is missing closure marker: {marker}")

    contracts = manifest.get("frozen_contracts", [])
    if not isinstance(contracts, list) or not contracts:
        errors.append("frozen_contracts must be a non-empty list")
    else:
        for contract in contracts:
            if not isinstance(contract, dict):
                errors.append("frozen_contracts contains a non-table row")
                continue
            path = ROOT / str(contract.get("path", ""))
            expected = contract.get("sha256")
            if not path.is_file():
                errors.append(f"frozen contract path does not exist: {path}")
            elif _sha256(path) != expected:
                errors.append(f"frozen contract changed: {path}")

    discovered = discover()
    discovered_keys = {
        (row["source_enum"], row["source_variant"])
        for row in discovered
    }
    mappings = manifest.get("mapping", [])
    mapping_keys: set[tuple[object, object]] = set()
    if not isinstance(mappings, list) or not mappings:
        errors.append("mapping must be a non-empty list")
        mappings = []

    expected_scope = {
        (package, relative_path, enum_name, "production")
        for package, relative_path, enum_name in ENUM_SPECS
    }
    actual_scope = {
        (
            row.get("package"),
            row.get("path"),
            row.get("enum"),
            row.get("classification"),
        )
        for row in manifest.get("source_scope", [])
        if isinstance(row, dict)
    }
    if actual_scope != expected_scope:
        errors.append("source_scope does not exactly match the closed production enum allowlist")

    expected_edges = {path for path, _needle in TRANSITIONAL_EDGE_SPECS}
    actual_edges = {
        row.get("path")
        for row in manifest.get("transitional_facade_edges", [])
        if isinstance(row, dict)
    }
    if actual_edges != expected_edges:
        errors.append("transitional_facade_edges does not exactly match the frozen consumer list")
    for path, needle in TRANSITIONAL_EDGE_SPECS:
        source_path = ROOT / path
        if not source_path.is_file() or needle not in source_path.read_text(encoding="utf-8"):
            errors.append(f"frozen transitional edge is absent from its source: {path}")
    for index, row in enumerate(mappings):
        if not isinstance(row, dict):
            errors.append(f"mapping[{index}] is not a table")
            continue
        missing = sorted(REQUIRED_MAPPING_FIELDS - row.keys())
        if missing:
            errors.append(f"mapping[{index}] missing fields: {', '.join(missing)}")
        key = (row.get("source_enum"), row.get("source_variant"))
        if key in mapping_keys:
            errors.append(f"duplicate migration mapping: {key}")
        mapping_keys.add(key)
        if row.get("classification") != "production":
            errors.append(f"non-production migration row is not allowed: {key}")
        if row.get("disposition") not in {"map", "merge", "retire"}:
            errors.append(f"invalid disposition for {key}: {row.get('disposition')!r}")
        if row.get("source_variant") == "*":
            errors.append(f"wildcard migration classifier: {key}")
        if not isinstance(row.get("surface_exposure"), list) or not row["surface_exposure"]:
            errors.append(f"surface_exposure is empty for {key}")
        for field in ("target", "typed_receipt", "effect_settlement", "replay_policy", "host_lane", "rationale", "test_id", "migration_phase"):
            if not isinstance(row.get(field), str) or not row[field].strip():
                errors.append(f"{field} is empty for {key}")

    missing = sorted(discovered_keys - mapping_keys, key=str)
    stale = sorted(mapping_keys - discovered_keys, key=str)
    if missing:
        errors.append(f"{len(missing)} discovered production variant(s) lack a migration row: {missing[:8]}")
    if stale:
        errors.append(f"{len(stale)} migration row(s) are not discovered production variants: {stale[:8]}")

    expected_smoke_ids = {row["id"] for row in manifest.get("smoke_matrix", []) if isinstance(row, dict)}
    smoke_script = (ROOT / "scripts/tui-mouse-smoke.sh").read_text(encoding="utf-8")
    for smoke_id in expected_smoke_ids:
        if smoke_id not in smoke_script:
            errors.append(f"smoke matrix id is absent from tui-mouse-smoke.sh: {smoke_id}")

    if errors:
        print("r70 migration manifest check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 2

    print(json.dumps({"discovered": len(discovered), "mapped": len(mappings), "status": "pass"}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
