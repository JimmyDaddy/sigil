#!/usr/bin/env python3
"""Validate and report the R70.4/T12/E13 real production-surface registration.

The checker is deliberately fail-closed.  It proves that a manifest names real enum/caller
anchors and real observed entry tests, but it does not turn those observations into a
qualification pass.  Every required surface and the shared fixture must be marked ready only
after a separate real runner emits the normalized receipt/frontier/event evidence.
"""

from __future__ import annotations

import argparse
import json
import platform
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

import tomllib


ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "scripts/run-r70-application-gate.toml"
GATE_SCRIPT_PATH = Path("scripts/run-r70-application-gate.sh")
REQUIRED_SURFACE_IDS = ("tui-keyboard", "tui-mouse", "cli", "http", "desktop")
VALID_STATUSES = {"ready", "blocked", "missing", "skip"}
PROOF_KEYS = (
    "real_entry",
    "application_runtime_owner",
    "durable_frontier",
    "domain_receipt",
    "normalized_compare",
    "crash_boundary",
)
NORMALIZATION_KEYS = (
    "receipt_fields",
    "frontier_fields",
    "domain_event_fields",
    "outcome_fields",
    "identity_fields",
)
REQUIRED_FRONTIER_FIELDS = {
    "scope",
    "writer_generation",
    "stream_generation",
    "through_sequence",
}
REQUIRED_RECEIPT_FIELDS = {
    "command_id",
    "command_kind",
    "frontier.scope",
    "frontier.writer_generation",
    "frontier.stream_generation",
    "frontier.through_sequence",
    "settlement",
    "domain_commit.source_event_id",
    "domain_commit.source_sequence",
    "domain_commit.source_digest",
    "outcome",
}
REQUIRED_EVENT_FIELDS = {
    "event_id",
    "base_frontier",
    "next_frontier",
    "payload_digest",
    "payload.event_kind",
}
REQUIRED_OUTCOME_FIELDS = {
    "receipt_variant",
    "settlement",
    "effect_count",
    "terminal_status",
    "error_code",
}


def _relative_path(root: Path, value: Any, label: str, errors: list[str]) -> Path | None:
    if not isinstance(value, str) or not value:
        errors.append(f"{label}: path is required")
        return None
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        errors.append(f"{label}: path must stay inside the repository: {value!r}")
        return None
    candidate = root / path
    try:
        candidate.resolve().relative_to(root.resolve())
    except ValueError:
        errors.append(f"{label}: resolved path must stay inside the repository: {value!r}")
        return None
    return candidate


def _required_string(mapping: dict[str, Any], key: str, label: str, errors: list[str]) -> str:
    value = mapping.get(key)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{label}.{key}: non-empty value is required")
        return ""
    return value


def _check_anchor(root: Path, source_file: Any, anchor: Any, label: str, errors: list[str]) -> int | None:
    path = _relative_path(root, source_file, f"{label}.source_file", errors)
    if path is None:
        return None
    if not path.is_file():
        errors.append(f"{label}: source file is missing: {path.relative_to(root)}")
        return None
    if not isinstance(anchor, str) or not anchor:
        errors.append(f"{label}.anchor: non-empty source anchor is required")
        return None
    text = path.read_text(encoding="utf-8")
    offset = text.find(anchor)
    if offset < 0:
        errors.append(f"{label}: source anchor not found: {anchor!r} in {path.relative_to(root)}")
        return None
    return text.count("\n", 0, offset) + 1


def _check_test_anchor(root: Path, source_file: Any, test_id: Any, label: str, errors: list[str]) -> None:
    path = _relative_path(root, source_file, f"{label}.source_file", errors)
    if path is None:
        return
    if not path.is_file():
        errors.append(f"{label}: test source is missing: {path.relative_to(root)}")
        return
    if not isinstance(test_id, str) or not test_id:
        errors.append(f"{label}.test_id: non-empty test id is required")
        return
    text = path.read_text(encoding="utf-8")
    if test_id not in text:
        errors.append(f"{label}: test id not found: {test_id!r} in {path.relative_to(root)}")


