#!/usr/bin/env python3
"""Run a command with isolated per-user test storage.

This is an execution-environment helper for offline tests.  It is not an OS
sandbox: a command which is deliberately given an absolute path can still
access that path.  The helper only prevents ambient user identity, Sigil
storage overrides, credentials, and proxy configuration from being inherited
by the command.

The public command-line contract is::

    python3 scripts/run-isolated-tests.py -- <command...>

The module also exposes the environment mapping helpers so independent test
drivers can use the same boundary without changing the process-global
environment.  On POSIX, interrupted-child cleanup is scoped to the child
process group; independently detached descendants are outside this bounded
helper's ownership and are not presented as a full-tree guarantee.
"""

from __future__ import annotations

import dataclasses
import ntpath
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from typing import Mapping, Sequence


MARKER_NAME = ".sigil-isolated-tests-root-v1"
MARKER_PREFIX = "sigil-isolated-tests-root-v1:"
ACTIVE_ROOT_ENV = "SIGIL_ISOLATED_TESTS_ROOT"
RUNNER_PREFIX = "sigil-isolated-tests-"
CHILD_SIGNAL_GRACE_SECONDS = 3.0

SIGIL_ROOT_ENV_NAMES = {
    "SIGIL_STATE_HOME",
    "SIGIL_CACHE_HOME",
    "SIGIL_SCRATCH_DIR",
    # This is an ambient config selector, not an explicit --config argument.
    "SIGIL_CONFIG",
    "SIGIL_CONFIG_PATH",
}
XDG_HOME_ENV_NAMES = {
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
}
WINDOWS_PROFILE_ENV_NAMES = {
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
}
TEMP_ENV_NAMES = {"TMPDIR", "TMP", "TEMP"}
PROXY_ENV_NAMES = {
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "SOCKS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "socks_proxy",
    "no_proxy",
}
CONFIG_PATH_ENV_NAMES = {
    "AWS_CONFIG_FILE",
    "AWS_SHARED_CREDENTIALS_FILE",
    "DOCKER_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "KUBECONFIG",
    "NETRC",
    "NPM_CONFIG_USERCONFIG",
    "PIP_CONFIG_FILE",
    "SSH_AUTH_SOCK",
}
EXACT_SECRET_ENV_NAMES = {
    "ANTHROPIC_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GOOGLE_API_KEY",
    "OPENAI_API_KEY",
    "SIGIL_ANTHROPIC_API_KEY",
    "SIGIL_API_KEY",
    "SIGIL_GEMINI_API_KEY",
    "SIGIL_HTTP_TOKEN",
    "SIGIL_MCP_TOKEN",
    "SIGIL_OPENAI_COMPATIBLE_API_KEY",
    "SIGIL_OPENAI_RESPONSES_API_KEY",
}
SECRET_NAME_PARTS = (
    "API_KEY",
    "API_TOKEN",
    "ACCESS_TOKEN",
    "AUTH_TOKEN",
    "CLIENT_SECRET",
    "CREDENTIAL",
    "PASSWORD",
    "PRIVATE_KEY",
    "SECRET",
    "TOKEN",
)


@dataclasses.dataclass(frozen=True)
class IsolationContext:
    """A marker-owned temporary root and the paths derived from it."""

    root: Path
    home: Path
    runtime: Path
    temporary: Path
    marker: str


def normalize_platform(platform: str | None = None) -> str:
    """Return a small platform enum used by the pure environment mapper."""
    value = (platform or os.name).lower()
    if value in {"nt", "windows", "win32", "msys", "cygwin"}:
        return "windows"
    if value in {"darwin", "macos", "mac"}:
        return "macos"
    return "posix"


def _get_env(source: Mapping[str, str], name: str) -> str | None:
    """Read an environment value with Windows' case-insensitive semantics."""
    if name in source:
        return source[name]
    wanted = name.upper()
    for key, value in source.items():
        if key.upper() == wanted:
            return value
    return None


def _drop_env_names(environment: dict[str, str], names: set[str]) -> None:
    wanted = {name.upper() for name in names}
    for key in list(environment):
        if key.upper() in wanted:
            del environment[key]


def is_sensitive_environment_name(name: str) -> bool:
    """Return whether an ambient variable is a known secret carrier."""
    upper = name.upper()
    # SIGIL_TEST_* is an explicit local fixture namespace.  In particular, a
    # fixture may use SIGIL_TEST_HTTP_TOKEN as a fake token; it is not a user's
    # provider credential and must survive an outer/nested runner boundary.
    if upper.startswith("SIGIL_TEST_"):
        return False
    if upper in EXACT_SECRET_ENV_NAMES:
        return True
    for part in SECRET_NAME_PARTS:
        if part == "TOKEN":
            if upper == part or upper.endswith("_TOKEN") or "_TOKEN_" in upper:
                return True
        elif part in upper:
            return True
    return False


