#!/usr/bin/env python3
"""Fail-closed R71 local process and resource-producer inventory checker."""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
import tomllib
from pathlib import Path
from types import ModuleType


ROOT = Path(__file__).resolve().parent.parent

INVENTORIES = {
    "process": {
        "manifest": "dev/governance/local-process-inventory-v1.toml",
        "scan_key": "process_sites",
        "label": "local-process-inventory",
        "noun": "process",
    },
    "producer": {
        "manifest": "dev/governance/local-resource-producer-inventory-v1.toml",
        "scan_key": "producer_sites",
        "label": "local-resource-producer-inventory",
        "noun": "resource producer",
    },
}

CONTRACT_FIELDS = (
    "class",
    "owner",
    "root_source",
    "input_taint",
    "child_access",
    "admission_contract",
    "resource_contract",
    "lifecycle_contract",
    "receipt_contract",
)


class InventoryError(RuntimeError):
    """An inventory input or exact-join failure that must fail the gate closed."""


def load_contract_generator(root: Path) -> ModuleType:
    """Load the generator so enforcement uses its declared classifications."""

    generator_path = root / "scripts" / "generate-r71-inventory-baseline.py"
    if not generator_path.is_file():
        raise InventoryError(f"missing inventory contract generator: {generator_path}")
    specification = importlib.util.spec_from_file_location(
        "r71_inventory_baseline_generator", generator_path
    )
    if specification is None or specification.loader is None:
        raise InventoryError(f"could not load inventory contract generator: {generator_path}")
    generator = importlib.util.module_from_spec(specification)
    try:
        specification.loader.exec_module(generator)
    except Exception as error:
        raise InventoryError(
            f"invalid inventory contract generator {generator_path}: {error}"
        ) from error
    for function_name in ("site_contract", "validate_exact_sites"):
        if not callable(getattr(generator, function_name, None)):
            raise InventoryError(
                f"inventory contract generator does not export {function_name}"
            )
    if tuple(getattr(generator, "CONTRACT_FIELDS", ())) != CONTRACT_FIELDS:
        raise InventoryError("inventory contract generator has an invalid contract field schema")
    for rules_name in ("PROCESS_RULES", "PRODUCER_RULES"):
        if not isinstance(getattr(generator, rules_name, None), list):
            raise InventoryError(f"inventory contract generator does not export {rules_name}")
    return generator


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check R71 local process and resource-producer inventory manifests."
    )
    parser.add_argument(
        "--kind",
        choices=("process", "producer", "all"),
        default="all",
        help="inventory to verify (default: all)",
    )
    parser.add_argument(
        "--mode",
        choices=("baseline", "enforce"),
        default="baseline",
        help="baseline exact join or enforce classification (default: baseline)",
    )
    return parser.parse_args(arguments)


def selected_kinds(kind: str) -> tuple[str, ...]:
    return tuple(INVENTORIES) if kind == "all" else (kind,)


def scan_inventory(root: Path) -> dict[str, object]:
    command = [sys.executable, str(root / "scripts" / "r71_inventory_scan.py"), str(root)]
    try:
        result = subprocess.run(command, capture_output=True, text=True, check=False)
    except OSError as error:
        raise InventoryError(f"inventory scanner could not start: {error}") from error
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        if detail:
            raise InventoryError(f"inventory scanner failed: {detail}")
        raise InventoryError(f"inventory scanner failed with exit {result.returncode}")
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise InventoryError(f"inventory scanner emitted invalid JSON: {error}") from error
    if not isinstance(data, dict):
        raise InventoryError("inventory scanner JSON root must be an object")
    return data


def load_manifest_sites(manifest_path: Path, noun: str) -> list[dict[str, object]]:
    if not manifest_path.is_file():
        raise InventoryError(f"missing manifest: {manifest_path}")
    try:
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise InventoryError(f"invalid {noun} manifest {manifest_path}: {error}") from error
    sites = manifest.get("sites")
    if not isinstance(sites, list) or not sites:
        raise InventoryError(f"{noun} inventory must not be empty")
    if not all(isinstance(site, dict) for site in sites):
        raise InventoryError(f"invalid {noun} manifest: sites must be tables")
    return sites


