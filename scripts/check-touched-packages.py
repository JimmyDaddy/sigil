#!/usr/bin/env python3
"""Resolve touched Cargo package names from the actual workspace metadata."""

from __future__ import annotations

import argparse
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
WORKSPACE_INPUTS = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml"}


def touched_packages(metadata: dict, changed_files: list[str]) -> list[str]:
    """Map paths to the most specific member; workspace inputs select every member."""
    root = Path(metadata["workspace_root"]).resolve()
    members = set(metadata["workspace_members"])
    packages = [package for package in metadata["packages"] if package["id"] in members]
    if not members or {package["id"] for package in packages} != members:
        raise ValueError("workspace package metadata is incomplete")

    roots: dict[PurePosixPath, str] = {}
    for package in packages:
        relative = Path(package["manifest_path"]).resolve().parent.relative_to(root)
        package_root = PurePosixPath(relative.as_posix())
        if package_root in roots:
            raise ValueError("multiple packages share one manifest directory")
        roots[package_root] = package["name"]

    paths = []
    for name in changed_files:
        candidate = PurePosixPath(name)
        if candidate.is_absolute() or ".." in candidate.parts:
            raise ValueError("changed file must be workspace-relative: " + name)
        paths.append(candidate)

    if any(path.as_posix() in WORKSPACE_INPUTS for path in paths):
        return sorted(set(roots.values()))

    selected = set()
    for path in paths:
        owners = [base for base in roots if path.is_relative_to(base)]
        if owners:
            owner = max(owners, key=lambda base: len(base.parts))
            selected.add(roots[owner])
        elif path.parts and path.parts[0] == "crates":
            raise ValueError("changed crate path has no workspace package: " + str(path))
    return sorted(selected)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changed-files", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        )
        metadata = json.loads(result.stdout)
        files = args.changed_files.read_text(encoding="utf-8").splitlines()
        for package in touched_packages(metadata, files):
            print(package)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        detail = error.stderr.strip() if isinstance(error, subprocess.CalledProcessError) else str(error)
        print("cannot resolve touched Cargo packages: " + detail, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
