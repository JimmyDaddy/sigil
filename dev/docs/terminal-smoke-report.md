# Terminal Smoke Report

Date: 2026-06-13
Workspace: `/Users/jimmydaddy/study/turbods`
Command: `cargo run -p sigil-cli -- doctor`

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
