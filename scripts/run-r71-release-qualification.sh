#!/usr/bin/env bash
# RFC-0071 R71.8: the only release qualification aggregator.
# It requires an exact clean candidate checkout, runs every fixed gate in order, and emits
# bounded JSON evidence. A platform job may run the same wrapper with --suite platform.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-release-qualification.sh \
  --candidate-sha SHA --base-sha SHA \
  [--platform current|macos|linux|windows|docker|toolchain] \
  [--backend auto|seatbelt|bubblewrap|windows-restricted|docker|toolchain-offline] \
  [--required] [--suite full|platform]
EOF
}

candidate_sha=""
base_sha=""
platform="current"
backend="auto"
required=0
suite="full"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate-sha) candidate_sha="${2:-}"; shift 2 ;;
    --base-sha) base_sha="${2:-}"; shift 2 ;;
    --platform) platform="${2:-}"; shift 2 ;;
    --backend) backend="${2:-}"; shift 2 ;;
    --required) required=1; shift ;;
    --suite) suite="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "$candidate_sha" && -n "$base_sha" ]] || { usage >&2; exit 2; }
[[ "$suite" == "full" || "$suite" == "platform" ]] || { echo "invalid --suite" >&2; exit 2; }
[[ "$platform" =~ ^(current|macos|linux|windows|docker|toolchain)$ ]] || {
  echo "invalid --platform" >&2
  exit 2
}
[[ "$backend" =~ ^(auto|seatbelt|bubblewrap|windows-restricted|docker|toolchain-offline)$ ]] || {
  echo "invalid --backend" >&2
  exit 2
}
if [[ "$required" != "1" ]]; then
  echo "R71.8 qualification requires --required; diagnostic-only jobs cannot pass release qualification" >&2
  exit 2
fi

case "$platform" in
  current)
    case "$(uname -s)" in
      Darwin) platform="macos" ;;
      Linux) platform="linux" ;;
      MINGW*|MSYS*|CYGWIN*) platform="windows" ;;
      *) echo "unsupported current host: $(uname -s)" >&2; exit 1 ;;
    esac
    ;;
esac

if [[ "$backend" == "auto" ]]; then
  case "$platform" in
    macos) backend="seatbelt" ;;
    linux) backend="bubblewrap" ;;
    windows) backend="windows-restricted" ;;
    docker) backend="docker" ;;
    toolchain) backend="toolchain-offline" ;;
  esac
fi

case "$platform:$backend" in
  macos:seatbelt|linux:bubblewrap|windows:windows-restricted|docker:docker|toolchain:toolchain-offline) ;;
  *) echo "platform/backend pair is not declared by R71.8: $platform/$backend" >&2; exit 2 ;;
esac

# Every qualification step runs under a newly-created HOME. This keeps the authority bootstrap
# namespace, config, state and cache writes out of the operator's real user directory. The marker
# is checked before cleanup so a future edit cannot accidentally turn this into broad deletion.
qualification_original_home="${HOME:-}"
[[ -n "$qualification_original_home" ]] || { echo "HOME is required for qualification" >&2; exit 1; }
qualification_original_userprofile="${USERPROFILE:-}"
qualification_rustup_home="${RUSTUP_HOME:-$qualification_original_home/.rustup}"
qualification_cargo_home="${CARGO_HOME:-$qualification_original_home/.cargo}"
qualification_corepack_home="${COREPACK_HOME:-$qualification_original_home/.cache/node/corepack}"
qualification_windows_vctip_baseline=""
qualification_home_windows=""
if [[ "$platform" == "windows" ]]; then
  command -v powershell.exe >/dev/null || {
    echo "powershell.exe is required for Windows qualification cleanup" >&2
    exit 1
  }
  command -v cygpath >/dev/null || {
    echo "cygpath is required to bind the Windows qualification HOME" >&2
    exit 1
  }
