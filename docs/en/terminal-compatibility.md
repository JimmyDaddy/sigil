<!-- public-doc-role: terminal-compatibility; authority: terminal-smoke-authority; sections: baseline,attention-notification-smoke,mouse-smoke,text-selection-and-copy,image-paste-smoke,tmux-screen-ssh-and-wsl,result-template; cta: open-troubleshooting -->

# Terminal Compatibility Checklist

[Docs home](README.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/terminal-compatibility.md)

Terminal, multiplexer, remote-shell, and desktop settings can block keys, mouse input, clipboard sequences, images, or notifications outside Sigil. Start with `sigil doctor`; use `scripts/tui-mouse-smoke.sh` when you want a saved local report.

## Baseline

1. Find the active user `sigil.toml` with Doctor or [Configuration](configuration.md#resolution-order).
2. Keep `keyboard_enhancement = "auto"`, `mouse_capture = true`, `osc52_clipboard = true`, and `scroll_sensitivity = 3` unless a test below fails; edit these `[terminal]` fields in TOML and restart Sigil.
3. Keep notifications off unless you need an out-of-focus signal; notification fields can also be changed in `/config` → **Terminal**.
4. Confirm ordinary text input, transcript scrolling, `Esc`, and `Ctrl-C` work before testing optional features.

On Windows, run a harmless `Write-Output 'hello'` and `exit 7`; the activity should show the actual shell, UTF-8 output, and exit code. Local execution is not an OS sandbox.

## Attention Notification Smoke

Temporarily enable notifications and set the long-run threshold to `1000` ms. Start a run longer than one second, move focus away, and expect one fixed completion signal. Approval and MCP input requests can notify without the long-run threshold. If tmux or screen exposes control text or ignores the signal, try `bell` or disable notifications.

For real-binary default-off and BEL checks, run `scripts/tui-attention-signals-pty-acceptance.py`.

## Mouse Smoke

Restart after changing mouse capture, then verify:

1. click and type in the composer;
2. open `/` and click a command candidate;
3. scroll the transcript;
4. change a `/config` field;
5. open `/resume`, select a row, and open Session Actions by right-click and by `Ctrl-O`;
6. use approval file, diff, allow, and deny controls.

Clicks and wheel input should affect only the focused surface; keyboard controls must remain available.

## Text Selection And Copy

Drag-select single-line, multiline, and wide-character transcript text, press `Ctrl-C`, and paste elsewhere. Confirm `Ctrl-L` copies an active selection too. Then click outside the transcript selection and press `Ctrl-L` again; it should copy the latest assistant reply. Every copy should exclude the right info rail. If OSC52 is disabled or blocked, Sigil reports that the clipboard is unavailable.

## Image Paste Smoke

With a recognized image-capable OpenAI Responses, Anthropic, or Gemini model:

1. copy a PNG and press `Ctrl-V` from an idle composer;
2. confirm a metadata chip appears without the local path;
3. select and remove the chip;
4. paste a local PNG, JPEG, or WebP path;
5. submit an image-only or image-plus-text turn.

An unsupported model must keep the draft and reject the image before sending. Remote layers may not expose the host image clipboard; paste a local path instead.

## tmux, screen, SSH, And WSL

Repeat `/doctor`, mouse, and copy checks inside each layer. If keys break, set `keyboard_enhancement = "off"` and restart. If mouse input breaks, set `mouse_capture = false` and restart. If copy is blocked or visible control text appears, set `osc52_clipboard = false`.

## Result Template

```text
Terminal / TERM:
Layers: none / tmux / screen / SSH / WSL
keyboard_enhancement / mouse_capture / osc52_clipboard:
notifications method / threshold:
Mouse smoke:
Selection copy / latest-response copy:
Image paste:
Notes:
```

<!-- public-doc-cta: open-troubleshooting -->
Next: [Continue with Troubleshooting](troubleshooting.md).