def _absolute_user_path(raw: str, *, platform: str) -> str:
    """Make a user-derived path absolute without reading any user files."""
    if platform == "windows":
        if ntpath.isabs(raw):
            return ntpath.normpath(raw)
        return ntpath.abspath(raw)
    return os.path.abspath(raw)


def original_user_home(source: Mapping[str, str], platform: str | None = None) -> str | None:
    """Resolve only the original identity needed for default cargo/rustup roots."""
    selected = normalize_platform(platform)
    if selected == "windows":
        profile = _get_env(source, "USERPROFILE")
        if profile:
            return _absolute_user_path(profile, platform=selected)
        drive = _get_env(source, "HOMEDRIVE") or ""
        path = _get_env(source, "HOMEPATH") or ""
        if drive or path:
            return _absolute_user_path(ntpath.join(drive, path), platform=selected)
    home = _get_env(source, "HOME")
    if home:
        return _absolute_user_path(home, platform=selected)
    try:
        # This resolves only the operating-system user identity; it does not
        # inspect the user's Sigil configuration or credentials.
        return _absolute_user_path(str(Path.home()), platform=selected)
    except (OSError, RuntimeError):
        return None


def toolchain_roots(source: Mapping[str, str], platform: str | None = None) -> dict[str, str]:
    """Return preserved or original-identity-derived cargo/rustup roots.

    Existing values are deliberately returned byte-for-byte.  Missing values
    are derived from the original user identity, never from the fake HOME.
    """
    roots: dict[str, str] = {}
    for name, relative in (("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")):
        existing = _get_env(source, name)
        if existing is not None:
            roots[name] = existing
            continue
        home = original_user_home(source, platform)
        if home:
            selected = normalize_platform(platform)
            if selected == "windows":
                roots[name] = ntpath.normpath(ntpath.join(home, relative))
            else:
                roots[name] = os.path.abspath(os.path.join(home, relative))
    return roots


def _windows_home_parts(home: Path) -> tuple[str, str]:
    drive, tail = ntpath.splitdrive(str(home))
    if not tail:
        tail = "\\"
    elif not tail.startswith(("\\", "/")):
        tail = "\\" + tail
    return drive, tail.replace("/", "\\")


def build_isolated_environment(
    source: Mapping[str, str],
    root: Path,
    *,
    platform: str | None = None,
) -> dict[str, str]:
    """Build a child environment without mutating ``os.environ``.

    Sigil storage roots are intentionally removed, rather than replaced.  An
    explicit fixture ``--config``/storage path therefore retains precedence;
    default path resolution falls back to the new HOME.  XDG roots with
    HOME-derived fallbacks are also removed.  Runtime and temporary roots have
    no safe HOME fallback on all platforms, so they are placed below fake HOME.
    """
    selected = normalize_platform(platform)
    root = Path(root).absolute()
    home = root / "home"
    runtime = home / ".runtime"
    temporary = home / "tmp"
    environment = dict(source)

    _drop_env_names(
        environment,
        {"HOME"}
        | SIGIL_ROOT_ENV_NAMES
        | XDG_HOME_ENV_NAMES
        | WINDOWS_PROFILE_ENV_NAMES
        | TEMP_ENV_NAMES
        | PROXY_ENV_NAMES
        | CONFIG_PATH_ENV_NAMES,
    )
    _drop_env_names(environment, {ACTIVE_ROOT_ENV, "SIGIL_ISOLATED_TESTS_ACTIVE"})
    for key in list(environment):
        if is_sensitive_environment_name(key):
            del environment[key]

    # Keep normal locale/toolchain/path variables and make all path fallbacks
    # unambiguously belong to this one fake user identity.
    environment["HOME"] = str(home)
    environment["XDG_RUNTIME_DIR"] = str(runtime)
    for name in TEMP_ENV_NAMES:
        environment[name] = str(temporary)

    if selected == "windows":
        environment["USERPROFILE"] = str(home)
        environment["APPDATA"] = str(home / "AppData" / "Roaming")
        environment["LOCALAPPDATA"] = str(home / "AppData" / "Local")
        drive, tail = _windows_home_parts(home)
        environment["HOMEDRIVE"] = drive
        environment["HOMEPATH"] = tail

    preserved_toolchains = toolchain_roots(source, selected)
    # Windows treats environment names case-insensitively.  Remove a possible
    # lower-case duplicate before restoring canonical spellings.
    _drop_env_names(environment, {"CARGO_HOME", "RUSTUP_HOME"})
    environment.update(preserved_toolchains)
    environment[ACTIVE_ROOT_ENV] = str(root)
    environment["SIGIL_ISOLATED_TESTS_PLATFORM"] = selected
    # This marker is informational and has no authority by itself.  Nested
    # entry only trusts it after checking the marker file and path containment.
    environment["SIGIL_ISOLATED_TESTS_ACTIVE"] = "1"
    return environment