fi
if [[ "$platform" == "windows" && "${GITHUB_ACTIONS:-}" == "true" ]]; then
  qualification_windows_vctip_baseline="$(
    # shellcheck disable=SC2016 # This single-quoted argument is PowerShell source; Bash must not expand it.
    powershell.exe -NoLogo -NoProfile -NonInteractive -Command \
      '$ErrorActionPreference = "Stop"; @(Get-Process -Name vctip -ErrorAction SilentlyContinue | ForEach-Object { "{0}:{1}" -f $_.Id, $_.StartTime.ToUniversalTime().Ticks }) -join ","' \
      | tr -d '\r\n'
  )"
fi
export SIGIL_R71_QUALIFICATION_VCTIP_BASELINE="$qualification_windows_vctip_baseline"
qualification_home="$(mktemp -d -t sigil-r71-qualification-home-XXXXXX)"
qualification_home_marker="$qualification_home/.sigil-r71-qualification-home"
touch "$qualification_home_marker"
if [[ "$platform" == "windows" ]]; then
  qualification_home_windows="$(cygpath -w "$qualification_home" | tr -d '\r\n')"
  [[ -n "$qualification_home_windows" ]] || {
    echo "failed to convert qualification HOME to a Windows path" >&2
    exit 1
  }
  export SIGIL_R71_QUALIFICATION_HOME_WINDOWS="$qualification_home_windows"
