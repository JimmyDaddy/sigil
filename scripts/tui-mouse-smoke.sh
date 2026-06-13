#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/tui-mouse-smoke.sh [--no-launch] [--skip-doctor] [--log-dir <dir>] [--report <path>]

Run a real-terminal Sigil TUI mouse smoke session.

The script captures terminal diagnostics, optionally launches sigil-tui, then
prompts for pass/fail/skip results and writes a Markdown report. It is intended
for local terminal profiles, tmux/screen, SSH, and clipboard bridge checks.

Options:
  --no-launch     Do not start sigil-tui; only capture diagnostics and prompt.
  --skip-doctor   Do not run cargo run -p sigil-cli -- doctor.
  --log-dir DIR   Directory for generated logs and reports.
  --report PATH   Markdown report path.
  -h, --help      Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

timestamp="$(date +%Y%m%d-%H%M%S)"
log_dir="${SIGIL_TUI_MOUSE_SMOKE_DIR:-.repo-local-dev/terminal-smoke}"
report_path=""
launch_tui=1
run_doctor=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-launch)
      launch_tui=0
      shift
      ;;
    --skip-doctor)
      run_doctor=0
      shift
      ;;
    --log-dir)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --log-dir" >&2
        exit 2
      fi
      log_dir="$2"
      shift 2
      ;;
    --report)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --report" >&2
        exit 2
      fi
      report_path="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

mkdir -p "${log_dir}"

if [[ -z "${report_path}" ]]; then
  report_path="${log_dir}/tui-mouse-smoke-${timestamp}.md"
fi
mkdir -p "$(dirname "${report_path}")"

doctor_log="${log_dir}/tui-mouse-smoke-${timestamp}-doctor.log"
tui_status="not launched"
doctor_status="skipped"

env_value() {
  local name="$1"
  printf '%s' "${!name-}"
}

prompt_text() {
  local prompt="$1"
  local value=""
  if [[ -t 0 ]]; then
    printf '%s' "${prompt}" >&2
    IFS= read -r value || value=""
  else
    value="not recorded"
  fi
  printf '%s' "${value}"
}

prompt_status() {
  local label="$1"
  local status=""
  if [[ -t 0 ]]; then
    while true; do
      printf '%s [p/f/s]: ' "${label}" >&2
      IFS= read -r status || status=""
      case "${status}" in
        p | P | pass | PASS)
          printf 'pass'
          return
          ;;
        f | F | fail | FAIL)
          printf 'fail'
          return
          ;;
        s | S | skip | SKIP | '')
          printf 'skip'
          return
          ;;
        *)
          echo "enter p, f, or s" >&2
          ;;
      esac
    done
  else
    printf 'skip'
  fi
}

markdown_escape() {
  local value="$1"
  value="${value//|/\\|}"
  value="${value//$'\n'/ }"
  printf '%s' "${value}"
}

if [[ "${run_doctor}" -eq 1 ]]; then
  echo "running doctor; raw log: ${doctor_log}"
  set +e
  cargo run -p sigil-cli -- doctor 2>&1 | tee "${doctor_log}"
  doctor_status="${PIPESTATUS[0]}"
  set -e
fi

cat <<'INSTRUCTIONS'

Real TUI mouse smoke steps:

1. Confirm /doctor terminal rows are understandable.
2. Click the composer and place the cursor.
3. Open /, click a slash command candidate, then close the selector.
4. Scroll transcript with the mouse wheel.
5. Open /config, click section rows and boolean/value fields.
6. Open /resume when sessions exist; click once to select and again to confirm.
7. Trigger an approval modal; click file rows, diff controls, allow, and deny.
8. Click tool activity body to focus it, then click its header to expand/collapse.
9. Move across clickable surfaces and check hover visual state.
10. Drag transcript text by displayed columns, including wide text when available.
11. Press Ctrl-C and paste elsewhere to verify OSC52 copy status.
12. Adjust Terminal scroll sensitivity if the wheel feels too fast or too slow.

Exit the TUI when finished; the script will ask for results.

INSTRUCTIONS

if [[ "${launch_tui}" -eq 1 ]]; then
  set +e
  cargo run -p sigil-tui
  tui_exit="$?"
  set -e
  tui_status="exit ${tui_exit}"
fi

echo
echo "Record smoke results. Use p=pass, f=fail, s=skip."

check_labels=(
  "Doctor terminal rows"
  "Composer click positions cursor"
  "Slash candidate click"
  "Transcript wheel scroll"
  "Config/setup row click"
  "Session selector click/confirm"
  "Approval modal controls"
  "Tool activity body focus"
  "Tool card header expand/collapse"
  "Hover visual state"
  "Column text selection"
  "OSC52 Ctrl-C copy"
  "Scroll sensitivity feels correct"
)

check_statuses=()
check_notes=()

for label in "${check_labels[@]}"; do
  status="$(prompt_status "${label}")"
  note="$(prompt_text "  note: ")"
  check_statuses+=("${status}")
  check_notes+=("${note}")
done

overall="$(prompt_text "Overall notes: ")"

{
  echo "# Sigil TUI Mouse Smoke Report"
  echo
  echo "Date: $(date)"
  echo "Workspace: \`${repo_root}\`"
  echo "Report: \`${report_path}\`"
  echo "Doctor log: \`${doctor_log}\`"
  echo "Doctor status: \`${doctor_status}\`"
  echo "TUI status: \`${tui_status}\`"
  echo
  echo "## Terminal"
  echo
  echo "| Field | Value |"
  echo "| --- | --- |"
  echo "| TERM | \`$(markdown_escape "$(env_value TERM)")\` |"
  echo "| TERM_PROGRAM | \`$(markdown_escape "$(env_value TERM_PROGRAM)")\` |"
  echo "| TERM_PROGRAM_VERSION | \`$(markdown_escape "$(env_value TERM_PROGRAM_VERSION)")\` |"
  echo "| COLORTERM | \`$(markdown_escape "$(env_value COLORTERM)")\` |"
  echo "| TMUX | \`$(markdown_escape "$(env_value TMUX)")\` |"
  echo "| STY | \`$(markdown_escape "$(env_value STY)")\` |"
  echo "| SSH_TTY | \`$(markdown_escape "$(env_value SSH_TTY)")\` |"
  echo "| WSL_DISTRO_NAME | \`$(markdown_escape "$(env_value WSL_DISTRO_NAME)")\` |"
  echo
  echo "## Checklist"
  echo
  echo "| Check | Result | Notes |"
  echo "| --- | --- | --- |"
  for i in "${!check_labels[@]}"; do
    echo "| $(markdown_escape "${check_labels[$i]}") | $(markdown_escape "${check_statuses[$i]}") | $(markdown_escape "${check_notes[$i]}") |"
  done
  echo
  echo "## Overall Notes"
  echo
  echo "$(markdown_escape "${overall}")"
} >"${report_path}"

echo "wrote ${report_path}"
