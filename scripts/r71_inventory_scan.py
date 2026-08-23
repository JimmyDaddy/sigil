#!/usr/bin/env python3
"""RFC-0071 R71.0 local producer scan.

Scans shipping Rust sources for production process-spawn and filesystem-producer constructor
sites. The output is a deterministic JSON document consumed by:

- scripts/check-local-process-inventory.sh
- scripts/check-local-resource-producer-inventory.sh
- scripts/generate-r71-inventory-baseline.py (one-shot baseline generation)

Classification uses the closed constructor enums from RFC-0071 section 9.5. This scanner is
line/regex based and intentionally conservative: a site is reported whenever the constructor
spelling appears in a shipping (non-test) Rust source file. AST-precise classification and
exhaustive wrapper expansion are R71.0 baselines; the check scripts verify forward existence
and manifest coverage, and unknown constructor variants fail closed.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

PROCESS_PATTERNS = [
    (r"Command::new\(", "CommandNew"),
    (r"\bprocess::Command\b", "ProcessCommand"),
    (r"\.spawn\(\s*\)", "SpawnCall"),
    (r"tokio::process::", "TokioProcess"),
]

PRODUCER_PATTERNS = [
    (r"create_dir_all\(", "CreateDirectory"),
    (r"\bcreate_dir\(\s*\)", "CreateDirectory"),
    (r"DirBuilder", "CreateDirectory"),
    (r"\btempdir\(\s*\)", "TemporaryDirectory"),
    (r"tempdir_in\(", "TemporaryDirectory"),
    (r"tempfile\(\s*\)", "TemporaryFile"),
    (r"NamedTempFile", "TemporaryFile"),
    (r"OpenOptions::new\(\)", "CreateOrOpenFile"),
    (r"\bFile::create\(", "CreateOrOpenFile"),
    (r"\bFile::create_new\(", "CreateOrOpenFile"),
    (r"fs::write\(", "DirectWriteOrAtomicReplace"),
    (r"write_atomic", "DirectWriteOrAtomicReplace"),
    (r"\bpersist\(", "DirectWriteOrAtomicReplace"),
    (r"\bfs::rename\(", "DirectWriteOrAtomicReplace"),
    (r"rusqlite::Connection::open", "DatabaseOrSidecar"),
    (r"\.sqlite3\b", "DatabaseOrSidecar"),
    (r"worktree\s*\(", "WorktreeOrCheckout"),
    (r"\bgit\s+worktree", "WorktreeOrCheckout"),
]


def is_test_path(path: Path) -> bool:
    parts = path.parts
    return any(part in ("tests", "test") for part in parts)


class _Ctx:
    """Per-file context for cfg(test) module detection."""

    def __init__(self, text: str):
        self.lines = text.splitlines()

    def cfg_test_or_mod_tests(self, index: int) -> bool:
        """Returns True when line index is inside a cfg(test) module or after a mod tests decl."""
        window = self.lines[max(0, index - 12): index + 1]
        if any(re.search(r"#\[cfg\(test\)\]", line) for line in window):
            return True
        if any(re.search(r"\bmod\s+tests\b", line) for line in window):
            return True
        return False


def rust_sources(root: Path) -> list[Path]:
    sources: list[Path] = []
    bases = [root / "crates", root / "apps" / "desktop" / "src-tauri"]
    for base in bases:
        if not base.exists():
            continue
        for path in base.rglob("*.rs"):
            posix = path.as_posix()
            if is_test_path(path):
                continue
            if "/tests/" in posix or posix.endswith("_tests.rs"):
                continue
            sources.append(path)
    return sorted(sources)


def scan_sites(root: Path) -> dict[str, list[dict]]:
    process_sites: list[dict] = []
    producer_sites: list[dict] = []
    seen: set[tuple[str, str, int, str]] = set()

    for path in rust_sources(root):
        try:
            text = path.read_text(encoding="utf-8")
        except OSError:
            continue
        rel = path.relative_to(root).as_posix()
        ctx = _Ctx(text)
        for index, line in enumerate(ctx.lines):
            if ctx.cfg_test_or_mod_tests(index):
                continue
            for pattern, constructor in PROCESS_PATTERNS:
                if re.search(pattern, line):
                    key = ("process", rel, index + 1, constructor)
                    if key in seen:
                        continue
                    seen.add(key)
                    process_sites.append({
                        "site_id": "",
                        "crate": rel.split("/")[1] if rel.startswith("crates/") else rel.split("/")[0],
                        "module": rel,
                        "line": index + 1,
                        "constructor": constructor,
                        "filename": path.name,
                        "snippet": line.strip()[:160],
                    })
                    break
            for pattern, constructor in PRODUCER_PATTERNS:
                if re.search(pattern, line):
                    key = ("producer", rel, index + 1, constructor)
                    if key in seen:
                        continue
                    seen.add(key)
                    producer_sites.append({
                        "site_id": "",
                        "crate": rel.split("/")[1] if rel.startswith("crates/") else rel.split("/")[0],
                        "module": rel,
                        "line": index + 1,
                        "constructor": constructor,
                        "filename": path.name,
                        "snippet": line.strip()[:160],
                    })
                    break
    return {"process_sites": process_sites, "producer_sites": producer_sites}


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    if len(sys.argv) > 1:
        root = Path(sys.argv[1]).resolve()
    data = scan_sites(root)
    json.dump(data, sys.stdout, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
