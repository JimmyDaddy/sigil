# Command And Key Reference

[Docs home](README.md) · [简体中文](../zh-CN/reference.md)

This page collects user-facing commands, keys, paths, shared config sections, approval outcomes, and recovery facts. Provider selection and credentials stay in the provider documentation so this reference does not duplicate them.

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
| Request cooperative cancellation of the current run | `Ctrl-C` |
| Leave overlay or clear activity focus | `Esc` |
| Focus latest activity | `Ctrl-G` |
| Move between activities | `Alt-J` / `Alt-K` |
| Focus task verification | `Alt-V`; then `Enter` runs the exact action, `I` inspects evidence |
| Open latest checkpoint restore | `Ctrl-R` opens and loads the reverse-diff dialog; `Enter` restores controlled files, `F` forks conversation without changing files, `Esc` closes |
| Open actions for a saved session | Select a `/resume` candidate, then press `Ctrl-O` or right-click it |
| Cycle visible agent transcript | Composer agent panel (`Down`, `Up/Down`, `Enter`), `Alt-A`, `Shift-Alt-A` |
| Expand or collapse thinking / activity | `Ctrl-T` |
| Run code diagnostics for changed source files | `Alt-D` |
| Cancel focused running terminal task | `Alt-X` |
| Select or activate an MCP server in `/config` | `Enter` cycles the inspected server; `Down` enters footer actions; choose `activate` and press `Enter` to activate or refresh |

When the composer is focused, `Up/Down` first handles prompt history or cursor movement inside multiline input. If child agents exist, `Down` from the last composer input row focuses the composer agent panel. `Ctrl-Z` is a single draft restore for text cleared by `Esc`, not a general undo stack.

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open the TUI config panel |
| `/doctor` | Run local setup diagnostics inside the transcript |
| `/new` | Start a fresh session with the current provider and model |
| `/resume` | Select and restore a previous session; `Ctrl-O` or right-click opens its lifecycle actions |
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
| `/compact` | Review the V2 fold plan; confirm one manual apply when exact local admission is ready |
| `/quit` | Quit the TUI |

Aliases: `/m` for `/model`, `/e` for `/effort`, and `/q` or `/exit` for `/quit`.

Workspace trust is handled by the startup workspace trust gate, not a slash command. Trust decisions are recorded in the session audit log. They allow repository-local verification candidates to be promoted for task readiness and permit an exact-workspace LSP with `trust_required = true` to start. They do not grant shell, plugin, MCP, or file-write permissions, and LSP write tools still require diff approval.

`/model`, `/effort`, `/resume`, `/agent`, and `/queue` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute. `/agent rename` also shows child-agent candidates before the new name is typed.

## CLI Commands

| Command | Use |
| --- | --- |
| `sigil` | Open the TUI in the current workspace |
| `sigil doctor` | Run local diagnostics |
| `sigil run "<task>" [--output text|json|jsonl]` | Run a non-interactive task; machine modes keep stdout parseable |
| `sigil resume [session-id]` | Open the TUI and restore the latest or requested session; TUI exit prints a copyable resume command |
| `sigil serve` | Start the loopback-only, bearer-authenticated local HTTP/SSE service |
| `sigil --version` | Print the installed version |
| `sigil --config <path> doctor` | Run diagnostics with an explicit config file |

Subcommands are for automation, diagnostics, scripts, and setup checks. The full product surface is the TUI.

## Machine Output And Local Server

`sigil run --output json` writes one versioned terminal record to stdout. `--output jsonl` writes ordered versioned event records and then exactly one terminal result or error. Human progress and safe network-disclosure notices stay on stderr. Stable exit codes are `0` for success, `1` for execution failure, `2` for invalid invocation or configuration, and `130` for a cooperatively cancelled run.

The local server is an advanced interface for a local client; it does not replace the TUI. Start it with a token stored in an environment variable:

```bash
export SIGIL_HTTP_TOKEN="$(openssl rand -hex 32)"
sigil serve
```

