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

Text streamed before a tool call is treated as a collapsible thinking/progress preamble, not as a
completed assistant reply. The transcript therefore keeps one assistant reply for the final answer.

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
| Focus task verification | `Alt-V`; then `Enter` runs the exact action, `I` inspects evidence |
| Open checkpoint restore | `Ctrl-R` opens and loads the reverse-diff dialog; `Enter` restores controlled files, `F` forks the conversation without changing files, `Esc` closes |
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
| `/compact` | Review the V2 fold plan and confirm a verified manual compaction when local exact proof is available |
| `/quit` | Quit the TUI |

`/model`, `/effort`, `/resume`, `/agent`, and `/queue` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute. `/agent rename` and `/agent cancel` also show child-agent candidates before the argument is completed.

When Sigil is already running, ordinary chat input becomes a visible Follow-ups item instead of being dropped or added immediately to the timeline or provider-visible history. Follow-up dispatch is FIFO after the active turn finishes; the normal user message is added when the item dispatches. Before it sends, Sigil checks local context pressure for reporting. Automatic pre-turn compaction remains disabled, so it sends the prepared request unchanged rather than changing the active context boundary. If a restart or transport failure leaves it unclear whether a follow-up reached the model, Sigil marks it stale and never resends it automatically. `next` moves an item to the front for the next turn; `interrupt` stops the current run before dispatching the selected item. Agent mentions are not silently converted into main-thread follow-ups while the session is busy; wait for the current turn or use the dedicated agent messaging surface.

`/plan` creates a Plan ready card only when the planner returns a structured plan with at least one executable step. Plain review text or unstructured summaries remain ordinary assistant output and do not create a task approval surface. Press `Enter` on Plan ready to create and run the durable task, or `Esc` to discard it.

## Config Panel

The `/config` panel groups provider, permission, Web, memory, compaction, code intelligence, terminal, appearance, Agents, Skills, Plugins, and MCP settings. The `Web` section can enable/disable Web tools, cycle the independent network policy and search route, and disable the bundled Exa profile; advanced destination and budget limits remain in `sigil.toml`. In the `Appearance` section, `Enter` cycles built-in themes, syntax themes, and usage cost currency; theme drafts preview immediately, and `Ctrl-S` saves the selected preferences to `sigil.toml`. In the `Plugins` section, Sigil discovers workspace plugin manifests under `.sigil/plugins/<id>/plugin.toml`.

Use `PgUp/PgDn` to move between discovered plugins. The detail view shows the current trust state, manifest path, full manifest hash, skills, hook commands with args and approval mode, and MCP server commands with args, startup, and required status. Footer `approve` trusts the currently displayed manifest hash; footer `deny` disables that hash. Sigil refreshes the manifest before writing the review decision and appends the review to the session log.

## Web Search And Fetch

