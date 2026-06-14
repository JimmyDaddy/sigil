# Terminal Smoke Report

Date: 2026-06-13
Workspace: `/Users/jimmydaddy/study/turbods`
Command: `sigil doctor`

## Repeatable Script

Use `scripts/tui-mouse-smoke.sh` for follow-up real TUI mouse smoke runs. The script captures terminal environment data, runs `/doctor` through `sigil doctor`, optionally launches the TUI, prompts for pass/fail/skip results, and writes a Markdown report under `.repo-local-dev/terminal-smoke/`.

## Installed For This Smoke

- WezTerm: `20240203-110809-5046fc22`
- kitty: `0.47.3`
- tmux: `3.6b`

## Results

| Environment | Invocation | Terminal result | Notes |
| --- | --- | --- | --- |
| Terminal.app | CuaDriver launched `.repo-local-dev/terminal-smoke/doctor-smoke.command` | `terminal`, `terminal:profile`, `terminal:mouse`, `terminal:clipboard` all `ok` | `TERM_PROGRAM=Apple_Terminal`, `TERM_PROGRAM_VERSION=466` |
| iTerm2 | Existing iTerm shell ran the smoke helper | `terminal`, `terminal:profile`, `terminal:mouse`, `terminal:clipboard` all `ok` | `TERM_PROGRAM=iTerm.app`, `TERM_PROGRAM_VERSION=3.6.10` |
| WezTerm | `wezterm start --always-new-process` | `terminal`, `terminal:profile`, `terminal:mouse`, `terminal:clipboard` all `ok` | Detected `profile=wezterm` |
| kitty | `kitty --config NONE --start-as=hidden` | `terminal`, `terminal:profile`, `terminal:mouse`, `terminal:clipboard` all `ok` | Detected `profile=kitty` via `TERM=xterm-kitty`; `TERM_PROGRAM` was inherited from parent iTerm |
| tmux | Detached `tmux` session | `terminal`, `terminal:profile`, `terminal:config` `ok`; mouse and clipboard `warn` | Expected multiplexer warning for mouse and OSC52 pass-through |
| screen | Detached `screen` session | `terminal`, `terminal:profile`, `terminal:config` `ok`; mouse and clipboard `warn` | Expected multiplexer warning for mouse and OSC52 pass-through |
| SSH localhost | `ssh -tt localhost` | `terminal`, `terminal:profile`, `terminal:mouse` `ok`; clipboard `warn` | Expected OSC52 bridge warning through SSH |
| WSL simulation | `WSL_DISTRO_NAME=Ubuntu` env simulation | `terminal`, `terminal:profile`, `terminal:mouse` `ok`; clipboard `warn` | macOS host cannot run real WSL; this only verifies Sigil's WSL diagnostic branch |

All runs reported overall `summary: warn` because the current workspace also warns about plaintext provider auth and missing `pyright-langserver`. No terminal-specific unexpected failure was found.

## TUI Mouse Smoke Follow-Up

Date: 2026-06-14
Command: `scripts/tui-mouse-smoke.sh`
Report: `.repo-local-dev/terminal-smoke/tui-mouse-smoke-20260614-073331.md`

| Environment | Result | Notes |
| --- | --- | --- |
| WezTerm | partial | Real TUI launched, `/doctor` baseline ran, and clicking the composer followed by typing input was verified by screenshot. Slash selector opened, but pixel click on a candidate did not select it because the WezTerm window was off the current Space for mouse-event delivery. |
| iTerm2 | assisted partial | A separate foreground/current-Space window launched the smoke script through clipboard paste and was closed after the run. Verified by screenshots: TUI launch, `/doctor` baseline, composer click plus keyboard input, slash candidate click opening `/config`, config section/tab click, tool activity/header click expanding structured payload, and visible transcript drag selection. CuaDriver could not complete the script's result prompts in iTerm because long text/status input was routed through AX text insertion rather than the terminal prompt. |

Outcome: this run produced real-terminal evidence for TUI launch, doctor baseline, composer mouse focus/input, slash candidate click, config selector click, tool activity/header click, and visible drag selection. Still open: true wheel-event sensitivity, session selector restore confirmation, approval modal controls, hover-only visual state, and OSC52 copy status confirmation. CuaDriver's `scroll` helper synthesizes keyboard scrolling rather than terminal wheel events, so wheel behavior still needs manual hardware or a lower-level wheel-event driver.

## Raw Logs

Raw logs are intentionally kept under ignored local development storage:

- `.repo-local-dev/terminal-smoke/manual-terminal.log`
- `.repo-local-dev/terminal-smoke/iterm.log`
- `.repo-local-dev/terminal-smoke/wezterm.log`
- `.repo-local-dev/terminal-smoke/kitty.log`
- `.repo-local-dev/terminal-smoke/tmux.log`
- `.repo-local-dev/terminal-smoke/screen.log`
- `.repo-local-dev/terminal-smoke/ssh.log`
- `.repo-local-dev/terminal-smoke/wsl-sim.log`
- `.repo-local-dev/terminal-smoke/tui-mouse-smoke-20260614-073331.md`
