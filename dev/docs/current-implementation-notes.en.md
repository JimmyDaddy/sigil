# Sigil Current Implementation Notes

[简体中文](current-implementation-notes.md)

This document records current repository implementation facts for developer alignment. The default English user entrypoint is `README.md`; the Chinese user entrypoint is `README.zh-CN.md`. User documentation is split by language under `docs/en/*` and `docs/zh-CN/*`.

## Repository Layout

```text
sigil/
  assets/logo/                 # Logo and wordmark PNG assets for README, releases, and package listings
  crates/
    sigil-kernel/              # Generic agent kernel and domain contracts
    sigil-provider-deepseek/   # DeepSeek provider implementation
    sigil-provider-openai-compat/ # OpenAI-compatible provider implementation
    sigil-tools-builtin/       # Built-in tools
    sigil-code-intel/          # LSP client, Tree-sitter fallback, and code intelligence tools
    sigil-mcp/                 # stdio MCP client and tool adapter
    sigil-runtime/             # Shared provider / tool / run option assembly for entrypoints
    sigil-http/                # HTTP/SSE adapter DTOs, auth/SSE helpers, and future server boundary
    sigil/                     # `sigil` binary: starts the TUI by default; subcommands are for automation/debugging
    sigil-tui/                 # TUI state, rendering, and runner for the primary user entrypoint
  docs/                        # User documentation
  site/                        # GitHub Pages static site source
  dev/governance/              # Development constraints, code standards, engineering standards
  dev/docs/                    # Architecture, roadmap, design evolution, and implementation notes
  sigil.toml                   # Local config file, ignored by default
```

## Current Capability Baseline

- `sigil-kernel` owns generic provider, tool, session, approval, permission, event, memory, compaction, and task orchestration contracts.
- `sigil-runtime` assembles providers, built-in tools, MCP tools, run options, and role-scoped task agents.
- `sigil-provider-deepseek` supports DeepSeek streaming chat, tool calls, reasoning replay, usage, pricing, Beta endpoints, prefix completion, and FIM-specific entrypoints.
- `sigil-provider-openai-compat` supports OpenAI-compatible Chat Completions streaming chat, tool calls, usage, base URL, organization/project headers, and model configuration.
- `sigil-tools-builtin` provides file read/write/edit/delete, multi-file change set apply, search, directory listing, and shell execution.
- `sigil-code-intel` provides optional LSP / Tree-sitter code intelligence, including read-only symbol, definition, reference, diagnostic, and code action query tools, plus code action / rename edit tools with approval diff previews.
- `sigil-mcp` supports stdio MCP servers, `initialize`, `tools/list`, `tools/call`, read-only `resources/list` / `resources/read`, read-only `prompts/list` / `prompts/get`, `roots/list`, elicitation handling, progress/listChanged runtime events, lazy activation, and trust enforcement.
- `sigil-http` currently owns the HTTP/SSE adapter server config DTO, bearer auth validator, `PublicRunEvent` SSE serialization, per-run event sequence helper, in-memory session/run registry, run start/cancel handling, and approval decision routing; later work will add HTTP routing and server wiring without depending on `sigil-tui` or duplicating the agent loop.
- `sigil` provides the `sigil` binary: no subcommand starts the TUI directly; `run`, `doctor`, and the `serve` HTTP/SSE adapter preflight remain explicit automation and diagnostics subcommands; `serve` currently validates localhost/token defaults and prints a routing-pending status without starting an HTTP listener; `prefix` and `fim` remain hidden debugging or provider-specific entrypoints rather than normal user concepts.
- `sigil --version` prints the package version, git commit, target, and profile for install smoke checks, release archive validation, and issue triage.
- `sigil-tui` owns the primary TUI implementation: chat/composer, slash selector, Quick Setup, `/config`, `/doctor`, `/new`, `/resume`, `/plan`, approval modal, tool activity, diff preview, session recovery, task status display, context compaction, markdown code block highlighting, and code intelligence status display.

