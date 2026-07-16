#!/usr/bin/env python3
"""Run an explicit, cost-admitted real-provider Plan-only TUI acceptance."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import time
import tomllib
from typing import Callable, Sequence
from urllib.parse import urlsplit


SUPPORT_SCRIPT = Path(__file__).with_name("tui-stateful-pty-acceptance.py")
SUPPORT_SPEC = importlib.util.spec_from_file_location("tui_stateful_support", SUPPORT_SCRIPT)
assert SUPPORT_SPEC is not None and SUPPORT_SPEC.loader is not None
SUPPORT = importlib.util.module_from_spec(SUPPORT_SPEC)
sys.modules[SUPPORT_SPEC.name] = SUPPORT
SUPPORT_SPEC.loader.exec_module(SUPPORT)

SCHEMA_VERSION = 1
DEFAULT_FIXTURE = Path("dev/evals/plan-fixtures/plan-only")
PLAN_RUN_WAIT_CAP_SECS = 120.0
COMMON_PROVIDER_ENV_NAMES = {
    "ALL_PROXY",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "all_proxy",
    "https_proxy",
    "http_proxy",
    "no_proxy",
}
PROVIDER_CREDENTIAL_ENV = {
    "anthropic": "SIGIL_ANTHROPIC_API_KEY",
    "deepseek": "SIGIL_API_KEY",
    "gemini": "SIGIL_GEMINI_API_KEY",
    "openai_compat": "SIGIL_OPENAI_COMPATIBLE_API_KEY",
    "openai_responses": "SIGIL_OPENAI_RESPONSES_API_KEY",
}
PROVIDER_ENV_BY_NAME = {
    "anthropic": {"SIGIL_ANTHROPIC_API_KEY", "SIGIL_ANTHROPIC_BASE_URL"},
    "deepseek": {
        "SIGIL_ANTHROPIC_BASE_URL",
        "SIGIL_API_KEY",
        "SIGIL_BASE_URL",
        "SIGIL_BETA_BASE_URL",
        "SIGIL_FIM_MODEL",
        "SIGIL_STRICT_TOOLS_MODE",
        "SIGIL_USER_ID_STRATEGY",
    },
    "gemini": {"SIGIL_GEMINI_API_KEY", "SIGIL_GEMINI_BASE_URL"},
    "openai_compat": {
        "SIGIL_OPENAI_COMPATIBLE_API_KEY",
        "SIGIL_OPENAI_COMPATIBLE_BASE_URL",
    },
    "openai_responses": {
        "SIGIL_OPENAI_RESPONSES_API_KEY",
        "SIGIL_OPENAI_RESPONSES_BASE_URL",
    },
}
TOML_KEY = re.compile(r"^[A-Za-z0-9_-]+$")


class PlanAcceptanceError(RuntimeError):
    """Raised when Plan acceptance admission or evidence is invalid."""


@dataclasses.dataclass(frozen=True)
class PlanFixture:
    fixture_id: str
    prompt: str
    expected_target_path: str
    allowed_tools: frozenset[str]
    files: tuple[tuple[str, Path, str], ...]
    manifest_digest: str


@dataclasses.dataclass(frozen=True)
class SourceConfig:
    path: Path
    provider: str
    model: str
    provider_config: dict[str, object]


@dataclasses.dataclass(frozen=True)
class PlanAudit:
    session_path: Path
    event_counts: dict[str, int]
    tool_names: tuple[str, ...]
    plan_target_paths: tuple[str, ...]
    plan_step_count: int
    usage_events: int
    prompt_tokens: int
    completion_tokens: int
    observed_cost_usd: float | None
    run_finalized_count: int
    failed_run_count: int
    task_created_count: int


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def parse_cost_microusd(raw: str) -> int:
    try:
        value = float(raw)
    except ValueError as error:
        raise PlanAcceptanceError("max cost must be a positive decimal") from error
    if not 0.0 < value <= 1.0 or not value < float("inf"):
        raise PlanAcceptanceError("max cost must be greater than zero and at most $1.00")
    microusd = int(value * 1_000_000 + 0.999999)
    if microusd <= 0:
        raise PlanAcceptanceError("max cost must reserve at least one microUSD")
    return microusd


def sha256_file(path: Path) -> str:
    return SUPPORT.sha256_file(path)


def reject_symlink_components(root: Path, relative: Path, description: str) -> None:
    current = root
    for component in relative.parts:
        current = current / component
        if current.is_symlink():
            raise PlanAcceptanceError(f"{description} must not traverse a symlink")


def load_fixture(path: Path) -> PlanFixture:
    root = path.expanduser().resolve(strict=True)
    manifest_path = root / "fixture.toml"
    manifest_bytes = manifest_path.read_bytes()
    payload = tomllib.loads(manifest_bytes.decode("utf-8"))
    if payload.get("schema_version") != SCHEMA_VERSION or payload.get("id") != "plan-only":
        raise PlanAcceptanceError("unsupported Plan fixture identity or schema")
    prompt_file = payload.get("prompt_file")
    prompt_sha = payload.get("prompt_sha256")
    if not isinstance(prompt_file, str) or not isinstance(prompt_sha, str):
        raise PlanAcceptanceError("Plan fixture is missing its prompt checksum")
    if Path(prompt_file).is_absolute() or ".." in Path(prompt_file).parts:
        raise PlanAcceptanceError("Plan fixture prompt path must stay inside the fixture")
    prompt_relative = Path(prompt_file)
    reject_symlink_components(root, prompt_relative, "Plan fixture prompt")
    prompt_path = (root / prompt_relative).resolve(strict=True)
    if root not in prompt_path.parents or not prompt_path.is_file():
        raise PlanAcceptanceError("Plan fixture prompt must be a regular in-fixture file")
    prompt = prompt_path.read_text(encoding="utf-8").strip()
    if not prompt or f"sha256:{sha256_file(prompt_path)}" != prompt_sha:
        raise PlanAcceptanceError("Plan fixture prompt checksum mismatch")
    target = payload.get("expected_target_path")
    tools = payload.get("allowed_tools")
    if not isinstance(target, str) or Path(target).is_absolute() or ".." in Path(target).parts:
        raise PlanAcceptanceError("Plan fixture target must be workspace-relative")
    if not isinstance(tools, list) or not tools or not all(isinstance(tool, str) for tool in tools):
        raise PlanAcceptanceError("Plan fixture requires a non-empty read-only tool allowlist")
    files: list[tuple[str, Path, str]] = []
    for item in payload.get("files", []):
        relative = item.get("path") if isinstance(item, dict) else None
        source = item.get("source") if isinstance(item, dict) else None
        digest = item.get("sha256") if isinstance(item, dict) else None
        if not isinstance(relative, str) or Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise PlanAcceptanceError("Plan fixture file path must be workspace-relative")
        if not isinstance(source, str) or Path(source).is_absolute() or ".." in Path(source).parts:
            raise PlanAcceptanceError("Plan fixture source path must stay inside the fixture")
        source_relative = Path(source)
        reject_symlink_components(root, source_relative, "Plan fixture source")
        source_path = (root / source_relative).resolve(strict=True)
        if root not in source_path.parents or not source_path.is_file():
            raise PlanAcceptanceError("Plan fixture source must be a regular in-fixture file")
        if not isinstance(digest, str) or digest != f"sha256:{sha256_file(source_path)}":
            raise PlanAcceptanceError("Plan fixture file checksum mismatch")
        files.append((relative, source_path, digest.removeprefix("sha256:")))
    if not files:
        raise PlanAcceptanceError("Plan fixture must materialize at least one file")
    return PlanFixture(
        fixture_id="plan-only",
        prompt=prompt,
        expected_target_path=target,
        allowed_tools=frozenset(tools),
        files=tuple(files),
        manifest_digest=hashlib.sha256(manifest_bytes).hexdigest(),
    )


def _scrub_provider_config(value: object, *, field_name: str = "") -> object:
    normalized = field_name.lower()
    if any(
        marker in normalized
        for marker in ("api_key", "token", "secret", "password", "authorization", "header")
    ):
        return None
    if isinstance(value, dict):
        return {
            key: scrubbed
            for key, child in value.items()
            if (scrubbed := _scrub_provider_config(child, field_name=key)) is not None
        }
    if isinstance(value, list):
        return [_scrub_provider_config(child, field_name=field_name) for child in value]
    if "base_url" in normalized and isinstance(value, str):
        parsed = urlsplit(value)
        if not parsed.scheme or not parsed.netloc or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise PlanAcceptanceError("provider base URL contains an unsafe or invalid carrier")
    if isinstance(value, (str, int, float, bool)):
        return value
    raise PlanAcceptanceError("provider config contains an unsupported value type")


def validate_source_config(path: Path) -> SourceConfig:
    resolved = path.expanduser().resolve(strict=True)
    payload = tomllib.loads(resolved.read_text(encoding="utf-8"))
    agent = payload.get("agent")
    if (
        not isinstance(agent, dict)
        or not isinstance(agent.get("provider"), str)
        or not isinstance(agent.get("model"), str)
    ):
        raise PlanAcceptanceError("Plan acceptance requires explicit [agent] provider and model")
    provider = agent["provider"]
    providers = payload.get("providers")
    provider_value = providers.get(provider) if isinstance(providers, dict) else None
    if not isinstance(provider_value, dict):
        raise PlanAcceptanceError("Plan acceptance requires the active provider config block")
    scrubbed = _scrub_provider_config(provider_value)
    if not isinstance(scrubbed, dict):
        raise PlanAcceptanceError("active provider config could not be normalized")
    required_credential = PROVIDER_CREDENTIAL_ENV.get(provider)
    if required_credential is None:
        raise PlanAcceptanceError("Plan acceptance does not recognize the active provider credential")
    if not os.environ.get(required_credential, "").strip():
        raise PlanAcceptanceError("Plan acceptance requires the active provider credential in its documented environment variable")
    return SourceConfig(resolved, provider, agent["model"], scrubbed)


def _toml_scalar(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, int) and not isinstance(value, bool):
        return str(value)
    if isinstance(value, float) and value == value and abs(value) != float("inf"):
        return repr(value)
    if isinstance(value, list) and all(not isinstance(item, (dict, list)) for item in value):
        return "[" + ", ".join(_toml_scalar(item) for item in value) + "]"
    raise PlanAcceptanceError("isolated provider config cannot serialize a complex array value")


def _render_toml_table(path: tuple[str, ...], value: dict[str, object], lines: list[str]) -> None:
    if not all(TOML_KEY.fullmatch(part) for part in path):
        raise PlanAcceptanceError("isolated provider config contains an unsafe TOML key")
    lines.append(f"[{'.'.join(path)}]")
    for key, child in value.items():
        if not TOML_KEY.fullmatch(key):
            raise PlanAcceptanceError("isolated provider config contains an unsafe TOML key")
        if not isinstance(child, dict):
            lines.append(f"{key} = {_toml_scalar(child)}")
    lines.append("")
    for key, child in value.items():
        if isinstance(child, dict):
            _render_toml_table((*path, key), child, lines)


def write_isolated_config(source: SourceConfig, case_root: Path) -> Path:
    case_root.mkdir(parents=True, exist_ok=True)
    config: dict[str, dict[str, object]] = {
        "workspace": {"root": "."},
        "storage": {
            "state_root": str(case_root / "state"),
            "cache_root": str(case_root / "cache"),
        },
        "session": {"log_dir": "sessions"},
        "agent": {
            "provider": source.provider,
            "model": source.model,
            "max_turns": 4,
            "tool_timeout_secs": 15,
        },
        "model_request": {
            "request_timeout_secs": 45,
            "stream_idle_timeout_secs": 30,
            "stream_total_timeout_secs": 90,
        },
        "permission": {"mode": "read-only"},
        "memory": {"enabled": False},
        "skills": {"enabled": False, "user_skills": False, "user_agents": False},
        "compaction": {"enabled": False},
        "code_intelligence": {"enabled": False},
        "task": {"enabled": False},
        "web": {"enabled": False, "network_mode": "deny"},
        "terminal": {
            "keyboard_enhancement": "off",
            "mouse_capture": False,
            "osc52_clipboard": False,
        },
        "providers": {source.provider: source.provider_config},
    }
    lines: list[str] = []
    for table, values in config.items():
        _render_toml_table((table,), values, lines)
    rendered = "\n".join(lines)
    lowered = rendered.lower()
    if any(marker in lowered for marker in ("api_key", "password", "authorization", "secret")):
        raise PlanAcceptanceError("isolated Plan config retained a secret-capable field")
    config_path = case_root / "config.toml"
    config_path.write_text(rendered, encoding="utf-8")
    round_trip = tomllib.loads(config_path.read_text(encoding="utf-8"))
    if (
        round_trip.get("agent", {}).get("provider") != source.provider
        or round_trip.get("agent", {}).get("max_turns") != 4
        or round_trip.get("permission", {}).get("mode") != "read-only"
        or round_trip.get("web", {}).get("enabled") is not False
    ):
        raise PlanAcceptanceError("isolated Plan config failed its safety round trip")
    return config_path


def materialize_fixture(fixture: PlanFixture, workspace: Path) -> str:
    workspace.mkdir(mode=0o700)
    for relative, source, expected_digest in fixture.files:
        destination = workspace / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        if sha256_file(destination) != expected_digest:
            raise PlanAcceptanceError("materialized Plan fixture checksum mismatch")
    return workspace_digest(workspace)


def workspace_digest(workspace: Path) -> str:
    digest = hashlib.sha256(b"sigil-plan-dogfood-workspace-v1\0")
    for path in sorted(workspace.rglob("*")):
        relative = path.relative_to(workspace).as_posix()
        if path.is_symlink():
            raise PlanAcceptanceError("Plan fixture workspace contains a symlink")
        if path.is_dir():
            digest.update(f"dir:{relative}\0".encode())
        elif path.is_file():
            digest.update(f"file:{relative}\0".encode())
            digest.update(path.read_bytes())
            digest.update(b"\0")
        else:
            raise PlanAcceptanceError("Plan fixture workspace contains a non-file entry")
    return digest.hexdigest()


def provider_environment(case_root: Path, provider: str) -> dict[str, str]:
    environment = SUPPORT.identity_environment(os.environ)
    allowed_names = COMMON_PROVIDER_ENV_NAMES | PROVIDER_ENV_BY_NAME.get(provider, set())
    for name in allowed_names:
        if name in os.environ:
            environment[name] = os.environ[name]
    environment.update(
        {
            "HOME": str(case_root / "home"),
            "XDG_CONFIG_HOME": str(case_root / "xdg-config"),
            "XDG_STATE_HOME": str(case_root / "xdg-state"),
            "XDG_CACHE_HOME": str(case_root / "xdg-cache"),
            "TMPDIR": str(case_root / "tmp"),
            "SIGIL_STATE_HOME": str(case_root / "state"),
            "SIGIL_CACHE_HOME": str(case_root / "cache"),
            "TERM": environment.get("TERM", "xterm-256color"),
        }
    )
    for name in ("home", "xdg-config", "xdg-state", "xdg-cache", "tmp", "state", "cache"):
        (case_root / name).mkdir(mode=0o700, exist_ok=True)
    return environment


def session_files(state_root: Path) -> list[Path]:
    return sorted(
        path
        for path in state_root.rglob("session-*.jsonl")
        if path.is_file() and path.parent.name == "sessions"
    )


def _control(record: dict[str, object]) -> dict[str, object]:
    payload = record.get("payload")
    entry = payload.get("session_log_entry") if isinstance(payload, dict) else None
    control = entry.get("control") if isinstance(entry, dict) else None
    return control if isinstance(control, dict) else {}


def read_plan_audit(path: Path) -> PlanAudit:
    event_counts: dict[str, int] = {}
    tool_names: set[str] = set()
    target_paths: set[str] = set()
    plan_step_count = 0
    usage_events = 0
    prompt_tokens = 0
    completion_tokens = 0
    observed_cost = 0.0
    observed_cost_present = False
    run_finalized = 0
    failed_runs = 0
    task_created = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        record = json.loads(line)
        event_type = record.get("event_type")
        if isinstance(event_type, str):
            event_counts[event_type] = event_counts.get(event_type, 0) + 1
        payload = record.get("payload")
        if event_type == "run_finalized":
            run_finalized += 1
            if not isinstance(payload, dict) or payload.get("run_status") != "completed":
                failed_runs += 1
        if event_type == "task_created_from_plan":
            task_created += 1
        control = _control(record)
        entry = payload.get("session_log_entry") if isinstance(payload, dict) else None
        assistant = entry.get("assistant") if isinstance(entry, dict) else None
        if isinstance(assistant, dict):
            for call in assistant.get("tool_calls", []):
                if isinstance(call, dict) and isinstance(call.get("name"), str):
                    tool_names.add(call["name"])
        tool = control.get("tool_execution")
        if isinstance(tool, dict) and isinstance(tool.get("tool_name"), str):
            tool_names.add(tool["tool_name"])
        plan = control.get("plan_draft_created")
        if isinstance(plan, dict):
            for target in plan.get("target_paths", []):
                if isinstance(target, str):
                    target_paths.add(target)
            steps = plan.get("steps", [])
            if isinstance(steps, list):
                plan_step_count = max(plan_step_count, len(steps))
                for step in steps:
                    if not isinstance(step, dict):
                        continue
                    for target in step.get("target_paths", []):
                        if isinstance(target, str):
                            target_paths.add(target)
        usage = control.get("usage_snapshot")
        if isinstance(usage, dict):
            usage_events += 1
            prompt_tokens += int(usage.get("prompt_tokens", 0) or 0)
            completion_tokens += int(usage.get("completion_tokens", 0) or 0)
            input_cost = usage.get("input_cost")
            output_cost = usage.get("output_cost")
            if isinstance(input_cost, (int, float)) and isinstance(output_cost, (int, float)):
                observed_cost += float(input_cost) + float(output_cost)
                observed_cost_present = True
    return PlanAudit(
        session_path=path,
        event_counts=event_counts,
        tool_names=tuple(sorted(tool_names)),
        plan_target_paths=tuple(sorted(target_paths)),
        plan_step_count=plan_step_count,
        usage_events=usage_events,
        prompt_tokens=prompt_tokens,
        completion_tokens=completion_tokens,
        observed_cost_usd=observed_cost if observed_cost_present else None,
        run_finalized_count=run_finalized,
        failed_run_count=failed_runs,
        task_created_count=task_created,
    )


def type_text_while_draining(runner: object, value: str) -> None:
    """Keep the PTY writable while a long prompt triggers repeated TUI redraws."""
    for character in value:
        runner.send(character)
        runner.read_available(0.002)


def wait_for_plan_audit(
    state_root: Path,
    timeout: float,
    *,
    tick: Callable[[], None] | None = None,
) -> PlanAudit:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        if tick is not None:
            tick()
        files = session_files(state_root)
        if len(files) > 1:
            raise PlanAcceptanceError("Plan case created more than one session stream")
        if files:
            try:
                audit = read_plan_audit(files[0])
                if (
                    audit.event_counts.get("plan_draft_created", 0) == 1
                    and audit.run_finalized_count == 1
                ):
                    return audit
            except (OSError, json.JSONDecodeError, PlanAcceptanceError) as error:
                last_error = error
        time.sleep(0.05)
    suffix = f": {type(last_error).__name__}" if last_error is not None else ""
    raise TimeoutError(f"timed out waiting for durable Plan evidence{suffix}")


def validate_audit(audit: PlanAudit, fixture: PlanFixture, _max_cost_microusd: int) -> None:
    if audit.event_counts.get("plan_draft_created", 0) != 1 or audit.plan_step_count < 1:
        raise PlanAcceptanceError("Plan case did not create exactly one structured draft")
    if fixture.expected_target_path not in audit.plan_target_paths:
        raise PlanAcceptanceError("Plan draft omitted the committed target path")
    unexpected_tools = set(audit.tool_names) - fixture.allowed_tools
    if unexpected_tools:
        raise PlanAcceptanceError("Plan case invoked a tool outside its read-only allowlist")
    if audit.task_created_count != 0 or audit.event_counts.get("task_created_from_plan", 0) != 0:
        raise PlanAcceptanceError("Plan-only case unexpectedly created a task")
    if audit.run_finalized_count != 1 or audit.failed_run_count != 0:
        raise PlanAcceptanceError("Plan provider run did not finalize exactly once as completed")
    if audit.usage_events == 0:
        raise PlanAcceptanceError("Plan provider run did not persist a usage snapshot")


def safe_manifest(
    *,
    status: str,
    identity: object,
    fixture: PlanFixture,
    started_at: str,
    finished_at: str,
    duration_ms: int,
    max_cost_microusd: int,
    charged_microusd: int,
    audit: PlanAudit | None,
    workspace_unchanged: bool,
    artifact_policy: str,
    failure_class: str | None,
) -> dict[str, object]:
    checks: dict[str, object] = {
        "workspace_unchanged": workspace_unchanged,
        "plan_draft_created_count": 0,
        "plan_step_count": 0,
        "target_path_present": False,
        "task_created_from_plan_count": 0,
        "failed_run_count": 0,
        "usage_event_count": 0,
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "observed_cost_usd": None,
        "cost_confidence": "unknown",
        "tool_names": [],
    }
    evidence: dict[str, str] = {"pty_log": "plan-process.log"}
    if audit is not None:
        checks.update(
            {
                "plan_draft_created_count": audit.event_counts.get("plan_draft_created", 0),
                "plan_step_count": audit.plan_step_count,
                "target_path_present": fixture.expected_target_path in audit.plan_target_paths,
                "task_created_from_plan_count": audit.task_created_count,
                "failed_run_count": audit.failed_run_count,
                "usage_event_count": audit.usage_events,
                "prompt_tokens": audit.prompt_tokens,
                "completion_tokens": audit.completion_tokens,
                "observed_cost_usd": audit.observed_cost_usd,
                "cost_confidence": "reported" if audit.observed_cost_usd is not None else "unknown",
                "tool_names": list(audit.tool_names),
            }
        )
        evidence["session"] = "session.jsonl"
        evidence["session_sha256"] = sha256_file(audit.session_path)
    manifest: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "campaign": "sigil-real-provider-plan-v1",
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_ms": duration_ms,
        "binary": identity.as_dict(),
        "fixture": {
            "id": fixture.fixture_id,
            "manifest_sha256": fixture.manifest_digest,
        },
        "budget": {
            "admission_max_microusd": max_cost_microusd,
            "charged_microusd": charged_microusd,
            "provider_side_cap": False,
        },
        "checks": checks,
        "evidence": evidence,
        "privacy": {
            "raw_artifacts_local_only": True,
            "raw_artifact_policy": artifact_policy,
            "automatic_upload": False,
            "manifest_contains_prompt_provider_or_session_content": False,
        },
    }
    if failure_class is not None:
        manifest["failure_class"] = failure_class
    serialized = json.dumps(manifest, sort_keys=True)
    forbidden = ("/Users/", "/home/", fixture.prompt, "PLAN_DOGFOOD_TYPOO")
    if any(value in serialized for value in forbidden):
        raise PlanAcceptanceError("safe Plan manifest contains private path or fixture content")
    return manifest


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run one production-TUI Plan-only case through an explicit real provider.",
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--max-cost-usd", required=True)
    parser.add_argument("--timeout-secs", type=int, required=True)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-commit")
    parser.add_argument("--expected-binary-sha256")
    args = parser.parse_args(argv)
    if not 30 <= args.timeout_secs <= 600:
        parser.error("--timeout-secs must be between 30 and 600")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    root = repo_root()
    started_at = utc_now()
    started = time.monotonic()
    runner = None
    fixture_root: Path | None = None
    audit: PlanAudit | None = None
    workspace_unchanged = False
    artifact_policy = "unadmitted"
    status = "failed"
    failure_class: str | None = None
    output_dir: Path | None = None
    workspace: Path | None = None
    before_digest: str | None = None
    identity = SUPPORT.BinaryIdentity("unadmitted", "", "", "", "", "")
    fixture = PlanFixture("unadmitted", "", "", frozenset(), (), "")
    deadline = SUPPORT.CampaignDeadline(args.timeout_secs)
    try:
        max_cost_microusd = parse_cost_microusd(args.max_cost_usd)
        source_config = validate_source_config(args.config)
        fixture_path = args.fixture if args.fixture.is_absolute() else root / args.fixture
        fixture = load_fixture(fixture_path)
        binary_source, identity = SUPPORT.inspect_binary(args.binary, timeout=deadline.remaining(15.0))
        SUPPORT.assert_expected_identity(identity, args)
        timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        selected = args.output_dir or root / ".repo-local-dev" / "dogfood" / f"plan-{timestamp}"
        output_dir = (selected if selected.is_absolute() else root / selected).expanduser().resolve()
        artifact_policy = SUPPORT.raw_artifact_policy(root, output_dir)
        output_dir.mkdir(parents=True, exist_ok=False)

        fixture_root = Path(tempfile.mkdtemp(prefix="sigil-real-plan-"))
        frozen_binary = SUPPORT.freeze_binary(binary_source, fixture_root, identity)
        config_path = write_isolated_config(source_config, fixture_root)
        workspace = fixture_root / "workspace"
        before_digest = materialize_fixture(fixture, workspace)
        environment = provider_environment(fixture_root, source_config.provider)
        raw_log = output_dir / "plan-process.log"
        runner = SUPPORT.PtyRunner(
            [str(frozen_binary), "--config", str(config_path)],
            workspace,
            environment,
            raw_log,
        )
        runner.start()
        SUPPORT.wait_for_main_tui(runner, deadline.remaining())
        type_text_while_draining(runner, f"/plan {fixture.prompt}")
        runner.send("\r")
        audit = wait_for_plan_audit(
            fixture_root / "state",
            deadline.remaining(PLAN_RUN_WAIT_CAP_SECS),
            tick=lambda: runner.read_available(0.01),
        )
        plan_screen = runner.wait_until(
            lambda text: "plan ready" in text.lower() and "create and run task" in text.lower(),
            deadline.remaining(),
            "visible Plan review surface",
            final_screen=True,
        )
        if "plan ready" not in plan_screen.lower():
            raise PlanAcceptanceError("Plan review surface was not visible")
        validate_audit(audit, fixture, max_cost_microusd)
        runner.quit(timeout=deadline.remaining(10.0))
        runner.stop()
        runner = None
        after_digest = workspace_digest(workspace)
        workspace_unchanged = before_digest == after_digest
        if not workspace_unchanged:
            raise PlanAcceptanceError("Plan-only case changed the workspace after terminal cleanup")
        audit = read_plan_audit(audit.session_path)
        validate_audit(audit, fixture, max_cost_microusd)
        shutil.copyfile(audit.session_path, output_dir / "session.jsonl")
        if sha256_file(audit.session_path) != sha256_file(output_dir / "session.jsonl"):
            raise PlanAcceptanceError("Plan session checksum changed while preserving evidence")
        status = "passed"
    except Exception as error:  # noqa: BLE001 - terminal evidence classifies all failures.
        failure_class = type(error).__name__
        if output_dir is not None:
            (output_dir / "runner-error.txt").write_text(f"{type(error).__name__}: {error}\n", encoding="utf-8")
    finally:
        if runner is not None:
            runner.stop()
        if audit is None and fixture_root is not None:
            incomplete_sessions = session_files(fixture_root / "state")
            if len(incomplete_sessions) == 1:
                try:
                    audit = read_plan_audit(incomplete_sessions[0])
                except (OSError, json.JSONDecodeError, PlanAcceptanceError):
                    pass
        if workspace is not None and before_digest is not None and workspace.exists():
            try:
                workspace_unchanged = before_digest == workspace_digest(workspace)
            except (OSError, PlanAcceptanceError):
                workspace_unchanged = False
            if status == "passed" and not workspace_unchanged:
                status = "failed"
                failure_class = "late_workspace_mutation"
        if output_dir is not None and audit is not None and audit.session_path.exists():
            preserved = output_dir / "session.jsonl"
            if audit.session_path != preserved:
                shutil.copyfile(audit.session_path, preserved)
                if sha256_file(audit.session_path) != sha256_file(preserved):
                    failure_class = "PlanAcceptanceError"
                    status = "failed"
                audit = dataclasses.replace(audit, session_path=preserved)
        if fixture_root is not None and fixture_root.exists():
            shutil.rmtree(fixture_root, ignore_errors=True)

    if output_dir is None:
        print(f"Plan acceptance admission failed: {failure_class or 'unknown'}", file=sys.stderr)
        return 2
    max_cost_microusd = parse_cost_microusd(args.max_cost_usd)
    observed_microusd = 0
    if audit is not None and audit.observed_cost_usd is not None:
        observed_microusd = int(audit.observed_cost_usd * 1_000_000 + 0.999999)
    charged_microusd = max(max_cost_microusd, observed_microusd)
    manifest = safe_manifest(
        status=status,
        identity=identity,
        fixture=fixture,
        started_at=started_at,
        finished_at=utc_now(),
        duration_ms=int((time.monotonic() - started) * 1000),
        max_cost_microusd=max_cost_microusd,
        charged_microusd=charged_microusd,
        audit=audit,
        workspace_unchanged=workspace_unchanged,
        artifact_policy=artifact_policy,
        failure_class=failure_class,
    )
    manifest_path = output_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output_dir / "manifest.sha256").write_text(
        f"{sha256_file(manifest_path)}  manifest.json\n",
        encoding="utf-8",
    )
    print(f"Plan acceptance: {status}")
    print(f"manifest SHA-256: {sha256_file(manifest_path)}")
    print(f"evidence: {output_dir}")
    return 0 if status == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
