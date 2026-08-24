#!/usr/bin/env python3
"""RFC-0071 R71.7 negative dependency and legacy-source gate.

This is intentionally a structural source scan rather than a diagnostic grep. Cargo metadata
provides the workspace dependency graph; Rust source is token-masked and evaluated only outside
cfg(test)/test modules. Any parser/import failure or unclassified inventory result is a gate
failure, including an empty source set.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import r71_inventory_scan


ROOT = Path(__file__).resolve().parent.parent

FORBIDDEN_DEPENDENCIES = {
    "sigil-kernel": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-resource-authority": {"sigil-runtime", "sigil-sandbox"},
    "sigil-sandbox": {"sigil-runtime"},
    "sigil-process": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-mcp": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-desktop": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
}

PHYSICAL_IMPORT_ALLOWLIST = {
    "sigil-runtime": {
        "crates/sigil-runtime/src/managed_resource_adapters.rs",
        "crates/sigil-runtime/src/managed_storage_writer.rs",
        "crates/sigil-runtime/src/r71_authority_composition.rs",
        "crates/sigil-runtime/src/r71_global_cutover.rs",
    },
    "sigil-http": {
        "crates/sigil-http/src/listener.rs",
        "crates/sigil-http/src/production_driver.rs",
        "crates/sigil-http/src/support.rs",
    },
    "sigil-release-tools": {"crates/sigil-release-tools/src/lib.rs"},
    "sigil-sandbox": {
        "crates/sigil-sandbox/src/lib.rs",
        "crates/sigil-sandbox/src/managed.rs",
        "crates/sigil-sandbox/src/provider.rs",
    },
    "sigil-resource-authority": set(),
}

LEGACY_PATTERNS = (
    (re.compile(r"\bMcpProcessLaunch::owned\s*\("), "production MCP raw child launch"),
    (re.compile(r"\blaunch_planned_mcp_process\s*\("), "production planned MCP child launch"),
    (re.compile(r"\bFileProjectionStore\b"), "production path-rooted FileProjectionStore"),
    (re.compile(r'PathBuf::from\("\.sigil-(?:state|cache)"\)'), "cwd-relative legacy writable root"),
    (re.compile(r'Path::new\("\.sigil-(?:state|cache)"\)'), "cwd-relative legacy writable root"),
)


def package_for_path(relative: str) -> str:
    if relative.startswith("crates/"):
        return relative.split("/", 2)[1]
    if relative.startswith("apps/desktop/src-tauri/"):
        return "sigil-desktop-app"
    return relative.split("/", 1)[0]


def production_lines(root: Path):
    count = 0
    for path in r71_inventory_scan.rust_sources(root):
        relative = path.relative_to(root).as_posix()
        context = r71_inventory_scan._Ctx(path.read_text(encoding="utf-8"))
        for index, (line, code) in enumerate(zip(context.lines, context.code_lines)):
            if context.cfg_test_or_mod_tests(index):
                continue
            count += 1
            yield relative, package_for_path(relative), index + 1, line, code
    if count == 0:
        raise RuntimeError("Rust production source scan returned zero lines")


def run_metadata(root: Path) -> tuple[dict, list[str]]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed: {result.stderr[-2000:]}")
    metadata = json.loads(result.stdout)
    packages = {package["name"]: package for package in metadata["packages"]}
    findings: list[str] = []
    for package in sorted(packages.values(), key=lambda item: item["name"]):
        name = package["name"]
        workspace_dependencies = sorted(
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["name"] in packages
        )
        for dependency in workspace_dependencies:
            if dependency in FORBIDDEN_DEPENDENCIES.get(name, set()):
                findings.append(f"dependency {name} -> {dependency} is forbidden")
    return metadata, findings


def check_source(root: Path) -> list[str]:
    findings: list[str] = []
    for relative, package, line_number, original, code in production_lines(root):
        for pattern, description in LEGACY_PATTERNS:
            if pattern.search(code) or (
                description == "cwd-relative legacy writable root" and pattern.search(original)
            ):
                findings.append(f"{relative}:{line_number}: {description}: {original.strip()}")

        if "sigil_resource_authority::" in code or "sigil_sandbox::" in code:
            if relative not in PHYSICAL_IMPORT_ALLOWLIST.get(package, set()):
                findings.append(
                    f"{relative}:{line_number}: concrete authority/sandbox import outside allowlist: "
                    f"{original.strip()}"
                )

        if relative in {
            "crates/sigil-kernel/src/resource.rs",
            "crates/sigil-kernel/src/managed_execution.rs",
        } and re.search(r"\b(?:PathBuf|std::path::|\bPath\b)", code):
            findings.append(
                f"{relative}:{line_number}: kernel public execution/resource contract contains a path type: "
                f"{original.strip()}"
            )

        if (
            relative != "crates/sigil-kernel/src/cutover_manifest.rs"
            and re.search(r"\bstruct\s+CutoverSurfaceStatusV1\b", code)
        ):
            findings.append(
                f"{relative}:{line_number}: runtime/surface duplicate of kernel CutoverSurfaceStatusV1"
            )

        if re.search(r"sigil[-_]application|RFC[-_]?0070", code, re.IGNORECASE):
            findings.append(
                f"{relative}:{line_number}: RFC-0070 package split/cutover marker in R71 production source: "
                f"{original.strip()}"
            )
    return findings


def check_inventory(root: Path) -> list[str]:
    findings: list[str] = []
    for checker in (
        "check-local-process-inventory.sh",
        "check-local-resource-producer-inventory.sh",
    ):
        result = subprocess.run(
            [str(root / "scripts" / checker), "--mode", "enforce"],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().splitlines()
            findings.append(f"{checker} enforce failed")
            findings.extend(f"  {line}" for line in detail[:40])
    return findings


def main() -> int:
    findings: list[str] = []
    try:
        _metadata, dependency_findings = run_metadata(ROOT)
        findings.extend(dependency_findings)
        findings.extend(check_source(ROOT))
        findings.extend(check_inventory(ROOT))
    except Exception as error:  # parser/metadata failures are fail-closed gate findings
        findings.append(f"negative dependency scanner error: {error}")

    findings = sorted(set(findings))
    if findings:
        print("R71 negative dependency gate: FAILED", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 2
    print("R71 negative dependency gate: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
