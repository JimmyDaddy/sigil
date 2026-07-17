<!-- public-doc-role: user-guide; authority: tui-daily-use-authority; sections: start,headless-and-local-api-workflows,main-screen,common-controls,image-attachments,slash-commands,config-panel,web-search-and-fetch,planned-tasks,approvals-and-file-changes,sessions-and-recovery,long-context-and-compaction,code-intelligence; cta: open-reference -->

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

Press `F1` or `/` for help and commands. `F2` shows or hides the info rail, while `Shift-F2` changes its detail. Use `Ctrl-G` for activity, `Alt-V` for task verification, `Ctrl-R` for the latest controlled restore, and `Ctrl-T` to expand or collapse thinking and activity. `Ctrl-C` cancels a run when no text is selected; `Esc` closes the current overlay. The complete key matrix lives in [Reference](reference.md#tui-keys).

The info rail is enabled by default when the terminal is wide enough. `F2` changes only the current run. To change the startup default, open `/config`, choose **Appearance**, toggle **Info rail**, and save with `Ctrl-S`; narrow terminals still collapse it automatically.

Drag across transcript text and press `Ctrl-C` to copy the selection when clipboard integration is available. `Ctrl-L` copies an active selection first; with no selection, it copies the latest assistant reply. Both use transcript content, so the info rail is excluded. With no selection, `Ctrl-C` keeps its normal cancel or exit behavior.

Mouse mode also supports scrolling, composer placement, approval controls, menus, session rows, activities, and tool-card expansion. Terminal-specific copy, keyboard, mouse, tmux, and SSH checks are in [Terminal compatibility](terminal-compatibility.md).

## Image Attachments

From an idle composer, paste a local PNG, JPEG, or WebP path, or press `Ctrl-V` when the clipboard contains an image. Review the metadata chip before sending; select a chip with `Up`, move with `Left/Right`, and remove it with `Backspace` or `Delete`.

Each turn accepts up to 4 images, 8 MiB per image, 24 MiB total, and bounded dimensions. Images cannot be queued or attached to plan, command, skill, task, or agent input. Only recognized image-capable OpenAI Responses, Anthropic, and Gemini models accept them. If a saved session refers to a missing local image, paste the original again or continue from a conversation that does not need it.

## Slash Commands

The most common control commands are:

- `/config` — change common settings.
- `/doctor` — diagnose setup, authentication, integrations, and terminal support.
- `/resume` — choose a saved session.
- `/plan <prompt>` — request a read-only plan before execution.
- `/task <task>` and `/task continue` — start or continue multi-step work.
- `/compact` — review a context-reduction proposal.
- `/feedback` — preview and save a local support report.
- `/quit` — close the TUI.

Model, agent, follow-up, and every other command form are listed in [Reference](reference.md#slash-commands).

When a run is active, ordinary input becomes a visible follow-up and normally runs after the current turn. Focus the follow-up panel with `Tab`; use its action selector only when you intentionally want to interrupt. Sigil does not resend a follow-up automatically when delivery is uncertain.

## Config Panel

`/config` groups common provider, permission, Web, memory, context, code-intelligence, terminal, appearance, agent, skill, plugin, and MCP settings. Theme changes preview immediately; save changes with `Ctrl-S`. Exact fields and defaults belong in [Configuration Reference](configuration-reference.md).

For a Streamable HTTP MCP server configured with OAuth, open its detail view and choose **Authentication**. The modal can show status, start sign-in, open or copy the authorization URL, accept a transient callback URL, refresh, sign out, or clear a retained local credential. See [MCP](mcp.md) before connecting a server.

## Web Search And Fetch

When enabled, search and fetch activity shows where data is going. Search results are external and untrusted. Fetch opens only a URL already observed in the current session and reapplies network limits. Route choice, opt-out, and destination rules are in [Permissions and sandbox](permissions-and-sandbox.md#network-and-web-tools).

## Planned Tasks

Use `/plan` for a read-only plan and accept the Plan ready card only when you want execution to begin. Use `/task` when you already want Sigil to split and run multi-step work. Ordinary chat stays chat-first; it does not continue an unfinished task by itself.

The task view shows steps, current status, child-agent work, and a Verification card when a check is needed. `Alt-V` focuses the card. Restoring a session shows the saved task state but never continues it automatically.

## Approvals and File Changes

Read-only file and search tools usually run directly. Writes, deletes, commands, network access, and external tools follow the configured permission policy.

Before allowing a risky action, check:

- what will run;
- which files or destination are involved;
- the visible diff or request preview;
- whether **allow**, **allow for this session**, or **deny** matches your intent.

Large diffs may be shortened in the activity view; inspect the final repository diff before committing.

## Sessions and Recovery

Session logs stay under the per-user Sigil state directory. On restart, Sigil can restore the latest supported session, including visible messages, task state, completed activity summaries, and interrupted tool results. It does not silently rerun an interrupted tool. Exiting prints the session id and a `sigil resume <session-id>` command.

Cancellation stops new work and waits briefly for active work to finish. **Cancelled** means cleanup completed; **Interrupted** means it could not be confirmed within the limit. Messages and results already saved remain available.

### Manage saved sessions

Open `/resume` and select a row. `Enter` resumes it. `Ctrl-O` or right-click opens actions to fork the conversation, export a safe transcript, pin the session, or review deletion. Delete requires a second confirmation and applies only to the reviewed inactive file. Retention cleanup is an explicit action under `/config` → **Storage**; normal startup never deletes sessions automatically.

### Controlled checkpoints and conversation forks

When the latest completed turn contains supported file edits, press `Ctrl-R` to review the reverse diff. `Enter` restores the reviewed files; `F` forks the conversation without changing files. A stale or changed file blocks the restore. Shell commands, remote services, directories, renames, symlinks, and other outside effects are not undone. Rerun verification after a successful restore.

## Long Context and Compaction

The info rail shows reported context use and warns as the model window fills. `/compact` opens a read-only review of what would be shortened and kept; apply only when the review says it is ready. If context size is unknown, set `fallback_context_window_tokens`. See [Advanced configuration](advanced-configuration.md) for settings and recovery guidance.

## Code Intelligence

When enabled, Sigil can use repository structure and an available language server for symbols, definitions, references, diagnostics, code actions, and rename previews. `Alt-D` runs diagnostics over changed source files. Editing actions still require a diff approval. If the language server is unavailable, normal chat and file tools continue to work. See [Advanced configuration](advanced-configuration.md#compaction-and-code-intelligence).

For setup symptoms, credential warnings, terminal problems, or integration failures, use [Troubleshooting](troubleshooting.md).

<!-- public-doc-cta: open-reference -->
Next: [Look up exact controls in Reference](reference.md).
