# Sigil TUI User Guide

[简体中文](../zh-CN/user-guide.md)

This guide is for day-to-day Sigil users. It focuses on what you see and control in the TUI. Development constraints, crate boundaries, and testing rules live under `dev/governance/*`.

## Start

Start the TUI:

```bash
cargo run -p sigil-tui
```

If no usable config exists, Sigil opens Quick Setup. You confirm the workspace, choose a model, and provide authentication. After setup, Sigil writes `workspace.root = "."`, so the directory where you started the TUI becomes the active workspace.

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
| Scroll transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| Cycle default permission mode | `Shift-Tab` |
| Cancel current run | `Ctrl-C` |
| Leave overlay or clear activity focus | `Esc` |
| Focus latest activity | `Ctrl-G` |
| Move between activities | `Alt-J` / `Alt-K` |
| Expand or collapse thinking / activity | `Ctrl-T` |

When the composer is focused, `Up/Down` first handles prompt history or cursor movement inside multiline input.

Mouse mode supports transcript scrolling, approval controls, slash candidates, setup/config rows, session selection, and activity selection when your terminal supports mouse capture. Drag across transcript text to select by displayed columns, then press `Ctrl-C` to copy the selection through OSC52 when clipboard integration is enabled.

For terminal-specific smoke checks and tmux/SSH guidance, see [terminal-compatibility.md](terminal-compatibility.md).

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open the TUI config panel |
| `/doctor` | Run local setup diagnostics |
| `/resume` | Select and restore a previous session |
| `/model <flash|pro|id>` | Switch the next run's model and start a fresh session |
| `/effort <low|medium|high|max>` | Switch the next run's reasoning effort |
| `/compact` | Manually compact the provider-visible context for the current session |
| `/quit` | Quit the TUI |

`/model`, `/effort`, and `/resume` show candidates. Use `Up/Down` to select, `Tab` to accept, and `Enter` to execute.

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

By default, session logs are written under the workspace:

```text
.sigil/sessions/
```

Sigil stores session and control state as append-only JSONL. For users, this means:

- Restarting the TUI restores the latest session by default.
- Cancelling a run does not discard messages and tool results already written to the log.
- Tool executions that started but did not finish are shown as interrupted after restore.
- File-change activities are restored with their captured diff summaries.
- Saving new provider/model defaults in `/config` does not rewrite the identity of the current session.

## Long Context and Compaction

The info rail shows current context usage. Sigil calculates soft and hard thresholds from the model context window or the configured fallback window:

- Soft threshold: context pressure is getting high.
- Hard threshold: automatic compaction runs after the current run returns to idle.
- `/compact`: manually compact the current session's provider-visible context.

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

Use the `Terminal` section in `/config`, or set `[terminal].mouse_capture = false` / `[terminal].osc52_clipboard = false` / `[terminal].scroll_sensitivity = 3` in `sigil.toml`. Mouse capture changes apply on the next launch; OSC52 clipboard changes apply to the next copy action; scroll sensitivity controls mouse wheel row steps in transcript and approval diff views.

Run `/doctor` to see the detected terminal profile, multiplexer or remote layers, and clipboard bridge warnings.

### Why is the CLI small?

The TUI is the normal user entrypoint. The CLI is currently for automation, scripts, and debugging, not the full product surface.

### Why do some tools need approval?

Sigil's permission layer decides allow / ask / deny. File writes, command execution, and external tools are conservative by default so users can review previews and risks before key changes.
