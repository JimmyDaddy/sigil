<!-- public-doc-role: troubleshooting; authority: symptom-to-action-authority; sections: decision-tree,quick-setup-opens-every-time,sigil-cannot-find-the-api-key,theme-colors-are-hard-to-read,the-wrong-workspace-is-being-used,a-file-tool-cannot-access-a-path,a-tool-needs-approval-in-headless-run,an-approval-was-denied-or-timed-out,mouse-or-clipboard-does-not-work,attention-notification-does-not-appear,session-restore-shows-interrupted-tools,context-usage-is-high,mcp-server-is-missing-failed-or-deferred,code-intelligence-is-not-ready,command-not-found-after-install,report-a-bug; cta: open-reference -->

# Troubleshooting

[Docs home](README.md) · [Reference](reference.md) · [简体中文](../zh-CN/troubleshooting.md)

Start with:

```bash
sigil doctor
```

Use `sigil doctor --output json` when you need a redacted diagnostic file for a support request. The checks below cover the most common next steps.

## Decision Tree

| Symptom | First check | Next action |
|---|---|---|
| Setup keeps reopening | Provider credentials | Reopen `/config` and save a valid provider |
| Wrong files appear | Workspace path | Restart from the intended directory |
| A tool is blocked | Approval or sandbox message | Review the reason; change policy only if intended |
| MCP is unavailable | `/config` → MCP Servers | Fix auth, config, or start mode |
| Terminal input behaves oddly | Terminal support | Try a supported terminal and run Doctor |
| Context is nearly full | Info rail | Finish the current step or use `/compact` when available |

## Quick Setup Opens Every Time

Sigil has no usable provider configuration or credential. Open `/config`, choose a provider, save it, and run `/doctor`. For exact fields, see [Providers](providers.md) and [Configuration Reference](configuration-reference.md).

## Sigil Cannot Find The API Key

Check that the credential name matches the selected provider and that the terminal launching Sigil inherited it. Restart Sigil after changing shell variables. Prefer provider config or the system credential flow where documented; do not paste secrets into issue reports.

## Theme Colors Are Hard To Read

Open `/config`, choose **Appearance**, and change the theme or syntax style. You can also disable the info rail. See [Appearance](appearance.md).

## The Wrong Workspace Is Being Used

Check the directory where Sigil was started and the configured `workspace.root`. Restart from the intended directory, or edit the active `sigil.toml`; `workspace.root = "."` follows the launch directory.

External-directory access is disabled by default. See [Permissions And Sandbox](permissions-and-sandbox.md).

## A File Tool Cannot Access A Path

Read the tool-card error first. Confirm that the path is inside the workspace, is not excluded by policy, and is not a symlink escape. If access is intentional, configure the narrowest additional root; do not broaden access just to silence the error.

## A Tool Needs Approval In Headless `run`

Headless mode cannot show an approval modal. Use a policy that permits the intended action, or run the task interactively. Never use a broader policy than the automation requires.

## An Approval Was Denied Or Timed Out

The action did not run. Inspect the preview, correct the request, and retry. A timeout is treated like a denial; Sigil does not silently continue.

## Mouse Or Clipboard Does Not Work

Check `[terminal].mouse_capture` and `osc52_clipboard` in the active `sigil.toml`, restart after changes, then test plain text selection. `Ctrl-C` copies a selection. `Ctrl-L` copies that selection when one is active, or the latest assistant reply when none is active. Image paste requires a supported system clipboard. See [Terminal Compatibility](terminal-compatibility.md).

## Attention Notification Does Not Appear

Notifications are off by default and depend on terminal support. Enable them under `/config` → **Terminal**, run Doctor, and ensure the terminal has not disabled OSC or bell notifications.

## Session Restore Shows Interrupted Tools

Sigil records work that was running when the process stopped as interrupted; it does not replay the command automatically. Review the tool card and retry only if the action is still needed.

## Context Usage Is High

The info rail shows context pressure. Finish or checkpoint the current work before starting a large new request. `/compact` is available only when Sigil can safely apply it for the selected model; otherwise start a fresh or forked conversation. See [User Guide](user-guide.md#long-context-and-compaction).

## MCP Server Is Missing, Failed, Or Deferred

Open `/config` → **MCP Servers** and inspect the server state:

- **missing/failed:** check the command or URL, authentication, and logs;
- **deferred:** activate the server before using its tools;
- **needs sign-in:** open `/config` → **MCP Servers** → **Authentication**.

For OAuth, use only the newest sign-in tab or complete callback URL. A failed remote revocation keeps the local credential and reports an error. You can retry revocation or explicitly choose **clear local only**; clearing local does not claim the remote token was revoked. Credential-store, callback, refresh, destination, and `401` recovery steps live in [MCP](mcp.md#oauth-authentication).

## Code Intelligence Is Not Ready

Run Doctor and check that the language tool is installed and available in the same environment that launched Sigil. Sigil may continue with reduced context when a language service is unavailable; the tool card reports that limitation.

## Command Not Found After Install

Open a new shell and check that the package manager's binary directory is on `PATH`. If multiple copies exist, run `command -v sigil` (or `Get-Command sigil` in PowerShell) and remove the stale one. Reinstall commands live only in [Installation](installation.md).

## Report A Bug

Run `sigil doctor --output json` or use `/feedback`, review the exported file, and attach it manually to the relevant GitHub form. Include the observed result, expected result, reproduction steps, platform and terminal, and the smallest safe log excerpt. Remove project content and secrets; reports are never uploaded automatically.

<!-- public-doc-cta: open-reference -->
Next: [Look up exact commands and keys](reference.md).
