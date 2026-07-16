#!/usr/bin/env python3
"""Run RFC-0034's explicit bounded real-provider dogfood matrix."""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from typing import Sequence


PLAN_SCRIPT = Path(__file__).with_name("tui-plan-provider-acceptance.py")
PLAN_SPEC = importlib.util.spec_from_file_location("tui_plan_provider_acceptance", PLAN_SCRIPT)
assert PLAN_SPEC is not None and PLAN_SPEC.loader is not None
PLAN = importlib.util.module_from_spec(PLAN_SPEC)
sys.modules[PLAN_SPEC.name] = PLAN
PLAN_SPEC.loader.exec_module(PLAN)
SUPPORT = PLAN.SUPPORT

SCHEMA_VERSION = 1
PLAN_CASE = "plan-only"
MODEL_CASES = (
    "small-code-edit",
    "stale-after-write",
    "workspace-trust",
    "sandbox-denial",
)
CASE_ORDER = (*MODEL_CASES, PLAN_CASE)


class CampaignError(RuntimeError):
    """Raised when campaign admission or evidence fails."""


@dataclasses.dataclass(frozen=True)
class ChildResult:
    status: str
    exit_code: int | None
    duration_ms: int
    failure_class: str | None


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def selected_cases(requested: Sequence[str] | None) -> list[str]:
    if not requested:
        return list(CASE_ORDER)
    unknown = sorted(set(requested) - set(CASE_ORDER))
    if unknown:
        raise CampaignError(f"unknown real-provider cases: {', '.join(unknown)}")
    return [case for case in CASE_ORDER if case in requested]


def budget_allocations(max_cost_microusd: int, cases: Sequence[str], repetitions: int) -> dict[str, object]:
    planned_runs = len(cases) * repetitions
    if planned_runs <= 0:
        raise CampaignError("real-provider campaign must plan at least one run")
    base, remainder = divmod(max_cost_microusd, planned_runs)
    if base <= 0:
        raise CampaignError("real-provider budget must reserve at least one microUSD per run")
    plan_allocations: list[int] = []
    model_budget = 0
    allocation_index = 0
    for case in cases:
        for _ in range(repetitions):
            allocation = base + (1 if allocation_index < remainder else 0)
            allocation_index += 1
            if case == PLAN_CASE:
                plan_allocations.append(allocation)
            else:
                model_budget += allocation
    return {
        "planned_runs": planned_runs,
        "base_reservation_microusd": base,
        "unallocated_microusd": 0,
        "model_budget_microusd": model_budget,
        "plan_run_budgets_microusd": plan_allocations,
    }


def format_microusd(value: int) -> str:
    return f"{value / 1_000_000:.6f}"


def admission_failure(
    *,
    accounting_charged_microusd: int,
    next_reservation_microusd: int,
    max_cost_microusd: int,
    remaining_seconds: float,
    minimum_seconds: float = 30.0,
) -> str | None:
    if remaining_seconds < minimum_seconds:
        return "deadline_before_admission"
    if (
        accounting_charged_microusd > max_cost_microusd
        or next_reservation_microusd > max_cost_microusd - accounting_charged_microusd
    ):
        return "budget_exhausted_before_admission"
    return None


def child_environment(case_root: Path, provider: str) -> dict[str, str]:
    environment = SUPPORT.identity_environment(os.environ)
    allowed_names = PLAN.COMMON_PROVIDER_ENV_NAMES | PLAN.PROVIDER_ENV_BY_NAME.get(provider, set())
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
            "TERM": environment.get("TERM", "xterm-256color"),
        }
    )
    for name in ("home", "xdg-config", "xdg-state", "xdg-cache", "tmp"):
        (case_root / name).mkdir(parents=True, mode=0o700, exist_ok=True)
    return environment