fi
qualification_home_cleaned=0
settle_qualification_windows_helpers() {
  if [[ "$platform" != "windows" || "${GITHUB_ACTIONS:-}" != "true" ]]; then
    return 0
  fi
  # The hosted runner otherwise reaps vctip only after this step and after our EXIT trap. Stop only
  # helper process identities that appeared after this qualification began; never target a
  # pre-existing process.
  # shellcheck disable=SC2016 # This single-quoted argument is PowerShell source; Bash must not expand it.
  powershell.exe -NoLogo -NoProfile -NonInteractive -Command \
    '$ErrorActionPreference = "Stop"; $baseline = @(); if ($env:SIGIL_R71_QUALIFICATION_VCTIP_BASELINE) { $baseline = @($env:SIGIL_R71_QUALIFICATION_VCTIP_BASELINE -split "," | Where-Object { $_ -ne "" }) }; $targets = @(Get-Process -Name vctip -ErrorAction SilentlyContinue | ForEach-Object { $identity = "{0}:{1}" -f $_.Id, $_.StartTime.ToUniversalTime().Ticks; if ($baseline -notcontains $identity) { $_ } }); if ($targets.Count -gt 0) { $targets | Stop-Process -Force -PassThru -ErrorAction Stop | Wait-Process -Timeout 10 -ErrorAction Stop }'
}
cleanup_qualification_home() {
  if [[ "$qualification_home_cleaned" == "1" ]]; then
    return
  fi
  if [[ ! -f "$qualification_home_marker" || -L "$qualification_home_marker" ]]; then
    echo "refusing qualification HOME cleanup without ownership marker" >&2
    return 1
  fi
  export HOME="$qualification_original_home"
  if [[ -n "$qualification_original_userprofile" ]]; then
    export USERPROFILE="$qualification_original_userprofile"
  else
    unset USERPROFILE
  fi
  if ! settle_qualification_windows_helpers; then
    echo "failed to settle Windows qualification helpers" >&2
    return 1
  fi
  if ! cleanup_windows_qualification_home; then
    echo "failed to remove Windows qualification HOME with the bounded PowerShell cleanup" >&2
    return 1
  fi
  # Windows may create and remove INetCache compatibility directories while `find` is walking
  # the isolated profile. Retry the same marker-bounded cleanup so a disappeared child is not
  # mistaken for a qualification failure; the exact root must still be gone after the retries.
  for _qualification_cleanup_pass in 1 2 3; do
    if [[ -e "$qualification_home" ]]; then
      find "$qualification_home" -depth \( -type f -o -type l -o -type p -o -type s \) -delete 2>/dev/null || true
      find "$qualification_home" -depth -type d -empty -delete 2>/dev/null || true
    fi
    if [[ ! -e "$qualification_home" ]]; then
      break
    fi
  done
  if [[ ! -e "$qualification_home" ]]; then
    qualification_home_cleaned=1
  else
    echo "qualification HOME cleanup left unexpected residue: $qualification_home" >&2
    find "$qualification_home" -mindepth 1 -maxdepth 8 -print 2>/dev/null \
      | LC_ALL=C sort \
      | head -50 >&2 || true
    return 1
  fi
}
cleanup_windows_qualification_home() {
  if [[ "$platform" != "windows" ]]; then
    return 0
  fi
  # Git Bash `find -delete` does not reliably remove the empty Windows compatibility-directory
  # chain created under INetCache. Use PowerShell literal paths, but keep the same exact marker
  # ownership proof and refuse to continue if a later pass no longer has that proof.
  # shellcheck disable=SC2016 # This single-quoted argument is PowerShell source; Bash must not expand it.
  powershell.exe -NoLogo -NoProfile -NonInteractive -Command \
    '$ErrorActionPreference = "Stop";
      $root = [IO.Path]::GetFullPath($env:SIGIL_R71_QUALIFICATION_HOME_WINDOWS);
      $marker = Join-Path $root ".sigil-r71-qualification-home";
      function Assert-QualificationHomeMarker {
        $item = Get-Item -LiteralPath $marker -Force -ErrorAction Stop;
        if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
          throw "qualification HOME ownership marker is not a regular file: $marker";
        }
      }
      if (-not (Test-Path -LiteralPath $root -PathType Any)) { exit 0 }
      Assert-QualificationHomeMarker;
      $lastError = $null;
      for ($attempt = 0; $attempt -lt 3; $attempt++) {
        try {
          @(Get-ChildItem -LiteralPath $root -Force -ErrorAction Stop |
            Where-Object { $_.FullName -ne $marker } |
            Sort-Object FullName -Descending) |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -Recurse -ErrorAction Stop };
          if (-not (Test-Path -LiteralPath $root -PathType Any)) { exit 0 }
          Assert-QualificationHomeMarker;
          Remove-Item -LiteralPath $root -Force -Recurse -ErrorAction Stop;
          if (-not (Test-Path -LiteralPath $root -PathType Any)) { exit 0 }
        } catch {
          $lastError = $_;
        }
        Start-Sleep -Milliseconds 250;
      }
      if ($lastError) { throw $lastError }
      throw "qualification HOME cleanup left Windows residue: $root"'
}
trap cleanup_qualification_home EXIT
export HOME="$qualification_home"
export USERPROFILE="$qualification_home"
export RUSTUP_HOME="$qualification_rustup_home"
export CARGO_HOME="$qualification_cargo_home"
export COREPACK_HOME="$qualification_corepack_home"
# VsDevCmd starts the vctip telemetry helper on Windows unless explicitly disabled. The helper can
# outlive Cargo and keep the isolated qualification profile active after every test has passed.
# Prevent it at process creation instead of killing an unowned machine-global process in cleanup.
export VSCMD_SKIP_SENDTELEMETRY=1
# Some product-surface fixtures intentionally exercise process-global environment seams. Keep the
# required release qualification deterministic while the tests share one isolated HOME.
export RUST_TEST_THREADS=1
export XDG_CONFIG_HOME="$qualification_home/.config"
export XDG_STATE_HOME="$qualification_home/.local/state"
export XDG_CACHE_HOME="$qualification_home/.cache"
export SIGIL_R71_BOOTSTRAP_ISOLATED=1
export SIGIL_R71_BOOTSTRAP_RETENTION_POLICY="exact-temp-home-with-marker"
export SIGIL_R71_QUALIFICATION_HOME="$qualification_home"

evidence_dir="${SIGIL_R71_EVIDENCE_DIR:-$(mktemp -d -t sigil-r71-release-XXXXXX)}"
mkdir -p "$evidence_dir/logs"
steps_file="$evidence_dir/steps.tsv"
: > "$steps_file"

identity_json="$(python3 - "$ROOT" "$candidate_sha" "$base_sha" <<'PY'
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import (
    load_conformance_manifest,
    load_platform_manifest,
    manifest_digests,
    validate_git_identity,
)

