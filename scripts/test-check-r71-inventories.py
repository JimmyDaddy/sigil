#!/usr/bin/env python3
"""Unit tests for the shared R71 inventory checker."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent
CHECKER_SPEC = importlib.util.spec_from_file_location(
    "check_r71_inventories", ROOT / "scripts" / "check-r71-inventories.py"
)
assert CHECKER_SPEC and CHECKER_SPEC.loader
checker = importlib.util.module_from_spec(CHECKER_SPEC)
CHECKER_SPEC.loader.exec_module(checker)

GENERATOR_SPEC = importlib.util.spec_from_file_location(
    "generate_r71_inventory_baseline",
    ROOT / "scripts" / "generate-r71-inventory-baseline.py",
)
assert GENERATOR_SPEC and GENERATOR_SPEC.loader
generator = importlib.util.module_from_spec(GENERATOR_SPEC)
GENERATOR_SPEC.loader.exec_module(generator)


def scanned_site(
    crate: str,
    module: str,
    line: int,
    constructor: str,
    snippet: str | None = None,
) -> dict[str, object]:
    return {
        "crate": crate,
        "module": module,
        "line": line,
        "constructor": constructor,
        "filename": module.rsplit("/", 1)[-1],
        "snippet": constructor if snippet is None else snippet,
    }


def manifest_site(kind: str, site_id: str, scanned: dict[str, object]) -> dict[str, object]:
    rules = generator.PROCESS_RULES if kind == "process" else generator.PRODUCER_RULES
    return {
        "site_id": site_id,
        "crate_name": scanned["crate"],
        "module": scanned["module"],
        "line": scanned["line"],
        "constructor": scanned["constructor"],
        **generator.site_contract(scanned, rules),
    }


def scan_data(
    process_sites: list[dict[str, object]], producer_sites: list[dict[str, object]]
) -> dict[str, object]:
    return {"process_sites": process_sites, "producer_sites": producer_sites}


RECOVERY_ROOT_LOCATORS = (
    (
        "sigil-resource-authority",
        "crates/sigil-resource-authority/src/bootstrap.rs",
        1407,
        "TemporaryDirectory",
        ".tempdir_in(&state_parent)",
    ),
    (
        "sigil-resource-authority",
        "crates/sigil-resource-authority/src/bootstrap.rs",
        1413,
        "TemporaryDirectory",
        ".tempdir_in(&cache_parent)",
    ),
    (
        "sigil-resource-authority",
        "crates/sigil-resource-authority/src/bootstrap.rs",
        1418,
        "CreateDirectory",
        "fs::create_dir_all(&scratch_root).map_err(|error| {",
    ),
)


def write_manifest(root: Path, kind: str, sites: list[dict[str, object]]) -> None:
    names = {
        "process": "local-process-inventory-v1.toml",
        "producer": "local-resource-producer-inventory-v1.toml",
    }
    path = root / "dev" / "governance" / names[kind]
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = ["schema_version = 1", f'kind = "local-{kind}"']
    for record in sites:
        lines.extend(["", "[[sites]]"])
        for field in (
            "site_id",
            "crate_name",
            "module",
            "constructor",
        ):
            if field in record:
                lines.append(f'{field} = "{record[field]}"')
        lines.append(f"line = {record['line']}")
        for field in generator.CONTRACT_FIELDS:
            if field in record:
                lines.append(f'{field} = "{record[field]}"')
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


class SharedInventoryCheckerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="r71-inventories-")
        self.root = Path(self.temporary_directory.name)
        self.process_scan = scanned_site(
            "sigil-desktop",
            "crates/sigil-desktop/src/launcher.rs",
            212,
            "CommandNew",
        )
        self.producer_scans = [scanned_site(*locator) for locator in RECOVERY_ROOT_LOCATORS]
        self.process = manifest_site("process", "P-0001", self.process_scan)
        self.producers = [
            manifest_site("producer", f"R-000{index}", scanned)
            for index, scanned in enumerate(self.producer_scans, start=1)
        ]
        self.assertEqual(self.process["class"], "trusted-product")
        self.assertEqual(self.process["input_taint"], "UserConfiguration")
        self.assertEqual(
            [site["resource_contract"] for site in self.producers],
            [
                "AuthorityBootstrapObject(StateAnchor)",
                "AuthorityBootstrapObject(CacheAnchor)",
                "AuthorityBootstrapObject(ExecutionTempAnchor)",
            ],
        )
        write_manifest(self.root, "process", [self.process])
        write_manifest(self.root, "producer", self.producers)

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def run_checker(self, arguments: list[str], runner) -> tuple[int, str, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = checker.run(
                arguments,
                self.root,
                runner,
                generator_loader=lambda _root: generator,
            )
        return result, stdout.getvalue(), stderr.getvalue()

    def test_all_scans_once_and_enforces_two_domain_contracts(self) -> None:
        calls = 0

        def runner(_root: Path) -> dict[str, object]:
            nonlocal calls
            calls += 1
            return scan_data([self.process_scan], self.producer_scans)

        result, stdout, stderr = self.run_checker(
            ["--kind", "all", "--mode", "enforce"], runner
        )

        self.assertEqual(result, 0, stderr)
        self.assertEqual(calls, 1)
        self.assertIn("local-process-inventory enforce: 1 sites verified", stdout)
        self.assertIn("local-resource-producer-inventory enforce: 3 sites verified", stdout)

    def test_exact_recovery_root_contracts_freeze_all_fields(self) -> None:
        self.assertEqual(
            [
                tuple(site[field] for field in generator.EXACT_LOCATOR_FIELDS)
                for site in generator.EXACT_RECOVERY_ROOT_SITES
            ],
            list(RECOVERY_ROOT_LOCATORS),
        )
        expected_resources = (
            ("managed-runtime-state", "AuthorityBootstrapObject(StateAnchor)"),
            ("managed-runtime-cache", "AuthorityBootstrapObject(CacheAnchor)"),
            (
                "managed-execution-temp",
                "AuthorityBootstrapObject(ExecutionTempAnchor)",
            ),
        )
        for scanned, (site_class, resource_contract) in zip(
            self.producer_scans, expected_resources, strict=True
        ):
            with self.subTest(line=scanned["line"]):
                contract = generator.site_contract(scanned, generator.PRODUCER_RULES)
                self.assertEqual(contract["class"], site_class)
                self.assertEqual(contract["owner"], "AuthorityBootstrap")
                self.assertEqual(
                    contract["root_source"], "ConfiguredPlatformBootstrapLocation"
                )
                self.assertEqual(contract["input_taint"], "UserConfiguration")
                self.assertEqual(contract["child_access"], "None")
                self.assertEqual(
                    contract["admission_contract"], "AuthorityBootstrapAdmission"
                )
                self.assertEqual(contract["resource_contract"], resource_contract)
                self.assertEqual(contract["lifecycle_contract"], "AuthorityBootstrapLifecycle")
                self.assertEqual(contract["receipt_contract"], "AuthorityBootstrapReceipt")
        generator.validate_exact_sites(self.producer_scans)

    def test_exact_recovery_root_locator_drift_missing_and_duplicate_fail_closed(self) -> None:
        for field, replacement in (
            ("line", 1408),
            ("constructor", "CreateDirectory"),
            ("snippet", ".tempdir_in(&wrong_parent)"),
        ):
            with self.subTest(field=field):
                drifted = [dict(site) for site in self.producer_scans]
                drifted[0][field] = replacement
                with self.assertRaisesRegex(ValueError, "exact producer locator"):
                    generator.validate_exact_sites(drifted)

        with self.assertRaisesRegex(ValueError, "exact producer locator"):
            generator.validate_exact_sites(self.producer_scans[1:])
        with self.assertRaisesRegex(ValueError, "found 2"):
            generator.validate_exact_sites([*self.producer_scans, self.producer_scans[0]])

        drifted = [dict(site) for site in self.producer_scans]
        drifted[0]["line"] = 1408
        with self.assertRaisesRegex(ValueError, "exact producer locator"):
            generator.emit_manifest(
                self.root / "producer.toml",
                "local-resource-producer",
                drifted,
                generator.PRODUCER_RULES,
            )

    def test_generator_validates_recovery_locators_before_writing_manifests(self) -> None:
        drifted = [dict(site) for site in self.producer_scans]
        drifted[0]["line"] = 1408
        with tempfile.TemporaryDirectory(prefix="r71-generator-") as directory:
            generator_root = Path(directory)
            with (
                patch.object(generator, "ROOT", generator_root),
                patch.object(
                    generator.subprocess,
                    "check_output",
                    return_value=json.dumps(scan_data([self.process_scan], drifted)),
                ),
            ):
                with self.assertRaisesRegex(ValueError, "exact producer locator"):
                    generator.main()
            self.assertFalse(
                (
                    generator_root
                    / "dev"
                    / "governance"
                    / "local-process-inventory-v1.toml"
                ).exists()
            )

    def test_checker_rejects_drift_before_bootstrap_wide_rule_can_classify_it(self) -> None:
        drifted = [dict(site) for site in self.producer_scans]
        drifted[0]["line"] = int(drifted[0]["line"]) + 1
        broad_manifest_site = manifest_site("producer", "R-0001", drifted[0])
        self.assertEqual(
            broad_manifest_site["resource_contract"],
            "AuthorityBootstrapObject(BootstrapMetadata)",
        )
        self.assertEqual(broad_manifest_site["input_taint"], "None")
        write_manifest(
            self.root,
            "producer",
            [broad_manifest_site, *self.producers[1:]],
        )

        result, _stdout, stderr = self.run_checker(
            ["--kind", "producer", "--mode", "enforce"],
            lambda _root: scan_data([self.process_scan], drifted),
        )

        self.assertEqual(result, 2)
        self.assertIn("exact producer locator", stderr)

    def test_missing_and_stale_sites_fail_closed(self) -> None:
        scanned = scanned_site(
            "sigil-process", "crates/sigil-process/src/process.rs", 11, "SpawnCall"
        )
        result, _stdout, stderr = self.run_checker(
            ["--kind", "process"],
            lambda _root: scan_data([scanned], self.producer_scans),
        )

        self.assertEqual(result, 2)
        self.assertIn("not in manifest", stderr)
        self.assertIn("no longer present", stderr)

    def test_empty_or_missing_manifest_fails_closed(self) -> None:
        write_manifest(self.root, "process", [])
        result, _stdout, stderr = self.run_checker(
            ["--kind", "all"],
            lambda _root: scan_data([self.process_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("process inventory must not be empty", stderr)

        (self.root / "dev" / "governance" / "local-resource-producer-inventory-v1.toml").unlink()
        result, _stdout, stderr = self.run_checker(
            ["--kind", "producer"],
            lambda _root: scan_data([self.process_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("missing manifest", stderr)

    def test_empty_scan_fails_closed(self) -> None:
        result, _stdout, stderr = self.run_checker(
            ["--kind", "process"], lambda _root: scan_data([], self.producer_scans)
        )
        self.assertEqual(result, 2)
        self.assertIn("process scanner result must not be empty", stderr)

    def test_scanner_subprocess_failure_and_invalid_json_fail_closed(self) -> None:
        scanner_path = self.root / "scripts" / "r71_inventory_scan.py"
        scanner_path.parent.mkdir(parents=True, exist_ok=True)
        scanner_path.write_text(
            "import sys\nprint('scanner fixture failure', file=sys.stderr)\nraise SystemExit(23)\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(checker.InventoryError, "scanner fixture failure"):
            checker.scan_inventory(self.root)

        scanner_path.write_text("print('{not-json')\n", encoding="utf-8")
        with self.assertRaisesRegex(checker.InventoryError, "invalid JSON"):
            checker.scan_inventory(self.root)

    def test_enforce_rejects_every_contract_axis_and_crate_name(self) -> None:
        for field in ("crate_name", *generator.CONTRACT_FIELDS):
            with self.subTest(field=field):
                tampered = dict(self.process)
                tampered[field] = (
                    "sigil-runtime"
                    if field == "crate_name"
                    else "Model"
                    if field == "input_taint"
                    else "tampered-contract"
                )
                write_manifest(self.root, "process", [tampered])
                result, _stdout, stderr = self.run_checker(
                    ["--kind", "process", "--mode", "enforce"],
                    lambda _root: scan_data([self.process_scan], self.producer_scans),
                )
                self.assertEqual(result, 2)
                self.assertIn(field, stderr)
                if field == "input_taint":
                    self.assertIn("UserConfiguration", stderr)

    def test_enforce_rejects_cross_swapped_declared_metadata(self) -> None:
        observer_scan = scanned_site(
            "sigil-process-observer",
            "crates/sigil-process-observer/src/lib.rs",
            91,
            "CommandNew",
        )
        observer = manifest_site("process", "P-0002", observer_scan)
        cross_swapped = dict(self.process)
        for field in generator.CONTRACT_FIELDS:
            cross_swapped[field] = observer[field]
        write_manifest(self.root, "process", [cross_swapped, observer])

        result, _stdout, stderr = self.run_checker(
            ["--kind", "process", "--mode", "enforce"],
            lambda _root: scan_data([self.process_scan, observer_scan], self.producer_scans),
        )

        self.assertEqual(result, 2)
        self.assertIn("owner", stderr)
        self.assertIn("input_taint", stderr)
        self.assertIn("UserConfiguration", stderr)

    def test_enforce_rejects_missing_contract_field_and_unclassified_site(self) -> None:
        missing_field = dict(self.process)
        del missing_field["receipt_contract"]
        write_manifest(self.root, "process", [missing_field])
        result, _stdout, stderr = self.run_checker(
            ["--kind", "process", "--mode", "enforce"],
            lambda _root: scan_data([self.process_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("missing receipt_contract", stderr)

        unknown_scan = scanned_site(
            "sigil-desktop", "crates/sigil-desktop/src/unknown.rs", 15, "CommandNew"
        )
        unknown_manifest = manifest_site("process", "P-0002", unknown_scan)
        write_manifest(self.root, "process", [unknown_manifest])
        result, _stdout, stderr = self.run_checker(
            ["--kind", "process", "--mode", "enforce"],
            lambda _root: scan_data([unknown_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("unclassified process scanner site", stderr)

    def test_each_domain_blocker_fails_all_enforce(self) -> None:
        blocked_process = {**self.process, "class": "migration-blocker"}
        write_manifest(self.root, "process", [blocked_process])
        result, _stdout, stderr = self.run_checker(
            ["--kind", "all", "--mode", "enforce"],
            lambda _root: scan_data([self.process_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("blocker process", stderr)

        write_manifest(self.root, "process", [self.process])
        blocked_producer = {**self.producers[0], "class": "unclassified"}
        write_manifest(self.root, "producer", [blocked_producer, *self.producers[1:]])
        result, _stdout, stderr = self.run_checker(
            ["--kind", "all", "--mode", "enforce"],
            lambda _root: scan_data([self.process_scan], self.producer_scans),
        )
        self.assertEqual(result, 2)
        self.assertIn("blocker resource producer", stderr)

    def test_domain_wrappers_and_release_wrapper_keep_one_inventory_path(self) -> None:
        process_wrapper = (ROOT / "scripts" / "check-local-process-inventory.sh").read_text(
            encoding="utf-8"
        )
        producer_wrapper = (
            ROOT / "scripts" / "check-local-resource-producer-inventory.sh"
        ).read_text(encoding="utf-8")
        release_wrapper = (ROOT / "scripts" / "run-r71-release-qualification.sh").read_text(
            encoding="utf-8"
        )

        self.assertTrue(process_wrapper.rstrip().endswith('"$@" --kind process'))
        self.assertTrue(producer_wrapper.rstrip().endswith('"$@" --kind producer'))
        self.assertIn("run_step negative-dependencies", release_wrapper)
        self.assertNotIn("run_step inventory-process", release_wrapper)
        self.assertNotIn("run_step inventory-producer", release_wrapper)


if __name__ == "__main__":
    unittest.main()