def _create_private_directory(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=False)
    try:
        path.chmod(0o700)
    except OSError:
        # Windows ACLs are inherited from the temporary parent; chmod is only
        # an owner-mode hardening hint there.
        if os.name != "nt":
            raise


def create_isolation_context() -> IsolationContext:
    """Create a new marker-owned temporary user root."""
    root = Path(tempfile.mkdtemp(prefix=RUNNER_PREFIX)).absolute()
    marker = f"{MARKER_PREFIX}{secrets.token_hex(24)}"
    try:
        root.chmod(0o700)
        marker_path = root / MARKER_NAME
        marker_path.write_text(marker + "\n", encoding="ascii")
        marker_path.chmod(0o600)
        home = root / "home"
        runtime = home / ".runtime"
        temporary = home / "tmp"
        _create_private_directory(home)
        _create_private_directory(runtime)
        _create_private_directory(temporary)
        return IsolationContext(root, home, runtime, temporary, marker)
    except BaseException:
        # This path is the exact root just allocated above, and no caller path
        # is ever passed to the cleanup operation.
        shutil.rmtree(root, ignore_errors=True)
        raise


def _read_owned_marker(root: Path) -> str | None:
    try:
        if not root.is_absolute() or root.is_symlink() or not root.is_dir():
            return None
        marker = root / MARKER_NAME
        if marker.is_symlink() or not marker.is_file():
            return None
        content = marker.read_text(encoding="ascii")
    except (OSError, UnicodeError):
        return None
    if not content.startswith(MARKER_PREFIX) or not content.endswith("\n"):
        return None
    token = content[len(MARKER_PREFIX) : -1]
    if len(token) != 48 or any(char not in "0123456789abcdef" for char in token):
        return None
    return content[:-1]


def _path_is_within(path: str, root: Path) -> bool:
    try:
        # Resolve existing symlinks and dot components before containment.  A
        # lexical prefix check would let ``root/home/../caller`` or a symlink
        # replacement escape the marker-owned root.
        candidate = Path(path).resolve(strict=False)
        candidate.relative_to(root.resolve(strict=False))
        return True
    except (OSError, ValueError):
        return False


def active_isolation_root(source: Mapping[str, str]) -> Path | None:
    """Return a validated active root suitable for nested entry, if any."""
    raw_root = _get_env(source, ACTIVE_ROOT_ENV)
    if not raw_root:
        return None
    root = Path(raw_root)
    if _read_owned_marker(root) is None:
        return None

    def allowed(value: str) -> bool:
        return _path_is_within(value, root)

    # HOME and platform identity roots must remain in the runner-owned root.
    # CARGO_HOME/RUSTUP_HOME are intentionally excluded because they are the
    # documented reusable toolchain inputs.
    selected = normalize_platform(_get_env(source, "SIGIL_ISOLATED_TESTS_PLATFORM"))
    identity_names = {
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "TMP",
        "TEMP",
    }
    required_names = {"HOME", "XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"}
    if selected == "windows":
        required_names.update({"USERPROFILE", "APPDATA", "LOCALAPPDATA", "HOMEDRIVE", "HOMEPATH"})
    for name in required_names:
        value = _get_env(source, name)
        if value is None or (name not in {"HOMEDRIVE"} and not value):
            return None
    for name in identity_names:
        value = _get_env(source, name)
        if value and not allowed(value):
            return None

    if selected == "windows":
        drive = _get_env(source, "HOMEDRIVE")
        path = _get_env(source, "HOMEPATH")
        if drive is None or path is None:
            return None
        combined_home = ntpath.normpath(ntpath.join(drive, path))
        if not allowed(combined_home):
            return None

    for name in SIGIL_ROOT_ENV_NAMES | XDG_HOME_ENV_NAMES:
        value = _get_env(source, name)
        if value and not allowed(value):
            return None
    return root


def cleanup_isolation_context(context: IsolationContext) -> bool:
    """Remove only an intact marker-owned root; refuse symlink replacement."""
    root = context.root
    if _read_owned_marker(root) != context.marker:
        return False
    try:
        shutil.rmtree(root)
    except OSError:
        return False
    return not root.exists()


