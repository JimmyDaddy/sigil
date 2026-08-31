#!/usr/bin/env python3
"""Regression checks for the offline test-entrypoint boundary.

These checks deliberately use synthetic shell/workflow snippets and a Python
child.  They do not invoke Cargo, Sigil, a provider, or an OS credential
backend.
"""

from __future__ import annotations

import os
from collections.abc import Callable
from contextlib import redirect_stderr
import importlib.util
from io import StringIO
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))


def load_entrypoint_guard() -> object:
    path = SCRIPTS / "check-isolated-test-entrypoints.py"
    spec = importlib.util.spec_from_file_location("sigil_entrypoint_guard", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load entrypoint guard: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


entrypoint_guard = load_entrypoint_guard()


class IsolatedEntrypointTests(unittest.TestCase):
    def expect_guard_failure(self, check: Callable[[], None]) -> None:
        with redirect_stderr(StringIO()), self.assertRaises(SystemExit) as raised:
            check()
        self.assertEqual(raised.exception.code, 1)

    def test_guard_rejects_new_bare_shell_test_and_comment_marker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-entrypoint-guard-") as raw:
            root = Path(raw)
            scripts = root / "scripts"
            scripts.mkdir()
            candidate = scripts / "new-offline-gate.sh"
            original_root = entrypoint_guard.ROOT
            try:
                entrypoint_guard.ROOT = root
                candidate.write_text("cargo test --workspace\n", encoding="utf-8")
                self.expect_guard_failure(entrypoint_guard.check_shell_direct_tests)

                # A comment naming the wrapper is not an invocation and must
                # not make the bare command appear protected.
                candidate.write_text(
                    "# python3 scripts/run-isolated-tests.py --\n"
                    "cargo test --workspace\n",
                    encoding="utf-8",
                )
                self.expect_guard_failure(entrypoint_guard.check_shell_direct_tests)

                # Leaving the isolation helper declaration in place does not
                # protect a reverted ``run_cmd cargo test`` call.
                candidate.write_text(
                    "python3 scripts/run-isolated-tests.py -- true\n"
                    "run_cmd cargo test --workspace\n",
                    encoding="utf-8",
                )
                self.expect_guard_failure(entrypoint_guard.check_shell_direct_tests)

                # A runner on an independent preceding command is likewise
                # not a wrapper for this command.
                candidate.write_text(
                    "python3 scripts/run-isolated-tests.py -- true\n"
                    "cargo test --workspace\n",
                    encoding="utf-8",
                )
                self.expect_guard_failure(entrypoint_guard.check_shell_direct_tests)

                candidate.write_text(
                    "python3 scripts/run-isolated-tests.py -- cargo test --workspace\n",
                    encoding="utf-8",
                )
                entrypoint_guard.check_shell_direct_tests()
            finally:
                entrypoint_guard.ROOT = original_root

    def test_guard_rejects_new_bare_workflow_test(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-workflow-guard-") as raw:
            root = Path(raw)
            workflow = root / ".github" / "workflows"
            workflow.mkdir(parents=True)
            ci = workflow / "new-offline.yml"
            original_root = entrypoint_guard.ROOT
            try:
                entrypoint_guard.ROOT = root
                ci.write_text(
                    "jobs:\n  tests:\n    steps:\n      - run: cargo test --workspace\n",
                    encoding="utf-8",
                )
                self.expect_guard_failure(entrypoint_guard.check_ci_cargo_tests)

                ci.write_text(
                    "jobs:\n  tests:\n    steps:\n"
                    "      - run: python3 scripts/run-isolated-tests.py -- "
                    "cargo test --workspace\n",
                    encoding="utf-8",
                )
                entrypoint_guard.check_ci_cargo_tests()

                ci.write_text(
                    "jobs:\n  tests:\n    steps:\n      - run: cargo llvm-cov --no-report\n",
                    encoding="utf-8",
                )
                self.expect_guard_failure(entrypoint_guard.check_ci_cargo_tests)

                ci.write_text(
                    "jobs:\n  tests:\n    steps:\n"
                    "      - run: python3 scripts/run-isolated-tests.py -- "
                    "cargo llvm-cov --no-report\n",
                    encoding="utf-8",
                )
                entrypoint_guard.check_ci_cargo_tests()
            finally:
                entrypoint_guard.ROOT = original_root

    def test_fault_campaign_resolves_repo_root_from_manifest(self) -> None:
        fault_campaign = REPO_ROOT / "scripts" / "run-r71-fault-campaign.sh"
        source = fault_campaign.read_text(encoding="utf-8")
        self.assertIn("root = Path(sys.argv[1]).parents[2]", source)

        manifest = REPO_ROOT / "dev" / "governance" / "conformance.toml"
        self.assertEqual(manifest.parents[2], REPO_ROOT)

    def test_release_qualification_resolves_evidence_before_temp_rebinding(self) -> None:
        source = (REPO_ROOT / "scripts" / "run-r71-release-qualification.sh").read_text(
            encoding="utf-8"
        )
        evidence = source.index('evidence_dir="${SIGIL_R71_EVIDENCE_DIR:-')
        temp_rebind = source.index('export TMPDIR="$qualification_home/tmp"')
        self.assertLess(evidence, temp_rebind)
        self.assertIn("SIGIL_R71_EVIDENCE_DIR", source)
        self.assertNotIn("${qualification_env_name^^}", source)

    def test_python_entry_reentry_nested_status_and_cleanup(self) -> None:
        child_source = textwrap.dedent(
            """
            from pathlib import Path
            import os
            import sys

            from isolated_test_entry import ensure_isolated_entry

            ensure_isolated_entry()
            root = Path(os.environ["SIGIL_ISOLATED_TESTS_ROOT"])
            assert root.is_dir()
            assert (root / ".sigil-isolated-tests-root-v1").is_file()
            home = Path(os.environ["HOME"])
            home.relative_to(root)
            assert "SIGIL_STATE_HOME" not in os.environ
            print(f"child-root={root}")
            raise SystemExit(int(sys.argv[1]))
            """
        )
        with tempfile.TemporaryDirectory(prefix="sigil-entrypoint-child-") as raw:
            child = Path(raw) / "synthetic_child.py"
            child.write_text(child_source, encoding="utf-8")

            source_env = os.environ.copy()
            # Make this self-test exercise direct entry even when a caller
            # happens to wrap the self-test in an outer runner invocation.
            for name in (
                "SIGIL_ISOLATED_TESTS_ROOT",
                "SIGIL_ISOLATED_TESTS_ACTIVE",
                "SIGIL_STATE_HOME",
            ):
                source_env.pop(name, None)
            source_env["PYTHONPATH"] = os.pathsep.join(
                (str(SCRIPTS), source_env.get("PYTHONPATH", ""))
            ).rstrip(os.pathsep)

            direct = subprocess.run(
                [sys.executable, str(child), "7"],
                cwd=REPO_ROOT,
                env=source_env,
                capture_output=True,
                text=True,
                check=False,
                timeout=15,
            )
            self.assertEqual(direct.returncode, 7, direct.stderr)
            direct_lines = [line for line in direct.stdout.splitlines() if line]
            self.assertEqual(len(direct_lines), 1, direct.stdout)
            direct_root = Path(direct_lines[0].split("=", 1)[1])
            self.assertFalse(direct_root.exists(), direct_root)

            runner = SCRIPTS / "run-isolated-tests.py"
            nested = subprocess.run(
                [
                    sys.executable,
                    str(runner),
                    "--",
                    sys.executable,
                    str(child),
                    "9",
                ],
                cwd=REPO_ROOT,
                env=source_env,
                capture_output=True,
                text=True,
                check=False,
                timeout=15,
            )
            self.assertEqual(nested.returncode, 9, nested.stderr)
            nested_lines = [line for line in nested.stdout.splitlines() if line]
            self.assertEqual(len(nested_lines), 1, nested.stdout)
            nested_root = Path(nested_lines[0].split("=", 1)[1])
            self.assertFalse(nested_root.exists(), nested_root)

    def test_fixture_tempdir_keep_survives_runner_cleanup(self) -> None:
        child_source = textwrap.dedent(
            """
            from pathlib import Path
            import sys

            from isolated_test_entry import create_fixture_tempdir, ensure_isolated_entry

            ensure_isolated_entry()
            fixture = create_fixture_tempdir(
                "sigil-keep-regression-",
                keep=sys.argv[2] == "keep",
                repository_root=Path(sys.argv[1]),
            )
            (fixture / "marker").write_text("synthetic", encoding="utf-8")
            print(fixture)
            """
        )
        with tempfile.TemporaryDirectory(prefix="sigil-entrypoint-fixture-") as raw:
            child = Path(raw) / "fixture_child.py"
            child.write_text(child_source, encoding="utf-8")
            source_env = os.environ.copy()
            for name in (
                "SIGIL_ISOLATED_TESTS_ROOT",
                "SIGIL_ISOLATED_TESTS_ACTIVE",
                "SIGIL_STATE_HOME",
            ):
                source_env.pop(name, None)
            source_env["PYTHONPATH"] = os.pathsep.join(
                (str(SCRIPTS), source_env.get("PYTHONPATH", ""))
            ).rstrip(os.pathsep)

            def run_fixture(mode: str) -> Path:
                result = subprocess.run(
                    [sys.executable, str(child), str(REPO_ROOT), mode],
                    cwd=REPO_ROOT,
                    env=source_env,
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=15,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                lines = [line for line in result.stdout.splitlines() if line]
                self.assertEqual(len(lines), 1, result.stdout)
                return Path(lines[0])

            temporary_fixture = run_fixture("temporary")
            self.assertFalse(temporary_fixture.exists(), temporary_fixture)

            kept_fixture: Path | None = None
            try:
                kept_fixture = run_fixture("keep")
                kept_fixture.relative_to(REPO_ROOT / ".repo-local-dev" / "test-artifacts")
                self.assertTrue(kept_fixture.is_dir())
                self.assertTrue((kept_fixture / "marker").is_file())
            finally:
                if kept_fixture is not None:
                    shutil.rmtree(kept_fixture, ignore_errors=True)
            self.assertIsNotNone(kept_fixture)
            assert kept_fixture is not None
            self.assertFalse(kept_fixture.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
