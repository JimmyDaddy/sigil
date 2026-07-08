# Command And Key Reference

[Docs home](README.md) · [简体中文](../zh-CN/reference.md)

This page collects the user-facing commands, keys, paths, and environment variables that are spread across the longer guides.

## TUI Keys

| Action | Key |
| --- | --- |
| Open help | `F1` |
| Open slash command selector | `/` |
| Submit prompt or selected slash command | `Enter` |
| Toggle right info rail compact/detail | `F2` |
| Scroll transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| Cycle default permission mode | `Shift-Tab` |
| Insert composer newline | `Ctrl-J`; `Shift-Enter` / `Alt-Enter` when terminal keyboard enhancement is active and reports modifiers |
| Move composer cursor by line or character | `Ctrl-A/E`, `Ctrl-B/F`, `Left/Right` |
| Move composer cursor by word | `Alt-B/F`, `Ctrl-Left/Right` |
| Delete composer text | `Backspace/Delete`, `Ctrl-H`, `Ctrl-W`, `Ctrl/Alt-Backspace`, `Ctrl/Alt-Delete` |
| Kill/yank composer line tail | `Ctrl-K/Y` |
| Restore last draft cleared with Esc | `Ctrl-Z` |
| Cancel current run | `Ctrl-C` |
| Leave overlay or clear activity focus | `Esc` |
| Focus latest activity | `Ctrl-G` |
| Move between activities | `Alt-J` / `Alt-K` |
| Cycle visible agent transcript | Composer agent panel (`Down`, `Up/Down`, `Enter`), `Alt-A`, `Shift-Alt-A` |
| Expand or collapse thinking / activity | `Ctrl-T` |
| Run code diagnostics for changed source files | `Alt-D` |
| Cancel focused running terminal task | `Alt-X` |

When the composer is focused, `Up/Down` first handles prompt history or cursor movement inside multiline input. If child agents exist, `Down` from the last composer input row focuses the composer agent panel. `Ctrl-Z` is a single draft restore for text cleared by `Esc`, not a general undo stack.

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open the TUI config panel |
| `/doctor` | Run local setup diagnostics inside the transcript |
| `/new` | Start a fresh session with the current provider and model |
| `/resume` | Select and restore a previous session |
| `/agent <main|child-id>` | Switch the main chat area between the parent session and child agent transcripts |
| `/agent rename <child-id|current> <name>` | Persist a short display name for a child agent transcript |
| `/agent cancel <child-id|current>` | Cancel a running background child agent that still has a live runtime handle |
| `/queue` | Advanced follow-up controls |
| `/queue next|interrupt|edit|delete [item]` | Keep a follow-up for the next turn, interrupt and run it now, edit it, or cancel it |
| `/plan` / `/plan <prompt>` | Run a read-only planning prompt; accept the plan card to create and run a durable task |
| `/task <task>` | Create a durable plan and execute the task step by step |
| `/task continue` | Continue the latest planned task without extra guidance |
| `/model <flash|pro|id>` | Switch the next run's model and start a fresh session |
| `/effort <low|medium|high|max>` | Switch the next run's reasoning effort |
| `/compact` | Manually compact the provider-visible context for the current session |
| `/quit` | Quit the TUI |

Aliases: `/m` for `/model`, `/e` for `/effort`, and `/q` or `/exit` for `/quit`.

Workspace trust is handled by the startup workspace trust gate, not a slash command. Trust decisions are recorded in the session audit log. They allow repository-local verification candidates to be promoted for task readiness, but they do not grant shell, plugin, MCP, or file-write permissions by themselves.

`/model`, `/effort`, `/resume`, `/agent`, and `/queue` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute. `/agent rename` also shows child-agent candidates before the new name is typed.

## CLI Commands

| Command | Use |
| --- | --- |
| `sigil` | Open the TUI in the current workspace |
| `sigil doctor` | Run local diagnostics |
| `sigil run "<task>"` | Run a non-interactive automation task |
| `sigil resume [session-id]` | Open the TUI and restore the latest or requested session; TUI exit prints a copyable resume command |
| `sigil serve` | Validate HTTP/SSE adapter local bind/token defaults; HTTP routing is not implemented yet |
| `sigil --version` | Print the installed version |
| `sigil --config <path> doctor` | Run diagnostics with an explicit config file |

Subcommands are for automation, diagnostics, scripts, and adapter preflight checks. The full product surface is the TUI.

## Config Resolution

Sigil resolves config in this order:

1. `--config <path>`
2. `sigil.toml` in the user-visible Sigil config directory

Default user config path:

- `~/.sigil/sigil.toml`

## Important Paths

| Path | Meaning |
| --- | --- |
| User state root `workspaces/<workspace-id>/sessions/` | Default append-only session logs |
| User state root `workspaces/<workspace-id>/input-history.jsonl` | Composer input history |
| User state root `workspaces/<workspace-id>/artifacts/` | Terminal and changeset artifacts |
| User cache root `workspaces/<workspace-id>/tmp/` | Shell scratch directory exposed as `$SIGIL_SCRATCH_DIR` and shown as `cache/tmp` |
| User config `sigil.toml` | Default local machine config |
| `.sigil/agents/`, `.sigil/commands/`, `.sigil/skills/`, `.sigil/plugins/` | Optional workspace project assets |
| `SIGIL.md` | Stable workspace memory file |
| `AGENTS.md` | Agent collaboration instructions that Sigil can load as memory |
| `SIGIL.local.md` | Local-only memory file |

Do not commit real secrets in `sigil.toml` or local memory files. A workspace-root `sigil.toml` is not loaded by default; pass `--config <path>` explicitly if you need one for an experiment.

## Provider Environment Variables

Model request:

- `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS`

DeepSeek:

- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_STRICT_TOOLS_MODE`

OpenAI-compatible:

- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`

Anthropic:

- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`

Gemini:

- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_BASE_URL`

## Common Config Sections

| Section | Purpose |
| --- | --- |
| `[workspace]` | Workspace root |
| `[agent]` | Provider, model, tool timeout, optional max turns |
| `[providers.deepseek]` | DeepSeek provider settings |
| `[providers.openai_compat]` | OpenAI-compatible provider settings |
| `[providers.anthropic]` | Anthropic provider settings |
| `[providers.gemini]` | Gemini provider settings |
| `[permission]` | Default approval policy |
| `[memory]` | Workspace memory loading |
| `[compaction]` | Context compaction thresholds |
| `[task]` | Planned task behavior and role settings |
| `[verification]` | Explicit user-approved verification checks |
| `[code_intelligence]` | LSP and code intelligence tools |
| `[terminal]` | Mouse, OSC52 clipboard, and scroll behavior |
| `[appearance]` | TUI theme, usage cost currency, and semantic color overrides |
| `[[mcp_servers]]` | stdio MCP server configuration |

See [configuration.md](configuration.md) for examples.

## Approval Outcomes

| Outcome | Meaning |
| --- | --- |
| allow | Run the tool call |
| deny | Return a structured denial to the model |
| timeout | Deny automatically after waiting too long |
| approval_required | Headless mode needed a decision but could not ask interactively |

## Session Recovery Facts

- Session logs are append-only JSONL.
- Restarting restores visible session state.
- Started tools without terminal records are restored as interrupted.
- Restore does not silently replay unfinished tools.
- `/new` starts a fresh append-only session log.
- `/resume` selects older sessions.
- Exiting the TUI prints the current session id and a `sigil resume <session-id>` command.
- `/task continue` continues the latest unfinished planned task when one exists.