The command prints the actual loopback address selected by the OS. `GET /health` is unauthenticated; `GET /openapi.json`, `GET /disclosures`, and every session, run, event, cancellation, or approval route require `Authorization: Bearer <token>`. V1 rejects non-loopback hosts, a missing token, and `--no-token` before opening a listener. It does not enable cookie auth, wildcard CORS, remote access, daemon auto-start, or multi-user isolation.

Run events replay durable history and then remain live until a terminal event, disconnect, stream gap, or server shutdown. `Last-Event-ID` resumes retained durable events; transient text and reasoning progress is best effort and has no replay id. `Ctrl-C` closes command admission, cooperatively cancels active runs, drains owned workers and connections, and then exits.

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
| User state root `workspaces/<workspace-id>/http-server-v1/` | Local-server recovery and audit data |
| User cache root `workspaces/<workspace-id>/tmp/` | Shell scratch directory exposed as `$SIGIL_SCRATCH_DIR` and shown as `cache/tmp` |
| User config `sigil.toml` | Default local machine config |
| `.sigil/agents/`, `.sigil/commands/`, `.sigil/skills/`, `.sigil/plugins/` | Optional workspace project assets |
| `SIGIL.md` | Stable workspace memory file |
| `AGENTS.md` | Agent collaboration instructions that Sigil can load as memory |
| `SIGIL.local.md` | Local-only memory file |

Do not commit real secrets in `sigil.toml` or local memory files. A workspace-root `sigil.toml` is not loaded by default; pass `--config <path>` explicitly if you need one for an experiment.

## Provider Setup

Use the [Provider guide](providers.md) for the supported provider values, model selection, and authentication priority. Each linked provider page owns its copyable config block and complete environment-variable list. Shared model-request timeout overrides are documented in [Advanced configuration](advanced-configuration.md#terminal-and-model-request-overrides).

## Common Config Sections

| Section | Purpose |
| --- | --- |
| `[workspace]` | Workspace root |
| `[agent]` | Shared agent settings |
| `[permission]` | Default approval policy |
| `[web]` | Stable search route, network policy, destination rules, and budgets |
| `[memory]` | Workspace memory loading |
| `[compaction]` | Context compaction thresholds |
| `[task]` | Planned task behavior and role settings |
| `[verification]` | Explicit user-approved verification checks |
| `[code_intelligence]` | LSP and code intelligence tools |
| `[terminal]` | Mouse, OSC52 clipboard, and scroll behavior |
| `[appearance]` | TUI theme, usage cost currency, and semantic color overrides |
| `[[mcp_servers]]` | Explicit `stdio` or user-root `streamable_http` MCP server configuration |

See [Sigil Configuration Guide](configuration.md) for examples.
Provider and model selection, `[providers.*]` blocks, and authentication variables are indexed in the [Provider guide](providers.md).

## Web Tool Inputs

| Tool | Required input | Important boundary |
| --- | --- | --- |
| `websearch` | `query`; optional `max_results` | Route resolution is provider-hosted, authoritative configured MCP, or bundled Exa. A failed selected route does not silently change destination. |
| `webfetch` | `source_id`; optional `format` (`markdown` or `text`) and `max_content_bytes` | Accepts only a session-local exact URL capability; it does not accept a novel raw `url` argument. |

Both tools are `Read` operations with an independent `NetworkRead` effect. `[web].network_mode = "deny"` blocks them even if the local permission mode is permissive. `ask` requires an explicit interactive action and therefore fails closed in headless/eager contexts that cannot ask.

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
- Cancellation is durable and exact-once per run scope: `cancel requested` is followed by either cleanup-confirmed `cancelled` or cleanup-unconfirmed `interrupted`; recovery never upgrades an unconfirmed cancellation to `cancelled`.
- Restore does not silently replay unfinished tools.
- `/new` starts a fresh append-only session log.
- `/resume` selects older sessions.
- A selected `/resume` row exposes resume, finalized-turn fork, safe export, pin/unpin, and exact delete preview through `Ctrl-O` or right-click. The modal keeps exclusive keyboard focus.
- The `Storage` section in `/config` exposes retention as an explicit preview and confirmation. Normal startup, runs, resume, and `sigil serve` never apply it automatically.
- Exiting the TUI prints the current session id and a `sigil resume <session-id>` command.
- `/task continue` continues the latest unfinished planned task when one exists.