## TUI Module Boundaries

`crates/sigil-tui/src/app.rs` remains the `AppState` facade. It owns fields, bootstrap, top-level key routing, and cross-state orchestration. Specific flows live under `src/app/*`:

- `input_flow.rs`
- `slash_flow.rs`
- `modal_flow.rs`
- `config_flow.rs`
- `setup_flow.rs`
- `session_flow.rs`
- `timeline_flow.rs`
- `tool_card_interaction.rs`
- `approval_flow.rs`
- `mouse_flow.rs`
- `worker_bridge.rs`
- `command_dispatch.rs`

Flow tests live in `crates/sigil-tui/src/app/tests/*_tests.rs`. New TUI behavior should land in the corresponding flow and same-domain tests instead of rebuilding the state machine inside `app.rs`.

`runner.rs` is the worker facade. Worker protocol, spawn assembly, run loop, event/approval bridge, and session/compaction flow live under `runner/*`; tests live under `runner/tests/*`.

`ui.rs` is the renderer module entrypoint. Shell layout, theme, geometry, text, timeline, tool card, markdown, approval, setup/config, and modal renderers live under `ui/*`.

TUI theme handling lives in `crates/sigil-tui/src/ui/theme/`. `sigil-kernel` only stores `[appearance]`, `ThemeId`, and raw color override strings, keeping it independent from `ratatui`; `sigil-tui` resolves that config into a `ThemePalette`, and renderers consume semantic tokens from the AppState config snapshot or `TimelineRenderOptions`. Theme switching affects TUI appearance only and is not written to session/control logs, approval records, or provider-visible context.

## User Interaction State

The TUI is currently chat-first:

- The inline viewport fills the visible terminal area.
- The left main area shows live transcript and the bottom composer.
- The right `Info rail` shows `Session / Permissions / Agents / LSP / Usage / Controls`.
- Narrow terminals collapse the info rail to keep chat/composer usable.
- When restoring an old session at startup, full scrollback is seeded into terminal scrollback in batches to avoid replaying a long session in a single frame.
- After prompt submission, the composer clears and remains visible.
- The composer supports common readline-style editing keys, including current line start/end, character/word movement, word deletion, `Ctrl-K/Y` kill/yank, `Shift-Enter` / `Alt-Enter` / `Ctrl-J` newline insertion, and `Ctrl-Z` restore for the latest non-empty draft cleared with `Esc`; paste uses bracketed paste as text insertion, so multiline paste is not interpreted as submit, and large paste is folded in the composer display while preserving the full submitted text. Draft restore and paste folding are runtime editor state and are not written to durable session/control logs.
- The main screen no longer requires `Tab` focus cycling between cards; `Shift-Tab` cycles and persists the default `allow / ask / deny` permission mode.

Running-state hints are render-layer projections and are not written back into durable transcript. Live phase remains in run state and event flow only.

## Tool Activity and Diff

Tool results are displayed as standalone activities. The renderer has dedicated handling for common built-in tools:

