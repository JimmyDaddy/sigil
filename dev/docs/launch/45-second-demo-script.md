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

- Record at 1280 × 720 or 1920 × 1080, 60 fps.
- Use a 112 × 32 terminal with the built-in `sigil_dark` theme and the info rail visible.
- Use a disposable repository with a clean worktree and one intentionally stale README install command.
- Preconfigure the provider and model; do not show API keys, usernames, home-directory paths, notifications, or unrelated tabs.
- Keep the mouse pointer outside the terminal unless a click is part of the shot.
- Record the complete real run first. Remove dead time in editing and speed up provider waits, but never alter the displayed result.

## Timeline

| Time | Picture and action | On-screen caption / voiceover |
| --- | --- | --- |
| 00:00–00:03 | Fade from the Sigil mark to the wordmark on the dark brand background. | `Repository work inside one terminal.` |
| 00:03–00:07 | Show a clean terminal in the demo repository. Run `sigil`. The TUI opens with the workspace name and manual permission mode visible. | `Open Sigil in a real repository.` |
| 00:07–00:13 | Paste the exact prompt below and submit it. | `Ask for a focused repository change.` |
| 00:13–00:20 | Show the read activity and the concise proposed change. Compress provider wait time to 2–3 seconds while keeping the progress sequence readable. | `Sigil inspects the repository before it acts.` |
| 00:20–00:29 | The write approval opens. Pause on the affected file and diff; move through the changed hunk, then approve. | `Review the exact diff before writing.` |
| 00:29–00:36 | Show the completed edit and focus the Verification card with `Alt-V`. Run the recommended check and show it pass. | `Verify the result without leaving the task.` |
| 00:36–00:41 | Open `/resume` long enough to show the saved session row, then return to the completed task. | `Sessions stay available when work is interrupted.` |
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
- End on the install command for at least 2 seconds.
- Export H.264 MP4 plus a muted looping WebM; create a GIF only when a platform requires it.

## Acceptance Checklist

- The recording is 43–47 seconds long.
- Every product interaction comes from the real TUI.
- The diff is readable at normal playback speed.
- No secret, personal path, provider balance, or unrelated repository data is visible.
- The command uses the `@alpha` dist-tag.
- The end card uses `assets/social/sigil-social-preview.svg` as its visual base.