def _check_gate_script(root: Path, errors: list[str]) -> None:
    path = root / GATE_SCRIPT_PATH
    if not path.is_file():
        errors.append(f"gate script is missing: {GATE_SCRIPT_PATH}")
        return
    text = path.read_text(encoding="utf-8")
    for marker in (
        "FakeApplication",
        "five_surface_clients_share_frontier_page_and_domain_receipt",
        "simulated surface",
    ):
        if marker in text:
            errors.append(f"gate script contains a forbidden fake qualification marker: {marker}")


def validate_manifest(root: Path, manifest: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    _check_gate_script(root, errors)
    if manifest.get("schema_version") != "r70.4.t12-e13.real-production-surface-v1":
        errors.append("schema_version: unsupported real production-surface schema")
    if manifest.get("qualification") != "T12/E13":
        errors.append("qualification: manifest must be registered for T12/E13")

    required_ids = manifest.get("required_surface_ids")
    if required_ids != list(REQUIRED_SURFACE_IDS):
        errors.append(f"required_surface_ids: expected {list(REQUIRED_SURFACE_IDS)!r}")

    shared = manifest.get("shared_fixture")
    if not isinstance(shared, dict):
        errors.append("shared_fixture: one shared fixture registration is required")
        shared = {}
    fixture_id = _required_string(shared, "id", "shared_fixture", errors)
    fixture_status = shared.get("status")
    if fixture_status not in VALID_STATUSES:
        errors.append("shared_fixture.status: one of ready, blocked, missing, or skip is required")
    if fixture_status != "ready":
        reason = shared.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            errors.append("shared_fixture: non-ready fixture requires an explicit reason")
        errors.append(f"shared_fixture: status={fixture_status!r}; qualification cannot proceed")
    source_files = shared.get("source_files", [])
    if not isinstance(source_files, list) or not source_files:
        errors.append("shared_fixture.source_files: at least one source registration is required")
    else:
        for index, source_file in enumerate(source_files):
            path = _relative_path(root, source_file, f"shared_fixture.source_files[{index}]", errors)
            if path is not None and not path.is_file():
                errors.append(f"shared_fixture.source_files[{index}]: missing file {path.relative_to(root)}")
    required_artifacts = shared.get("required_artifacts")
    if not isinstance(required_artifacts, list) or not required_artifacts:
        errors.append("shared_fixture.required_artifacts: explicit durable fixture artifacts are required")

    normalization = manifest.get("normalization")
    if not isinstance(normalization, dict):
        errors.append("normalization: normalized receipt/frontier/event/outcome contract is required")
        normalization = {}
    for key in NORMALIZATION_KEYS:
        fields = normalization.get(key)
        if not isinstance(fields, list) or not all(isinstance(field, str) and field for field in fields):
            errors.append(f"normalization.{key}: non-empty field list is required")
    if not REQUIRED_FRONTIER_FIELDS.issubset(set(normalization.get("frontier_fields", []))):
        errors.append("normalization.frontier_fields: durable frontier fields are incomplete")
    if not REQUIRED_RECEIPT_FIELDS.issubset(set(normalization.get("receipt_fields", []))):
        errors.append("normalization.receipt_fields: application receipt fields are incomplete")
    if not REQUIRED_EVENT_FIELDS.issubset(set(normalization.get("domain_event_fields", []))):
        errors.append("normalization.domain_event_fields: domain event fields are incomplete")
    if not REQUIRED_OUTCOME_FIELDS.issubset(set(normalization.get("outcome_fields", []))):
        errors.append("normalization.outcome_fields: normalized outcome fields are incomplete")
    _required_string(normalization, "comparison", "normalization", errors)

    rerun = manifest.get("rerun")
    if not isinstance(rerun, dict):
        errors.append("rerun: crash/re-run boundary metadata is required")
        rerun = {}
    if rerun.get("same_command_id") is not True or rerun.get("same_command_fingerprint") is not True:
        errors.append("rerun: command id and fingerprint must be retained across response-lost retries")
    _required_string(rerun, "replay_must_be", "rerun", errors)
    if not isinstance(rerun.get("boundary_metadata"), list) or not rerun["boundary_metadata"]:
        errors.append("rerun.boundary_metadata: durable phase metadata is required")
    crash_boundaries = manifest.get("crash_boundaries")
    if not isinstance(crash_boundaries, list) or len(crash_boundaries) < 4:
        errors.append("crash_boundaries: all registered journal boundaries are required")
    else:
        for index, boundary in enumerate(crash_boundaries):
            if not isinstance(boundary, dict):
                errors.append(f"crash_boundaries[{index}]: object is required")
                continue
            _required_string(boundary, "id", f"crash_boundaries[{index}]", errors)
            _required_string(boundary, "required_receipt", f"crash_boundaries[{index}]", errors)

    surfaces = manifest.get("surfaces")
    if not isinstance(surfaces, list):
        errors.append("surfaces: five real production surfaces are required")
        surfaces = []
    seen_ids: set[str] = set()
    for index, surface in enumerate(surfaces):
        label = f"surfaces[{index}]"
        if not isinstance(surface, dict):
            errors.append(f"{label}: object is required")
            continue
        surface_id = _required_string(surface, "id", label, errors)
        if surface_id in seen_ids:
            errors.append(f"{label}: duplicate surface id {surface_id!r}")
        seen_ids.add(surface_id)
        if surface_id not in REQUIRED_SURFACE_IDS:
            errors.append(f"{label}: unexpected surface id {surface_id!r}")
        status = surface.get("status")
        if status not in VALID_STATUSES:
            errors.append(f"{label}.status: one of ready, blocked, missing, or skip is required")
        if surface.get("required") is not True:
            errors.append(f"{label}.required: every surface must be required")
        if surface.get("fixture_id") != fixture_id:
            errors.append(f"{label}.fixture_id: must use the one shared fixture id")
        if surface.get("input_evidence") not in {"keyboard", "mouse", "process", "http", "typed-native"}:
            errors.append(f"{label}.input_evidence: real interaction evidence is required")
        for key in ("backend", "caller", "application_runtime_owner", "durable_frontier", "domain_receipt", "qualification_runner", "rerun_boundary", "crash_boundary"):
            _required_string(surface, key, label, errors)

        proof = surface.get("proof")
        if not isinstance(proof, dict):
            errors.append(f"{label}.proof: real entry/owner/frontier/receipt proof is required")
            proof = {}
        for key in PROOF_KEYS:
            if not isinstance(proof.get(key), bool):
                errors.append(f"{label}.proof.{key}: boolean proof state is required")
        if status == "ready" and any(proof.get(key) is not True for key in PROOF_KEYS):
            errors.append(f"{label}: ready surface must prove every entry/owner/frontier/receipt boundary")

        blockers = surface.get("blockers")
        if status != "ready":
            if not isinstance(blockers, list) or not blockers or not all(isinstance(item, str) and item.strip() for item in blockers):
                errors.append(f"{label}.blockers: non-ready surface requires explicit blockers")
            errors.append(f"{surface_id}: status={status!r}; qualification cannot proceed")
        elif blockers:
            errors.append(f"{label}.blockers: ready surface cannot retain blockers")
        if status == "skip" and not surface.get("skip_reason"):
            errors.append(f"{label}: skip requires an explicit skip_reason")

        expected_count = surface.get("qualification_test_count_min")
        if not isinstance(expected_count, int) or expected_count < 1:
            errors.append(f"{label}.qualification_test_count_min: must be a positive non-zero count")

        entries = surface.get("entries")
        if not isinstance(entries, list) or not entries:
            errors.append(f"{label}.entries: actual enum/caller registration is required")
        else:
            for entry_index, entry in enumerate(entries):
                entry_label = f"{label}.entries[{entry_index}]"
                if not isinstance(entry, dict):
                    errors.append(f"{entry_label}: object is required")
                    continue
                _required_string(entry, "role", entry_label, errors)
                symbol = _required_string(entry, "symbol", entry_label, errors)
                _required_string(entry, "caller", entry_label, errors)
                line = _check_anchor(root, entry.get("source_file"), entry.get("anchor"), entry_label, errors)
                if line is not None:
                    source_path = root / str(entry["source_file"])
                    source_text = source_path.read_text(encoding="utf-8")
                    if symbol and symbol not in source_text:
                        errors.append(f"{entry_label}: registered symbol not found: {symbol!r}")
                if entry.get("role") == "enum":
                    variant = _required_string(entry, "variant", entry_label, errors)
                    if variant:
                        source_path = _relative_path(root, entry.get("source_file"), entry_label, errors)
                        if source_path is not None and source_path.is_file() and variant not in source_path.read_text(encoding="utf-8"):
                            errors.append(f"{entry_label}: registered enum variant not found: {variant!r}")

        interaction = surface.get("interaction_evidence")
        if not isinstance(interaction, dict):
            errors.append(f"{label}.interaction_evidence: keyboard/mouse/process evidence is required")
        else:
            marker = _required_string(interaction, "marker", f"{label}.interaction_evidence", errors)
            source_path = _relative_path(root, interaction.get("source_file"), f"{label}.interaction_evidence", errors)
            if source_path is not None:
                if not source_path.is_file():
                    errors.append(f"{label}.interaction_evidence: missing source file {source_path.relative_to(root)}")
                elif marker and marker not in source_path.read_text(encoding="utf-8"):
                    errors.append(f"{label}.interaction_evidence: marker not found: {marker!r}")

        observed_source = surface.get("observed_test_source")
        observed_id = surface.get("observed_test_id")
        if observed_source or observed_id:
            _check_test_anchor(root, observed_source, observed_id, f"{label}.observed_test", errors)
        evidence_log = surface.get("evidence_log")
        if not isinstance(evidence_log, dict):
            errors.append(f"{label}.evidence_log: test id/count/log/compare metadata is required")
        else:
            for key in ("test_id", "count", "log", "compare_scope"):
                _required_string(evidence_log, key, f"{label}.evidence_log", errors)
            count = evidence_log.get("count")
            if (isinstance(count, int) and count <= 0) or (
                isinstance(count, str) and count.strip() in {"0", "0 passed", "zero"}
            ):
                errors.append(f"{label}.evidence_log: zero test count is a hard failure")

        qualification_source = surface.get("qualification_test_source")
        qualification_id = surface.get("qualification_test_id")
        if status == "ready":
            _check_test_anchor(root, qualification_source, qualification_id, f"{label}.qualification_test", errors)
        elif qualification_source or qualification_id:
            _check_test_anchor(root, qualification_source, qualification_id, f"{label}.qualification_test", errors)
        if qualification_source and qualification_id:
            qualification_path = _relative_path(root, qualification_source, f"{label}.qualification_test", errors)
            if qualification_path is not None and qualification_path.is_file():
                text = qualification_path.read_text(encoding="utf-8")
                if "FakeApplication" in text or "five_surface_clients_share_frontier_page_and_domain_receipt" in text:
                    errors.append(f"{label}.qualification_test: fake application unit tests cannot qualify production surfaces")

    if set(seen_ids) != set(REQUIRED_SURFACE_IDS) or len(surfaces) != len(REQUIRED_SURFACE_IDS):
        errors.append(f"surfaces: expected exactly {list(REQUIRED_SURFACE_IDS)!r}")
    return errors


def _git_metadata(root: Path) -> dict[str, Any]:
    def run(args: list[str]) -> str:
        try:
            result = subprocess.run(args, cwd=root, check=True, capture_output=True, text=True, timeout=10)
        except (OSError, subprocess.SubprocessError):
            return "unavailable"
        return result.stdout.strip()

    status = run(["git", "status", "--porcelain=v1"])
    return {
        "candidate_sha": run(["git", "rev-parse", "HEAD"]),
        "dirty": bool(status),
        "dirty_paths": len(status.splitlines()) if status else 0,
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
    }


def build_report(root: Path, manifest: dict[str, Any], errors: list[str]) -> dict[str, Any]:
    metadata = _git_metadata(root)
    surfaces = []
    raw_surfaces = manifest.get("surfaces", [])
    if not isinstance(raw_surfaces, list):
        raw_surfaces = []
    for surface in raw_surfaces:
        if not isinstance(surface, dict):
            continue
        anchors = []
        for entry in surface.get("entries", []):
            if not isinstance(entry, dict):
                continue
            path = _relative_path(root, entry.get("source_file"), "report.entry", [])
            line = None
            if path is not None and path.is_file() and isinstance(entry.get("anchor"), str):
                text = path.read_text(encoding="utf-8")
                offset = text.find(entry["anchor"])
                line = text.count("\n", 0, offset) + 1 if offset >= 0 else None
            anchors.append({"role": entry.get("role"), "symbol": entry.get("symbol"), "file": entry.get("source_file"), "line": line})
        surfaces.append({
            "id": surface.get("id"),
            "status": surface.get("status"),
            "backend": surface.get("backend"),
            "input_evidence": surface.get("input_evidence"),
            "caller": surface.get("caller"),
            "entry_anchors": anchors,
            "observed_test_id": surface.get("observed_test_id"),
            "qualification_test_id": surface.get("qualification_test_id") or "not-registered",
            "test_count": "not-run",
            "log": surface.get("evidence_log", {}).get("log") if isinstance(surface.get("evidence_log"), dict) else "not-registered",
            "compare_scope": surface.get("evidence_log", {}).get("compare_scope") if isinstance(surface.get("evidence_log"), dict) else "not-registered",
            "proof": surface.get("proof"),
            "blockers": surface.get("blockers", []),
        })
    return {
        "schema_version": manifest.get("schema_version"),
        "qualification": manifest.get("qualification"),
        "operation": manifest.get("operation"),
        **metadata,
        "shared_fixture": manifest.get("shared_fixture"),
        "normalization": manifest.get("normalization"),
        "rerun": manifest.get("rerun"),
        "crash_boundaries": manifest.get("crash_boundaries"),
        "surfaces": surfaces,
        "validation": {"ok": not errors, "errors": errors},
    }


class ManifestCheckerTests(unittest.TestCase):
    def _manifest(self, root: Path, *, status: str = "ready", fake: bool = False) -> dict[str, Any]:
        source = root / "entry.rs"
        test = root / "test.rs"
        gate = root / GATE_SCRIPT_PATH
        gate.parent.mkdir(parents=True, exist_ok=True)
        gate.write_text("run_suite application-unit-contract cargo test\n", encoding="utf-8")
        source.write_text("pub enum Entry { Cancel }\npub fn caller() {}\n", encoding="utf-8")
        test.write_text(
            "fn real_qualification() {}\n"
            + ("FakeApplication\n" if fake else ""),
            encoding="utf-8",
        )
        return {
            "schema_version": "r70.4.t12-e13.real-production-surface-v1",
            "qualification": "T12/E13",
            "required_surface_ids": list(REQUIRED_SURFACE_IDS),
            "shared_fixture": {"id": "fixture", "status": "ready", "reason": "ready", "source_files": ["entry.rs"], "required_artifacts": ["journal"]},
            "normalization": {
                "receipt_fields": sorted(REQUIRED_RECEIPT_FIELDS),
                "frontier_fields": sorted(REQUIRED_FRONTIER_FIELDS),
                "domain_event_fields": sorted(REQUIRED_EVENT_FIELDS),
                "outcome_fields": sorted(REQUIRED_OUTCOME_FIELDS),
                "identity_fields": ["command_id"],
                "comparison": "exact",
            },
            "rerun": {"same_command_id": True, "same_command_fingerprint": True, "replay_must_be": "replayed", "boundary_metadata": ["phase"]},
            "crash_boundaries": [{"id": str(i), "required_receipt": "typed"} for i in range(4)],
            "surfaces": [
                {
                    "id": surface_id,
                    "status": status,
                    "required": True,
                    "fixture_id": "fixture",
                    "input_evidence": "keyboard" if surface_id == "tui-keyboard" else "mouse" if surface_id == "tui-mouse" else "process",
                    "backend": "test",
                    "caller": "caller",
                    "application_runtime_owner": "owner",
                    "durable_frontier": "frontier",
                    "domain_receipt": "receipt",
                    "qualification_runner": "runner",
                    "qualification_test_source": "test.rs",
                    "qualification_test_id": "FakeApplication" if fake else "real_qualification",
                    "qualification_test_count_min": 1,
                    "observed_test_source": "test.rs",
                    "observed_test_id": "real_qualification",
                    "interaction_evidence": {"source_file": "test.rs", "marker": "real_qualification"},
                    "evidence_log": {"test_id": "real_qualification", "count": "1", "log": "log", "compare_scope": "scope"},
                    "proof": {key: True for key in PROOF_KEYS},
                    "rerun_boundary": "rerun",
                    "crash_boundary": "crash",
                    "blockers": [] if status == "ready" else ["blocked"],
                    "entries": [{"role": "enum", "source_file": "entry.rs", "symbol": "Entry", "variant": "Cancel", "anchor": "pub enum Entry", "caller": "caller"}],
                }
                for surface_id in REQUIRED_SURFACE_IDS
            ],
        }

    def test_ready_registration_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            errors = validate_manifest(Path(directory), self._manifest(Path(directory)))
        self.assertEqual(errors, [])

    def test_blocked_surface_and_fixture_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root, status="blocked")
            manifest["shared_fixture"]["status"] = "missing"
            errors = validate_manifest(root, manifest)
        self.assertTrue(any("shared_fixture: status='missing'" in error for error in errors))
        self.assertTrue(any("tui-keyboard: status='blocked'" in error for error in errors))

    def test_fake_qualification_test_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root, fake=True)
            errors = validate_manifest(root, manifest)
        self.assertTrue(any("fake application unit tests" in error for error in errors))

    def test_gate_script_cannot_call_fake_qualification(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            (root / GATE_SCRIPT_PATH).write_text("run_suite fake FakeApplication\n", encoding="utf-8")
            errors = validate_manifest(root, manifest)
        self.assertTrue(any("forbidden fake qualification marker" in error for error in errors))

    def test_zero_count_and_incomplete_normalization_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            manifest["surfaces"][0]["qualification_test_count_min"] = 0
            manifest["normalization"]["outcome_fields"] = []
            manifest["surfaces"][1]["evidence_log"]["count"] = 0
            manifest["surfaces"][2]["status"] = "skip"
            errors = validate_manifest(root, manifest)
        self.assertTrue(any("positive non-zero count" in error for error in errors))
        self.assertTrue(any("normalization.outcome_fields" in error for error in errors))
        self.assertTrue(any("zero test count" in error for error in errors))
        self.assertTrue(any("skip requires an explicit skip_reason" in error for error in errors))


def _load_manifest(path: Path) -> dict[str, Any]:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise SystemExit(f"FAIL(r70.4/real-surface-registration): cannot read {path}: {error}") from error


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=MANIFEST_PATH)
    parser.add_argument("--check", action="store_true", help="validate the checked-in registration")
    parser.add_argument("--self-test", action="store_true", help="run checker unit tests")
    parser.add_argument("--json", action="store_true", help="emit only the JSON report")
    parser.add_argument("--report", type=Path, help="write the JSON report to a local path")
    args = parser.parse_args(argv)
    if args.self_test:
        result = unittest.main(module=__name__, argv=[sys.argv[0]], exit=False)
        return 0 if result.result.wasSuccessful() else 1
    manifest = _load_manifest(args.manifest)
    errors = validate_manifest(ROOT, manifest)
    report = build_report(ROOT, manifest, errors)
    serialized = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report:
        args.report.write_text(serialized, encoding="utf-8")
    if args.json:
        print(serialized, end="")
    else:
        print(f"r70.4 real-surface registration: candidate_sha={report['candidate_sha']} dirty={report['dirty']}")
        shared_report = report.get("shared_fixture")
        if not isinstance(shared_report, dict):
            shared_report = {}
        print(f"shared fixture: {shared_report.get('id', 'not-registered')} status={shared_report.get('status', 'missing')}")
        for surface in report["surfaces"]:
            print(f"surface {surface['id']}: status={surface['status']} test_count={surface['test_count']} observed={surface['observed_test_id']}")
        if errors:
            for error in errors:
                print(f"BLOCKER: {error}", file=sys.stderr)
            print("qualification result: FAIL-CLOSED (real shared fixture/receipt evidence is not complete)")
        else:
            print("qualification result: READY (runtime qualification still must emit non-zero evidence)")
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
