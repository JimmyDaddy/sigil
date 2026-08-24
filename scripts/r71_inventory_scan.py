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
    (r"\bConnection::open(?:_with_flags)?\(", "DatabaseOrSidecar"),
    (r"worktree\s*\(", "WorktreeOrCheckout"),
    (r"\bgit\s+worktree", "WorktreeOrCheckout"),
]


def is_test_path(path: Path) -> bool:
    parts = path.parts
    return any(part in ("tests", "test") for part in parts)


def _mask_rust_non_code(lines: list[str]) -> list[str]:
    """Mask strings/comments so braces and test attributes are lexical, not textual."""

    masked: list[str] = []
    block_comment = False
    raw_hashes: int | None = None

    for line in lines:
        output: list[str] = []
        index = 0
        while index < len(line):
            if block_comment:
                end = line.find("*/", index)
                if end < 0:
                    index = len(line)
                    break
                output.extend("  ")
                index = end + 2
                block_comment = False
                continue
            if raw_hashes is not None:
                terminator = '"' + ("#" * raw_hashes)
                end = line.find(terminator, index)
                if end < 0:
                    index = len(line)
                    break
                output.extend(" " * (len(terminator) + (end - index)))
                index = end + len(terminator)
                raw_hashes = None
                continue
            if line.startswith("//", index):
                output.extend(" " * (len(line) - index))
                break
            if line.startswith("/*", index):
                output.extend("  ")
                index += 2
                block_comment = True
                continue
            raw_match = re.match(r"(?:b)?r(#+)?\"", line[index:])
            if raw_match:
                hashes = len(raw_match.group(1) or "")
                opening_length = len(raw_match.group(0))
                output.extend(" " * opening_length)
                index += opening_length
                raw_hashes = hashes
                continue
            if line[index] == '"' or (
                line[index] == "'"
                and index + 2 < len(line)
                and (line[index + 2] == "'" or line[index + 1] == "\\")
            ):
                quote = line[index]
                output.append(" ")
                index += 1
                while index < len(line):
                    if line[index] == "\\":
                        output.append("  ")
                        index += 2
                        continue
                    if line[index] == quote:
                        output.append(" ")
                        index += 1
                        break
                    output.append(" ")
                    index += 1
                continue
            output.append(line[index])
            index += 1
        masked.append("".join(output))
    return masked


class _Ctx:
    """Per-file lexical context for cfg(test) module detection."""

    def __init__(self, text: str):
        self.lines = text.splitlines()
        self.code_lines = _mask_rust_non_code(self.lines)
        self.test_lines = self._build_test_mask()

    def _build_test_mask(self) -> list[bool]:
        test_lines = [False] * len(self.code_lines)
        test_scope_depths: list[int] = []
        pending_test_item = False
        brace_depth = 0

        for index, line in enumerate(self.code_lines):
            if any(brace_depth >= scope for scope in test_scope_depths):
                test_lines[index] = True
            if re.search(r"#\[cfg\(test\)\]", line) or re.search(
                r"\bmod\s+tests\b", line
            ):
                pending_test_item = True
                test_lines[index] = True
            if pending_test_item:
                test_lines[index] = True

            opens = line.count("{")
            closes = line.count("}")
            if pending_test_item and opens:
                test_scope_depths.append(brace_depth + 1)
                pending_test_item = False
            elif pending_test_item and ";" in line and not opens:
                pending_test_item = False

            brace_depth += opens - closes
            test_scope_depths = [
                scope for scope in test_scope_depths if brace_depth >= scope
            ]

        return test_lines

    def cfg_test_or_mod_tests(self, index: int) -> bool:
        return self.test_lines[index]


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
