# Troubleshooting

[Docs home](README.md) · [简体中文](../zh-CN/troubleshooting.md)

Start with the built-in diagnostics whenever setup, authentication, MCP, code intelligence, or terminal behavior looks wrong:

```bash
sigil doctor
```

Inside the TUI:

```text
/doctor
```

The report shows a status summary, warnings/errors, and remediation lines. It reports where credentials were resolved from, but it does not print secret values.

For a structured report that is easier to attach to a private or public support request, run:

```bash
sigil doctor --output json
```

Both forms are generated locally and make no provider request.

## Decision Tree

Start here when you know the symptom:

| Symptom | Check first | Then read |
| --- | --- | --- |
| Sigil opens Quick Setup every time | Config resolution and load errors in `sigil doctor` | [Quick Setup Opens Every Time](#quick-setup-opens-every-time) |
| Provider authentication fails | API key source in `sigil doctor` | [Sigil Cannot Find The API Key](#sigil-cannot-find-the-api-key) |
| Sigil reads or edits the wrong repository | Workspace path in `/doctor` | [The Wrong Workspace Is Being Used](#the-wrong-workspace-is-being-used) |
| A file path is denied | Workspace confinement and symlink target | [A File Tool Cannot Access A Path](#a-file-tool-cannot-access-a-path) |
| `sigil run` says approval is required | Headless mode cannot show approval cards | [A Tool Needs Approval In Headless run](#a-tool-needs-approval-in-headless-run) |
| Approval disappeared or was denied | Timeout or deny action | [An Approval Was Denied Or Timed Out](#an-approval-was-denied-or-timed-out) |
| Mouse or copy does not work | Terminal section in `/config` and `/doctor` | [Mouse Or Clipboard Does Not Work](#mouse-or-clipboard-does-not-work) |
| Theme colors are hard to read | Appearance warnings in `sigil doctor` or `/doctor` | [Theme Colors Are Hard To Read](#theme-colors-are-hard-to-read) |
| Restored session shows interrupted tools | Recovery projected unfinished tools | [Session Restore Shows Interrupted Tools](#session-restore-shows-interrupted-tools) |
| MCP tools are missing | Server startup mode and lifecycle state | [MCP Server Is Missing, Failed, Or Deferred](#mcp-server-is-missing-failed-or-deferred) |
| LSP tools are unavailable | Code-intelligence readiness rows | [Code Intelligence Is Not Ready](#code-intelligence-is-not-ready) |

## Quick Setup Opens Every Time

Likely causes:

- No config file exists in the current resolution path.
- The config file exists but failed to load.
- The workspace or provider fields are incomplete.

Check:

```bash
sigil doctor
```

If you use a non-default config path, pass the same path to doctor:

```bash
sigil --config ./sigil.toml doctor
```

## Sigil Cannot Find The API Key

1. Open the [provider authentication map](providers.md#authentication-priority) and select the provider configured in `[agent].provider`.
2. Follow that provider page's copyable environment-variable command in the same shell that launches `sigil`.
3. Run `sigil doctor` again and confirm that the provider and key source are the ones you intended.

Sigil deliberately ignores common generic credential names that could share state with other tools. The relevant provider page is the source of truth for accepted authentication variables and config fallbacks.

If you saved a key in `/config`, it is stored as plaintext in `sigil.toml`. That can be acceptable for a private local config, but do not commit it.

## Theme Colors Are Hard To Read

Run `sigil doctor` or `/doctor` and check `appearance:*` warnings. These checks cover user-visible text/surface contrast, semantic color separation, and structural cues such as borders against nearby surfaces.

Remove or edit the listed `[appearance.colors]` entries so the warning's token pair has stronger contrast or clearer separation. Switching themes in `/config` can help when no overrides remain, or when the existing overrides are compatible with the new built-in theme.

## The Wrong Workspace Is Being Used

With the normal setup:

```toml
[workspace]
root = "."
```

`.` resolves to the directory where you launched `sigil`, not the directory that contains the config file.

Fix:

```bash
cd /path/to/the/repository
sigil
```

Run `/doctor` and check the workspace path in the report.

## A File Tool Cannot Access A Path

Sigil confines file tools to the workspace root. It rejects:

- absolute paths outside the workspace;
- paths using `..` to escape the workspace;
- symlinks that resolve outside the workspace.

If you intentionally need external-directory access, configure `[permission.external_directory]` and keep the default mode conservative.

## A Tool Needs Approval In Headless `run`

Interactive TUI sessions can show an approval modal. Headless `sigil run` cannot ask you interactively, so an `ask` decision is returned to the model as a structured `approval_required` tool error.

For automation, either keep the task read-only or define explicit permission rules for the narrow action you trust.

## An Approval Was Denied Or Timed Out

If no decision is made for a long time, Sigil denies the request so the worker does not wait forever.

When this happens:

1. Read the denied tool summary.
2. Restate the task with narrower scope.
3. Ask Sigil to propose first if the diff was too large.

## Mouse Or Clipboard Does Not Work

Open `/config` and review the `Terminal` section.

Common mitigations:

```toml
[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false
scroll_sensitivity = 3
```

`keyboard_enhancement` is resolved on the next launch. `mouse_capture` applies on the next launch. `osc52_clipboard` is checked for each copy action. `scroll_sensitivity` applies after the saved config is reloaded.

See [Terminal Compatibility Checklist](terminal-compatibility.md) for tmux, screen, SSH, WSL, and manual smoke checks.

## Attention Notification Does Not Appear

Open `/config` → `Terminal` and confirm notifications are enabled. Temporarily set the long-run threshold to `1000` ms and choose `bell` to separate terminal protocol support from notification timing. Some terminals mute BEL; in a known compatible profile, try `osc9` or `osc777`. Under tmux or screen, verify escape-sequence pass-through. A focused Sigil window suppresses notifications only after the terminal has supplied reliable focus events.

Run `/doctor` to confirm the saved switch, method, and threshold. Notification failures never fail the active run, and no notification is sent for short replies, cancellation, progress, or a restored historical session.

## Session Restore Shows Interrupted Tools

That is expected after a process exit, crash, terminal close, or cancellation while a tool was running. Sigil restores started-but-unfinished tools as interrupted results. It does not replay them silently.

Use `/resume` to select a session. If a planned task is still unfinished, continue with guidance in the composer or run:

```text
/task continue
```

## Context Usage Is High

The info rail shows the latest prompt usage reported by the provider. If the `ctx` line says the window is unavailable, set `fallback_context_window_tokens`. Soft and hard thresholds show context pressure. After a successful turn reaches the hard threshold and becomes fully idle, Sigil may prepare one locally verified compaction in the background. A queued request may also compact before promotion when its exact frozen material exceeds the admitted budget. The pinned official OpenAI Responses profile may perform one guarded overflow recovery after an exact output-free context rejection and two successful audited counts.

Manual compaction:

```text
/compact
```

This opens a read-only V2 fold preview. Opening it does not append a compaction record or rewrite session history. If the review says the target is ready, `Enter` confirms one manual apply; otherwise the review explains which local admission prerequisite is missing. Idle and queued pre-turn paths may use the same exact local admission. Guarded OpenAI Responses overflow recovery uses separately audited remote count evidence and is not initiated from this review.

You can install the checksum-verified DeepSeek V4 Flash tokenizer required by local manual admission with:

```text
sigil tokenizer install deepseek-v4-flash
```

The command prints a network disclosure before downloading the public artifact. Installing it does not apply compaction or change a session.

## MCP Server Is Missing, Failed, Or Deferred

Check:

- Is `command` available on `PATH`?
- Are paths in `args` absolute and present?
- Should the server be `required = false` while you test it?
- Is `startup = "lazy"` expected? Lazy servers do not register tools until activated.
- Does pinned identity match the observed server identity when `pin_version = true`?

Run:

```text
/doctor
```

In the TUI, a failing eager MCP server should not block ordinary chat or planned tasks with built-in tools.

For OAuth, use the typed state shown in `/config` → **MCP Servers** → **Authentication**:

| State or error | What to do |
| --- | --- |
| `authentication required` | Choose **Sign in** explicitly; eager startup never opens a browser. |
| metadata unavailable / destination rejected | Check HTTPS, the resource URL, Network disclosure, proxy/domain policy, and the authorization server metadata. |
| callback invalid | Restart sign-in and use only the newest browser tab. For manual return, paste the complete callback URL with its original state. |
| credential store unavailable/rejected | Unlock or enable the native Keychain, Credential Manager, or Secret Service. Sigil has no plaintext credential fallback. |
| refresh rejected | Sign in again. Sigil disables an invalid refresh credential instead of looping. |
| remote revoke failed | The local credential remains. Retry remote sign-out or explicitly clear it after reviewing the warning. |
| budget exhausted | Increase the bounded Web budget or finish other Web work, then retry the explicit action. |

OAuth redirects and automatic request retries are disabled. If a request receives `401` after sending, Sigil does not replay it. See [MCP OAuth authentication](mcp.md#oauth-authentication) for the callback and credential contract.

## Code Intelligence Is Not Ready

Check:

- `[code_intelligence].enabled`
- whether the relevant language server is installed and on `PATH`;
- whether discovery is enabled;
- whether this exact workspace is trusted when the server keeps the default `trust_required = true`;
- the LSP readiness rows in `/config`;
- `/doctor` output.

For a fresh headless `sigil run`, the trust state is `Unknown`; it does not reuse another session's decision, so a trust-required LSP intentionally stays stopped. Use the TUI when a session-bound trusted LSP is required, or explicitly set `trust_required = false` only when that headless process-start policy is appropriate. If no LSP server is available, Rust projects can still use Tree-sitter fallback for outline and syntax diagnostics. Normal chat and file tools are not blocked.

## Command Not Found After Install

Confirm the installer completed, then inspect the current shell's `PATH`:

```bash
echo "$PATH"
```

Use the matching channel in [Installation](installation.md) to confirm that channel's binary location and repeat its install or update command. Keeping installer-specific commands there prevents stale recovery instructions on this page.

## Report A Bug

If the decision tree and `sigil doctor` do not resolve the problem, run `/feedback` in the TUI. The first screen is a privacy preview: nothing has been written or uploaded yet. It lists the broad diagnostic categories that will be included and the private categories that are excluded.

Press `Enter` only if you want to save one JSON report in Sigil's local cache. Sigil never uploads it automatically. After export, press `Enter` again to review the exact redacted JSON in the TUI, `O` to reveal the file, or `B` to open the [structured bug-report form](https://github.com/JimmyDaddy/sigil/issues/new?template=bug-report.yml). `C` copies the report path and `U` copies the form URL. Attaching and submitting the report remain your actions.

The report can include build, operating-system and architecture details; redacted doctor status; provider and model labels; MCP aliases; and capability or sandbox status. It excludes conversation text, tool input and output, file content and diffs, config-file content, credential and environment names and values, private endpoints, local paths, and session-log content.

Do not open a public issue for a suspected vulnerability. Follow the repository [Security Policy](https://github.com/JimmyDaddy/sigil/blob/main/SECURITY.md) for private reporting instead.

## What To Include In A Bug Report

Include:

- `sigil --version`
- the reviewed `/feedback` JSON report, or `sigil doctor --output json` when the TUI is unavailable
- operating system and terminal emulator
- whether you are inside tmux, screen, SSH, or WSL
- the smallest prompt or command that reproduces the issue

Do not attach a config file, session log, source file, or raw terminal transcript unless it is necessary and you have reviewed and removed private content yourself.
