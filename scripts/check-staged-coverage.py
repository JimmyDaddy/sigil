#!/usr/bin/env python3
"""Check staged Rust business-code additions against line coverage."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path


MIN_COVERAGE = float(os.environ.get("STAGED_COVERAGE_MIN_LINES", "96"))
BUSINESS_RUST_RE = re.compile(r"^crates/[^/]+/src/.+\.rs$")
COVERAGE_IGNORE_FILENAME_REGEX = os.environ.get(
    "COVERAGE_IGNORE_FILENAME_REGEX",
    r"crates/sigil-kernel/src/agent\.rs|crates/sigil-tui/src/runner/worker_loop\.rs",
)
COVERAGE_IGNORE_RE = (
    re.compile(COVERAGE_IGNORE_FILENAME_REGEX) if COVERAGE_IGNORE_FILENAME_REGEX else None
)


@dataclass(frozen=True)
class StagedCoverageResult:
    """Coverage result for staged business-code additions."""

    checked_files: int = 0
    checked_lines: int = 0
    failures: list[str] = field(default_factory=list)


def run(
    args: list[str],
    *,
    env: dict[str, str] | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        check=True,
        env=env,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def git_output(*args: str) -> str:
    return run(["git", *args]).stdout


def is_business_rust_file(path: str) -> bool:
    if not BUSINESS_RUST_RE.match(path):
        return False
    if COVERAGE_IGNORE_RE is not None and COVERAGE_IGNORE_RE.search(path):
        return False
    parts = path.split("/")
    name = Path(path).name
    return (
        "tests" not in parts
        and name != "tests.rs"
        and not name.endswith("_tests.rs")
        and not name.endswith("_test_support.rs")
    )


def is_non_executable_added_line(line: str) -> bool:
    stripped = line.strip()
    if not stripped:
        return True
    if stripped.startswith(("//", "/*", "*", "*/", "#[")):
        return True
    if stripped in {"{", "}", "};", ");", ")", "],", "[", "]", "},", ","}:
        return True
    if stripped.startswith(("use ", "pub use ", "mod ", "pub mod ", "type ", "pub type ")):
        return True
    if re.match(r"^(?:pub(?:\([^)]+\))?\s+)?[A-Z][A-Za-z0-9_]*(?:\s*\{)?[,]?$", stripped):
        return True
    if re.match(r"^[A-Z][A-Za-z0-9_]*(?:,\s*[A-Z][A-Za-z0-9_]*)+,\s*$", stripped):
        return True
    type_fragment = (
        r"(?:[A-Z][A-Za-z0-9_:<> ,&'\[\]]*|"
        r"[ui](?:8|16|32|64|128|size)|"
        r"f(?:32|64)|bool|char|str|&[A-Za-z0-9_:<> ,&'\[\]]+)"
    )
    if re.match(
        rf"^(?:pub(?:\([^)]+\))?\s+)?[A-Z][A-Za-z0-9_]*\s*\{{\s*"
        rf"[A-Za-z_][A-Za-z0-9_]*:\s*{type_fragment}"
        rf"(?:,\s*[A-Za-z_][A-Za-z0-9_]*:\s*{type_fragment})*"
        rf"\s*,?\s*\}},?$",
        stripped,
    ):
        return True
    if re.match(
        r"^(?:pub(?:\([^)]+\))?\s+)?[A-Za-z_][A-Za-z0-9_]*:\s*[^=]+,?$",
        stripped,
    ):
        return True
    return False


def parse_staged_added_lines(diff_text: str) -> dict[str, dict[int, str]]:
    added: dict[str, dict[int, str]] = {}
    current_file: str | None = None
    next_new_line: int | None = None

    for line in diff_text.splitlines():
        if line.startswith("+++ b/"):
            current_file = line.removeprefix("+++ b/")
            added.setdefault(current_file, {})
            next_new_line = None
            continue
        if line.startswith("+++ /dev/null"):
            current_file = None
            next_new_line = None
            continue
        if line.startswith("@@ "):
            match = re.search(r"\+(\d+)(?:,(\d+))?", line)
            next_new_line = int(match.group(1)) if match else None
            continue
        if current_file is None or next_new_line is None:
            continue
        if line.startswith("+") and not line.startswith("+++"):
            added[current_file][next_new_line] = line[1:]
            next_new_line += 1
            continue
        if line.startswith("-") and not line.startswith("---"):
            continue
        next_new_line += 1

    return {path: lines for path, lines in added.items() if lines}


def parse_lcov(path: Path, repo_root: Path) -> dict[str, dict[int, int]]:
    coverage: dict[str, dict[int, int]] = {}
    current_file: str | None = None

    for raw_line in path.read_text(encoding="utf-8").splitlines():
        if raw_line.startswith("SF:"):
            source = Path(raw_line.removeprefix("SF:"))
            if source.is_absolute():
                try:
                    current_file = source.relative_to(repo_root).as_posix()
                except ValueError:
                    current_file = None
            else:
                current_file = source.as_posix()
            if current_file is not None:
                coverage.setdefault(current_file, {})
            continue
        if current_file is None or not raw_line.startswith("DA:"):
            continue
        payload = raw_line.removeprefix("DA:")
        line_no_text, count_text, *_ = payload.split(",")
        line_no = int(line_no_text)
        count = int(count_text)
        coverage[current_file][line_no] = max(coverage[current_file].get(line_no, 0), count)

    return coverage


def format_lines(lines: list[int]) -> str:
    if len(lines) <= 12:
        return ", ".join(str(line) for line in lines)
    head = ", ".join(str(line) for line in lines[:12])
    return f"{head}, ... (+{len(lines) - 12} more)"


def compute_staged_coverage(
    staged_files: list[str],
    added_lines: dict[str, dict[int, str]],
    coverage: dict[str, dict[int, int]],
    min_coverage: float = MIN_COVERAGE,
) -> StagedCoverageResult:
    failures: list[str] = []
    checked_files = 0
    checked_lines = 0

    for path in staged_files:
        file_counts = coverage.get(path, {})
        if not file_counts:
            if all(
                is_non_executable_added_line(line)
                for line in added_lines.get(path, {}).values()
            ):
                continue
            failures.append(f"{path}: no coverage data for staged business-code additions")
            continue
        instrumented = sorted(
            line_no for line_no in added_lines.get(path, {}) if line_no in file_counts
        )
        if not instrumented:
            continue
        checked_files += 1
        checked_lines += len(instrumented)
        covered = [line_no for line_no in instrumented if file_counts[line_no] > 0]
        percent = 100.0 * len(covered) / len(instrumented)
        if percent + 1e-9 < min_coverage:
            uncovered = [line_no for line_no in instrumented if file_counts[line_no] == 0]
            failures.append(
                f"{path}: {percent:.2f}% ({len(covered)}/{len(instrumented)}) "
                f"covered; uncovered added lines: {format_lines(uncovered)}"
            )

    return StagedCoverageResult(
        checked_files=checked_files,
        checked_lines=checked_lines,
        failures=failures,
    )


def main() -> int:
    repo_root = Path(git_output("rev-parse", "--show-toplevel").strip())
    os.chdir(repo_root)

    staged_files = [
        path
        for path in git_output("diff", "--cached", "--name-only", "--diff-filter=ACMR").splitlines()
        if is_business_rust_file(path)
    ]
    if not staged_files:
        print("staged coverage: no staged Rust business-code changes")
        return 0

    unstaged_files = set(git_output("diff", "--name-only", "--", *staged_files).splitlines())
    conflicting = sorted(unstaged_files.intersection(staged_files))
    if conflicting:
        print("staged coverage: cannot check staged snapshot while these files also have unstaged changes:")
        for path in conflicting:
            print(f"  - {path}")
        print("stage or stash those unstaged edits before committing")
        return 1

    diff_text = git_output("diff", "--cached", "--unified=0", "--diff-filter=ACMR", "--", *staged_files)
    added_lines = parse_staged_added_lines(diff_text)
    if not added_lines:
        print("staged coverage: no added Rust business-code lines")
        return 0

    with tempfile.TemporaryDirectory(prefix="sigil-staged-coverage-") as temp_dir:
        lcov_path = Path(temp_dir) / "coverage.lcov"
        env = os.environ.copy()
        env["COVERAGE_SUMMARY_ONLY"] = "0"
        print("staged coverage: running ./scripts/coverage.sh for line data")
        run(
            ["./scripts/coverage.sh", "--lcov", "--output-path", str(lcov_path)],
            env=env,
            capture=False,
        )
        coverage = parse_lcov(lcov_path, repo_root)

    result = compute_staged_coverage(staged_files, added_lines, coverage)

    if result.failures:
        print(f"staged coverage: added business-code line coverage must be >= {MIN_COVERAGE:.2f}%")
        for failure in result.failures:
            print(f"  - {failure}")
        return 1

    if result.checked_lines == 0:
        print("staged coverage: staged business-code additions had no instrumented lines")
        return 0

    print(
        f"staged coverage: ok, {result.checked_lines} added executable lines across "
        f"{result.checked_files} file(s) meet >= {MIN_COVERAGE:.2f}%"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