root = Path(sys.argv[1])
identity = validate_git_identity(root, sys.argv[2], sys.argv[3])
conformance = load_conformance_manifest()
platform_manifest = load_platform_manifest()
identity.update(manifest_digests())
identity["required_fault_case_count"] = len(conformance["cases"])
identity["required_platform_job_count"] = len(platform_manifest["jobs"])
print(json.dumps(identity, sort_keys=True))
PY
)"

python3 - "$ROOT" "$platform" "$backend" <<'PY'
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import load_platform_manifest

platform, backend = sys.argv[2:]
expected = {
    ("macos", "seatbelt"): "macos-seatbelt",
    ("linux", "bubblewrap"): "linux-bubblewrap",
    ("windows", "windows-restricted"): "windows-restricted",
    ("docker", "docker"): "docker-declared",
    ("toolchain", "toolchain-offline"): "toolchain-offline",
}[(platform, backend)]
jobs = load_platform_manifest()["jobs"]
job = next(item for item in jobs if item["job_id"] == expected)
expected_host_platform = "linux" if platform in {"docker", "toolchain"} else platform
if job["platform"] != expected_host_platform or job["backend"] != backend:
    raise SystemExit(f"platform manifest drift for {expected}")
print(f"R71 platform manifest: {expected} required")
PY

finalize() {
  local result="$1"
  python3 - "$ROOT" "$evidence_dir" "$identity_json" "$suite" "$platform" "$backend" "$result" "$steps_file" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import sha256_file, write_evidence

evidence_dir = Path(sys.argv[2])
identity = json.loads(sys.argv[3])
qualification_home = Path(os.environ["SIGIL_R71_QUALIFICATION_HOME"])
qualification_bootstrap_root = qualification_home / ".sigil" / "authority-bootstrap-v1"
if qualification_bootstrap_root.is_dir():
    bootstrap_residue_count = sum(
        1 for entry in qualification_bootstrap_root.iterdir() if entry.is_dir()
    )
else:
    bootstrap_residue_count = 0
steps = []
for line in Path(sys.argv[8]).read_text(encoding="utf-8").splitlines():
    label, status, log_path = line.split("\t", 2)
    log = Path(log_path)
    steps.append({"command_id": label, "status": status, "log_sha256": sha256_file(log)})
payload = {
    "schema": "r71-release-qualification-evidence-v1",
    **identity,
    "suite": sys.argv[4],
    "platform": sys.argv[5],
    "backend": sys.argv[6],
    "result": sys.argv[7],
    "bootstrap_root_isolated": os.environ.get("SIGIL_R71_BOOTSTRAP_ISOLATED") == "1",
    "bootstrap_root_retention_policy": os.environ.get("SIGIL_R71_BOOTSTRAP_RETENTION_POLICY"),
    "bootstrap_root_residue_count_before_cleanup": bootstrap_residue_count,
    "test_thread_policy": "single-threaded-process-global-fixtures",
    "steps": steps,
    "finished_at": datetime.now(timezone.utc).isoformat(),
}
write_evidence(evidence_dir / "qualification.json", payload)
print(json.dumps(payload, sort_keys=True))
PY
}

run_step() {
  local label="$1"
  shift
  local safe_label="${label//[^A-Za-z0-9_.-]/_}"
  local log_path="$evidence_dir/logs/${safe_label}.log"
  echo "RUN($label): $*"
  if "$@" >"$log_path" 2>&1; then
    printf '%s\tpassed\t%s\n' "$label" "$log_path" >> "$steps_file"
    echo "PASS($label)"
  else
    local status=$?
    printf '%s\tfailed\t%s\n' "$label" "$log_path" >> "$steps_file"
    echo "FAIL($label): exit $status" >&2
    tail -40 "$log_path" >&2 || true
    finalize failed >/dev/null || true
    exit "$status"
  fi
}

