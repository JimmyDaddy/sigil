# Sigil TUI User Guide

[Docs home](README.md) · [Quickstart](quickstart.md) · [Workflows](workflows.md) · [Reference](reference.md) · [简体中文](../zh-CN/user-guide.md)

This guide is for day-to-day Sigil users. It focuses on what you see and control in the TUI. Development constraints, crate boundaries, and testing rules live under `dev/governance/*`.

If you are using Sigil for the first time, read [quickstart.md](quickstart.md) first. If you already know the UI and want prompt patterns for real tasks, use [workflows.md](workflows.md).

## Start

Start the TUI:

```bash
sigil
```

If no usable config exists, Sigil opens Quick Setup. You confirm the workspace, choose a model, and provide authentication. After setup, Sigil writes `workspace.root = "."`, so the directory where you started the TUI becomes the active workspace.

If you have not installed Sigil yet, see [installation.md](installation.md). During development inside a checkout, `cargo run -p sigil` is equivalent.

For authentication options, including environment variables, see [configuration.md](configuration.md).

## Main Screen

The TUI is organized around these areas:

- Chat / transcript: user messages, assistant responses, thinking summaries, and tool activities.
- Composer: the bottom input area. It remains visible and clears after submit.
- Info rail: the right-side status area for session, permissions, model, LSP, usage, and controls.
- Activity: tool results such as file reads, searches, shell commands, file edits, and code diagnostics.
- Approval modal: a review card for tool calls that need confirmation, including summary, files, diff, and actions.

The main workflow is typing tasks directly in the composer. Slash commands are reserved for a small set of high-frequency control actions.

## Common Controls

| Action | Key |
| --- | --- |
| Open help | `F1` |
| Open slash command selector | `/` |
| Toggle right info rail compact/detail | `F2` |
| Scroll transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| Cycle default permission mode | `Shift-Tab` |
| Edit composer text | `Ctrl-A/E`, `Ctrl-B/F`, `Alt-B/F`, `Ctrl-K/Y`, `Ctrl-Z` |
| Cancel current run | `Ctrl-C` |
| Leave overlay or clear activity focus | `Esc` |
| Focus latest activity | `Ctrl-G` |
| Move between activities | `Alt-J` / `Alt-K` |
| Cycle visible agent transcript | Composer agent panel (`Down`, `Up/Down`, `Enter`), `Alt-A` / `Shift-Alt-A` |
| Focus queued input panel | `Down` from the last composer row when queued input is visible |
| Run selected queued input now | `Enter` on a selected queued input when the action is `now`; this interrupts the current run |
| Change queued input action | `Tab` while the queue panel is focused |
| Expand or collapse thinking / activity | `Ctrl-T` |

When the composer is focused, `Up/Down` first handles prompt history or cursor movement inside multiline input. `Shift-Enter`, `Alt-Enter`, and `Ctrl-J` insert a newline. `Ctrl-Z` restores the last non-empty draft cleared with `Esc`; it is a single draft restore, not a general undo stack.

When `[terminal].mouse_capture = true`, mouse mode supports transcript scrolling, composer cursor placement, approval controls, slash candidates, setup/config rows, session selection, activity selection, and tool card header or hidden-preview expand/collapse. Drag across transcript text to select by displayed columns, then press `Ctrl-C` to copy the selection through OSC52 when clipboard integration is enabled.

Use the `Terminal` section in `/config` to review keyboard enhancement, mouse capture, OSC52 copy, and scroll sensitivity. Edit `sigil.toml` for compatibility overrides.

For terminal-specific smoke checks and tmux/SSH guidance, see [terminal-compatibility.md](terminal-compatibility.md).

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open the TUI config panel |
| `/doctor` | Run local setup and appearance diagnostics with a summary and remediation list |
| `/resume` | Select and restore a previous session |
| `/agent <main|child-id>` | Switch the main chat area between the parent session and child agent transcripts |
| `/agent rename <child-id|current> <name>` | Persist a short display name for a child agent transcript |
| `/queue` | Focus queued input |
| `/queue next|now|edit|delete [item]` | Keep a queued input for the next turn, run it now, edit it, or cancel it |
| `/plan` / `/plan <prompt>` | Enter plan mode or run one read-only planning prompt |
| `/task <task>` | Create a durable plan and execute the task step by step |
| `/task continue` | Continue the latest planned task without extra guidance |
| `/model <flash|pro|id>` | Switch the next run's model and start a fresh session |
| `/effort <low|medium|high|max>` | Switch the next run's reasoning effort |
| `/compact` | Manually compact the provider-visible context for the current session |
| `/quit` | Quit the TUI |

`/model`, `/effort`, `/resume`, `/agent`, and `/queue` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute. `/agent rename` also shows child-agent candidates before the new name is typed.

When Sigil is already running, ordinary chat input and `/plan <prompt>` are queued instead of being dropped or added to provider-visible history. Queue dispatch is FIFO after the active turn finishes. `next` moves an item to the front for the next turn; `now` interrupts the current run before dispatching the selected item.

## Config Panel

The `/config` panel groups provider, permission, memory, compaction, code intelligence, terminal, appearance, Agents, Skills, Plugins, and MCP settings. In the `Appearance` section, `Enter` cycles built-in themes, syntax themes, and usage cost currency; theme drafts preview immediately, and `Ctrl-S` saves the selected preferences to `sigil.toml`. In the `Plugins` section, Sigil discovers workspace plugin manifests under `.sigil/plugins/<id>/plugin.toml`.