- `read_file`
- `ls`
- `glob`
- `grep`
- `bash`
- `write_file`
- `edit_file`
- `delete_file`
- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`

Simple read-only `rg / grep / fd / find` bash commands are recognized as `Searched`. Other structured payloads use a tree fallback instead of dumping raw JSON or call ids.

`write_file`, `edit_file`, and `delete_file` result activities expand the bounded unified diff captured at execution time by default. Diff lines include old/new line numbers; activity bodies skip repeated hunk headers and summarize hunk counts in the file header. Large diffs show `diff truncated` and the number of hidden lines.

`apply_changeset` supports multi-file create / update / delete after one approval. Before writing, it validates workspace paths, hashes, mtimes, snippets, symlinks, and binary text boundaries; validation failures write no files. Successful or partial executions write `.sigil/changesets/<id>/preview.diff` and `reverse.diff` artifacts, and return structured artifact path, hash, stats, and apply status metadata. Model-visible content stays summary-only and does not include the full diff.

Approval cards use fixed `Summary / Files / Diff / Actions` sections. Diff previews for `write_file`, `edit_file`, `delete_file`, and `apply_changeset` support file switching, hunk navigation, and diff mode switching. `apply_changeset` approvals also show the change set id, overall risk, per-file action/risk, and file-type-based formatting guidance.

## Session and Control State

Default session logs are stored under:

```text
.sigil/sessions/
```

The current implementation uses append-only JSONL:

- Session identity is restored from the durable log instead of blindly falling back to the current config provider/model.
- Response handles, provider continuation state, prefix snapshots, compaction records, and usage snapshots are written into append-only control logs.
- Tool approval, execution lifecycle, and reasoning deltas append control records.
- Task run, plan, step, child-session, and subagent approval-route summaries append control records and are projected through `Session::task_state_projection`.
- Skill index snapshots and loaded-skill summaries now have `SkillIndexCaptured` / `SkillLoaded` control entries and are projected through `Session::skill_state_projection`; runtime discovery now covers `.sigil/skills`, `.sigil/agents`, `.claude/skills`, `.claude/agents`, `.reasonix/agents` when the compatibility source is explicitly enabled, and optional user skills, including frontmatter parsing, shadowing warnings, hashing, and invalid path/name rejection; internal read-only `load_skill` validates enabled/trusted/model-invocable state and permission policy, reads only the skill entrypoint, injects the loaded skill body as current-run transient context, and appends a `SkillLoaded` control entry; the TUI `/config` `Agents` section now uses the workspace-aware `AgentProfileRegistry` to display built-in, native, and compatibility profiles, including source/kind/trust/effective enabled/user/model, provider/model, tool scope, and nickname candidates, and its footer trust/block/enable/user/model actions append the profile snapshot plus append-only trust/policy decisions; the `Skills` section displays only inline/reusable skills, including enabled/trust/source/hash/run mode/tool scope/path patterns, and keeps footer load/invoke actions; TUI slash fallback also lists only trusted inline skills, so `run_as=child_session` compatibility resources are no longer displayed as ordinary skill slash rows or resolved through `/skill-id`; composer-leading `@` opens an agent mention selector that lists only enabled, trusted, user-invocable agent profiles; submitting `@profile <prompt>` goes through the TUI worker `InvokeAgentProfile` command and runtime `AgentToolRuntime::invoke_agent_profile`, starts a foreground child thread with `AgentInvocationSource::Mention`, validates enabled/trusted/user-invocable policy, and does not rely on the ordinary chat delegation hard gate; plugin manifest discovery now covers `.sigil/plugins/<id>/plugin.toml`, and the TUI `/config` Plugins section displays manifest path, id/name/version, skills/hooks/MCP commands, hash, and execution implications, with footer approve/deny actions that append `PluginManifestCaptured` and `PluginTrustDecision` control entries; profile alias/slash metadata and plugin agent schema expansion remain later P7 work.
- Terminal task handles, statuses, and output preview summaries have a dedicated control entry and `Session::terminal_task_projection`; terminal tool metadata is mirrored into append-only `TerminalTask` control entries, and the TUI restores them as activity cards, shows the running terminal count in the info rail, and can cancel a focused running terminal card with `Alt-X` confirmation through the worker `terminal_cancel` path while preserving execution audit entries.
- `sigil-tools-builtin` now has a terminal process manager: the default non-PTY backend writes output under `.sigil/tasks/<task-id>/{meta.json,output.log,stdout.log,stderr.log}` with bounded read, status, and cooperative cancel support; explicit `terminal_start` `pty=true` uses the `portable-pty` backend, runs a dedicated blocking read thread for the combined artifact log, and supports bounded-queue `terminal_input`, `terminal_resize`, and cancel. Each terminal input is capped at 8 KiB, and permission/audit metadata records only the task id and input byte count, not raw stdin; input/resize on non-PTY tasks still returns structured unsupported results.
- Tool executions that started without a terminal record are marked `interrupted` on restore.
- Dangling tool calls are projected as structured `interrupted` tool results.
- Historical file-change result cards are restored with the session.
- Compaction only appends `CompactionApplied` control records and does not rewrite old history.
- Hard-threshold automatic compaction runs only after a run returns to idle; it does not preempt streaming execution.

After restore, the next request recovers the latest matching provider response handle. The current session identity is not silently rewritten when `/config` saves new default provider/model settings.

Planned tasks are not auto-replayed on restore. Normal composer input stays chat-first and no longer triggers `ContinueTask` just because the current session has an unfinished task. The explicit durable-task continuation entry is `/task continue`; `/plan continue` is no longer a compatibility alias. The worker resumes the latest unfinished task from the durable task projection and skips completed steps; if an explicit continue is requested when only completed/cancelled tasks remain, it returns the corresponding terminal-state explanation.

## Current Task Planning Implementation

Planned tasks enter through TUI `/task <task>`; `/plan` only enters one-shot Plan mode or immediately runs a read-only planning prompt, and does not create durable task state. The kernel now provides `PlanApproved` control entries and `PlanApprovalProjection`, separate from durable tasks, recording plan version/hash, approval time, `ask` or `workspace_edits` permission, scope, expiry, and whether planning context should be cleared; `workspace_edits` covers only required-preview workspace file write tools and does not relax shell/execute, network, MCP, or Agent spawn. `ApprovePlan` conservatively extracts workspace paths from the plan text into `PlanApprovalScope.workspace_paths`; if the plan contains no path-like token, the scope stays empty and preserves the existing whole-workspace behavior. The agent loop now enforces approved scope during execution: an active `PlanApproved(workspace_edits)` only downgrades in-scope workspace file-write `Ask` decisions to `Allow`; explicit `Deny`, external directories, empty subjects, out-of-scope paths, and non-file-write tools continue through the normal permission policy. Detecting semantic drift from the approved plan and requiring reapproval remains future work. After a plan prompt finishes, the TUI live band shows a plan approval surface with `A` approve-as-ask, `W` approve-workspace-edits, `C` continue planning, and `Esc` discard; approvals go through worker `ApprovePlan`, which appends `PlanApproved` before syncing entries back to the TUI. Plan prompts use the ordinary agent loop, but the user prompt and plan-mode instructions are injected only as current-request transient context and are not appended as parent User entries; their tool surface uses the planner-scoped registry while retaining agent-thread tools for explicit read-only delegation. Normal composer input is always chat-first; continuing a durable task requires `/task continue` or a task UI action, and unfinished tasks no longer hijack plain input. When ordinary chat input explicitly asks for subagent delegation, the TUI maps that intent into `AgentDelegationRequirement`; if no terminal or result-bearing Agent-category tool result happens, the agent loop first retries with a transient instruction to call an agent-thread tool such as `spawn_agent`, and if the retry still does not satisfy the requirement it does not persist that final answer; invalid inputs, tool execution errors, or still-running agent tool results do not release the hard gate. Model-visible `spawn_agent` brings only a bounded summary plus a `read_agent_result` reference back into the parent context; `wait_agent` returns lightweight status and result references only, not child final-answer text. The full child final answer remains in the child session, and extra detail must be fetched explicitly with `read_agent_result` pages; page text is delivered to the model only as current-request transient context, while the durable parent tool result and TUI tool card keep only offset, size, truncation state, and result refs. This avoids larger summaries, repeated summary replay through `wait_agent`, replaying page text after restore, or dumping full child transcripts into the parent context. Pre-tool assistant text from the same turn as an Agent-category tool call is streamed live only, is not persisted into parent history, and is downgraded to Thinking styling in the TUI instead of a formal assistant answer; the user-facing final summary must happen after the child result/status returns to the parent. The worker protocol uses `SubmitPlanPrompt` for plan-mode prompts, and `SubmitTask` / `ContinueTask` commands plus `TaskRunStarted` / `TaskRunFinished` messages for durable tasks; task run / step / child-session control entries are also streamed to the TUI through live `RunEvent::Control` updates. The info rail renders task status, latest plan version, completion progress, the current or last failed step, and a compact summary of the current plan steps from durable task control entries; the `Agents` section lists `main` plus concrete child agents, the composer renders a compact agent panel when child agents exist, `Down` focuses that panel from the last composer input row, `Up/Down` chooses an agent row, `Enter` switches the visible transcript, `Alt-A` / `Shift-Alt-A` cycle the visible agent transcript, and `/agent <main|child-id>` opens slash-selector-driven precise switching. `/agent rename <child-id|current> <name>` appends a `TaskChildSessionDisplayName` control entry as a presentation-only display-name override; `/agent close <child-id|current>` is parsed in the TUI, sent to the worker as `CloseAgent`, and executed through runtime `close_agent_thread`, reusing the model-visible `close_agent` validation before appending `AgentThreadClosed`; running threads still need a later cancel path. The runtime delegate records requested -> rejected `AgentThreadMessageRouted` audit entries for valid-target `message_agent` attempts and returns unsupported, but `message_agent` is not registered as a model-visible tool until active child-agent mailbox support exists. Display names resolve from persisted rename, then plan step `display_name`, then role+ordinal fallback such as `read 1` or `write 1`. Selecting a child agent switches the main chat area to that child session transcript with a sticky breadcrumb. Step rows use status markers and matching text colors: running is highlighted, completed is green, failed/blocked is red, cancelled/interrupted is gold, and pending is muted.

Foreground `join_before_final` child agents render a main-timeline agent activity card from durable `AgentThreadStarted` control state. The card and footer advertise `Ctrl-B background`, which requests detaching the active foreground child agent into background execution.

`sigil-kernel::SequentialTaskOrchestrator` runs a planner role first, accepts plan updates through the internal `task_plan_update` tool, then executes steps sequentially. Executor steps run against the parent session with transient step context so plan prompts do not become ordinary user history. Subagent read/write steps run in child sessions, and the parent session records child-session lifecycle links plus approval and MCP elicitation route summaries for child interactions.

When a step encounters an ordinary tool error but the agent reads that error and still produces a final answer, the orchestrator treats the step as recovered and continues later steps, while preserving a `recovered tool error` summary in `TaskStepEntry.reason`. Max turns, interrupted tool calls, approval denial, and permission-class tool errors still stop the task.

Role-specific providers and run options are assembled in `sigil-runtime`. Planner and subagent-read default to a read-only scoped tool registry; executor defaults to the full registry; subagent-write uses the full registry only when `[task].allow_write_subagents = true`. `ScopedToolRegistry` gates specs, preview, execution, permission hooks, and egress hooks. Runtime workers now build a workspace-aware `AgentProfileRegistry` that discovers Sigil-native workspace agent profiles from `[skills].workspace_agents_dir` (default `.sigil/agents`): `.sigil/agents/<id>/agent.toml` or `.sigil/agents/<id>/AGENT.md`. Native profiles default to enabled, manual-only, needs-review, and read-only, and enter the model-visible agent index only when explicitly trusted and model_allowed; `AgentProfileTrustDecision` append-only control entries are projected through `AgentProfileTrustProjection` and overlaid onto non-system profiles, and both TUI worker agent-tool registration and the runtime supervisor use the session-aware registry, so source/profile hash changes invalidate stale trust decisions, return the profile to `needs_review`, and remove it from the model-visible agent index by default; duplicate ids and symlink escapes warn and are skipped. The same registry also projects trusted compatibility entries with `run_as=child_session` from skill discovery into subagent profiles, including `.sigil/agents/*.md`, default-enabled `.claude/agents/*.md`, and `.reasonix/agents/*.md` after `[skills].compatibility_sources = ["claude", "reasonix"]` is configured; `disable-model-invocation` / `disableModelInvocation` maps to manual-only, `allowed-tools` / `allowedTools` can only narrow the profile tool scope, and entries with `disallowed-tools` / `disallowedTools` are warned and skipped because subtractive scopes cannot be represented safely as an `AgentProfile`. `spawn_agent` intersects the role tool scope with the profile tool scope when building the child registry, so a profile cannot expand the role's tool surface; child runs receive profile description/instructions as transient child system prompts without persisting them into parent history.

`AgentProfilePolicyDecision` append-only control entries are projected through `AgentProfilePolicyProjection` and overlaid onto non-system profiles as effective `enabled` / `user_invocable` / `model_invocable` policy. The overlay is bound to the current source/profile hash, so stale policy expires after profile edits; runtime filtering and `spawn_agent` registration use the effective policy without mutating the source `AgentProfile`, keeping profile snapshot hashes stable.

## Configuration and Provider

Root config is parsed by `sigil-kernel`:

- `[workspace]`
- `[session]`
- `[agent]`
- `[permission]`
- `[memory]`
- `[compaction]`
- `[code_intelligence]`
- `[terminal]`
- `[task]`
- `[providers.*]`
- `[[mcp_servers]]`

DeepSeek provider configuration lives under `[providers.deepseek]`. OpenAI-compatible provider configuration lives under `[providers.openai_compat]`; `agent.provider` uses `openai_compat` and accepts `openai-compatible` / `openai_compatible` as input aliases. Runtime environment overrides are resolved in the provider config layer: DeepSeek uses `SIGIL_API_KEY` / `DEEPSEEK_API_KEY`, while OpenAI-compatible uses `SIGIL_OPENAI_COMPATIBLE_API_KEY` / `OPENAI_API_KEY`.

TUI `/config` exposes only high-frequency provider fields, permissions, memory, compaction, code intelligence controls, terminal mouse/OSC52/scroll sensitivity compatibility settings, the Agents browser, Skills browser, Plugins trust review, and common MCP server fields. It can switch between `deepseek` and `openai_compat`; DeepSeek FIM is shown as a provider-specific advanced field, while OpenAI-compatible marks it unsupported. Lower-frequency provider-specific fields remain available through config files and environment variables. The Agents browser uses `AgentProfileRegistry` to display built-in, native, and compatibility profiles, supports PgUp/PgDn switching, shows source/kind/trust/effective enabled/user/model, provider/model, tool scope, and nickname candidates, and appends profile snapshots plus trust/policy decisions through trust/block/enable/user/model footer actions. The Skills browser shows only inline/reusable skills, supports PgUp/PgDn switching, shows trust/source/hash/run mode/invocable/tool scope/path patterns, and uses footer load/invoke actions to submit requests still governed by the runtime `load_skill` policy; the slash selector's skill fallback follows the same trusted-inline-only boundary. Plugins review uses the current session control projection for existing trust decisions, supports PgUp/PgDn plugin switching, and appends the reviewed manifest snapshot plus trust decision to the current session JSONL.

`sigil doctor` and TUI `/doctor` reuse runtime diagnostics to check config loading, workspace resolution, session log location, provider/auth source, MCP command/trust state, code intelligence LSP plan, terminal `TERM`, terminal profile/layers, and mouse/OSC52/scroll sensitivity compatibility settings. Diagnostics report only the secret source, not secret values.

## Current Packaging Implementation

Two local distribution-validation paths are supported:

- Source install: `cargo install --path crates/sigil --locked`
- Local release archive: `scripts/build-release-archive.sh`

The release archive script builds `sigil` in release mode, injects git commit, target, and profile build metadata, runs `sigil --version` and `sigil doctor` smoke checks against the built binary, then writes `dist/sigil-<version>-<target>.tar.gz` and a matching `.sha256` file. The archive payload contains the `sigil` binary, README files, `assets/logo/*`, and installation docs so repository-relative README logo paths still work after extraction.

The GitHub release workflow lives at `.github/workflows/release.yml`. On `v*` tags or manual dispatch with an existing tag, it builds release archives on Linux, macOS, and Windows runners, generates GitHub artifact provenance attestations, aggregates checksums, generates release notes from Conventional Commits, renders a `sigil.rb` Homebrew formula asset, and publishes the GitHub release through `gh release create`. The maintainer runbook lives in [`release-process.md`](release-process.md).

Synchronizing an independent Homebrew tap and self-update remain future work.

## Current MCP Implementation

MCP servers are configured through `[[mcp_servers]]`. Current support includes:

- stdio startup
- `initialize`
- `tools/list`
- `tools/call`
- `resources/list`
- `resources/read`
- `prompts/list`
- `prompts/get`
- provider-visible name sanitization, truncation, and hash de-duplication
- `roots/list`
- `elicitation/create`
- `notifications/progress`
- `notifications/*/list_changed`
- lazy activation
- TUI eager MCP background activation; one server failure does not block ordinary tasks
- required / optional server failure policy
- trust class
- per-server approval default
- egress logging
- secret egress blocking
- pinned identity validation

`resources/list` / `resources/read` and `prompts/list` / `prompts/get` are registered as provider-visible read-only tools only when the server declares the matching initialize capability. They reuse MCP trust policy, permission subjects, egress logging, and secret egress blocking, and they are never injected into the system prompt.

MCP tool/resource/prompt outputs are redacted locally and bounded by default output limits. `ToolResultMeta` records truncation data plus MCP server/tool/trust/operation metadata.

`roots/list` exposes only the resolved workspace root. `notifications/progress` updates the TUI live panel instead of writing repeated timeline entries. `notifications/tools|resources|prompts/list_changed` marks the server stale and refreshes that server's provider-visible tools at an idle worker boundary.

TUI elicitation uses a modal to let users confirm flat primitive object fields. The default non-interactive runtime returns explicit unsupported responses. Elicitation decisions are written to append-only control state, but user-provided values are not stored.

## Current Code Intelligence Implementation

Code intelligence is disabled by default. When enabled, runtime registers read-only tools:

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`
- `code_actions`

