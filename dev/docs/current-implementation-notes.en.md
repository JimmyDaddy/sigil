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
- `sigil` provides the `sigil` binary: no subcommand starts the TUI directly; `run` and `doctor` remain explicit automation and diagnostics subcommands; `prefix` and `fim` remain hidden debugging or provider-specific entrypoints rather than normal user concepts.
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

## User Interaction State

The TUI is currently chat-first:

- The inline viewport fills the visible terminal area.
- The left main area shows live transcript and the bottom composer.
- The right `Info rail` shows `Session / Permissions / Agents / LSP / Usage / Controls`.
- Narrow terminals collapse the info rail to keep chat/composer usable.
- When restoring an old session at startup, full scrollback is seeded into terminal scrollback in batches to avoid replaying a long session in a single frame.
- After prompt submission, the composer clears and remains visible.
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
- Skill index snapshots and loaded-skill summaries now have `SkillIndexCaptured` / `SkillLoaded` control entries and are projected through `Session::skill_state_projection`; runtime discovery now covers `.sigil/skills`, `.sigil/agents`, `.claude/skills`, `.claude/agents`, and optional user skills, including frontmatter parsing, shadowing warnings, hashing, and invalid path/name rejection; internal read-only `load_skill` validates enabled/trusted/model-invocable state and permission policy, reads only the skill entrypoint, injects the loaded skill body as current-run transient context, and appends a `SkillLoaded` control entry; the TUI `/config` Skills section displays discovered skills, model/user invocable flags, run mode, trust, source/hash, paths, and tool scope, and can submit footer-driven load or argument-bearing invoke requests; plugin manifest discovery now covers `.sigil/plugins/<id>/plugin.toml`, and the TUI `/config` Plugins section displays manifest path, id/name/version, skills/hooks/MCP commands, hash, and execution implications, with footer approve/deny actions that append `PluginManifestCaptured` and `PluginTrustDecision` control entries; non-user-message injection and child-session scheduling for user direct invocation remain later P1 phases.
- Terminal task handles, statuses, and output preview summaries have a dedicated control entry and `Session::terminal_task_projection`; terminal tool metadata is mirrored into append-only `TerminalTask` control entries, and the TUI restores them as activity cards, shows the running terminal count in the info rail, and can cancel a focused running terminal card with `Alt-X` confirmation through the worker `terminal_cancel` path while preserving execution audit entries.
- `sigil-tools-builtin` now has a terminal process manager: the default non-PTY backend writes output under `.sigil/tasks/<task-id>/{meta.json,output.log,stdout.log,stderr.log}` with bounded read, status, and cooperative cancel support; explicit `terminal_start` `pty=true` uses the `portable-pty` backend, runs a dedicated blocking read thread for the combined artifact log, and supports bounded-queue `terminal_input`, `terminal_resize`, and cancel. Each terminal input is capped at 8 KiB, and permission/audit metadata records only the task id and input byte count, not raw stdin; input/resize on non-PTY tasks still returns structured unsupported results.
- Tool executions that started without a terminal record are marked `interrupted` on restore.
- Dangling tool calls are projected as structured `interrupted` tool results.
- Historical file-change result cards are restored with the session.
- Compaction only appends `CompactionApplied` control records and does not rewrite old history.
- Hard-threshold automatic compaction runs only after a run returns to idle; it does not preempt streaming execution.

After restore, the next request recovers the latest matching provider response handle. The current session identity is not silently rewritten when `/config` saves new default provider/model settings.

Planned tasks are not auto-replayed on restore. When the current session has an unfinished task, normal composer input triggers `ContinueTask` with that input as continuation guidance; if only completed/cancelled tasks remain, normal input returns to chat-first behavior. `/plan continue` remains an explicit continue entry with no extra guidance. The worker resumes the latest unfinished task from the durable task projection and skips completed steps; if an explicit continue is requested when only completed/cancelled tasks remain, it returns the corresponding terminal-state explanation.

## Current Task Planning Implementation

Planned tasks enter through TUI `/plan <task>`. When the current session has an unfinished task, normal composer input is converted into a `ContinueTask` attempt and injected into the current executor/subagent step prompt as continuation guidance. Without unfinished task context, normal composer input remains chat-first. Ordinary chat does not expose a direct `task` / `subagent` launcher; if the model mistakenly calls those tools, the agent loop returns guidance telling it to use `/plan` and `task_plan_update` step roles for delegated work, without pretending a child task was launched. The worker protocol uses `SubmitTask` / `ContinueTask` commands and `TaskRunStarted` / `TaskRunFinished` messages; task run / step / child-session control entries are also streamed to the TUI through live `RunEvent::Control` updates. The info rail renders task status, latest plan version, completion progress, the current or last failed step, and a compact summary of the current plan steps from durable task control entries. Step rows use status markers and matching text colors: running is highlighted, completed is green, failed/blocked is red, cancelled/interrupted is gold, and pending is muted.

`sigil-kernel::SequentialTaskOrchestrator` runs a planner role first, accepts plan updates through the internal `task_plan_update` tool, then executes steps sequentially. Executor steps run against the parent session with transient step context so plan prompts do not become ordinary user history. Subagent read/write steps run in child sessions, and the parent session records child-session lifecycle links plus approval and MCP elicitation route summaries for child interactions.

When a step encounters an ordinary tool error but the agent reads that error and still produces a final answer, the orchestrator treats the step as recovered and continues later steps, while preserving a `recovered tool error` summary in `TaskStepEntry.reason`. Max turns, interrupted tool calls, approval denial, and permission-class tool errors still stop the task.

Role-specific providers and run options are assembled in `sigil-runtime`. Planner and subagent-read default to a read-only scoped tool registry; executor defaults to the full registry; subagent-write uses the full registry only when `[task].allow_write_subagents = true`. `ScopedToolRegistry` gates specs, preview, execution, permission hooks, and egress hooks.

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

TUI `/config` exposes only high-frequency provider fields, permissions, memory, compaction, code intelligence controls, terminal mouse/OSC52/scroll sensitivity compatibility settings, the Skills browser, Plugins trust review, and common MCP server fields. It can switch between `deepseek` and `openai_compat`; DeepSeek FIM is shown as a provider-specific advanced field, while OpenAI-compatible marks it unsupported. Lower-frequency provider-specific fields remain available through config files and environment variables. The Skills browser runs discovery against the current workspace/config, supports PgUp/PgDn skill switching, shows trust/source/hash/run mode/invocable/tool scope/path patterns, and uses footer load/invoke actions to submit requests still governed by the runtime `load_skill` policy. Plugins review uses the current session control projection for existing trust decisions, supports PgUp/PgDn plugin switching, and appends the reviewed manifest snapshot plus trust decision to the current session JSONL.

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
