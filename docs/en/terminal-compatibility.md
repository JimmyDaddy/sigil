# Terminal Compatibility Checklist

[简体中文](../zh-CN/terminal-compatibility.md)

This checklist helps verify Sigil mouse capture and OSC52 clipboard behavior in real terminals. It is intentionally manual because terminal multiplexers, remote shells, and desktop terminal preferences can block features outside Sigil's process.

Start with diagnostics:

```bash
cargo run -p sigil-cli -- doctor
```

Inside the TUI, run `/doctor` to see the same terminal checks in the transcript. The report reads `[terminal].mouse_capture`, `[terminal].osc52_clipboard`, `TERM`, common terminal profile variables, tmux/screen, SSH, WSL, and clipboard bridge risk.

## Baseline

1. Confirm `/doctor` reports `terminal`, `terminal:config`, `terminal:mouse`, and `terminal:clipboard`.
2. Open `/config` and review the `Terminal` section.
3. Keep `mouse_capture = true` unless the terminal or multiplexer mishandles mouse mode.
4. Keep `osc52_clipboard = true` unless copy sequences are blocked or printed visibly.

## Mouse Smoke

Run these checks in iTerm2, Terminal.app, WezTerm, kitty, and any terminal profile you support:

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
4. If mouse events are broken, set `[terminal].mouse_capture = false` and restart the TUI.
5. If copy is blocked or control sequences are visible, set `[terminal].osc52_clipboard = false`.

`mouse_capture` applies on the next launch. `osc52_clipboard` is checked on each copy action.

## Result Template

```text
Terminal:
TERM:
Layers: none / tmux / screen / SSH / WSL
mouse_capture:
osc52_clipboard:
Doctor terminal status:
Mouse smoke:
Text selection:
OSC52 copy:
Notes:
```