It also registers write tools that require a diff approval:

- `code_action`
- `code_rename`

When `code_intelligence.discovery.enabled = true`, Sigil discovers Rust, TypeScript/JavaScript, Python, and Go from workspace markers / file extensions, and only includes built-in allowlist servers available on `PATH`. Rust projects use `rust-analyzer` by default; without an available LSP server, they fall back to Tree-sitter Rust outline / syntax diagnostics.

Tool results are bounded by `max_results` and `max_payload_bytes`, with truncation metadata written into results.

## Development Checks

Code changes should run the relevant repository gates:

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

The coverage gate requires workspace unit-test line coverage `>= 96%` and is executed through `scripts/coverage.sh`.

Local pre-commit hook:

```bash
git config core.hooksPath .githooks
```

Run staged coverage check directly:

```bash
./scripts/check-staged-coverage.py
```

The staged gate reads the staged source snapshot before calculating added-line coverage. Recognized `enum`, `struct`, and `union` declaration lines are excluded from the executable-line denominator even when LCOV reports zero-count line records for them.

To keep local commits fast, the staged gate generates LCOV only for packages containing staged business-code files; full workspace coverage still belongs to explicit `./scripts/coverage.sh` runs and CI.

The staged coverage script has focused Python unit tests for diff classification, LCov parsing, and added-line coverage calculation:

```bash
python3 -m unittest scripts/test_check_staged_coverage.py
```

## Documentation Split

- `README.md`: English user entrypoint.
- `README.zh-CN.md`: Chinese user entrypoint.
- `docs/en/*`: English user documentation.
- `docs/zh-CN/*`: Chinese user documentation.
- `site/*`: GitHub Pages static site source, published by `.github/workflows/pages.yml`.
- `assets/logo/*`: brand PNG assets for README, release pages, package listings, and social previews.
- `dev/governance/*`: directly binding development governance documents.
- `dev/docs/*`: architecture, roadmap, design evolution, and implementation notes.
- `AGENTS.md`: in-repository agent collaboration instructions.
