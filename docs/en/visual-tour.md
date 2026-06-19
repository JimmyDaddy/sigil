# Visual Tour

[Docs home](README.md) · [Quickstart](quickstart.md) · [简体中文](../zh-CN/visual-tour.md)

This page walks through the main Sigil surfaces with SVG captures generated from the TUI renderer. The captures are deterministic documentation assets, not hand-drawn mockups, so they stay close to the real terminal layout.

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

Use `/config` for common provider, permission, memory, compaction, code intelligence, terminal, Skills, Plugins trust review, and MCP settings. Lower-frequency provider details remain in `sigil.toml` and environment variables.

## Regenerating These Captures

The checked-in SVGs are generated from `ratatui::TestBackend` fixtures. Regenerate them after meaningful TUI layout changes:

```bash
scripts/generate-tui-screenshots.sh
scripts/check-pages-site.sh
```

For release-quality bitmap screenshots or GIFs, capture the running TUI in a controlled demo workspace:

1. Use a test repository with no secrets.
2. Run `sigil` in a terminal size close to `120x36`.
3. Capture the main screen, approval modal, and config panel.
4. Save images under `site/assets/screenshots/` with a new file name.
5. Link them from this page and re-run `scripts/check-pages-site.sh`.
