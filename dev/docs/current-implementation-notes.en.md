# Sigil Current Implementation Notes

[简体中文](current-implementation-notes.md)

This document records current repository implementation facts for developer alignment. The default English user entrypoint is `README.md`; the Chinese user entrypoint is `README.zh-CN.md`. User documentation is split by language under `docs/en/*` and `docs/zh-CN/*`.

## Repository Layout

```text
sigil/
  crates/
    sigil-kernel/              # Generic agent kernel and domain contracts
    sigil-provider-deepseek/   # DeepSeek provider implementation
    sigil-tools-builtin/       # Built-in tools
    sigil-code-intel/          # LSP client, Tree-sitter fallback, and code intelligence tools
    sigil-mcp/                 # stdio MCP client and tool adapter
    sigil-runtime/             # Shared provider / tool / run option assembly for entrypoints
    sigil-cli/                 # Thin CLI launcher and debugging entrypoint
    sigil-tui/                 # Primary user entrypoint
  docs/                        # User documentation
  dev/governance/              # Development constraints, code standards, engineering standards
  dev/docs/                    # Architecture, roadmap, design evolution, and implementation notes
  sigil.toml                   # Local config file, ignored by default
```

## Current Capability Baseline

- `sigil-kernel` owns generic provider, tool, session, approval, permission, event, memory, and compaction contracts.
- `sigil-runtime` assembles providers, built-in tools, MCP tools, and run options.
- `sigil-provider-deepseek` supports DeepSeek streaming chat, tool calls, reasoning replay, usage, pricing, Beta endpoints, prefix completion, and FIM-specific entrypoints.
- `sigil-tools-builtin` provides file read/write/edit/delete, search, directory listing, and shell execution.
- `sigil-code-intel` provides optional LSP / Tree-sitter code intelligence, including read-only symbol, definition, reference, and diagnostic tools.
- `sigil-mcp` supports stdio MCP servers, `initialize`, `tools/list`, `tools/call`, read-only `resources/list` / `resources/read`, read-only `prompts/list` / `prompts/get`, `roots/list`, elicitation handling, progress/listChanged runtime events, lazy activation, and trust enforcement.
- `sigil-cli` currently exposes the public `run` automation entrypoint and the `doctor` local diagnostics entrypoint; `prefix` and `fim` remain debugging or provider-specific entrypoints rather than normal user concepts.
- `sigil-tui` is the primary user entrypoint. It owns chat/composer, slash selector, Quick Setup, `/config`, `/doctor`, `/resume`, approval modal, tool activity, diff preview, session recovery, context compaction, markdown code block highlighting, and code intelligence status display.

## TUI Module Boundaries

`crates/sigil-tui/src/app.rs` remains the `AppState` facade. It owns fields, bootstrap, top-level key routing, and cross-state orchestration. Specific flows live under `src/app/*`:

- `input_flow.rs`
- `slash_flow.rs`
- `modal_flow.rs`
- `config_flow.rs`
- `setup_flow.rs`
- `session_flow.rs`
- `timeline_flow.rs`
- `tool_focus.rs`
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

Approval cards use fixed `Summary / Files / Diff / Actions` sections. Diff previews for `write_file`, `edit_file`, and `delete_file` support file switching, hunk navigation, and diff mode switching.

## Session and Control State

Default session logs are stored under:

```text
.sigil/sessions/
```

The current implementation uses append-only JSONL:

- Session identity is restored from the durable log instead of blindly falling back to the current config provider/model.
- Response handles, provider continuation state, prefix snapshots, compaction records, and usage snapshots are written into append-only control logs.
- Tool approval, execution lifecycle, and reasoning deltas append control records.
- Tool executions that started without a terminal record are marked `interrupted` on restore.
- Dangling tool calls are projected as structured `interrupted` tool results.
- Historical file-change result cards are restored with the session.
- Compaction only appends `CompactionApplied` control records and does not rewrite old history.
- Hard-threshold automatic compaction runs only after a run returns to idle; it does not preempt streaming execution.

After restore, the next request recovers the latest matching provider response handle. The current session identity is not silently rewritten when `/config` saves new default provider/model settings.

## Configuration and Provider

Root config is parsed by `sigil-kernel`:

- `[workspace]`
- `[session]`
- `[agent]`
- `[permission]`
- `[memory]`
- `[compaction]`
- `[code_intelligence]`
- `[providers.*]`
- `[[mcp_servers]]`

DeepSeek provider configuration lives under `[providers.deepseek]`. Runtime environment overrides are resolved in the provider config layer, with `SIGIL_API_KEY` taking highest priority and `DEEPSEEK_API_KEY` retained as a fallback source.

TUI `/config` exposes only high-frequency provider fields, permissions, memory, compaction, and common MCP server fields. Lower-frequency provider-specific fields remain available through config files and environment variables.

`sigil doctor` and TUI `/doctor` reuse runtime diagnostics to check config loading, workspace resolution, session log location, provider/auth source, MCP command/trust state, code intelligence LSP plan, and terminal `TERM`. Diagnostics report only the secret source, not secret values.

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

The staged coverage script has focused Python unit tests for diff classification, LCov parsing, and added-line coverage calculation:

```bash
python3 -m unittest scripts/test_check_staged_coverage.py
```

## Documentation Split

- `README.md`: English user entrypoint.
- `README.zh-CN.md`: Chinese user entrypoint.
- `docs/en/*`: English user documentation.
- `docs/zh-CN/*`: Chinese user documentation.
- `dev/governance/*`: directly binding development governance documents.
- `dev/docs/*`: architecture, roadmap, design evolution, and implementation notes.
- `AGENTS.md`: in-repository agent collaboration instructions.
