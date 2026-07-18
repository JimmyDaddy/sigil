# Sigil 45-second Demo Script

This is the canonical short product demo for repository pages and launch posts.
It must use the real TUI and a disposable demo repository. Provider wait time
may be accelerated in editing, but product interactions and results must not be
mocked.

## Message

**One-line promise:** Reviewable edits. Resumable sessions. Repository work
inside one terminal.

The demo proves three things in order:

1. Sigil understands a real repository.
2. Risky edits stop for review with an inspectable diff.
3. Verification and session recovery remain visible after the edit.

## Recording Setup

- Record at 1920 × 1080, 30 or 60 fps.
- Use an iTerm2 window with the operator's normal profile and a large fixed 16:9
  viewport. Do not stretch the TUI across an oversized full-screen terminal grid.
- Hide the info rail in public footage so provider balance and session metadata do
  not appear. Keep manual permission mode visible in the approval surface.
- Use a disposable repository with a clean worktree and one intentionally stale README install command.
- Preconfigure the provider and model; do not show API keys, usernames, home-directory paths, notifications, or unrelated tabs.
- Keep the mouse pointer outside the terminal unless a click is part of the shot.
- Record the complete real run first. Remove dead time in editing and speed up provider waits, but never alter the displayed result.

## Timeline

| Time | Picture and action | On-screen caption / voiceover |
| --- | --- | --- |
| 00:00–00:04 | Fade in the Sigil social-preview lockup on the dark brand background. | `Repository work inside one terminal.` |
| 00:04–00:11 | Show the exact prompt and the first real repository read. | `Ask for one focused repository change.` |
| 00:11–00:16 | Pause on the write approval, affected file, and exact diff, then approve once. | `Review the exact diff before writing.` |
| 00:16–00:36 | Show the applied edit, the real search tool result with zero old-command matches, and the final summary. | `Approve once. Sigil applies the edit and verifies the result.` |
| 00:36–00:41 | Open `/resume` long enough to show the saved session row. | `Resume the saved repository session later.` |
| 00:41–00:45 | Cut to the branded end card with the install command and website. | `npm install -g @sigil-ai/sigil@alpha`<br>`sigil.corerobin.com` |

## Exact Demo Prompt

```text
The README still uses the old install command `npm install -g sigil`.
Replace only that command with `npm install -g @sigil-ai/sigil@alpha`,
show me the diff before writing, then verify that no old install command remains.
```

The disposable repository should contain exactly one matching line so the
approval stays narrow and the verification result is unambiguous.

## Edit Notes

- Use hard cuts around provider latency; do not add fake typing or fake tool output.
- Keep the approval diff on screen for at least 2.5 seconds.
- Use captions even when voiceover is present; many launch feeds autoplay muted.
- Keep captions to one line and inside the central 80% safe area.
- Use the Coral accent for decisions, Cyan for progress, and off-white for body text.
- End on the install command for at least 2 seconds. Build the card from
  `assets/demo/sigil-45-second-demo-end-card.svg`.
- Export H.264 MP4 plus a muted looping WebM; create a GIF only when a platform requires it.

## Acceptance Checklist

- The recording is 43–47 seconds long.
- Every product interaction comes from the real TUI.
- The diff is readable at normal playback speed.
- No secret, personal path, provider balance, or unrelated repository data is visible.
- The command uses the `@alpha` dist-tag.
- The published MP4 and WebM are 1920 × 1080 and 43–47 seconds long.
- English and Simplified Chinese WebVTT caption tracks cover the complete run.
- The end card uses `assets/social/sigil-social-preview.png` as its visual base.
