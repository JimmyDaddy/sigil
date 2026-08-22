<!-- public-doc-role: changelog; authority: user-visible-release-history; sections: unreleased-main,v0-0-1-beta-4-2026-08-11,v0-0-1-beta-3-2026-08-06,v0-0-1-beta-2-2026-08-03,v0-0-1-beta-1-2026-08-02,v0-0-1-alpha-6-2026-07-30,v0-0-1-alpha-5-2026-07-18,v0-0-1-alpha-4-2026-07-16,v0-0-1-alpha-3-2026-07-15,v0-0-1-alpha-2-2026-07-15,v0-0-1-alpha-1-2026-07-08,v0-0-1-alpha-2026-07-07; cta: open-installation -->

# User Changelog

[Docs home](README.md) · [Installation](installation.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page lists user-facing release notes. For support boundaries and early-preview caveats, see [Supported Today And Future Work](status.md).

## Unreleased - main

- Transient provider disconnects, timeouts, rate limits, and server failures now recover within
  the same durable generation when no response or external effect was committed. Recovery is
  bounded, visible as reconnecting/waiting state, and survives restart only when the original
  child session has an exact scheduled request proof. Partial output and unknown tool, hosted, or
  workspace effects remain safely blocked for review instead of being silently replayed or
  failing the whole Task.
- Plan review now makes the readable Plan itself reviewable; an optional precompile is advisory
  and never rejects a valid Plan. `Run` atomically approves the exact Plan, creates one stable Task
  with a host-owned linear execution unit, and starts the runner immediately—no second model call
  must generate a Task DAG or structured contract. A model's bounded final prose is preserved as a
  reviewable Plan when typed submission fails, and ordinary Tasks also fall back to direct linear
  execution when their optional planner cannot return a valid plan. Real provider, permission,
  tool, verification, and effect failures remain typed and recoverable.
- Plan review now opens a complete, scrollable workbench instead of truncating the plan inside a
  compact status card. All plan steps remain reachable on 32x8 through wide terminals; `Esc` closes
  without rejecting, printable input returns to the composer, and actions stay bound to the exact
  durable plan.
- Revise now asks what should change, keeps the original plan active until a replacement succeeds,
  and restores it after every terminal revision failure. Older affected sessions are recovered by a
  strict read-only compatibility view, and `sigil doctor` reports ambiguous legacy lineage.
- Agents can ask bounded, typed questions through a durable attention queue. Pending questions
  survive TUI exit and resume without a timeout, background-agent questions route to the root
  session, and an accepted answer resumes exactly one provider attempt without replaying the turn
  that asked the question. MCP elicitation reuses the form renderer without inheriting durable
  replay semantics.

## v0.0.1-beta.4 - 2026-08-11

This beta stabilizes long-running TUI sessions, makes shell execution auditable in the transcript,
and closes durable Task continuation, memory-routing, and final-answer coordination gaps.

- Fixed TUI transcript corruption after a final answer and restored access to older history after
  terminal resize or reflow. History inspection now keeps a stable content anchor while new output
  arrives, and repeated artifact-maintenance failures are shown once per session.
- Busy-turn follow-ups now wait for a safe delivery point without interrupting the active run, and
  compact plan, queue, verification, attachment, and agent controls remain aligned and actionable on
  narrow or short terminals.
- Bash cards now show the policy-safe command during live execution, after reload, and in child-agent
  transcripts; running and terminal states no longer collapse into duplicate or misleading cards.
  Bounded read-only Git metadata checks are recognized without weakening protected `.git` mutation
  rules or `danger-full-access` hard-deny boundaries.
- Continuing a paused Task now selects the exact durable Task, carries the user's continuation
  guidance through a reviewed replacement plan, and shows only the current Task in the TUI instead
  of reviving an obsolete initial plan. Starting an explicit plan review also clears the prior Task
  from the current-work surface while preserving it as history.
- Explicit durable-memory intent can now be handled in the automatic routing turn. Approved memory
  writes settle durably before the selected Chat / PlanReview / Task handoff, while tool results keep
  the provider's declaration order.
- Automatic-routing reasoning no longer appears as a duplicate Thinking block before the real work
  turn. Final-answer coordination now scopes child-agent facts to the active logical run, removes
  stale snapshots when those facts settle, and bounds repeated blocked-final attempts. A blocked
  child or Task participant can no longer be reported as completed, and a final candidate rejected
  by the terminal blocker is no longer left visible as an accepted answer.
- Complete zero-byte tool artifacts are now valid retrievable artifacts, preventing maintenance from
  repeatedly deferring otherwise healthy session state.

## v0.0.1-beta.3 - 2026-08-06

This beta releases the automatic plan-review and
AI orchestration lifecycle, session-scoped scratch with quota/TTL, and the RFC-0062 tool-result
settlement closures.


- Automatic Task routing is now the default: ordinary input first runs a host-owned
  Chat / PlanReview / Task decision on the review-first baseline, and only an accepted plan can
  create a durable Task. Direct task execution requires an exact-route qualification manifest;
  explicit `routing_policy = "manual"` is the coarse rollback. `sigil doctor` reports the three
  automatic-orchestration facts.
- The plan review surface is now a first-class lifecycle: `/plan` and automatic review share the
  same typed plan draft, the same pending plan card (Run / Save / Revise / Reject), and the same
  HTTP/Desktop/TUI decision commands. Reconnect and reload restore a pending plan.
- Durable Task execution now carries versioned per-step paths, capabilities, deliverables,
  acceptance criteria, and check references through planning, compaction, interruption, and
  resume. Exact scoped-tool capability admission happens before child or provider dispatch, and a
  durable progress guard forces bounded finalization when a participant repeats the same analysis
  without progress.
- A fixed read-only `vcs_inspect` tool provides bounded status/diff facts without arbitrary Git
  arguments, while semantic cache layout V2 excludes per-turn authorization identities from the
  provider-visible tool-schema fingerprint.
- Workspace instruction memory and durable memory are now enabled by default. Durable mutations ask
  in ordinary permission modes and run without an approval prompt in `danger-full-access`; set
  `[memory].enabled = false` or `[memory].writable = false` to opt out of either capability.
- Plain `sigil` launches now start a fresh session instead of reopening the most recent conversation.
  Explicit resume keeps the portable transcript across safe endpoint corrections, asks before using
  a changed or unproven destination, and prevents the same session from being opened for writing in
  two TUI/Desktop/headless owners at once.
- TUI and Desktop now configure an optional context-window size independently for each connection/model pair, including first-run setup. Empty values keep automatic provider metadata and global fallback behavior, so providers without a model catalog are not blocked.
- TUI transcript selections now copy automatically when the mouse is released. Copy attempts both
  OSC52 and the system clipboard, preserves a failed selection for `Ctrl-C` retry, and still
  excludes the info rail.
- Long TUI runs no longer stop when native scrollback and asynchronous input compete for a cursor
  query. Active-run facts no longer look like a host finalization instruction, and typed artifact
  retrieval remains visible after the initial tool-preview budget is exhausted.
- Interactive tool approvals no longer expire automatically after 300 seconds. `Allow for
  session` persists across reopening the same session and reuses recognized validation grants
  across presentation-only `tail`, `head`, or `grep` changes while retaining exact execution checks.


## v0.0.1-beta.2 - 2026-08-03

This beta removes model-catalog discovery from the critical path for first-run setup and allows
the frozen TUI packages to ship before the matching Desktop artifacts are ready.

- Quick Setup, `/config`, Desktop Settings, and the HTTP setup route now treat remote model lists
  as optional guidance. Bundled models stay selectable while discovery runs, and an exact model
  ID can always be entered when a provider has no `/models` endpoint or discovery fails. The first
  real generation request remains the authority for credential, protocol, endpoint, and model
  compatibility.
- Unsupported reasoning parameters are omitted using the exact provider/model capability mapping
  instead of being guessed for unknown or non-reasoning models.
- Alpha and beta TUI npm packages can now be published from the frozen release candidate while
  the GitHub Release remains a draft. Desktop DMGs, updater archives, Pages updates, Homebrew, and
  public GitHub Release publication continue later from the same immutable tag.
- macOS Desktop notarization is now asynchronous and resumable. Immutable DMG and app submissions
  are recorded in an append-only ledger, status checks are one-shot, and an offline finalizer
  verifies accepted submissions before the Desktop assets can be uploaded.

## v0.0.1-beta.1 - 2026-08-02

This beta publishes the first signed macOS Desktop distribution and the matching cross-platform TUI beta channel.

- Shell permissions now use one immutable, structured plan across policy, approval, audit, and
  execution. Compound validation commands are classified per child command; dangerous flags,
  redirections, dynamic syntax, protected targets, and missing sandbox capabilities continue to
  fail closed. Exact, containment-bound session grants reduce repeated approval without widening
  access to unrelated commands. Native PowerShell background work is kept out of the one-shot
  Shell path, and unresolved shell paths remain conservative instead of aborting the plan.
- Approval decisions now converge from the exact command receipt in both Desktop and TUI instead
  of waiting solely for a later live event. Accepted, resolving, execution-started, stale,
  expired, uncertain, and terminal states are distinct and protected against older receipts.
- Finite commands now use the foreground Shell path exclusively. Persistent and interactive work
  uses explicit terminal tasks with readiness and event-driven waits; the runtime no longer uses
  periodic terminal status discovery or asks the model to poll unchanged logs.
- Release tags now build TUI/npm bytes once and freeze a commit-bound candidate manifest in the
  draft. Final publication reuses those admitted tarballs instead of rebuilding, while the release
  doctor and Desktop upload command bind versions, tag/main/CI, updater keys, signatures,
  notarization, and both macOS architectures. Publishing automatically starts a bounded public
  npm/GitHub/Desktop/Pages/Homebrew installation smoke.
- Fixed invalid or non-current configuration blocking every Desktop workspace before Settings was
  reachable. Desktop now starts a recovery-capable workspace service and offers an explicit
  current-format replacement in Settings; TUI Quick Setup offers the same replacement path.
  Neither surface reuses or migrates values from the invalid file, and both refuse to overwrite a
  concurrently repaired valid config.
- Published the macOS Desktop beta channel with signed, Apple-notarized Apple Silicon and Intel DMGs plus signed architecture-specific update bundles.
- Added explicit version checks and updates across Desktop Settings, TUI `/update`, and CLI `sigil update`; managed installations receive their package-manager command, while standalone updates remain checksum/signature verified and never restart silently.
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
- Context compaction now uses cache-aware V3 exclusively: stable provider/tool prefixes, source-bound intent continuity across repeated compactions, complete-turn tails, and trusted cache-cost admission. Manual compaction directly requests one recoverable semantic checkpoint; normal semantic compaction makes one extra no-tool LLM request on the current route. Provider-native materialization remains fail-closed until exact-route resume is implemented.
- Reworked the desktop dogfood shell around workspace/session navigation, one conversation task surface, and a verification inspector. It replays bounded saved messages, retains control of runs across navigation while the workspace service stays open, separates final replies from progress and tool output, and provides focused approval, diff, evidence, and draft-aware composer behavior.
- Added one consistent desktop visual system, adaptive wide/two-pane/compact layouts, system light and dark themes, high-contrast and reduced-motion handling, keyboard focus capture/restore, terminal-only streaming announcements, and usable reflow down to 320 CSS pixels.
- Incompatible configurations are rejected rather than migrated. Doctor reports release qualification, and `manual + explicit_request_only` remains the coarse rollout rollback without deleting Task history.
- New sessions now use the V2 tool-result schema and deterministic tool-output aging before semantic compaction. Development logs written with the pre-V2 tool-result schema are intentionally unsupported: Sigil reports the inline body as `Unavailable` in a bounded schema diagnostic, leaves the file untouched, and does not backfill, rewrite, or guess missing artifacts.

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
