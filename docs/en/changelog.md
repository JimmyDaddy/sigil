# User Changelog

[Docs home](README.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page lists user-facing release notes. For support boundaries and early-preview caveats, see [status.md](status.md).

## v0.0.1-alpha.1 - 2026-07-08

### Added

- Published the scoped npm package as `@sigil-ai/sigil@alpha`.
- Published the Homebrew tap formula as `JimmyDaddy/sigil/sigil-ai`.
- Documented npm, Homebrew, Cargo git-tag, source, and manual release-archive install paths.
- Added generated GitHub Pages documentation pages for installation, configuration, providers, safety, privacy, MCP, visual tour, troubleshooting, reference, and current status.

### Changed

- Clarified that `v0.0.1-alpha.1` is an early preview: core TUI workflows are usable, while config, plugin APIs, advanced sandbox behavior, and automation surfaces may still change.
- Made the documentation entrypoints more task-focused: quickstart first, then installation, visual tour, daily workflow, safety, troubleshooting, and reference.
- Updated the user docs to describe the current provider set: DeepSeek, OpenAI-compatible, Anthropic, and Gemini.

### Known Limitations

- Self-update is not available.
- Stable plugin API compatibility is not promised for the alpha line.
- Sandbox coverage and execution receipts vary by platform and backend.
- Headless automation cannot show interactive approval modals.

## v0.0.1-alpha - 2026-07-07

### Added

- First public alpha release for the Sigil TUI.
- TUI entrypoint through the `sigil` command.
- Quick Setup, `/config`, `sigil doctor`, and `/doctor`.
- Durable task and planning flows through `/task` and `/plan`.
- Approval-backed file changes, shell execution, MCP usage, and code-intelligence edits.
- Session recovery from append-only local state.

### Known Limitations

- This release was an initial preview and was superseded by `v0.0.1-alpha.1`.
- Users should prefer the `alpha` install channel or the latest documented release tag.