def site_key(site: dict[str, object], source: str) -> tuple[str, int, str]:
    try:
        module = site["module"]
        line = site["line"]
        constructor = site["constructor"]
    except KeyError as error:
        raise InventoryError(f"invalid {source} site: missing {error.args[0]}") from error
    if not isinstance(module, str) or not isinstance(line, int) or not isinstance(constructor, str):
        raise InventoryError(f"invalid {source} site key")
    return (module, line, constructor)


def site_index(
    sites: list[dict[str, object]], source: str
) -> dict[tuple[str, int, str], dict[str, object]]:
    indexed: dict[tuple[str, int, str], dict[str, object]] = {}
    for site in sites:
        key = site_key(site, source)
        if key in indexed:
            raise InventoryError(f"invalid {source}: duplicate site key {key}")
        indexed[key] = site
    return indexed


def required_string(site: dict[str, object], field: str, source: str) -> str:
    value = site.get(field)
    if not isinstance(value, str) or not value:
        raise InventoryError(f"invalid {source}: missing {field}")
    return value


def declared_contract(
    generator: ModuleType, site: dict[str, object], rules: list, noun: str
) -> dict[str, str]:
    try:
        contract = generator.site_contract(site, rules)
    except Exception as error:
        raise InventoryError(f"could not classify {noun} scanner site: {error}") from error
    if not isinstance(contract, dict):
        raise InventoryError(f"invalid declared {noun} contract")
    result: dict[str, str] = {}
    for field in CONTRACT_FIELDS:
        value = contract.get(field)
        if not isinstance(value, str) or not value:
            raise InventoryError(f"invalid declared {noun} contract: missing {field}")
        result[field] = value
    return result


def enforce_contracts(
    kind: str,
    noun: str,
    manifest_by_key: dict[tuple[str, int, str], dict[str, object]],
    scan_by_key: dict[tuple[str, int, str], dict[str, object]],
    generator: ModuleType,
) -> list[str]:
    rules_name = "PROCESS_RULES" if kind == "process" else "PRODUCER_RULES"
    rules = getattr(generator, rules_name)
    failures: list[str] = []
    for key in sorted(scan_by_key.keys() & manifest_by_key.keys(), key=str):
        scanned = scan_by_key[key]
        manifest = manifest_by_key[key]
        expected = declared_contract(generator, scanned, rules, noun)
        expected_crate = required_string(scanned, "crate", f"{noun} scanner")
        actual_crate = required_string(manifest, "crate_name", f"{noun} manifest")
        if actual_crate != expected_crate:
            failures.append(
                f"enforce mode: {noun} site {key} crate_name={actual_crate!r}, "
                f"expected {expected_crate!r}"
            )
        for field in CONTRACT_FIELDS:
            actual = required_string(manifest, field, f"{noun} manifest")
            if actual != expected[field]:
                failures.append(
                    f"enforce mode: {noun} site {key} {field}={actual!r}, "
                    f"expected {expected[field]!r}"
                )
        if expected["class"] == "unclassified":
            failures.append(f"enforce mode: unclassified {noun} scanner site {key}")
    return failures