def _signal_numbers() -> list[int]:
    names = ("SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK")
    return [getattr(signal, name) for name in names if hasattr(signal, name)]


def wait_for_child(process: subprocess.Popen[bytes]) -> int:
    """Wait for one child, forwarding signals with a bounded grace period."""
    forwarded_at: float | None = None
    previous_handlers: dict[int, object] = {}

    def signal_group(signum: int) -> None:
        if os.name == "posix":
            try:
                os.killpg(process.pid, signum)
                return
            except (OSError, ProcessLookupError):
                pass
        try:
            process.send_signal(signum)
        except OSError:
            pass

    def process_group_exists() -> bool:
        if os.name != "posix":
            return process.poll() is None
        try:
            os.killpg(process.pid, 0)
        except (OSError, ProcessLookupError):
            return False
        return True

    def forward(signum: int, _frame: object) -> None:
        nonlocal forwarded_at
        signal_group(signum)
        forwarded_at = forwarded_at or time.monotonic()

    try:
        for signum in _signal_numbers():
            try:
                previous_handlers[signum] = signal.getsignal(signum)
                signal.signal(signum, forward)
            except (OSError, RuntimeError, ValueError):
                continue
        while True:
            result = process.poll()
            if result is not None and forwarded_at is None:
                return result
            if forwarded_at is not None:
                if result is not None:
                    # Reap the leader before probing the process group.  A
                    # zombie leader otherwise makes killpg(pid, 0) look alive
                    # for the whole grace interval even when no descendant
                    # remains.
                    result = process.wait()
                if not process_group_exists():
                    return result if result is not None else process.wait()
                if time.monotonic() - forwarded_at >= CHILD_SIGNAL_GRACE_SECONDS:
                    signal_group(signal.SIGKILL if hasattr(signal, "SIGKILL") else signal.SIGTERM)
                    return process.wait()
            time.sleep(0.05)
    finally:
        for signum, handler in previous_handlers.items():
            try:
                signal.signal(signum, handler)
            except (OSError, RuntimeError, ValueError):
                pass


def _child_failure_status(error: OSError) -> int:
    if isinstance(error, FileNotFoundError):
        return 127
    if isinstance(error, PermissionError):
        return 126
    return 1


def _normalized_exit_status(returncode: int) -> int:
    # sys.exit(-N) is encoded as 256-N, while shell-compatible signal status
    # is 128+N.  Preserve ordinary child exit codes exactly.
    return 128 - returncode if returncode < 0 else returncode


def run_isolated(command: Sequence[str], source: Mapping[str, str] | None = None) -> int:
    """Run ``command`` and return its child status, cleaning only our root."""
    if not command:
        print("missing command after '--'", file=sys.stderr)
        return 2
    original = dict(os.environ if source is None else source)
    nested = active_isolation_root(original) is not None
    context: IsolationContext | None = None
    environment: dict[str, str]
    returncode = 1
    try:
        if nested:
            environment = original
        else:
            context = create_isolation_context()
            environment = build_isolated_environment(original, context.root)
        try:
            popen_options: dict[str, object] = {"env": environment}
            if os.name == "posix":
                # Keep signal forwarding scoped to this invocation's process
                # tree.  No system-wide process enumeration or kill is used.
                popen_options["start_new_session"] = True
            process = subprocess.Popen(list(command), **popen_options)
        except OSError as error:
            executable = str(command[0])
            print(
                f"failed to start isolated test command {executable!r}: {error.strerror or error}",
                file=sys.stderr,
            )
            returncode = _child_failure_status(error)
        else:
            returncode = _normalized_exit_status(wait_for_child(process))
    except BaseException:
        # Keep cleanup in the finally block, then re-raise so Python still
        # reports an unexpected launcher failure instead of false success.
        raise
    finally:
        if context is not None and not cleanup_isolation_context(context):
            print(
                f"failed to clean isolated test root {context.root}",
                file=sys.stderr,
            )
            if returncode == 0:
                returncode = 1
    return returncode


def _usage() -> str:
    return "usage: python3 scripts/run-isolated-tests.py -- <command...>"


def main(argv: Sequence[str] | None = None) -> int:
    """Parse the fixed separator contract and run the child command."""
    arguments = list(sys.argv[1:] if argv is None else argv)
    if arguments in (["-h"], ["--help"]):
        print(_usage())
        return 0
    if not arguments or arguments[0] != "--":
        print(_usage(), file=sys.stderr)
        print("expected '--' before the test command", file=sys.stderr)
        return 2
    try:
        return run_isolated(arguments[1:])
    except Exception as error:
        print(f"isolated test runner failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
