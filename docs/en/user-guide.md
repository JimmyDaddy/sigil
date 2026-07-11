# Sigil TUI User Guide

[Docs home](README.md) · [Quickstart](quickstart.md) · [Workflows](workflows.md) · [Reference](reference.md) · [简体中文](../zh-CN/user-guide.md)

This guide is for day-to-day Sigil users. It focuses on what you see and control in the TUI.

If you are using Sigil for the first time, read [Quickstart](quickstart.md) first. If you already know the UI and want prompt patterns for real tasks, use [Common Workflows](workflows.md).

## Start

Start the TUI:

```bash
sigil
```

If no usable config exists, Sigil opens Quick Setup. You confirm the workspace, choose a model, and provide authentication. After setup, Sigil writes `workspace.root = "."`, so the directory where you started the TUI becomes the active workspace.

If you have not installed Sigil yet, see [Installation](installation.md). During development inside a checkout, `cargo run -p sigil` is equivalent.

For authentication options, including environment variables, see [Sigil Configuration Guide](configuration.md).

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
| Focus follow-up panel | `Tab` from the composer when a follow-up is pending |
| Run selected follow-up next | `Enter` on a selected follow-up uses the safe `next` action by default |
| Change follow-up action | `Tab` while the follow-up panel is focused; choose `Interrupt` only when you intend to stop the current run |
| Expand or collapse thinking / activity | `Ctrl-T` |

When the composer is focused, `Up/Down` first handles prompt history or cursor movement inside multiline input. `Ctrl-J` inserts a newline reliably; `Shift-Enter` and `Alt-Enter` also insert a newline when terminal keyboard enhancement is active and reports those modifiers. `Ctrl-Z` restores the last non-empty draft cleared with `Esc`; it is a single draft restore, not a general undo stack.

When `[terminal].mouse_capture = true`, mouse mode supports transcript scrolling, composer cursor placement, approval controls, slash candidates, setup/config rows, session selection, activity selection, and tool card header or hidden-preview expand/collapse. Drag across transcript text to select by displayed columns, then press `Ctrl-C` to copy the selection through OSC52 when clipboard integration is enabled.

Use the `Terminal` section in `/config` to review keyboard enhancement, mouse capture, OSC52 copy, and scroll sensitivity. Edit `sigil.toml` for compatibility overrides.

For terminal-specific smoke checks and tmux/SSH guidance, see [Terminal Compatibility Checklist](terminal-compatibility.md).

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open the TUI config panel |
| `/doctor` | Run local setup and appearance diagnostics with a summary and remediation list |
| `/resume` | Select and restore a previous session |
| `/agent <main|child-id>` | Switch the main chat area between the parent session and child agent transcripts |
| `/agent rename <child-id|current> <name>` | Persist a short display name for a child agent transcript |
| `/agent cancel <child-id|current>` | Cancel a running background child agent that still has a live runtime handle |
| `/queue` | Advanced follow-up controls |
| `/queue next|interrupt|edit|delete [item]` | Keep a follow-up for the next turn, interrupt and run it now, edit it, or cancel it |
| `/plan` / `/plan <prompt>` | Enter plan mode or run one read-only planning prompt; structured plans can be accepted into a durable task |
| `/task <task>` | Create a durable plan and execute the task step by step |
| `/task continue` | Continue the latest planned task without extra guidance |
| `/model <flash|pro|id>` | Switch the next run's model and start a fresh session |
| `/effort <low|medium|high|max>` | Switch the next run's reasoning effort |
| `/compact` | Manually compact the provider-visible context for the current session |
| `/quit` | Quit the TUI |

`/model`, `/effort`, `/resume`, `/agent`, and `/queue` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute. `/agent rename` and `/agent cancel` also show child-agent candidates before the argument is completed.

When Sigil is already running, ordinary chat input becomes a visible Follow-ups item instead of being dropped or added immediately to the timeline or provider-visible history. Follow-up dispatch is FIFO after the active turn finishes; the normal user message is added when the item dispatches. `next` moves an item to the front for the next turn; `interrupt` stops the current run before dispatching the selected item. Agent mentions are not silently converted into main-thread follow-ups while the session is busy; wait for the current turn or use the dedicated agent messaging surface.

`/plan` creates a Plan ready card only when the planner returns a structured plan with at least one executable step. Plain review text or unstructured summaries remain ordinary assistant output and do not create a task approval surface. Press `Enter` on Plan ready to create and run the durable task, or `Esc` to discard it.

## Config Panel

The `/config` panel groups provider, permission, memory, compaction, code intelligence, terminal, appearance, Agents, Skills, Plugins, and MCP settings. In the `Appearance` section, `Enter` cycles built-in themes, syntax themes, and usage cost currency; theme drafts preview immediately, and `Ctrl-S` saves the selected preferences to `sigil.toml`. In the `Plugins` section, Sigil discovers workspace plugin manifests under `.sigil/plugins/<id>/plugin.toml`.