def run_child(
    command: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
    log_path: Path,
    timeout: float,
) -> ChildResult:
    started = time.monotonic()
    descendants: set[int] = set()
    monitor_stop = threading.Event()
    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("wb") as log:
        try:
            process = subprocess.Popen(
                command,
                cwd=cwd,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError:
            return ChildResult(
                status="failed",
                exit_code=None,
                duration_ms=int((time.monotonic() - started) * 1000),
                failure_class="process_start_failed",
            )

        def monitor() -> None:
            while not monitor_stop.wait(0.05):
                descendants.update(SUPPORT.posix_descendant_pids(process.pid))

        monitor_thread = threading.Thread(target=monitor, name="sigil-real-provider-descendants", daemon=True)
        monitor_thread.start()
        failure_class: str | None = None
        try:
            exit_code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            failure_class = "deadline_exceeded"
            exit_code = None
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=3)
            except (OSError, subprocess.TimeoutExpired):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except OSError:
                    pass
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
        finally:
            descendants.update(SUPPORT.posix_descendant_pids(process.pid))
            monitor_stop.set()
            monitor_thread.join(timeout=1)
        remaining = {pid for pid in descendants if SUPPORT.process_is_running(pid)}
        if remaining:
            SUPPORT.signal_processes(remaining, signal.SIGTERM)
            remaining = SUPPORT.wait_for_processes(remaining, 2)
            if remaining:
                SUPPORT.signal_processes(remaining, signal.SIGKILL)
                SUPPORT.wait_for_processes(remaining, 2)
            failure_class = failure_class or "descendant_cleanup"
    if failure_class is None and exit_code != 0:
        failure_class = "child_exit_nonzero"
    return ChildResult(
        status="passed" if failure_class is None else "failed",
        exit_code=exit_code,
        duration_ms=int((time.monotonic() - started) * 1000),
        failure_class=failure_class,
    )


def parse_plan_result(output_dir: Path, child: ChildResult, repetition: int, budget: int) -> dict[str, object]:
    manifest_path = output_dir / "manifest.json"
    status = child.status
    checks: dict[str, object] = {}
    charged_microusd = budget
    failure_class = child.failure_class
    manifest_sha256: str | None = None
    if manifest_path.is_file():
        manifest_sha256 = SUPPORT.sha256_file(manifest_path)
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            if manifest.get("schema_version") != 1 or manifest.get("campaign") != "sigil-real-provider-plan-v1":
                raise CampaignError("Plan child manifest has the wrong schema or campaign")
            status = (
                "passed"
                if child.status == "passed" and manifest.get("status") == "passed"
                else "failed"
            )
            source_checks = manifest.get("checks")
            if not isinstance(source_checks, dict):
                raise CampaignError("Plan child manifest is missing checks")
            checks = {
                "workspace_unchanged": source_checks.get("workspace_unchanged") is True,
                "plan_draft_created_count": source_checks.get("plan_draft_created_count", 0),
                "task_created_from_plan_count": source_checks.get("task_created_from_plan_count", 0),
                "usage_event_count": source_checks.get("usage_event_count", 0),
                "observed_cost_usd": source_checks.get("observed_cost_usd"),
                "cost_confidence": source_checks.get("cost_confidence", "unknown"),
            }
            child_budget = manifest.get("budget")
            raw_charged = child_budget.get("charged_microusd") if isinstance(child_budget, dict) else None
            if not isinstance(raw_charged, int) or raw_charged < budget:
                raise CampaignError("Plan child manifest has invalid charged cost")
            charged_microusd = raw_charged
        except (CampaignError, json.JSONDecodeError, OSError, TypeError, ValueError):
            status = "failed"
            failure_class = "invalid_plan_evidence"
    else:
        status = "failed"
        failure_class = failure_class or "missing_plan_evidence"
    return {
        "case_id": PLAN_CASE,
        "repetition": repetition,
        "status": status,
        "admission_budget_microusd": budget,
        "duration_ms": child.duration_ms,
        "charged_microusd": charged_microusd,
        "failure_class": failure_class,
        "checks": checks,
        "evidence_dir": output_dir.name,
        "manifest_sha256": manifest_sha256,
    }


