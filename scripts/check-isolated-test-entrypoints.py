#!/usr/bin/env python3
"""Check the small, checked-in inventory of offline test entrypoints.

This is a static guard, not a shell parser and not proof that a child process
cannot access an absolute path.  It catches the two regressions this boundary
is intended to prevent: adding a test runner without the isolated wrapper and
adding a direct CI ``cargo test`` invocation outside a documented qualification
exception.
"""

from __future__ import annotations

from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parent.parent
RUNNER = "run-isolated-tests.py"

# These are the executable, offline test gates.  The values deliberately name
# the local wrapper/helper rather than trying to interpret arbitrary shell.
REQUIRED_SCRIPT_MARKERS: dict[str, tuple[str, ...]] = {
    "check-touched.sh": (
        "run_isolated_test_cmd",
        "run_isolated_test_cmd bash -c 'cd apps/desktop && pnpm check'",
    ),
    "coverage.sh": (RUNNER,),
    "check-orchestration-deterministic.sh": ("run_isolated_test",),
    "run-context-quality.sh": (RUNNER,),
    "run-evals.sh": ("isolated_test_runner",),
    "run-r70-application-gate.sh": (RUNNER,),
    "run-r70-cold-cache-transcript.sh": (RUNNER,),
    "run-r70-framework-qualification.sh": (RUNNER,),
    "run-r70-host-ownership-gate.sh": (RUNNER,),
    "run-r70-legacy-retirement-gate.sh": (RUNNER,),
    "run-r70-preview-package-gate.sh": (RUNNER,),
    "profile-r70-tui-baseline.sh": ("ISOLATED_TEST_RUNNER",),
    "generate-tui-screenshots.sh": (RUNNER,),
    "test-sigil-tui-input-flow.sh": (RUNNER,),
    "run-r71-authority-conformance.sh": (RUNNER,),
    "run-r71-characterization.sh": (RUNNER,),
    "run-r71-consumer-conformance.sh": (RUNNER,),
    "run-r71-fault-campaign.sh": (RUNNER, "parents[2]"),
    "run-r71-global-cutover-conformance.sh": (RUNNER,),
    "run-r71-sandbox-conformance.sh": (RUNNER,),
    "run-r71-surface-conformance.sh": (RUNNER,),
    "run-r71-tui-shipping-e2e.sh": (RUNNER,),
    "check-r71-contract-goldens.sh": (RUNNER,),
}

REQUIRED_PYTHON_ENTRY_MARKERS: dict[str, tuple[str, ...]] = {
    "context-v1-binary-acceptance.py": (
        "ensure_isolated_entry()",
        "create_fixture_tempdir",
        "keep=args.keep_temp",
    ),
    "image-attachment-v1-acceptance.py": (
        "ensure_isolated_entry()",
        "create_fixture_tempdir",
        "keep=args.keep_temp",
    ),
    "long-session-evidence.py": ("ensure_isolated_entry()",),
    "tui-attention-signals-pty-acceptance.py": (
        "ensure_isolated_entry()",
        "create_fixture_tempdir",
        "keep=args.keep_fixture",
    ),
    "tui-feedback-pty-acceptance.py": (
        "ensure_isolated_entry()",
        "create_fixture_tempdir",
        "keep=args.keep_fixture",
    ),
    "tui-web-pty-acceptance.py": (
        "ensure_isolated_entry()",
        "create_fixture_tempdir",
        "keep=args.keep_workspace",
    ),
}

# These acceptance drivers either consume the stateful harness' already
# whitelisted fixture environment or are deliberately live/keyring-qualified.
# Their contract tests must not silently turn into generic offline tests.
PYTHON_ENTRY_EXCEPTIONS: dict[str, tuple[str, ...]] = {
    "alpha-dogfood-campaign.py": ("case_environment", "identity_environment"),
    "real-provider-dogfood-campaign.py": ("child_environment", "allowed_names"),
    "tui-stateful-pty-acceptance.py": ("SAFE_ENV_NAMES", "identity_environment"),
    "tui-cache-pty-acceptance.py": ("SUPPORT.isolated_environment",),
    "tui-orchestration-pty-acceptance.py": ("SUPPORT.isolated_environment",),
    "tui-user-input-pty-acceptance.py": ("SUPPORT.isolated_environment",),
    "tui-mcp-oauth-pty-acceptance.py": ("native credential store",),
    "tui-plan-task-smoke.py": ("real provider configuration",),
    "deepseek-real-cache-conformance.py": ("explicitly authorized",),
}

# These are intentionally not routed through the generic offline wrapper.  A
# qualification owns a temporary HOME itself and tests a real OS backend or
# warm toolchain; changing its environment would change what it qualifies.
QUALIFICATION_EXCEPTIONS: dict[str, tuple[str, ...]] = {
    "run-r71-release-qualification.sh": ("qualification_home", "XDG_CONFIG_HOME"),
    "run-r71-toolchain-conformance.sh": ("fixture_home", "CARGO_NET_OFFLINE=true"),
}

