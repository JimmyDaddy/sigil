<!-- public-doc-role: changelog; authority: user-visible-release-history; sections: unreleased-main,v0-0-1-beta-1-2026-07-31,v0-0-1-alpha-6-2026-07-30,v0-0-1-alpha-5-2026-07-18,v0-0-1-alpha-4-2026-07-16,v0-0-1-alpha-3-2026-07-15,v0-0-1-alpha-2-2026-07-15,v0-0-1-alpha-1-2026-07-08,v0-0-1-alpha-2026-07-07; cta: open-installation -->

# User Changelog

[Docs home](README.md) · [Installation](installation.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page lists user-facing release notes. For support boundaries and early-preview caveats, see [Supported Today And Future Work](status.md).

## Unreleased - main

No user-facing changes have landed after `v0.0.1-beta.1`.

## v0.0.1-beta.1 - 2026-07-31

These changes are included in the packaged `v0.0.1-beta.1` release.

- Added the public macOS Desktop beta channel with signed, Apple-notarized Apple Silicon and Intel DMGs plus signed architecture-specific update bundles.
- Added explicit version checks and updates across Desktop Settings, TUI `/update`, and CLI `sigil update`; managed installations receive their package-manager command, while standalone updates remain checksum/signature verified and never restart silently.
- Release tags now stage a mutable draft first. The explicit publish run verifies all Desktop assets, makes the completed GitHub Release public, publishes npm with the matching `beta` or `alpha` dist-tag, and only then updates the website and Homebrew; this remains compatible with immutable GitHub Releases.
- Updated the website, README, installation/status docs, and visual tour so Desktop and TUI are coequal entry points with architecture-specific download guidance, a real Desktop capture tour, and the existing TUI real-run demo.
- `/compact` now treats the command itself as explicit intent: one invocation generates, validates, and atomically activates an admitted recoverable checkpoint without a confirmation modal. Failures keep the current context and remain visible with their exact reason.
- Fixed semantic compaction discarding its own result as stale after its audited provider-attempt and usage records advanced the durable stream.
- Fixed Desktop Enter submissions during active work failing to reach the durable follow-up queue, intermittent loss of live runtime controls, and valid live/durable replay being misclassified as a message conflict.
- Conversation titles can now be generated semantically by the active model after the first turn, while manual and generated renames stay synchronized with the open conversation page; title generation no longer competes with the primary request.
- Aligned the Desktop timeline, activity state, approval card, and composer widths, narrowed the approval presentation, and stopped unknown totals from appearing as stalled percentage progress.
- Added current-source Desktop Gherkin E2E coverage for real approvals, Enter queuing, skill/agent loading, `/plan`, automatic planning, and parallel Agents, alongside the existing stateful and orchestration TUI PTY acceptance campaigns.

## v0.0.1-alpha.6 - 2026-07-30

These changes are included in the packaged `v0.0.1-alpha.6` release.

### Added

- Added AI-planned Task execution: Sigil can turn one repository goal into a visible, reviewable step plan, run independent steps in parallel, preserve normal tool approvals, and finish against repository-owned verification evidence.
- Added an authenticated, restart-durable historical session catalog to `sigil serve` for future desktop clients, with bounded pagination, title search, provider/pin/state filters, and explicit stale-cursor recovery. Session logs remain the source of truth, and catalog failures do not stop runs or recording.
- Added a desktop runtime bridge for trusted local clients: durable catalog entries can be reopened after restart, startup and server metadata have one versioned JSON shape, and an opt-in stdin owner pipe triggers graceful shutdown without PID polling.
- Added a source-built desktop dogfood shell with native workspace selection, durable history, conversation runs, exact approval and cancellation controls, and verification evidence over the same authenticated local server used by automation. CI builds short-lived unsigned macOS, Linux, and Windows dogfood artifacts; these are not a public install channel.
- Added exact-route orchestration rollout manifests. Qualified releases may enable `auto + proactive` for matching new installations, while missing, stale, invalid, or different-route manifests fail closed.
- Added durable, session-scoped tool-output artifacts. Large shell, file, search, terminal, and MCP results keep bounded conversation cards while their policy-safe bytes remain available through typed, size-limited page or literal-search reads in the model, TUI, HTTP, and desktop surfaces.

### Changed

- Provider API-key storage now defaults to the owner-only credential file. Existing explicit `auto` configuration is non-interactive and strictly file-backed; it never queries an older native-system record. Only explicit `keyring` mode may show a system password prompt.
- Context compaction now defaults to cache-aware V3: stable provider/tool prefixes, source-bound intent continuity across repeated compactions, complete-turn tails, and trusted cache-cost admission. Manual compaction first produces a local-only plan with separate keep-current, recoverable tool-output cleanup, and full-semantic choices; only the full-semantic choice sends a billed request. Normal semantic compaction makes one extra LLM request on the current route, retaining the old request as its cacheable prefix and appending only a strict summary instruction; model narrative cannot grant authority or verification, and observed cache/output cost is shown before final activation confirmation. Provider-native materialization remains fail-closed until exact-route resume is implemented, preventing a billed request whose result would not yet be reused. Legacy threshold/tail fields remain readable for rollback and migration.
- Reworked the desktop dogfood shell around workspace/session navigation, one conversation task surface, and a verification inspector. It replays bounded saved messages, retains control of runs across navigation while the workspace service stays open, separates final replies from progress and tool output, and provides focused approval, diff, evidence, and draft-aware composer behavior.
- Added one consistent desktop visual system, adaptive wide/two-pane/compact layouts, system light and dark themes, high-contrast and reduced-motion handling, keyboard focus capture/restore, terminal-only streaming announcements, and usable reflow down to 320 CSS pixels.
- Existing and legacy configurations are never silently migrated to automatic orchestration. Doctor reports release qualification, and `manual + explicit_request_only` remains the coarse rollback without deleting Task history.
- New sessions now use the V2 tool-result schema and deterministic tool-output aging before semantic compaction. Development logs written with the pre-V2 tool-result schema are intentionally unsupported: Sigil reports the inline body as `LegacyUnavailable` in a bounded schema diagnostic, leaves the file untouched, and does not backfill, rewrite, or guess missing artifacts.

### Fixed

- Fixed non-interactive `auto` credential checks waiting indefinitely on macOS Keychain. `auto` no longer contacts native storage, and Doctor falls back to an offline status if an explicit native check does not finish promptly.
- Fixed the packaged desktop app reading Tauri-managed state before its setup lifecycle ran, which previously made the macOS app exit before creating a window.
- Fixed the event-driven TUI input loop stalling after the first key while idle.
- Fixed background tool-artifact maintenance racing with session actions, which could surface a transient lock failure instead of completing the requested action.

## v0.0.1-alpha.5 - 2026-07-18

These changes are included in the packaged `v0.0.1-alpha.5` release.

### Added

- Added explicit OAuth sign-in for remote Streamable HTTP MCP servers, including automatic or manual callback, native credential storage, refresh, sign-out, and specific recovery errors. Every destination still passes the normal network disclosure and destination checks; headless startup never opens a browser.
- Added configurable info-rail visibility, an `F2` show/hide shortcut, and a copy command that uses selected transcript text or the latest assistant reply.

### Changed

- Windows shell and terminal tools now use PowerShell by default, show the detected shell in Doctor and tool cards, and stop child processes more reliably after a timeout. Local execution remains unconfined.
- Activating or refreshing a remote MCP server now updates its available tools without leaving stale duplicates. Windows also cleans up stopped local MCP process trees more reliably.
- Refreshed the Sigil logo, repository landing page, documentation site, social preview, and launch materials around one consistent product story.

### Fixed

- Reply completion, queued work, and session transitions now recover more reliably without duplicating or stranding a final response.
- Long sessions keep timeline tail-index updates bounded, reducing redraw work as histories grow.

## v0.0.1-alpha.4 - 2026-07-16

These changes are included in the packaged `v0.0.1-alpha.4` release.

### Added

- Added default-off, privacy-bounded terminal attention notifications for completed long work, approvals, failures, and user-input requests, with automatic OSC 9/OSC 777/BEL selection.
- Added bounded repository context for Rust, Python, JavaScript/TypeScript, and Go, using available language services with a built-in parser fallback.
- Added TUI image attachments for PNG, JPEG, and WebP through local paths or the system image clipboard, with removable attachment chips and clear provider/model compatibility checks.
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
- Added stable `websearch` and supported `webfetch` routes with separate network controls and visible sources.
- Added a task Verification card, `Alt-V` focus, recommended checks, and inspectable evidence tied to the reviewed files and changes.
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
- Sandbox coverage varies by platform and backend.
- Headless automation cannot show interactive approval modals.

## v0.0.1-alpha - 2026-07-07

### Added

- First public alpha release for the Sigil TUI.
- TUI entrypoint through the `sigil` command.
- Quick Setup, `/config`, `sigil doctor`, and `/doctor`.
- Multi-step task and planning flows through `/task` and `/plan`.
- Approval-backed file changes, shell execution, MCP usage, and code-intelligence edits.
- Recovery of saved local sessions after a restart.

### Known Limitations

- This release was an initial preview and was superseded by `v0.0.1-alpha.1`.
- Users should prefer the `alpha` install channel or the latest documented release tag.

<!-- public-doc-cta: open-installation -->
Next: [Review installation and update paths](installation.md).
