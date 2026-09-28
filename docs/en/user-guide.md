<!-- public-doc-role: user-guide; authority: tui-daily-use-authority; sections: start,headless-and-local-api-workflows,main-screen,common-controls,rich-conversation-content,image-attachments,slash-commands,config-panel,web-search-and-fetch,planned-tasks,approvals-and-file-changes,sessions-and-recovery,long-context-and-compaction,code-intelligence; cta: open-reference -->

# Sigil TUI User Guide

[Docs home](README.md) · [Quickstart](quickstart.md) · [Workflows](workflows.md) · [Reference](reference.md) · [简体中文](../zh-CN/user-guide.md)

This guide covers the TUI you use every day. Use [Reference](reference.md) for complete command and key tables.

## Start

Run `sigil` inside the repository you want to work on. When configuration is missing, Quick Setup asks for the workspace, provider, model, and authentication. See [Installation](installation.md) if the command is not available and [Configuration](configuration.md) for repeatable setup.

## Headless And Local API Workflows

The TUI is the normal user surface. `sigil run` provides text, JSON, or JSONL output for scripts; unresolved approvals fail instead of opening a modal. `sigil serve` is an advanced authenticated, loopback-only interface for a trusted local client. Commands, authentication, output, and exit behavior are in [Machine output and local server](reference.md#machine-output-and-local-server).

## Main Screen

- **Transcript:** messages, assistant replies, and tool activity.
- **Composer:** the input area at the bottom.
- **Info rail:** session, permission, model, usage, code-intelligence, and control status when width allows.
- **Activity:** file reads, searches, commands, edits, diagnostics, and results.
- **Approval modal:** the action, affected files, preview, and decision for a risky tool call.

Type ordinary tasks in the composer. Use slash commands for a small set of control actions.

## Common Controls

Press `F1` or `/` for help and commands. `F2` shows or hides the info rail, while `Shift-F2` changes its detail. Use `Ctrl-G` for activity, `Alt-V` for task verification, `Ctrl-R` for the latest controlled restore, `Alt-S` for the current Intent Stack, and `Ctrl-T` to expand or collapse the current task list, thinking, or activity. A collapsed task list also exposes a clickable `+N more tasks` row. `Ctrl-C` cancels a run when no text is selected; `Esc` closes the current overlay. The complete key matrix lives in [Reference](reference.md#tui-keys).

The info rail is enabled by default when the terminal is wide enough. Its Git summary keeps the branch and total on the first row, then places staged, modified, untracked, conflict, and ahead/behind counts on a compact second row instead of truncating one long sentence. `F2` changes only the current run. To change the startup default, open `/config`, choose **Appearance**, toggle **Info rail**, and save with `Ctrl-S`; narrow terminals still collapse it automatically.

Drag across transcript text and release the mouse to copy immediately. Sigil attempts both the system clipboard and, when enabled, the OSC52 terminal bridge; either path can complete the copy. A successful copy clears the highlight, while a failed copy keeps it available for `Ctrl-C` retry. `Ctrl-L` copies an active selection first; with no selection, it copies the latest assistant reply. All of these paths use transcript content, so the info rail is excluded. With no selection, `Ctrl-C` keeps its normal cancel or exit behavior.

Mouse mode also supports scrolling, composer placement, approval controls, menus, session rows, activities, and tool-card expansion. Terminal-specific copy, keyboard, mouse, tmux, and SSH checks are in [Terminal compatibility](terminal-compatibility.md).

## Rich Conversation Content

Sigil preserves the original Markdown as the durable message and renders headings, lists, tables, task lists, links, code, inline math, display math, and closed Mermaid fences as presentation only. A malformed or still-streaming fence is isolated to the live tail; it never rewrites the saved response or hides the remaining conversation.

The desktop app renders formulas locally with KaTeX and shows closed Mermaid diagrams in a bounded local viewer. It does not load remote diagram assets or enable Mermaid links, callbacks, raw HTML, or scripts. Formula or diagram failures stay local to that block and retain copyable source.

The TUI keeps the same content order without pretending that a terminal can reproduce browser layout: formulas are labeled LaTeX source, and Mermaid is a diagram section with type, state, summary, and optional source. When no higher-priority `Ctrl-O` action is active, `Ctrl-O` toggles the latest diagram source; `Ctrl-L` still copies the raw latest assistant reply. Wide tables and code use a local content viewport instead of widening the whole transcript.

## Image Attachments

In Desktop, use **Attach image** to select a local PNG, JPEG, or WebP, or paste an image from the clipboard. The preview has a remove button; sending errors keep the text and images for retry. You can send images with or without text. Saved messages offer **View image** after reopening the conversation.

In TUI, from an idle composer, paste a local PNG, JPEG, or WebP path, or press `Ctrl-V` when the clipboard contains an image. Review the metadata chip before sending; select a chip with `Up`, move with `Left/Right`, and remove it with `Backspace` or `Delete`.

Each turn accepts up to 4 images, 8 MiB per image, 24 MiB total, and bounded dimensions. Direct messages and inline skills can carry images. Follow-up queues, native commands, plans, tasks, and agent sessions do not yet carry attachments; their drafts are kept instead of silently dropping images. Only recognized image-capable models accept them, including OpenAI Responses, Anthropic, Gemini, and recognized DeepSeek image-capable model aliases. DeepSeek `deepseek-flash` also accepts images through its official Responses and Anthropic-compatible endpoints; a custom endpoint does not inherit this capability. If a saved session refers to a missing local image, paste the original again or continue from a conversation that does not need it.

## Slash Commands

The most common control commands are:

- `/config` — change common settings.
- `/doctor` — diagnose setup, authentication, integrations, and terminal support.
- `/resume` — choose a saved session.
- `/plan <prompt>` — request a read-only plan before execution.
- `/task <task>` and `/task continue` — start or continue multi-step work.
- `/compact` — generate, validate, and activate a recoverable context checkpoint.
- `/update [check|refresh|apply]` — check or explicitly install an admitted update.
- `/feedback` — preview and save a local support report.
- `/quit` — close the TUI.

Model, agent, follow-up, and every other command form are listed in [Reference](reference.md#slash-commands).

When a run is active, ordinary input becomes a visible follow-up and the first pending item is already scheduled to run after the current turn. Focus the follow-up panel with `Tab`, or click an item and its `Run next`, `Interrupt`, `Edit`, or `Delete` action directly. Editing a follow-up temporarily uses the composer; cancelling or completing the edit restores any unsent draft, including its cursor position and attachments. Pressing `Run next` while a new item is still being saved is acknowledged immediately; when a later or paused item needs reordering, the action is forwarded as soon as its durable queue id is confirmed. `Run next` also resumes a paused queue; use `Interrupt` only when you intentionally want to stop the current turn. Sigil does not resend a follow-up automatically when delivery is uncertain. On short terminals the composer collapses to three rows so a disappearing follow-up strip returns space to the transcript instead of leaving an oversized input panel.

Queue changes show a pending indicator until confirmed. Repeated clicks while a change is pending
do not send it again. Deleting an already deleted item is harmless; if it has already been consumed,
the queue explains that result. A failed queue change keeps the current Task and tool activity visible.

## Config Panel

`/config` groups common provider, permission, Web, memory, context, code-intelligence, terminal, appearance, agent, skill, plugin, and MCP settings. The per-model context-window field cycles through Automatic, 64K, 128K, 256K, and 1M instead of requiring a raw number; an existing custom value remains intact until you cycle the field. **Max output tokens** is an optional default for normal conversations: leave it Automatic or choose a preset / enter a positive value such as `8K`. Theme changes preview immediately; save changes with `Ctrl-S`. Exact fields and defaults belong in [Configuration Reference](configuration-reference.md).

In **MCP** settings, press `Ctrl-N` to choose a JSON file with `mcpServers`. Review the safe summaries, select entries with Space, and press Enter to add them to the draft. `Ctrl-S` saves through the normal configuration transaction. Import does not start servers or overwrite existing names. Inline environment/header values are omitted; command arguments are saved, so check them for secrets before selecting. Missing model credentials do not prevent saving this draft.

For a Streamable HTTP MCP server configured with OAuth, open its detail view and choose **Authentication**. The modal can show status, start sign-in, open or copy the authorization URL, accept a transient callback URL, refresh, sign out, or clear a retained local credential. See [MCP](mcp.md) before connecting a server.

## Web Search And Fetch

When enabled, search and fetch activity shows where data is going. Search results are external and untrusted. Fetch opens only a URL already observed in the current session and reapplies network limits. Route choice, opt-out, and destination rules are in [Permissions and sandbox](permissions-and-sandbox.md#network-and-web-tools).

## Planned Tasks

Use `/plan` for a read-only plan and accept its reviewable Plan card only when you want execution to begin. A readable Plan can be reviewed, revised, or saved without creating execution authority. Structured Plan submission improves the card, but is optional: when a model only returns complete prose, Sigil preserves that bounded text as the Plan instead of failing generation. **Run** atomically approves the exact Plan, creates one stable Task with a host-owned linear execution unit, and starts the runner immediately; it does not wait for the model to generate a Task DAG or another structured contract. Provider, permission, tool, or verification problems that occur during execution remain recoverable on the Task. Use `/task` when you want durable task execution directly. If its optional model planner cannot produce a valid structure, Sigil falls back to the same linear execution path instead of failing admission. With `routing_policy = "auto"`, the model can choose ordinary chat, plan review, or an available Task executor. Continuing an existing task requires a typed continuation decision.

Continuing a task preserves its identity and completed operations; a direct task does not need a new multi-step plan. Repeated observations or an unresolved failure across several operation batches first produce a reminder, then pause with a partial result. A pause does not mean the task is complete. Inspect the reason, repair the specific issue and continue, or provide new guidance. Long tests and waiting do not consume the allowance merely because time passes.

Open a Plan ready item to review the complete, immutable plan before choosing an action. The
workbench keeps every step, dependency, path, check, risk, and note reachable even on a short or
narrow terminal. Use the arrow keys, `PageUp`/`PageDown`, `Home`/`End`, and `Tab`/`Shift-Tab` to
navigate. `Esc` only closes the workbench; it never rejects the plan. Typing a printable character
returns to the composer and preserves that character, while `Shift-Tab` reopens the pending plan.
Run, Save for later, Revise, and Reject remain explicit actions bound to the exact plan id and hash.
Save for later keeps the draft without starting a Task. The workbench shows an operation as pending
until its result arrives, then displays its success or failure on the same page. Only currently
available actions appear in the keyboard hints.

Revise opens a **Plan revision** form with a multi-line editor and one **Submit revision** action.
The original plan remains reviewable while revision research runs, and a failed or cancelled revision restores its actions
instead of replacing it with an empty failed draft. In this form, `Enter` submits the revision and
`Shift-Enter` inserts a newline. Arrow keys and `Home`/`End` move the editing cursor; pasted text is
inserted at that cursor. `Esc` preserves the draft, and a failed submission keeps the text available
for retry. The Plan card and workbench show the same revision progress. An open review switches to
the new draft when it is ready; execution still requires an explicit Run action. Returning with `Esc`
keeps the current plan and revision draft without saving or running the plan. Questions from an agent use the same durable
attention area: Submit, Decline, and Cancel run are distinct actions; `Esc` closes the form without
answering it. Pending questions have no wall-clock timeout and are restored by `sigil resume`;
`Shift-Tab` returns to them from the composer. Sigil resumes the exact suspended continuation after
an accepted answer instead of replaying the provider turn that asked the question.

The task view shows steps, current status, child-agent work, and a Verification card when a check is needed. `Alt-V` focuses the card. Restoring a session shows the saved task state but never continues it automatically.

The release default is `auto / explicit_request_only`. Automatic Task routing uses the configured provider's tool capability and the available executor; it does not require a release manifest. Release qualification can enable proactive read-only agents by default for a matching new installation. Existing configurations stay unchanged. Set `routing_policy = "manual"` and `multi_agent_mode = "explicit_request_only"` to disable automatic handoff and proactive spawning.

Local Plan actions remain available after a model connection is removed. A failed review can be retried even before a draft exists. Revising after failure accepts new guidance; retrying an already accepted Run returns the same Task and preserves its progress.

## Approvals and File Changes

Read-only file and search tools usually run directly. Writes, deletes, commands, network access, and external tools follow the configured permission policy.

The approval modal centers the content that needs review: commands and tool requests use a dedicated high-contrast area, while file writes use the exact diff. Tool type and risk stay in a compact header; press `M` for policy, effect, and containment details. Requests without a file diff do not show empty panels.

Before allowing a risky action, check:

- what will run;
- which files or destination are involved;
- the visible diff or request preview;
- whether **allow**, **allow for this session**, or **deny** matches your intent.

Large diffs may be shortened in the activity view; inspect the final repository diff before committing.

## Sessions and Recovery

Session logs stay under the per-user Sigil state directory. A plain `sigil` launch always starts a fresh session, even when another Sigil window is already open in the same workspace. Resume is explicit: use `sigil resume` for the latest supported session, `sigil resume <session-id>` for an exact session, or `/resume` to choose one. Resume restores visible messages, task state, completed activity summaries, and interrupted tool results; it does not silently rerun an interrupted tool. Exiting prints the session id and the exact resume command.

Only one write-capable interactive surface may attach to a session at a time. If that session is already active in another TUI or Desktop run, Sigil keeps the current shell available and offers retry, a new session, or the session library. Close or leave the original owner before retrying; never delete an attachment sidecar or force a takeover.

Provider endpoint path corrections on the same trusted origin are rebound automatically when a session resumes. A changed origin, account/tenant boundary, missing connection, or older session without a proven trust binding requires one explicit route confirmation or replacement. Review and save the intended connection in `/config` (or Settings), or choose a replacement route; the same session id and portable transcript remain available while provider-private continuation state is discarded.

### Provider-only safe mode when authority is damaged

If startup detects a damaged or temporarily unavailable authority journal, Sigil reports an authority recovery problem instead of mislabeling it as provider failure. After confirming that the configuration itself is valid, the TUI offers **Ctrl-M provider-only safe mode**. This mode keeps only the current provider conversation in an ephemeral session; it does not read or write workspace, history, memory, cache, MCP, process, or tool state, and it never shows a fake `sigil ready` or `Thinking` state. Run `sigil doctor`, repair the authority, and restart to restore sessions, tools, approvals, and other durable features.

Cancellation stops new work and waits briefly for active work to finish. **Cancelled** means cleanup completed; **Interrupted** means it could not be confirmed within the limit. Messages and results already saved remain available.

### Manage saved sessions

Open `/resume` and select a row. `Enter` resumes it. `Ctrl-O` or right-click opens actions to fork the conversation, export a safe transcript, pin the session, or review deletion. Delete requires a second confirmation and applies only to the reviewed inactive file. Retention cleanup is an explicit action under `/config` → **Storage**; normal startup never deletes sessions automatically.

### Controlled checkpoints and conversation forks

Press `Alt-B` with an empty composer or activity focus to choose any completed turn in the current conversation. The selector shows the source session and a bounded prompt preview. `Up/Down` chooses a turn; `Enter` creates a branch through that turn and returns to an editable composer without sending anything. An existing draft stays intact. The original conversation and shared workspace files are unchanged; no checkpoint or file restore preview is required. While editing in the composer, `Alt-B` retains word navigation.

When the latest completed turn contains supported file edits, press `Ctrl-R` to review the reverse diff. `Enter` restores the reviewed files; `F` forks the conversation without changing files. A stale or changed file blocks the restore. Shell commands, remote services, directories, renames, symlinks, and other outside effects are not undone. Rerun verification after a successful restore.

Press `Alt-R` to review recorded changes and add comments. Choose a turn, then a file and old/new lines; Shift-Up/Down selects a range and Tab switches sides. Enter adds a comment to the batch, and `Ctrl-S` sends it through the current conversation or queues it while a run is active. The original draft stays intact. Changed or unavailable current files remain reviewable; comments do not grant write permission or restore files.

For recorded file changes, choose **Review changes** beside a checkpoint. Select an old/new line, Shift-click to extend the range, add comments, and send the batch. Comments on outdated or unavailable current files remain usable; Sigil includes the recorded source and requires fresh reads before editing. While a run is active the comments enter the normal follow-up queue. Failed admission preserves the batch and your regular draft.

To bring a conclusion back, select its source in `/resume`, press `Ctrl-O`, then `K`. Review the branch relations and completed conclusion, use `Up/Down` to choose and `PgUp/PgDn` to read, then press `Enter` to import it into the current conversation. Imported text is unverified knowledge and may omit details; it transfers no permissions or passed checks. The current draft stays intact and nothing is sent automatically.

In Desktop, open **Recovery controls → Branches and conclusions** to navigate parent/child conversations or search for a source conversation. Preview a completed conclusion, then choose **Import conclusion**. Importing keeps your draft, works during an active run, and supplies unverified reference material to future requests without running the model. It transfers no permissions or verification results.

### Intent Stack review

When the current session has accepted Intent Stack history, press `Alt-S` or run `/intents` to review each intent, its dependencies, verification state, retained artifacts, and conflicts. Select an intent with `Up/Down`; `D` creates an exact Drop preview, and `Enter` confirms only that reviewed preview. Shared, drifted, unavailable, read-only, or out-of-scope contributions remain visible but cannot be dropped. Shell, network, remote, and other unsupported side effects are never undone. A current session without accepted durable intent history shows an explicit unavailable state rather than a guessed stack.

## Long Context and Compaction

The info rail shows reported context use and warns as the model window fills. `/compact` directly generates, validates, and atomically activates one recoverable semantic checkpoint; there is no confirmation modal. On an admitted route this performs one billed semantic-summary request. Sigil shows progress and then an applied receipt or the exact refusal reason. A failed summary, token proof, economics check, or concurrent conversation change leaves the active context unchanged. If context size is unknown, set `fallback_context_window_tokens`. See [Advanced configuration](advanced-configuration.md) for settings and recovery guidance.

## Code Intelligence

When enabled, Sigil can use repository structure and an available language server for symbols, definitions, references, diagnostics, code actions, and rename previews. `Alt-D` runs diagnostics over changed source files. Editing actions still require a diff approval. If the language server is unavailable, normal chat and file tools continue to work. See [Advanced configuration](advanced-configuration.md#compaction-and-code-intelligence).

For setup symptoms, credential warnings, terminal problems, or integration failures, use [Troubleshooting](troubleshooting.md).

<!-- public-doc-cta: open-reference -->
Next: [Look up exact controls in Reference](reference.md).