When `[web]` is enabled, Sigil exposes stable search through provider-hosted search or the `websearch` client tool. Before client query or remote transport egress, the TUI reserves a disclosure strip at the top of the live panel without covering the transcript, status band, or composer. MCP handshake, query, and tool-call disclosures for the same destination are aggregated into one continuous operation card; every underlying message still receives its own successful-frame receipt, and the card is removed only when the corresponding tool/activation finishes or the run ends. CLI `run` writes and flushes the same safe disclosure to stderr. Search results are external/untrusted and source metadata is shown in activity/audit views when available. Configure routes, limits, and opt-out controls in [Permissions and sandbox](permissions-and-sandbox.md#network-and-web-tools).

`webfetch` reads one exact HTTP(S) URL that Sigil already observed in the current session. The model passes a session-local `source_id`, not a newly invented raw URL. The model should use search snippets directly when they answer the task and call `webfetch` only when the user explicitly asks to read a page or one specific fact is missing; it must not fan out across search results by default. Read-only fetches do not ask per request when `network_mode = "allow"`; with `network_mode = "ask"`, `Allow session` applies only to the same read-only Web tool in the current session. User-message URLs, structured search results, provider-hosted sources, prior fetches, and cross-origin redirect targets can produce these capabilities. Query-bearing or signed URLs remain process-local and become unavailable after restart; Sigil never reconstructs them from the redacted display URL. Every fetch re-applies disclosure, SSRF/DNS rules, redirect limits, byte budgets, bounded decoding, and external/untrusted provenance.

## Planned Tasks

Normal composer input always stays chat-first and no longer auto-continues a durable task just because the current session has unfinished task state. Use `/task continue` or a task UI action when you want to continue a task. Use `/plan` or `/plan <prompt>` when you want a read-only planning answer before editing; press `Enter` on the plan-ready card only when you want Sigil to create and run a normal durable task from that plan. Use `/task <task>` when you want Sigil to break a larger request into durable steps before execution without a separate planning pass.

Planned tasks use role-specific agents:

- Planner: reads context and writes the task plan.
- Executor: performs normal workspace changes for executor steps.
- Subagent read/write: runs delegated steps in child sessions and links the child session back to the parent task.

Task runs, plans, step status, child-session links, and subagent approval route summaries are stored as append-only control entries. The info rail shows the latest task status, plan version, and current step from that durable state. When child agents exist, the composer shows a compact agent panel under the input with each agent's status. Press `Down` from the last composer input row to focus that panel, use `Up/Down` to choose an agent, and press `Enter` to switch the main chat area. `Alt-A` / `Shift-Alt-A` still cycle the visible transcript between `main` and concrete child agents, and `/agent` picks a specific target. Child agent display names come from explicit plan metadata, then persisted `/agent rename` overrides, and otherwise fall back to generic role labels such as `read 1` or `write 1`.

When a planned task needs verification, the task status band shows a compact Verification card with one recommended check and a short reason. It only offers a trusted task check or an approval that must happen before a check can run; it never starts a check by itself. Click the card or press `Alt-V` to focus it, then press `Enter` to run the exact rendered action. Press `I` to inspect the terminal reason, receipt, workspace snapshot, and any proven changeset/command/artifact link. Missing evidence is shown as `not linked`, not inferred. A queued or running check is shown as in progress instead of being recommended again.

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
- Current builds restore V2 session JSONL only. If Sigil finds the older raw session format, it reports the file as unsupported and leaves it unchanged; archive that file and start a new session. Sigil does not migrate the older format.
- Exiting the TUI prints the current session id and `sigil resume <session-id>` for command-line restore.
- Cancelling a run first closes admission for new provider, tool, process, socket, retry, redirect, and child-work effects, then waits up to a bounded cleanup deadline. The UI shows `Cancelling` while active work is converging; `Cancelled` means cleanup was confirmed, while `Interrupted` means the deadline expired or cleanup could not be proven.
- Cancelling a run does not discard messages and tool results already written to the log. `/queue interrupt` dispatches its follow-up only after the prior run reaches a durable cancellation terminal state.
- Tool executions that started but did not finish are shown as interrupted after restore.
- File-change activities are restored with their captured diff summaries.
- Saving new provider/model defaults in `/config` does not rewrite the identity of the current session.
- Before prompts, queued follow-ups, tool arguments, task controls, or external URLs are kept in session records, Sigil stores a redacted, size-limited description. Sensitive exact values remain only for active use and are not reconstructed after restart; if exact information is required again, Sigil asks you to retry the action.
- External material remains explicitly untrusted through compaction and recovery. Source records and claim citations are stored separately, and citations bind only to the final safe assistant text they support.

### Controlled checkpoints and conversation forks

When the latest completed turn contains controlled ordinary-file edits, press `Ctrl-R` to open its
restore dialog. Sigil immediately asks the worker to rebuild an exact preview from the durable log;
there is no separate first `Enter` step. The modal owns keyboard focus and directly shows the
reverse diff, file restore directions, conflicts, and excluded unknown side effects. If no durable
line diff was recorded, it shows current and restore-target hash evidence instead. Use
`Up/Down` or `PageUp/PageDown` (or the mouse wheel) to scroll, `Ctrl-R` to refresh, and `Esc` to
close without changing files. The composer draft is preserved and action keys are not inserted into
it while the dialog is open.

After the modal reaches `READY`, press `Enter` to restore the controlled files. While preview or
restore is in flight, duplicate actions are ignored. A completed restore closes the dialog and adds
a `RESTORED` result to the timeline; late responses from an older dialog request are ignored.

Restore checks the current file hashes and stored snapshots before its first write. A changed file,
missing or sensitive snapshot, different workspace, or stale preview blocks the whole operation.
After a successful restore, prior verification is stale and should be rerun. This operation restores
only controlled ordinary-file mutations: it does not undo shell commands, MCP/plugin effects,
network requests, databases, remote services, directories, renames, or symlinks.

Press `F` in a ready or blocked restore dialog to fork the conversation through that completed turn.
The fork becomes the active session and keeps only safe user/assistant/tool history plus rebound
source provenance; active approvals, tasks, queues, continuation handles, and mutation state are not
copied. The parent session stays append-only. Conversation fork does not isolate or restore the
workspace—both sessions still refer to the same files.

## Long Context and Compaction

The info rail shows the latest provider-reported prompt usage against the model context window. The `ctx` line labels whether the window came from provider metadata or `fallback_context_window_tokens`, and Sigil calculates soft and hard thresholds from that same window:

- Soft threshold: context pressure is getting high.
- Hard threshold: context pressure is critical. Automatic compaction apply is temporarily frozen while correctness fixes are in progress.
- `/compact`: opens a V2 fold review with fold, keep, and protection details. When the selected profile has an installed checksum-pinned tokenizer and exact local target-fit proof, `Enter` confirms one manual apply.
- If the window is unknown, configure `fallback_context_window_tokens` so the TUI can show percentages and threshold hints.

Opening the review is read-only: it never rewrites history or appends a compaction record, and it does not download a tokenizer. Applying is available only when the review says the target is ready; the confirmed apply appends the V2 lifecycle and activates the verified boundary without rewriting raw history. An unavailable review cannot apply.

To prepare a verified local DeepSeek V4 Flash tokenizer without changing any session state, run `sigil tokenizer install deepseek-v4-flash`. The command discloses its public network download before it starts. Installation enables local admission but does not itself alter a session.

Idle automation never preempts streaming work, queued input, model switching, or overflow recovery. While apply is frozen, it does not create a lifecycle record or alter the session context.

The guarded overflow apply path is also frozen. It does not count, compact, or retry a request while correctness fixes are in progress.

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
