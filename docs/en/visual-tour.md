# Visual Tour

[Docs home](README.md) · [Quickstart](quickstart.md) · [简体中文](../zh-CN/visual-tour.md)

This page walks through the main Sigil surfaces with SVG captures generated from the real TUI renderer.

## Main TUI Session

![Sigil TUI session preview](../../site/assets/screenshots/tui-session.svg)

The normal workflow is:

1. Start `sigil` in a workspace.
2. Type a task in the composer.
3. Watch repository reads, searches, and tool activity in the transcript.
4. Use the info rail to check session, permissions, model, LSP, usage, and controls.

## Approval Review

![Sigil approval review preview](../../site/assets/screenshots/approval-review.svg)

Before risky actions run, review:

- tool summary;
- affected files;
- diff preview;
- allow or deny action.

If the diff does not match your intent, deny it and ask for a narrower change.

## Configuration Panel

![Sigil config panel preview](../../site/assets/screenshots/config-panel.svg)

Use `/config` for common provider, permission, memory, compaction, code intelligence, terminal, Agents, Skills, Plugins trust review, and MCP settings. Lower-frequency provider details remain in `sigil.toml` and environment variables.

## Task Verification

![Sigil task verification preview](../../site/assets/screenshots/verification-card.svg)

When a durable task needs completion evidence, the Verification card keeps the current verdict, recommended check, and evidence together. Press `Alt-V` to focus the card, `I` to inspect snapshot and changeset details, and `Enter` to run the bound check when one is available.

## Checkpoint Restore

![Sigil checkpoint restore preview](../../site/assets/screenshots/checkpoint-restore.svg)

Press `Ctrl-R` while idle to rebuild the latest controlled checkpoint from durable evidence. Review the exact reverse diff before restoring files, or choose the conversation fork when you want earlier context without changing shared workspace files. Shell and remote side effects remain outside this file restore boundary.

## Context Compaction Preview

![Sigil context compaction preview](../../site/assets/screenshots/compaction-preview.svg)

Use `/compact` to review which older messages would fold and why the target request is or is not admissible. Opening the review is read-only; when exact local admission is ready, `Enter` confirms one manual V2 apply. A completed hard-threshold chat turn may use the same verified path after the session becomes fully idle.