def parse_model_results(output_dir: Path, cases: Sequence[str], repetitions: int) -> tuple[list[dict[str, object]], int]:
    results_path = output_dir / "results.jsonl"
    manifest_path = output_dir / "manifest.json"
    expected_keys = {(case, repetition) for case in cases for repetition in range(1, repetitions + 1)}
    results_sha256 = SUPPORT.sha256_file(results_path) if results_path.is_file() else None
    manifest_sha256 = SUPPORT.sha256_file(manifest_path) if manifest_path.is_file() else None

    def failed_records(failure_class: str) -> list[dict[str, object]]:
        failed: list[dict[str, object]] = []
        for case in cases:
            for repetition in range(1, repetitions + 1):
                failed.append(
                    {
                        "case_id": case,
                        "repetition": repetition,
                        "status": "failed",
                        "execution_status": "missing_or_invalid_evidence",
                        "failure_class": failure_class,
                        "evidence_dir": "model-eval",
                        "manifest_sha256": manifest_sha256,
                        "results_sha256": results_sha256,
                    }
                )
        return failed

    if not results_path.is_file() or not manifest_path.is_file():
        return failed_records("missing_model_evidence"), 0
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if (
            manifest.get("report_schema_version") != 3
            or manifest.get("requested_repetitions") != len(expected_keys)
        ):
            raise CampaignError("model manifest has the wrong schema or requested count")
        raw_charged = manifest.get("charged_microusd")
        if not isinstance(raw_charged, int) or raw_charged < 0:
            raise CampaignError("model manifest has invalid charged cost")
        records: list[dict[str, object]] = []
        observed_keys: set[tuple[str, int]] = set()
        for line in results_path.read_text(encoding="utf-8").splitlines():
            record = json.loads(line)
            if record.get("report_schema_version") != 3:
                raise CampaignError("model result has the wrong schema")
            result = record.get("result")
            metadata = result.get("metadata") if isinstance(result, dict) else None
            case_id = metadata.get("case_id") if isinstance(metadata, dict) else None
            repetition = record.get("repetition")
            if case_id not in cases or not isinstance(repetition, int):
                raise CampaignError("model result contains an invalid case or repetition")
            key = (case_id, repetition)
            if key not in expected_keys or key in observed_keys:
                raise CampaignError("model result key set is not unique and exact")
            observed_keys.add(key)
            records.append(
                {
                    "case_id": case_id,
                    "repetition": repetition,
                    "status": "passed" if record.get("acceptance_passed") is True else "failed",
                    "execution_status": record.get("execution_status"),
                    "evidence_dir": "model-eval",
                    "manifest_sha256": manifest_sha256,
                    "results_sha256": results_sha256,
                }
            )
        if observed_keys != expected_keys:
            raise CampaignError("model result key set is incomplete")
        return records, raw_charged
    except (CampaignError, json.JSONDecodeError, OSError, TypeError, ValueError):
        return failed_records("invalid_model_evidence"), 0


def safe_manifest(
    *,
    status: str,
    started_at: str,
    finished_at: str,
    duration_ms: int,
    identity: object,
    selected: Sequence[str],
    repetitions: int,
    timeout_secs: int,
    max_cost_microusd: int,
    accounting_charged_microusd: int,
    results: Sequence[dict[str, object]],
    artifact_policy: str,
) -> dict[str, object]:
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "campaign": "sigil-real-provider-dogfood-v1",
        "status": status,
        "started_at": started_at,
        "finished_at": finished_at,
        "duration_ms": duration_ms,
        "binary": identity.as_dict(),
        "selection": {"cases": list(selected), "repetitions": repetitions},
        "deadline_secs": timeout_secs,
        "budget": {
            "local_admission_max_microusd": max_cost_microusd,
            "accounting_charged_microusd": accounting_charged_microusd,
            "provider_side_cap": False,
        },
        "results": list(results),
        "privacy": {
            "raw_artifacts_local_only": True,
            "raw_artifact_policy": artifact_policy,
            "automatic_upload": False,
            "manifest_contains_prompt_provider_or_session_content": False,
        },
    }
    serialized = json.dumps(manifest, sort_keys=True)
    forbidden = ("/Users/", "/home/", "PLAN_DOGFOOD_TYPOO", "api_key", "session_log_entry")
    if any(value in serialized for value in forbidden):
        raise CampaignError("safe real-provider manifest contains private material")
    return manifest


