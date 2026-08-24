#!/usr/bin/env python3
"""Shared fail-closed validation for RFC-0071 R71.8 qualification runners."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
CONFORMANCE_MANIFEST = ROOT / "dev/governance/r71-conformance-inventory-v1.toml"
PLATFORM_MANIFEST = ROOT / "dev/governance/r71-platform-qualification-v1.toml"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REQUIRED_PLATFORM_JOBS = {
    "macos-seatbelt",
    "linux-bubblewrap",
    "windows-restricted",
    "docker-declared",
    "toolchain-offline",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_canonical_text_file(path: Path) -> str:
    """Hash a text manifest with platform line endings canonicalized to LF."""
    return hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()


def git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, check=False, capture_output=True, text=True
    )
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def validate_sha(value: str, label: str) -> str:
    normalized = value.strip().lower()
    if not SHA_RE.fullmatch(normalized):
        raise ValueError(f"{label} must be a full 40-character commit SHA")
    return normalized


def validate_git_identity(root: Path, candidate_sha: str, base_sha: str) -> dict[str, Any]:
    candidate = validate_sha(candidate_sha, "candidate_sha")
    base = validate_sha(base_sha, "base_sha")
    head = git(root, "rev-parse", "HEAD")
    if head != candidate:
        raise RuntimeError(f"HEAD {head} does not equal candidate_sha {candidate}")
    candidate_object = git(root, "rev-parse", f"{candidate}^{{commit}}")
    if candidate_object != candidate:
        raise RuntimeError("candidate_sha does not resolve to the checked-out commit")
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", base, candidate],
        cwd=root,
        check=False,
    )
    if ancestor.returncode != 0:
        raise RuntimeError(f"base_sha {base} is not an ancestor of candidate_sha {candidate}")
    dirty = git(root, "status", "--porcelain=v1")
    if dirty:
        raise RuntimeError("qualification requires a clean checkout; git status is not empty")
    return {"candidate_sha": candidate, "base_sha": base, "head_sha": head, "dirty": False}


def load_conformance_manifest(path: Path = CONFORMANCE_MANIFEST) -> dict[str, Any]:
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    cases = document.get("cases")
    if document.get("schema_version") != 2 or document.get("manifest_hash") != "r71-conformance-v2-200":
        raise ValueError("R71 conformance manifest schema/hash drifted")
    if not isinstance(cases, list) or len(cases) != 200:
        raise ValueError(f"R71 conformance manifest must contain 200 cases, found {len(cases or [])}")
    ids = [case.get("case_id") for case in cases]
    if any(not isinstance(case, dict) for case in cases) or len(ids) != len(set(ids)):
        raise ValueError("R71 conformance manifest contains duplicate or malformed case ids")
    for case in cases:
        if not case.get("required") or case.get("expected_assertion_count") != 1:
            raise ValueError(f"R71 case is not a required one-assertion case: {case.get('case_id')}")
    return document


def load_platform_manifest(path: Path = PLATFORM_MANIFEST) -> dict[str, Any]:
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    jobs = document.get("jobs")
    if document.get("schema_version") != 1 or not isinstance(jobs, list):
        raise ValueError("R71 platform qualification manifest schema is invalid")
    job_ids = [job.get("job_id") for job in jobs]
    if set(job_ids) != REQUIRED_PLATFORM_JOBS or len(job_ids) != len(set(job_ids)):
        raise ValueError("R71 platform qualification jobs do not match the fixed required set")
    if any(not job.get("required") for job in jobs):
        raise ValueError("every R71 platform qualification job must be required")
    return document


def manifest_digests() -> dict[str, str]:
    load_conformance_manifest()
    load_platform_manifest()
    return {
        "conformance_manifest_sha256": sha256_canonical_text_file(CONFORMANCE_MANIFEST),
        "platform_manifest_sha256": sha256_canonical_text_file(PLATFORM_MANIFEST),
    }


def write_evidence(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)
