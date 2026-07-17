# Terminal Compatibility Checklist

[Docs home](README.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/terminal-compatibility.md)

This checklist helps verify Sigil mouse capture, OSC52 clipboard behavior, and optional attention notifications in real terminals. Some checks remain manual because terminal multiplexers, remote shells, and desktop terminal preferences can block features outside Sigil's process.

Start with diagnostics:

```bash
sigil doctor
```

Inside the TUI, run `/doctor` to see the same terminal checks in the transcript. The report includes the resolved command shell, process-tree owner, unconfined local-backend boundary, configured notification switch, method, and threshold alongside mouse, clipboard, scroll, profile, tmux/screen, SSH, WSL, and clipboard bridge facts. It does not print notification payloads or raw environment values.

For a repeatable local run that captures `/doctor`, launches the real TUI, prompts for pass/fail/skip results, and writes a Markdown report, use:

```bash
scripts/tui-mouse-smoke.sh
```

## Baseline

1. Confirm `/doctor` reports `terminal`, `terminal:shell`, `terminal:process_owner`, `terminal:config`, `terminal:mouse`, and `terminal:clipboard`.
2. Open `/config` and review the `Terminal` section.
3. Keep `keyboard_enhancement = "auto"` unless you need to force `on` for a known-good profile or force `off` for a broken terminal layer.
4. Keep the default `mouse_capture = true` for mouse support; set it to `false` if the terminal or multiplexer mishandles mouse mode.
5. Keep `osc52_clipboard = true` unless copy sequences are blocked or printed visibly.
6. Keep `scroll_sensitivity = 3` unless the mouse wheel feels too fast or too slow in transcript and approval diff views.
7. Keep attention notifications off unless background or long-running work needs an out-of-focus signal. Prefer `method = "auto"`; use explicit `bell`, `osc9`, or `osc777` only when testing a known terminal profile.

On Windows, `terminal:shell` should report PowerShell and `terminal:process_owner` should report `windows_job_object`. Run a harmless `Write-Output '你好'` command and a non-zero `exit 7` command; the tool card should retain UTF-8 output, show the actual shell, and report the exit code. The Job Object is not a sandbox—`local_backend=unconfined` is expected.

## Attention Notification Smoke

Use `/config` → `Terminal` to enable notifications and temporarily set the long-run threshold to `1000` ms. Start a run that lasts longer than one second, then move focus away from the terminal.

- Expected: one fixed notification after completion. Approval and MCP input requests notify without using the long-run threshold.
- While Sigil is focused: terminals that report focus reliably suppress the notification. If no focus event has ever been received, Sigil does not pretend focus detection is reliable.
- Under tmux/screen: OSC methods use multiplexer pass-through. If the terminal exposes control text or ignores it, use `bell` or disable the feature.
- Privacy: notification text never includes the prompt, reply, path, tool/MCP name, arguments, error details, provider, or session id.

For deterministic real-binary verification of default-off and explicit BEL bytes, run:

```bash
scripts/tui-attention-signals-pty-acceptance.py
```

## Mouse Smoke

Confirm `/doctor` reports `mouse_capture=true` (or remove an explicit `false` override), restart the TUI, then run these checks in iTerm2, Terminal.app, WezTerm, kitty, and any terminal profile you support:

1. Click the composer and type a short prompt.
2. Open `/`, click a slash command candidate, then press `Esc`.
3. Scroll the transcript with the mouse wheel.
4. Open `/config`, click a section, click a boolean field, and confirm the focus changes.
5. Open `/resume` when sessions exist. Click a candidate once to select it, then right-click it to open Session Actions. Close the dialog, select it again, and press `Ctrl-O` to verify the keyboard path reaches the same exclusive dialog.
6. In Session Actions, use a harmless action such as safe export and confirm that typing does not reach the composer until the dialog closes.
7. When an approval modal appears, click file rows, diff controls, and allow/deny actions.

Expected result: clicks and wheel events affect only the focused TUI surface. Keyboard controls still work at every step.

## Text Selection And Copy

1. Drag across visible transcript text.
2. Include at least one short single-line selection and one multi-line selection.
3. Include CJK or wide characters if the transcript contains them.
4. Press `Ctrl-C`.
5. Paste into another application or shell prompt.

Expected result: Sigil shows `copied ...` when OSC52 is enabled and the terminal accepts the sequence. If OSC52 is disabled in config, Sigil shows `clipboard unavailable: OSC52 disabled`.

## Image Paste Smoke

Image input is separate from OSC52 text-selection copy. Configure an explicitly supported OpenAI Responses, Anthropic, or Gemini model, then run these checks from an idle Build composer:

1. Put a PNG image in the system clipboard and press `Ctrl-V`.
2. Confirm that a metadata chip appears above the composer without a local path.
3. Select the chip with `Up`, move with `Left/Right` when several chips exist, and remove it with `Backspace` or `Delete`.
4. Paste a local PNG, JPEG, or WebP path and confirm that it becomes a chip instead of prompt text.
5. Submit an image-only turn or add text and submit; unsupported model IDs must keep the draft and fail before provider transport.

tmux, screen, SSH, WSL, and remote terminal applications may not expose the host system's image clipboard to Sigil. In those environments, paste an admitted local file path instead. OSC52 is only Sigil's outbound text-selection copy mechanism; enabling it does not make clipboard image input available.

## tmux, screen, SSH, And WSL

These layers commonly require explicit clipboard or mouse pass-through:

1. Run `/doctor` inside the layer and read `terminal:mouse` / `terminal:clipboard`.
2. Repeat the mouse smoke checks inside the layer.
3. Repeat the copy check, then paste outside the layer.
4. If keyboard input appears stuck after launch, set `[terminal].keyboard_enhancement = "off"` and restart the TUI.
5. If mouse events are broken or scrolling feels heavy, set `[terminal].mouse_capture = false` and restart the TUI.
6. If copy is blocked or control sequences are visible, set `[terminal].osc52_clipboard = false`.

`keyboard_enhancement` is resolved on the next launch. `mouse_capture` applies on the next launch. `osc52_clipboard` is checked on each copy action. `scroll_sensitivity` applies after the saved config is reloaded.

## Result Template

```text
Terminal:
TERM:
Layers: none / tmux / screen / SSH / WSL
keyboard_enhancement:
mouse_capture:
osc52_clipboard:
scroll_sensitivity:
notifications enabled / method / threshold:
Long-run notification:
Focused suppression:
Doctor terminal status:
Mouse smoke:
Text selection:
OSC52 copy:
Image paste:
Notes:
```