def render_summary(manifest: dict[str, object]) -> str:
    budget = manifest["budget"]
    assert isinstance(budget, dict)
    lines = [
        "# Sigil Real-provider Dogfood Campaign",
        "",
        f"Status: `{manifest['status']}`",
        f"Deadline: `{manifest['deadline_secs']}s`",
        f"Local admission budget: `${budget['local_admission_max_microusd'] / 1_000_000:.6f}`",
        "Provider-side billing cap: `false`",
        "",
        "| Case | Repetition | Status |",
        "| --- | ---: | --- |",
    ]
    for result in manifest["results"]:
        assert isinstance(result, dict)
        lines.append(f"| `{result['case_id']}` | {result['repetition']} | `{result['status']}` |")
    return "\n".join(lines) + "\n"


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run RFC-0034's explicit edit/verification/Plan real-provider matrix.",
    )
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--case", action="append", choices=CASE_ORDER)
    parser.add_argument("--repetitions", type=int, required=True)
    parser.add_argument("--max-cost-usd", required=True)
    parser.add_argument("--timeout-secs", type=int, required=True)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-commit")
    parser.add_argument("--expected-binary-sha256")
    args = parser.parse_args(argv)
    if not 1 <= args.repetitions <= 3:
        parser.error("--repetitions must be between 1 and 3")
    if not 60 <= args.timeout_secs <= 600:
        parser.error("--timeout-secs must be between 60 and 600")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    root = repo_root()
    started_at = utc_now()
    started = time.monotonic()
    try:
        selected = selected_cases(args.case)
        max_cost_microusd = PLAN.parse_cost_microusd(args.max_cost_usd)
        allocations = budget_allocations(max_cost_microusd, selected, args.repetitions)
        source_config = PLAN.validate_source_config(args.config)
        config_path = source_config.path
        if PLAN_CASE in selected:
            PLAN.load_fixture(root / PLAN.DEFAULT_FIXTURE)
        binary_source, identity = SUPPORT.inspect_binary(args.binary)
        SUPPORT.assert_expected_identity(identity, args)
        timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        selected_output = args.output_dir or root / ".repo-local-dev" / "dogfood" / f"real-{timestamp}"
        output_dir = (selected_output if selected_output.is_absolute() else root / selected_output).resolve()
        artifact_policy = SUPPORT.raw_artifact_policy(root, output_dir)
        output_dir.mkdir(parents=True, exist_ok=False)
    except Exception as error:  # noqa: BLE001 - admission must fail before provider dispatch.
        print(f"real-provider campaign admission failed: {type(error).__name__}", file=sys.stderr)
        return 2

    deadline = time.monotonic() + args.timeout_secs
    results: list[dict[str, object]] = []
    accounting_charged = 0
    with tempfile.TemporaryDirectory(prefix="sigil-real-provider-campaign-") as temporary:
        temporary_root = Path(temporary)
        frozen_binary = SUPPORT.freeze_binary(binary_source, temporary_root, identity)
        plan_budgets = iter(allocations["plan_run_budgets_microusd"])
        if PLAN_CASE in selected:
            for repetition in range(1, args.repetitions + 1):
                budget = next(plan_budgets)
                remaining = deadline - time.monotonic()
                case_output = output_dir / "plan-only" / f"repetition-{repetition}"
                failure_class = admission_failure(
                    accounting_charged_microusd=accounting_charged,
                    next_reservation_microusd=budget,
                    max_cost_microusd=max_cost_microusd,
                    remaining_seconds=remaining,
                )
                if failure_class is not None:
                    results.append(
                        {
                            "case_id": PLAN_CASE,
                            "repetition": repetition,
                            "status": "failed",
                            "failure_class": failure_class,
                            "charged_microusd": 0,
                            "evidence_dir": case_output.relative_to(output_dir).as_posix(),
                        }
                    )
                    continue
                command = [
                    sys.executable,
                    str(PLAN_SCRIPT),
                    "--binary",
                    str(frozen_binary),
                    "--config",
                    str(config_path),
                    "--max-cost-usd",
                    format_microusd(budget),
                    "--timeout-secs",
                    str(min(180, int(remaining))),
                    "--output-dir",
                    str(case_output),
                    "--expected-version",
                    identity.version,
                    "--expected-commit",
                    identity.commit,
                    "--expected-binary-sha256",
                    identity.sha256,
                ]
                environment = child_environment(
                    temporary_root / f"plan-child-{repetition}",
                    source_config.provider,
                )
                child = run_child(
                    command,
                    cwd=root,
                    environment=environment,
                    log_path=output_dir / "plan-only" / f"repetition-{repetition}.log",
                    timeout=min(190.0, remaining),
                )
                result = parse_plan_result(case_output, child, repetition, budget)
                result["evidence_dir"] = case_output.relative_to(output_dir).as_posix()
                results.append(result)
                raw_charged = result.get("charged_microusd")
                accounting_charged += raw_charged if isinstance(raw_charged, int) else budget

        selected_model_cases = [case for case in selected if case in MODEL_CASES]
        if selected_model_cases:
            model_budget = allocations["model_budget_microusd"]
            assert isinstance(model_budget, int)
            remaining = deadline - time.monotonic()
            model_output = output_dir / "model-eval"
            failure_class = admission_failure(
                accounting_charged_microusd=accounting_charged,
                next_reservation_microusd=model_budget,
                max_cost_microusd=max_cost_microusd,
                remaining_seconds=remaining,
            )
            if failure_class is not None:
                model_records = [
                    {
                        "case_id": case,
                        "repetition": repetition,
                        "status": "failed",
                        "execution_status": "not_admitted",
                        "failure_class": failure_class,
                        "evidence_dir": "model-eval",
                    }
                    for case in selected_model_cases
                    for repetition in range(1, args.repetitions + 1)
                ]
                model_charged = 0
                results.extend(model_records)
                accounting_charged += model_charged
            else:
                command = [
                    str(frozen_binary),
                    "--config",
                    str(config_path),
                    "model-eval",
                    "--repetitions",
                    str(args.repetitions),
                    "--max-cost-usd",
                    format_microusd(model_budget),
                    "--timeout-secs",
                    str(max(30, int(remaining))),
                    "--output-dir",
                    str(model_output),
                ]
                for case in selected_model_cases:
                    command.extend(("--case", case))
                environment = child_environment(
                    temporary_root / "model-child",
                    source_config.provider,
                )
                model_child = run_child(
                    command,
                    cwd=root,
                    environment=environment,
                    log_path=output_dir / "model-eval.log",
                    timeout=remaining,
                )
                model_records, model_charged = parse_model_results(
                    model_output,
                    selected_model_cases,
                    args.repetitions,
                )
                if model_child.status != "passed":
                    for record in model_records:
                        record["status"] = "failed"
                        record["failure_class"] = model_child.failure_class
                results.extend(model_records)
                accounting_charged += model_charged

    result_order = {case: index for index, case in enumerate(CASE_ORDER)}
    results.sort(key=lambda result: (result_order[str(result["case_id"])], int(result["repetition"])))
    expected_results = len(selected) * args.repetitions
    status = (
        "passed"
        if len(results) == expected_results and all(result.get("status") == "passed" for result in results)
        else "failed"
    )
    manifest = safe_manifest(
        status=status,
        started_at=started_at,
        finished_at=utc_now(),
        duration_ms=int((time.monotonic() - started) * 1000),
        identity=identity,
        selected=selected,
        repetitions=args.repetitions,
        timeout_secs=args.timeout_secs,
        max_cost_microusd=max_cost_microusd,
        accounting_charged_microusd=accounting_charged,
        results=results,
        artifact_policy=artifact_policy,
    )
    manifest_path = output_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output_dir / "summary.md").write_text(render_summary(manifest), encoding="utf-8")
    (output_dir / "manifest.sha256").write_text(
        f"{SUPPORT.sha256_file(manifest_path)}  manifest.json\n",
        encoding="utf-8",
    )
    print(f"real-provider dogfood: {status}")
    print(f"manifest SHA-256: {SUPPORT.sha256_file(manifest_path)}")
    print(f"evidence: {output_dir}")
    return 0 if status == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
