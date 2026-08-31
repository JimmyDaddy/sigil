"""Shared process entry for Python offline acceptance harnesses.

The command-line runner has a hyphenated filename because it is also a shell
entrypoint, so Python harnesses load its small public module API here.  A
directly invoked harness is restarted through the runner; a harness already
inside a marker-owned root continues in place.  The runner therefore owns the
temporary root and its cleanup for the whole harness lifetime.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import os
import sys
import tempfile
from types import ModuleType


_RUNNER_MODULE_NAME = "_sigil_run_isolated_tests"


def _runner_module() -> ModuleType:
    loaded = sys.modules.get(_RUNNER_MODULE_NAME)
    if loaded is not None:
        return loaded
    runner_path = Path(__file__).with_name("run-isolated-tests.py")
    spec = importlib.util.spec_from_file_location(_RUNNER_MODULE_NAME, runner_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"unable to load isolated test runner: {runner_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[_RUNNER_MODULE_NAME] = module
    spec.loader.exec_module(module)
    return module


def ensure_isolated_entry(script: str | Path | None = None) -> None:
    """Run a direct Python harness under the process isolation runner.

    This function returns for a validated nested runner context.  Otherwise it
    runs the same script as the isolated child and exits with the child status;
    the parent process never mutates or cleans the runner-owned environment.
    """
    runner = _runner_module()
    if runner.active_isolation_root(os.environ) is not None:
        return
    target = Path(script or sys.argv[0]).resolve()
    status = runner.run_isolated([sys.executable, str(target), *sys.argv[1:]])
    raise SystemExit(status)


def create_fixture_tempdir(
    prefix: str,
    *,
    keep: bool,
    repository_root: str | Path,
) -> Path:
    """Create a fixture directory, retaining only explicit debug fixtures.

    Normal fixtures use the process temporary directory and are cleaned by the
    runner (or by the caller).  An explicit keep flag places only the fixture
    below the repository's debug-artifact directory, outside the runner-owned
    HOME and without registering that path as an active isolation root.
    """
    if not keep:
        return Path(tempfile.mkdtemp(prefix=prefix))
    artifact_root = Path(repository_root) / ".repo-local-dev" / "test-artifacts"
    artifact_root.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=artifact_root))
