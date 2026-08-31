#!/usr/bin/env python3
"""Contract tests for the non-shipping isolated test entrypoint."""

from __future__ import annotations

import importlib.util
import json
import ntpath
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest


ROOT = Path(__file__).resolve().parent.parent
RUNNER_PATH = ROOT / "scripts" / "run-isolated-tests.py"
SPEC = importlib.util.spec_from_file_location("run_isolated_tests", RUNNER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("failed to load isolated test runner")
RUNNER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNNER
SPEC.loader.exec_module(RUNNER)


def child_script(body: str) -> str:
    return textwrap.dedent(body)


class IsolatedRunnerTests(unittest.TestCase):
    def run_child(
        self,
        code: str,
        *child_args: str,
        environment: dict[str, str] | None = None,
        cwd: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        if environment:
            env.update(environment)
        return subprocess.run(
            [sys.executable, str(RUNNER_PATH), "--", sys.executable, "-c", code, *child_args],
            cwd=cwd or ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_mapping_does_not_mutate_outer_environment_and_clears_ambient_roots(self) -> None:
        original = dict(os.environ)
        source = {
            "HOME": "/caller/user",
            "SIGIL_STATE_HOME": "/caller/state",
            "SIGIL_CACHE_HOME": "/caller/cache",
            "SIGIL_SCRATCH_DIR": "/caller/scratch",
            "SIGIL_CONFIG": "/caller/sigil.toml",
            "XDG_STATE_HOME": "/caller/xdg-state",
            "XDG_CACHE_HOME": "/caller/xdg-cache",
            "XDG_CONFIG_HOME": "/caller/xdg-config",
            "XDG_DATA_HOME": "/caller/xdg-data",
            "XDG_RUNTIME_DIR": "/caller/runtime",
            "TMPDIR": "/caller/tmp",
            "TMP": "/caller/tmp",
            "TEMP": "/caller/tmp",
            "CARGO_HOME": "/caller/cargo",
            "RUSTUP_HOME": "/caller/rustup",
            "SIGIL_API_KEY": "secret-value",
            "OPENAI_API_KEY": "secret-value",
            "HTTPS_PROXY": "http://proxy.invalid",
            "KEEP_THIS": "yes",
        }
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            mapped = RUNNER.build_isolated_environment(source, Path(temporary))

        self.assertEqual(dict(os.environ), original)
        self.assertEqual(mapped["KEEP_THIS"], "yes")
        self.assertEqual(mapped["CARGO_HOME"], "/caller/cargo")
        self.assertEqual(mapped["RUSTUP_HOME"], "/caller/rustup")
        for name in (
            "SIGIL_STATE_HOME",
            "SIGIL_CACHE_HOME",
            "SIGIL_SCRATCH_DIR",
            "SIGIL_CONFIG",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "SIGIL_API_KEY",
            "OPENAI_API_KEY",
            "HTTPS_PROXY",
        ):
            self.assertNotIn(name, mapped)
        for name in ("TMPDIR", "TMP", "TEMP"):
            self.assertTrue(mapped[name].startswith(mapped["HOME"]))

    def test_missing_toolchain_roots_derive_from_original_home_not_fake_home(self) -> None:
        source = {"HOME": "/caller/original-user", "PATH": os.defpath}
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            mapped = RUNNER.build_isolated_environment(source, Path(temporary))
            self.assertEqual(mapped["CARGO_HOME"], "/caller/original-user/.cargo")
            self.assertEqual(mapped["RUSTUP_HOME"], "/caller/original-user/.rustup")
            self.assertNotIn(str(Path(temporary).resolve()), mapped["CARGO_HOME"])

    def test_default_writes_land_under_fake_home_and_root_is_cleaned(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            report = Path(temporary) / "report.json"
            result = self.run_child(
                child_script(
                    """
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    home = Path(os.environ["HOME"])
                    marker = home / ".sigil" / "default-marker"
                    marker.parent.mkdir(parents=True)
                    marker.write_bytes(b"isolated")
                    Path(sys.argv[1]).write_text(
                        json.dumps({"home": str(home), "cwd": os.getcwd()}),
                        encoding="utf-8",
                    )
                    """
                ),
                str(report),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(report.read_text(encoding="utf-8"))
            home = Path(observed["home"])
            self.assertTrue(home.name == "home")
            self.assertFalse(home.exists(), "runner must clean its exact temporary root")
            self.assertEqual(Path(observed["cwd"]), ROOT)

    def test_explicit_fixture_config_argument_wins_over_ambient_config_selector(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            fixture_config = root / "fixture.toml"
            fixture_config.write_text("fixture = true\n", encoding="utf-8")
            report = root / "report.json"
            result = self.run_child(
                child_script(
                    """
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    config = Path(sys.argv[1])
                    Path(sys.argv[3]).write_text(
                        json.dumps({
                            "config": str(config),
                            "config_exists": config.is_file(),
                            "ambient_config_present": "SIGIL_CONFIG" in os.environ,
                            "args": sys.argv[1:],
                        }),
                        encoding="utf-8",
                    )
                    """
                ),
                str(fixture_config),
                "--fixture-argument",
                str(report),
                environment={"SIGIL_CONFIG": str(root / "caller-secret.toml")},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(report.read_text(encoding="utf-8"))
            self.assertEqual(observed["config"], str(fixture_config))
            self.assertTrue(observed["config_exists"])
            self.assertFalse(observed["ambient_config_present"])
            self.assertEqual(
                observed["args"], [str(fixture_config), "--fixture-argument", str(report)]
            )

    def test_credentials_and_proxies_are_not_inherited_without_printing_values(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            report = Path(temporary) / "report.json"
            result = self.run_child(
                "import json, os, sys; "
                "Path = __import__('pathlib').Path; "
                "Path(sys.argv[1]).write_text(json.dumps({k: k in os.environ for k in "
                "('SIGIL_API_KEY', 'SIGIL_HTTP_TOKEN', 'OPENAI_API_KEY', 'HTTPS_PROXY')}), "
                "encoding='utf-8')",
                str(report),
                environment={
                    "SIGIL_API_KEY": "do-not-print",
                    "SIGIL_HTTP_TOKEN": "do-not-print",
                    "OPENAI_API_KEY": "do-not-print",
                    "HTTPS_PROXY": "http://proxy.invalid",
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                json.loads(report.read_text(encoding="utf-8")),
                {
                    "SIGIL_API_KEY": False,
                    "SIGIL_HTTP_TOKEN": False,
                    "OPENAI_API_KEY": False,
                    "HTTPS_PROXY": False,
                },
            )

    def test_child_and_grandchild_share_only_the_isolated_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            report = Path(temporary) / "report.json"
            grandchild_code = (
                "import json, os, sys; from pathlib import Path; "
                "Path(sys.argv[1]).write_text(json.dumps({'home': os.environ.get('HOME'), "
                "'state': os.environ.get('SIGIL_STATE_HOME'), "
                "'active': os.environ.get('SIGIL_ISOLATED_TESTS_ACTIVE')}), encoding='utf-8')"
            )
            child_code = (
                "import os, subprocess, sys; from pathlib import Path; "
                "subprocess.run([sys.executable, '-c', sys.argv[2], sys.argv[1]], "
                "env=os.environ.copy(), check=True)"
            )
            result = self.run_child(
                child_code,
                str(report),
                grandchild_code,
                environment={"SIGIL_STATE_HOME": str(Path(temporary) / "caller-state")},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(report.read_text(encoding="utf-8"))
            self.assertEqual(observed["state"], None)
            self.assertEqual(observed["active"], "1")
            self.assertFalse(Path(observed["home"]).exists())

    def test_nested_runner_preserves_explicit_fixture_override_inside_owned_root(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            report = root / "report.json"
            nested_code = (
                "import json, os, sys; from pathlib import Path; "
                "Path(sys.argv[1]).write_text(json.dumps({'state': os.environ.get('SIGIL_STATE_HOME'), "
                "'config': os.environ.get('SIGIL_CONFIG')}), encoding='utf-8')"
            )
            child_code = (
                "import os, subprocess, sys; from pathlib import Path; "
                "env=os.environ.copy(); owned=Path(env['HOME']).parent; "
                "env['SIGIL_STATE_HOME']=str(owned / 'fixture-state'); "
                "env['SIGIL_CONFIG']=str(owned / 'fixture-config.toml'); "
                "raise SystemExit(subprocess.run([sys.executable, sys.argv[2], '--', "
                "sys.executable, '-c', sys.argv[3], sys.argv[1]], env=env).returncode)"
            )
            result = self.run_child(
                child_code,
                str(report),
                str(RUNNER_PATH),
                nested_code,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(report.read_text(encoding="utf-8"))
            self.assertTrue(observed["state"].endswith("/fixture-state"))
            self.assertTrue(observed["config"].endswith("/fixture-config.toml"))

    def test_nested_marker_rejects_dotdot_identity_escape(self) -> None:
        context = RUNNER.create_isolation_context()
        try:
            environment = RUNNER.build_isolated_environment(
                {"HOME": "/caller/original-user"}, context.root
            )
            environment["HOME"] = str(context.root / "home" / ".." / ".." / "escape")
            self.assertIsNone(RUNNER.active_isolation_root(environment))
        finally:
            self.assertTrue(RUNNER.cleanup_isolation_context(context))

    def test_original_caller_sentinel_bytes_mode_and_mtime_are_unchanged(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            sentinel = root / "caller-sentinel"
            sentinel.write_bytes(b"caller bytes\x00\xff\n")
            sentinel.chmod(0o640)
            fixed_mtime_ns = 1_600_000_000_123_456_789
            os.utime(sentinel, ns=(fixed_mtime_ns, fixed_mtime_ns))
            before = sentinel.stat()
            result = self.run_child(
                "import sys; sys.exit(0)",
                environment={
                    "SIGIL_CONFIG": str(sentinel),
                    "SIGIL_STATE_HOME": str(root),
                    "SIGIL_CACHE_HOME": str(root),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            after = sentinel.stat()
            self.assertEqual(sentinel.read_bytes(), b"caller bytes\x00\xff\n")
            self.assertEqual(stat.S_IMODE(after.st_mode), stat.S_IMODE(before.st_mode))
            self.assertEqual(after.st_mtime_ns, before.st_mtime_ns)

    def test_two_concurrent_runs_do_not_share_home(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            reports = [root / "one.json", root / "two.json"]
            code = (
                "import json, os, sys, time; from pathlib import Path; "
                "home=Path(os.environ['HOME']); (home / 'marker').write_text('x'); time.sleep(0.25); "
                "Path(sys.argv[1]).write_text(json.dumps({'home': str(home)}), encoding='utf-8')"
            )
            processes = [
                subprocess.Popen(
                    [sys.executable, str(RUNNER_PATH), "--", sys.executable, "-c", code, str(report)],
                    cwd=ROOT,
                    env={**os.environ, "SIGIL_STATE_HOME": str(root / "caller-state")},
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                for report in reports
            ]
            completed = [process.communicate(timeout=10) for process in processes]
            self.assertEqual([process.returncode for process in processes], [0, 0])
            homes = [Path(json.loads(report.read_text(encoding="utf-8"))["home"]) for report in reports]
            self.assertNotEqual(homes[0], homes[1])
            self.assertTrue(all(not home.exists() for home in homes))

    def test_child_exit_code_is_transparent_and_failed_child_is_cleaned(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            report = Path(temporary) / "report.json"
            result = self.run_child(
                "import os, sys; from pathlib import Path; "
                "Path(sys.argv[1]).write_text(os.environ['HOME'], encoding='utf-8'); sys.exit(37)",
                str(report),
            )
            self.assertEqual(result.returncode, 37)
            self.assertFalse(Path(report.read_text(encoding="utf-8")).exists())

    def test_invalid_command_and_missing_separator_fail_and_clean_their_roots(self) -> None:
        temp_parent = Path(tempfile.gettempdir())
        before = {path.name for path in temp_parent.glob("sigil-isolated-tests-*")}
        invalid = subprocess.run(
            [sys.executable, str(RUNNER_PATH), "--", "definitely-not-a-real-command-sigil"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        after = {path.name for path in temp_parent.glob("sigil-isolated-tests-*")}
        self.assertNotEqual(invalid.returncode, 0)
        self.assertIn("failed to start isolated test command", invalid.stderr)
        self.assertEqual(after, before)

        missing_separator = subprocess.run(
            [sys.executable, str(RUNNER_PATH), "--help-not-a-command"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(missing_separator.returncode, 2)
        self.assertIn("expected '--'", missing_separator.stderr)

    @unittest.skipUnless(os.name == "posix", "signal status is platform-specific")
    def test_signal_exit_is_bounded_and_cleanup_is_applied(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            report = Path(temporary) / "report.txt"
            result = self.run_child(
                "import os, signal, sys; from pathlib import Path; "
                "Path(sys.argv[1]).write_text(os.environ['HOME'], encoding='utf-8'); "
                "os.kill(os.getpid(), signal.SIGTERM)",
                str(report),
            )
            self.assertEqual(result.returncode, 128 + signal.SIGTERM)
            self.assertFalse(Path(report.read_text(encoding="utf-8")).exists())

    @unittest.skipUnless(os.name == "posix", "process-group cleanup is POSIX-specific")
    def test_interruption_reaches_grandchild_process_group_and_cleans_home(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            report = root / "child.json"
            grandchild_signal = root / "grandchild-signal"
            grandchild_code = child_script(
                """
                import os
                import signal
                import sys
                import time
                from pathlib import Path

                signal.signal(
                    signal.SIGTERM,
                    lambda _signum, _frame: Path(sys.argv[1]).write_text(
                        "term", encoding="utf-8"
                    ),
                )
                Path(sys.argv[1]).with_suffix(".ready").write_text("ready", encoding="utf-8")
                while True:
                    time.sleep(1)
                """
            )
            child_code = child_script(
                """
                import json
                import os
                import signal
                import subprocess
                import sys
                import time
                from pathlib import Path

                signal.signal(signal.SIGTERM, signal.SIG_IGN)
                grandchild = subprocess.Popen(
                    [sys.executable, "-c", sys.argv[2], sys.argv[3]],
                    env=os.environ.copy(),
                )
                while not Path(sys.argv[3]).with_suffix(".ready").exists():
                    time.sleep(0.01)
                Path(sys.argv[1]).write_text(
                    json.dumps({"home": os.environ["HOME"], "pid": grandchild.pid}),
                    encoding="utf-8",
                )
                while True:
                    time.sleep(1)
                """
            )
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(RUNNER_PATH),
                    "--",
                    sys.executable,
                    "-c",
                    child_code,
                    str(report),
                    grandchild_code,
                    str(grandchild_signal),
                ],
                cwd=ROOT,
                env={**os.environ, "SIGIL_STATE_HOME": str(root / "caller-state")},
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not report.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(report.exists(), "child did not start")
                process.send_signal(signal.SIGTERM)
                _stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 128 + signal.SIGKILL, stderr)
                self.assertEqual(grandchild_signal.read_text(encoding="utf-8"), "term")
                observed = json.loads(report.read_text(encoding="utf-8"))
                self.assertFalse(Path(observed["home"]).exists())
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)

    def test_windows_mapping_is_pure_shape_only(self) -> None:
        source = {
            "home": r"C:\Users\caller",
            "USERPROFILE": r"C:\\Users\\caller",
            "APPDATA": r"C:\\Users\\caller\\AppData\\Roaming",
            "LOCALAPPDATA": r"C:\\Users\\caller\\AppData\\Local",
            "HOMEDRIVE": "C:",
            "HOMEPATH": r"\\Users\\caller",
            "CARGO_HOME": r"C:\\toolchain\\cargo",
        }
        with tempfile.TemporaryDirectory(prefix="sigil-isolated-test-") as temporary:
            root = Path(temporary)
            mapped = RUNNER.build_isolated_environment(source, root, platform="windows")
            fake_home = root / "home"
            self.assertNotIn("home", mapped)
            self.assertEqual(mapped["HOME"], str(fake_home))
            self.assertEqual(Path(mapped["USERPROFILE"]), fake_home)
            self.assertEqual(Path(mapped["APPDATA"]), fake_home / "AppData" / "Roaming")
            self.assertEqual(Path(mapped["LOCALAPPDATA"]), fake_home / "AppData" / "Local")
            self.assertEqual(mapped["CARGO_HOME"], r"C:\\toolchain\\cargo")
            self.assertTrue(mapped["XDG_RUNTIME_DIR"].startswith(str(fake_home)))
            self.assertEqual(mapped["HOMEDRIVE"], ntpath.splitdrive(str(fake_home))[0])
            self.assertTrue(mapped["HOMEPATH"].startswith("\\"))


if __name__ == "__main__":
    unittest.main()