Use `PgUp/PgDn` to move between discovered plugins. The detail view shows the current trust state, manifest path, full manifest hash, skills, hook commands with args and approval mode, and MCP server commands with args, startup, and required status. Footer `approve` trusts the currently displayed manifest hash; footer `deny` disables that hash. Sigil refreshes the manifest before writing the review decision and appends the review to the session log.

## Planned Tasks

Normal composer input always stays chat-first and no longer auto-continues a durable task just because the current session has unfinished task state. Use `/task continue` or a task UI action when you want to continue a task. Use `/plan` or `/plan <prompt>` when you want a read-only planning answer before editing; press `Enter` on the plan-ready card only when you want Sigil to create and run a normal durable task from that plan. Use `/task <task>` when you want Sigil to break a larger request into durable steps before execution without a separate planning pass.

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
- Exiting the TUI prints the current session id and `sigil resume <session-id>` for command-line restore.
- Cancelling a run first closes admission for new provider, tool, process, socket, retry, redirect, and child-work effects, then waits up to a bounded cleanup deadline. The UI shows `Cancelling` while active work is converging; `Cancelled` means cleanup was confirmed, while `Interrupted` means the deadline expired or cleanup could not be proven.
- Cancelling a run does not discard messages and tool results already written to the log. `/queue interrupt` dispatches its follow-up only after the prior run reaches a durable cancellation terminal state.
- Tool executions that started but did not finish are shown as interrupted after restore.
- File-change activities are restored with their captured diff summaries.
- Saving new provider/model defaults in `/config` does not rewrite the identity of the current session.
- Before prompts, queued follow-ups, tool arguments, task/agent controls, or external URLs reach durable storage, Sigil writes a bounded safe projection. Exact carrier-like values remain process-local for the active provider call, prompt history, or queued dispatch; after restart, Sigil uses the safe projection or marks exact-only work stale/interrupted instead of reconstructing secrets or signed/query-bearing URLs.
- External material remains explicitly untrusted through compaction and recovery. Source records and claim citations are stored separately, and citations bind only to the final safe assistant text they support.

## Long Context and Compaction

The info rail shows the latest provider-reported prompt usage against the model context window. The `ctx` line labels whether the window came from provider metadata or `fallback_context_window_tokens`, and Sigil calculates soft and hard thresholds from that same window:

- Soft threshold: context pressure is getting high.
- Hard threshold: automatic compaction runs after the current run returns to idle.
- `/compact`: manually compact the current session's provider-visible context.
- If the window is unknown, configure `fallback_context_window_tokens` so the TUI can show percentages and threshold hints.

Compaction appends control records. It does not rewrite old history.

## Code Intelligence

Code intelligence is disabled by default. When enabled, Sigil registers code-query tools:

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`
- `code_actions`

It also registers LSP edit tools that require an approval diff before writing:

- `code_action`
- `code_rename`

Language servers with the default `trust_required = true` start only after this exact workspace has a durable `Trusted` decision in the current session. A missing, restricted, or denied decision blocks the LSP process, but does not block normal chat, file tools, or the Rust Tree-sitter fallback. Workspace trust does not approve `code_action` or `code_rename`; their diff approval remains separate.

In the TUI, `Alt-D` can run diagnostics over git changed source files. Results appear as a normal activity and the LSP section of the info rail keeps a summary.

If no LSP server is available, Rust projects can fall back to Tree-sitter Rust outline / syntax diagnostics. Failure does not block normal chat or tool calls.

See [Sigil Configuration Guide](configuration.md) for configuration.

## FAQ

### The TUI opens Quick Setup immediately

No usable config was found, or the config failed to load. Complete Quick Setup to enter the main screen.

### Should I write the API key into the config file?

For temporary use or CI, choose the provider first and use its variable from the [provider authentication map](providers.md#authentication-priority). If you save the key through Quick Setup or `/config`, it is stored as plaintext in the local config file. `doctor` reports that state as a warning with a remediation hint. Do not commit a real `sigil.toml`.

### What if my terminal has broken mouse or clipboard support?

Review the `Terminal` section in `/config`, or set `[terminal].keyboard_enhancement = "off"` / `[terminal].mouse_capture = false` / `[terminal].osc52_clipboard = false` / `[terminal].scroll_sensitivity = 3` in `sigil.toml`. Keyboard enhancement is resolved on the next launch; mouse capture changes apply on the next launch; OSC52 clipboard changes apply to the next copy action; scroll sensitivity controls mouse wheel row steps in transcript and approval diff views.

Run `/doctor` to see the detected terminal profile, multiplexer or remote layers, and clipboard bridge warnings.

### Why are subcommands limited?

Running `sigil` opens the TUI. Subcommands such as `sigil run` and `sigil doctor` are for automation, scripts, and debugging, not the full product surface.

### Why do some tools need approval?

Sigil's permission layer decides allow / ask / deny. File writes, command execution, and external tools are conservative by default so users can review previews and risks before key changes.