Use `PgUp/PgDn` to move between discovered plugins. The detail view shows the current trust state, manifest path, full manifest hash, skills, hook commands with args and approval mode, and MCP server commands with args, startup, and required status. Footer `approve` trusts the currently displayed manifest hash; footer `deny` disables that hash. Sigil refreshes the manifest before writing the review decision and appends the review to the session log.

## Planned Tasks

Normal composer input always stays chat-first and no longer auto-continues a durable task just because the current session has unfinished task state. Use `/task continue` or a task UI action when you want to continue a task. Use `/plan` or `/plan <prompt>` when you want a read-only planning answer before editing. Use `/task <task>` when you want Sigil to break a larger request into durable steps before execution.

Planned tasks use role-specific agents:

- Planner: reads context and writes the task plan.
- Executor: performs normal workspace changes for executor steps.
- Subagent read/write: runs delegated steps in child sessions and links the child session back to the parent task.

Task runs, plans, step status, child-session links, and subagent approval route summaries are stored as append-only control entries. The info rail shows the latest task status, plan version, and current step from that durable state. When child agents exist, the composer shows a compact agent panel under the input with each agent's status. Press `Down` from the last composer input row to focus that panel, use `Up/Down` to choose an agent, and press `Enter` to switch the main chat area. `Alt-A` / `Shift-Alt-A` still cycle the visible transcript between `main` and concrete child agents, and `/agent` picks a specific target. Child agent display names come from explicit plan metadata, then persisted `/agent rename` overrides, and otherwise fall back to generic role labels such as `read 1` or `write 1`.

Session restore only rebuilds the visible task state. It does not automatically continue unfinished work. Type the next instruction in the composer to continue the latest task with guidance, or use `/task continue` to continue without extra guidance.

## Approvals and File Changes

Read-only file and search tools usually run directly. File writes, edits, deletes, shell execution, and external MCP tools follow the permission policy and may require approval.

In an approval card, focus on:

- Summary: what the tool call is about to do.
- Files: which files may be affected.
- Diff: preview of file changes.
- Actions: allow or deny.

Use `Left/Right` to choose an action and `Enter` to confirm. `Y/N` shortcuts are also supported. If no decision is made for a long time, the request is denied automatically so the worker does not wait forever.

After a file-changing tool runs, the activity keeps a bounded diff. Large diffs are truncated and show how many lines were hidden.

## Sessions and Recovery

By default, session logs are written under Sigil's per-user state directory:

```text
<state-root>/workspaces/<workspace-id>/sessions/
```

Sigil stores session and control state as append-only JSONL. For users, this means:

- Restarting the TUI restores the latest session by default.
- Cancelling a run does not discard messages and tool results already written to the log.
- Tool executions that started but did not finish are shown as interrupted after restore.
- File-change activities are restored with their captured diff summaries.
- Saving new provider/model defaults in `/config` does not rewrite the identity of the current session.

## Long Context and Compaction

The info rail shows the latest provider-reported prompt usage against the model context window. The `ctx` line labels whether the window came from provider metadata or `fallback_context_window_tokens`, and Sigil calculates soft and hard thresholds from that same window:

- Soft threshold: context pressure is getting high.
- Hard threshold: automatic compaction runs after the current run returns to idle.
- `/compact`: manually compact the current session's provider-visible context.
- If the window is unknown, configure `fallback_context_window_tokens` so the TUI can show percentages and threshold hints.

Compaction appends control records. It does not rewrite old history.

## Code Intelligence

Code intelligence is disabled by default. When enabled, Sigil registers read-only code tools:

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`
- `code_actions`

It also registers LSP edit tools that require an approval diff before writing:

- `code_action`
- `code_rename`

In the TUI, `Alt-D` can run diagnostics over git changed source files. Results appear as a normal activity and the LSP section of the info rail keeps a summary.

If no LSP server is available, Rust projects can fall back to Tree-sitter Rust outline / syntax diagnostics. Failure does not block normal chat or tool calls.

See [configuration.md](configuration.md) for configuration.

## FAQ

### The TUI opens Quick Setup immediately

No usable config was found, or the config failed to load. Complete Quick Setup to enter the main screen.

### Should I write the API key into the config file?

For temporary use or CI, prefer an environment variable such as `SIGIL_API_KEY`. If you save the key through Quick Setup or `/config`, it is stored as plaintext in the local config file. `doctor` reports that state as a warning with a remediation hint. Do not commit a real `sigil.toml`.

### What if my terminal has broken mouse or clipboard support?

Review the `Terminal` section in `/config`, or set `[terminal].keyboard_enhancement = false` / `[terminal].mouse_capture = false` / `[terminal].osc52_clipboard = false` / `[terminal].scroll_sensitivity = 3` in `sigil.toml`. Keyboard enhancement and mouse capture changes apply on the next launch; OSC52 clipboard changes apply to the next copy action; scroll sensitivity controls mouse wheel row steps in transcript and approval diff views.

Run `/doctor` to see the detected terminal profile, multiplexer or remote layers, and clipboard bridge warnings.

### Why are subcommands limited?

Running `sigil` opens the TUI. Subcommands such as `sigil run` and `sigil doctor` are for automation, scripts, and debugging, not the full product surface.

### Why do some tools need approval?

Sigil's permission layer decides allow / ask / deny. File writes, command execution, and external tools are conservative by default so users can review previews and risks before key changes.