KEYRING_CI_EXCEPTIONS = (
    "native_system_keyring_round_trip_is_exact_and_cleanup_safe",
    "native_provider_credential_store_roundtrip_and_cleanup",
)


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def check_script_inventory() -> None:
    for name, markers in REQUIRED_SCRIPT_MARKERS.items():
        path = ROOT / "scripts" / name
        if not path.is_file():
            fail(f"offline entrypoint is missing: scripts/{name}")
        text = path.read_text(encoding="utf-8")
        missing = [marker for marker in markers if marker not in text]
        if missing:
            fail(f"scripts/{name} has no isolated wrapper marker(s): {missing}")

    for name, markers in QUALIFICATION_EXCEPTIONS.items():
        path = ROOT / "scripts" / name
        if not path.is_file():
            fail(f"registered qualification exception is missing: scripts/{name}")
        text = path.read_text(encoding="utf-8")
        missing = [marker for marker in markers if marker not in text]
        if missing:
            fail(f"qualification exception scripts/{name} lost its isolation evidence: {missing}")

    for name, markers in REQUIRED_PYTHON_ENTRY_MARKERS.items():
        path = ROOT / "scripts" / name
        if not path.is_file():
            fail(f"offline Python entrypoint is missing: scripts/{name}")
        text = path.read_text(encoding="utf-8")
        missing = [marker for marker in markers if marker not in text]
        if missing:
            fail(f"scripts/{name} has no isolated entry marker(s): {missing}")

    for name, markers in PYTHON_ENTRY_EXCEPTIONS.items():
        path = ROOT / "scripts" / name
        if not path.is_file():
            fail(f"registered Python qualification exception is missing: scripts/{name}")
        text = path.read_text(encoding="utf-8").lower()
        missing = [marker.lower() for marker in markers if marker.lower() not in text]
        if missing:
            fail(f"Python qualification exception scripts/{name} lost its evidence: {missing}")


def check_ci_cargo_tests() -> None:
    workflow_dir = ROOT / ".github" / "workflows"
    paths = sorted((*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")))
    for path in paths:
        relative = path.relative_to(ROOT)
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if not line.lstrip().startswith(("#", "- name:")):
                code = line.split("#", 1)[0]
                if "cargo test" in code:
                    if not any(exception in code for exception in KEYRING_CI_EXCEPTIONS):
                        if RUNNER not in code:
                            fail(f"{relative}:{line_number} has bare cargo test")
                if "cargo llvm-cov" in code and not any(
                    token in code for token in ("--version", " clean", "cargo install")
                ) and RUNNER not in code:
                    fail(f"{relative}:{line_number} has bare cargo llvm-cov")


def _logical_shell_lines(lines: list[str]) -> list[tuple[int, str]]:
    """Join only explicit Bash backslash continuations for bounded checks."""
    logical: list[tuple[int, str]] = []
    start = 0
    parts: list[str] = []
    for line_number, line in enumerate(lines, 1):
        if not parts:
            start = line_number
        parts.append(line)
        trailing = line.rstrip()
        backslashes = len(trailing) - len(trailing.rstrip("\\"))
        if not trailing or backslashes % 2 == 0:
            logical.append((start, "\n".join(parts)))
            parts = []
    if parts:
        logical.append((start, "\n".join(parts)))
    return logical


def check_shell_direct_tests() -> None:
    """Catch newly bare shell test commands without pretending to parse Bash."""
    exception_names = set(QUALIFICATION_EXCEPTIONS)
    for path in sorted((ROOT / "scripts").glob("*.sh")):
        if path.name in exception_names:
            continue
        lines = path.read_text(encoding="utf-8").splitlines()
        for line_number, line in _logical_shell_lines(lines):
            # Ignore shell comments when looking for the wrapper.  A comment
            # mentioning the runner must not make a newly bare command pass.
            code = "\n".join(part.split("#", 1)[0] for part in line.splitlines())
            stripped = code.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if "cargo test" in code:
                wrapped = (
                    RUNNER in code
                    or "ISOLATED_TEST_RUNNER" in code
                    or "isolated_test_runner" in code
                    or any(
                        stripped.startswith(prefix)
                        for prefix in (
                            "run_isolated_test ",
                            "run_isolated_test_cmd ",
                            "run_cargo_fixture ",
                            "run_suite ",
                            "run_golden ",
                            "run ",
                        )
                    )
                )
                direct_dispatch = stripped.startswith(("cargo test", "env ", "run_cmd "))
                if direct_dispatch and not wrapped:
                    fail(f"scripts/{path.name}:{line_number} has bare cargo test")
            if "cargo llvm-cov" in code and stripped.startswith(("cargo llvm-cov", "run_cmd ")):
                fail(f"scripts/{path.name}:{line_number} has bare cargo llvm-cov")


def check_pty_workflow_invocations() -> None:
    path = ROOT / ".github" / "workflows" / "ci.yml"
    previous = ""
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if "python3 scripts/" not in line or "pty-acceptance.py" not in line:
            previous = line
            continue
        if RUNNER not in line and RUNNER not in previous:
            fail(f".github/workflows/ci.yml:{line_number} has unwrapped PTY acceptance")
        previous = line


def check_frontend_workflow_tests() -> None:
    for relative in (".github/workflows/ci.yml", ".github/workflows/desktop-package.yml"):
        path = ROOT / relative
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if "pnpm" not in line or not any(token in line for token in (" check", " test")):
                continue
            if RUNNER not in line:
                fail(f"{relative}:{line_number} has unwrapped frontend test command")


def main() -> int:
    check_script_inventory()
    check_ci_cargo_tests()
    check_shell_direct_tests()
    check_pty_workflow_invocations()
    check_frontend_workflow_tests()
    print(
        f"isolated entrypoint inventory passed: {len(REQUIRED_SCRIPT_MARKERS)} shell scripts, "
        f"{len(REQUIRED_PYTHON_ENTRY_MARKERS)} Python scripts, "
        f"{len(QUALIFICATION_EXCEPTIONS) + len(PYTHON_ENTRY_EXCEPTIONS)} qualification exceptions",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
