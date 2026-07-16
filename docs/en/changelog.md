# User Changelog

[Docs home](README.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page lists user-facing release notes. For support boundaries and early-preview caveats, see [Supported Today And Future Work](status.md).

## Unreleased - main

### Added

- Added default-off, privacy-bounded terminal attention notifications for completed long work, approvals, failures, and user-input requests, with automatic OSC 9/OSC 777/BEL selection.
- Added bounded request-local repository context for Rust, Python, JavaScript/TypeScript, and Go, preferring relevant warm LSP snapshots and falling back to bundled Tree-sitter adapters.
- Added TUI image attachments for bounded PNG, JPEG, and WebP input through local paths or the system image clipboard, with removable metadata chips, controlled cache storage, safe session projections, and exact provider/model admission.
- Added `sigil doctor --output json`, a versioned and redacted local diagnostics format for support requests.
- Added `/feedback`, which previews included and excluded data before an explicit local-only JSON export; reports are never uploaded automatically.
- Added structured GitHub forms for bugs, feature requests, and documentation issues.

### Changed

- Completed the `/feedback` handoff: exported reports can now be reviewed in the TUI, revealed in the file manager, or paired with an explicitly opened bug form; reports remain local until the user attaches them.

## v0.0.1-alpha.3 - 2026-07-15

These changes are included in the packaged `v0.0.1-alpha.3` release.

### Added

- Added stable `sigil run --output json` and `--output jsonl` formats for scripts, plus an advanced bearer-authenticated `sigil serve` interface that only listens on the local machine.
- Added explicit saved-session actions for safe export, conversation fork, pinning, exact delete review, and retention cleanup preview and confirmation.

### Changed

- `/compact` can now confirm one manual context compaction when the selected model has installed local counting support and the compacted request is proven to fit. Completed long conversations and queued requests may use the same checked path. One pinned official OpenAI Responses model can also recover once from a confirmed pre-output context-limit rejection after separate count and savings checks.

## v0.0.1-alpha.2 - 2026-07-15

These changes are included in the packaged `v0.0.1-alpha.2` release.

### Added

- Added the OpenAI Responses provider through `[providers.openai_responses]`.
- Added stable `websearch` and capability-backed `webfetch` routes with separate network policy and source provenance.
- Added a task Verification card, `Alt-V` focus, recommended checks, and inspectable snapshot and changeset evidence.
- Added `Ctrl-R` checkpoint review with controlled restore or conversation fork choices.
- Added a read-only Context Compaction V2 preview through `/compact`.

### Changed

- Expanded local MCP support from stdio servers to include user-root Streamable HTTP servers under the same trust, approval, and secret-egress policy.
- Refreshed the user docs and website navigation around verification, recovery, and context controls.

### Current Limitation

- Context Compaction V2 apply, including guarded overflow recovery, remains temporarily frozen while correctness fixes are in progress; `/compact` is a review-only preview.

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