platform_gate() {
  case "$platform:$backend" in
    macos:seatbelt)
      [[ "$(uname -s)" == "Darwin" ]] || { echo "Seatbelt required on macOS" >&2; return 1; }
      run_step macos-seatbelt-unit cargo test --locked -p sigil-tools-builtin --lib sandbox_conformance_macos_seatbelt -- --format terse
      run_step macos-seatbelt-real cargo test --locked -p sigil-tools-builtin --lib macos_seatbelt_execution_backend_allows_workspace_write_and_denies_external_write -- --exact --format terse
      ;;
    linux:bubblewrap)
      [[ "$(uname -s)" == "Linux" ]] || { echo "Bubblewrap required on Linux" >&2; return 1; }
      command -v bwrap >/dev/null || { echo "bwrap is required for Linux qualification" >&2; return 1; }
      run_step linux-bubblewrap-real cargo test --locked -p sigil-tools-builtin --lib linux_bubblewrap_execution_backend_real_conformance -- --ignored --exact --nocapture
      ;;
    windows:windows-restricted)
      case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "Windows restricted qualification requires Windows" >&2; return 1 ;; esac
      run_step windows-restricted-tests cargo test --locked -p sigil-tools-builtin --lib restricted_launch_probe -- --format terse
      ;;
    docker:docker)
      command -v docker >/dev/null || { echo "docker is required for declared Docker qualification" >&2; return 1; }
      [[ -n "${SIGIL_DOCKER_CONFORMANCE_IMAGE:-}" ]] || { echo "SIGIL_DOCKER_CONFORMANCE_IMAGE is required" >&2; return 1; }
      run_step docker-real env SIGIL_DOCKER_CONFORMANCE_IMAGE="$SIGIL_DOCKER_CONFORMANCE_IMAGE" cargo test --locked -p sigil-tools-builtin --lib docker_execution_backend_real_daemon_conformance -- --ignored --exact --nocapture
      ;;
    toolchain:toolchain-offline)
      run_step toolchain-offline bash scripts/run-r71-toolchain-conformance.sh --candidate-sha "$candidate_sha" --base-sha "$base_sha"
      ;;
  esac
}

if [[ "$suite" == "platform" ]]; then
  platform_gate
else
  run_step inventory-process bash scripts/check-local-process-inventory.sh --mode enforce
  run_step inventory-producer bash scripts/check-local-resource-producer-inventory.sh --mode enforce
  run_step shipping-targets bash scripts/check-shipping-targets.sh --mode enforce
  run_step negative-dependencies bash scripts/check-r71-negative-dependencies.sh
  run_step touched-full bash scripts/check-touched.sh --scope base --base "$base_sha" --tier full

  for package in sigil-kernel sigil-resource-authority sigil-sandbox sigil-process-observer sigil-tools-builtin sigil-runtime sigil-mcp; do
    run_step "targeted-$package" cargo test --locked -p "$package"
  done
  run_step cross-surface-tui cargo test --locked -p sigil-tui-app
  run_step cross-surface-http cargo test --locked -p sigil-http
  run_step cross-surface-desktop cargo test --locked -p sigil-desktop
  run_step cross-surface-cli cargo test --locked -p sigil
  run_step tui-shipping-e2e bash scripts/run-r71-tui-shipping-e2e.sh
  run_step desktop-contract bash scripts/generate-desktop-contract.sh --check
  run_step desktop-renderer-check pnpm --dir apps/desktop check

  run_step characterization bash scripts/run-r71-characterization.sh
  run_step contract-goldens bash scripts/check-r71-contract-goldens.sh
  run_step authority-conformance bash scripts/run-r71-authority-conformance.sh
  run_step sandbox-conformance bash scripts/run-r71-sandbox-conformance.sh
  run_step toolchain-conformance bash scripts/run-r71-toolchain-conformance.sh --candidate-sha "$candidate_sha" --base-sha "$base_sha"
  run_step consumer-conformance bash scripts/run-r71-consumer-conformance.sh --all --epoch current
  run_step fault-campaign bash scripts/run-r71-fault-campaign.sh
  run_step surface-conformance bash scripts/run-r71-surface-conformance.sh --epoch current
  run_step global-cutover-conformance bash scripts/run-r71-global-cutover-conformance.sh --epoch current
  platform_gate
fi

finalize passed
echo "r71-release-qualification: passed suite=$suite platform=$platform backend=$backend"