def checked_inventory(
    root: Path,
    kind: str,
    scan: dict[str, object],
    mode: str,
    contract_generator: ModuleType | None = None,
) -> tuple[str, int]:
    specification = INVENTORIES[kind]
    noun = str(specification["noun"])
    scan_key = str(specification["scan_key"])
    scan_sites = scan.get(scan_key)
    if not isinstance(scan_sites, list) or not scan_sites:
        raise InventoryError(f"{noun} scanner result must not be empty")
    if not all(isinstance(site, dict) for site in scan_sites):
        raise InventoryError(f"invalid {noun} scanner result: sites must be objects")
    if kind == "producer":
        if contract_generator is None:
            raise InventoryError("inventory contract generator is required for producer scans")
        try:
            contract_generator.validate_exact_sites(scan_sites)
        except Exception as error:
            raise InventoryError(f"invalid exact {noun} scanner sites: {error}") from error

    manifest_sites = load_manifest_sites(root / str(specification["manifest"]), noun)
    manifest_by_key = site_index(manifest_sites, f"{noun} manifest")
    scan_by_key = site_index(scan_sites, f"{noun} scanner")
    manifest_keys = set(manifest_by_key)
    scan_keys = set(scan_by_key)
    for site in manifest_sites:
        if not isinstance(site.get("class"), str) or not isinstance(site.get("site_id"), str):
            raise InventoryError(f"invalid {noun} manifest site classification")
    missing = sorted(scan_keys - manifest_keys, key=str)
    stale = sorted(manifest_keys - scan_keys, key=str)
    failures: list[str] = []
    if missing:
        failures.append(f"{noun} scanner found {len(missing)} site(s) not in manifest:")
        failures.extend(f"  {key}" for key in missing[:30])
    if stale:
        failures.append(f"manifest contains {len(stale)} site(s) no longer present:")
        failures.extend(f"  {key}" for key in stale[:30])
    if missing or stale:
        failures.append("regenerate: python3 scripts/generate-r71-inventory-baseline.py")

    if mode == "enforce":
        if contract_generator is None:
            raise InventoryError("inventory contract generator is required for enforce mode")
        failures.extend(
            enforce_contracts(
                kind, noun, manifest_by_key, scan_by_key, contract_generator
            )
        )
        bad = [
            site
            for site in manifest_sites
            if site["class"] in ("unclassified", "migration-blocker")
        ]
        if bad:
            failures.append(
                f"enforce mode: {len(bad)} unclassified/blocker {noun} site(s)"
            )
            failures.extend(
                "  {site_id} {module}:{line} class={site_class}".format(
                    site_id=site["site_id"],
                    module=site["module"],
                    line=site["line"],
                    site_class=site["class"],
                )
                for site in bad[:30]
            )
            failures.append(
                "RFC-0071 R71.4 (mandatory consumer qualification) must resolve them first"
            )

    if failures:
        raise InventoryError("\n".join(failures))
    return str(specification["label"]), len(manifest_sites)


def check_inventories(
    root: Path,
    kind: str,
    mode: str,
    scan_runner=scan_inventory,
    generator_loader=load_contract_generator,
) -> list[tuple[str, int]]:
    try:
        scan = scan_runner(root)
    except InventoryError:
        raise
    except Exception as error:
        raise InventoryError(f"inventory scanner failed: {error}") from error
    if not isinstance(scan, dict):
        raise InventoryError("inventory scanner result must be an object")

    selected = selected_kinds(kind)
    needs_contract_generator = mode == "enforce" or "producer" in selected
    contract_generator = generator_loader(root) if needs_contract_generator else None

    results: list[tuple[str, int]] = []
    failures: list[str] = []
    for selected_kind in selected:
        try:
            results.append(
                checked_inventory(root, selected_kind, scan, mode, contract_generator)
            )
        except InventoryError as error:
            failures.append(str(error))
    if failures:
        raise InventoryError("\n".join(failures))
    return results


def run(
    arguments: list[str],
    root: Path = ROOT,
    scan_runner=scan_inventory,
    generator_loader=load_contract_generator,
) -> int:
    arguments = parse_arguments(arguments)
    try:
        results = check_inventories(
            root, arguments.kind, arguments.mode, scan_runner, generator_loader
        )
    except InventoryError as error:
        print(error, file=sys.stderr)
        return 2
    for label, count in results:
        print(f"{label} {arguments.mode}: {count} sites verified")
    return 0


def main() -> int:
    return run(sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
