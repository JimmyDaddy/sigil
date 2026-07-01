# Terminal Compatibility Checklist

[Docs home](README.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/terminal-compatibility.md)

This checklist helps verify Sigil mouse capture and OSC52 clipboard behavior in real terminals. It is intentionally manual because terminal multiplexers, remote shells, and desktop terminal preferences can block features outside Sigil's process.

Start with diagnostics:

```bash
sigil doctor
```

Inside the TUI, run `/doctor` to see the same terminal checks in the transcript. The report reads `[terminal].mouse_capture`, `[terminal].osc52_clipboard`, `[terminal].scroll_sensitivity`, `TERM`, common terminal profile variables, tmux/screen, SSH, WSL, and clipboard bridge risk.

For a repeatable local run that captures `/doctor`, launches the real TUI, prompts for pass/fail/skip results, and writes a Markdown report, use:

```bash
scripts/tui-mouse-smoke.sh
```

## Baseline

1. Confirm `/doctor` reports `terminal`, `terminal:config`, `terminal:mouse`, and `terminal:clipboard`.
2. Open `/config` and review the `Terminal` section.
3. Keep `keyboard_enhancement = "auto"` unless you need to force `on` for a known-good profile or force `off` for a broken terminal layer.
4. Keep `mouse_capture = false` unless you need mouse support and the terminal or multiplexer handles mouse mode well.
5. Keep `osc52_clipboard = true` unless copy sequences are blocked or printed visibly.
6. Keep `scroll_sensitivity = 3` unless the mouse wheel feels too fast or too slow in transcript and approval diff views.

## Mouse Smoke

Temporarily set `[terminal].mouse_capture = true` in `sigil.toml`, restart the TUI, then run these checks in iTerm2, Terminal.app, WezTerm, kitty, and any terminal profile you support:

1. Click the composer and type a short prompt.
2. Open `/`, click a slash command candidate, then press `Esc`.
3. Scroll the transcript with the mouse wheel.
4. Open `/config`, click a section, click a boolean field, and confirm the focus changes.
5. Open `/resume` when sessions exist, click a candidate once to select it and again to confirm.
6. When an approval modal appears, click file rows, diff controls, and allow/deny actions.

Expected result: clicks and wheel events affect only the focused TUI surface. Keyboard controls still work at every step.

## Text Selection And Copy

1. Drag across visible transcript text.
2. Include at least one short single-line selection and one multi-line selection.
3. Include CJK or wide characters if the transcript contains them.
4. Press `Ctrl-C`.
5. Paste into another application or shell prompt.

Expected result: Sigil shows `copied ...` when OSC52 is enabled and the terminal accepts the sequence. If OSC52 is disabled in config, Sigil shows `clipboard unavailable: OSC52 disabled`.

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
Doctor terminal status:
Mouse smoke:
Text selection:
OSC52 copy:
Notes:
```
