<!-- public-doc-role: safety; authority: risk-model-authority; sections: risk-model,review-an-approval,hard-limits-to-remember; cta: configure-permissions -->

# Safety

[Docs home](README.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Privacy](privacy.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/safety.md)

Safety in Sigil is a decision process: understand the proposed action, inspect the relevant preview, and grant only the access needed for this task.

## Risk Model

<!-- public-doc-topic: approval-risk-model -->

Repository reads are usually lower risk. Writes, deletes, commands, external paths, network access, MCP calls, code edits from language tools, and secret-bearing requests deserve more scrutiny. Configuration decides whether an action runs, asks, or is denied; it does not make an approved action correct.

## Review An Approval

Before allowing an action, confirm:

1. The goal matches your request.
2. The files, command, server, or destination are expected.
3. The diff or request preview is narrow enough.
4. A one-time decision is sufficient; avoid a session-wide grant unless repeated access is intentional.
5. You know how to verify the result.

Deny and restate the scope when the preview is surprising or too broad.

## Hard Limits To Remember

- Headless runs cannot ask interactively; unresolved approvals fail.
- Permission is not a sandbox. The default local command strategy does not provide OS isolation.
- External-directory, network, and sandbox behavior must be configured separately; none is a blanket guarantee.
- A file restore does not undo shell commands, remote services, MCP effects, or other outside changes.
- An interrupted tool is shown as interrupted after restore and is not silently run again.
- `sigil serve` is for a trusted local client: it listens on loopback and requires authentication for privileged routes.
- Saving credentials through Quick Setup or `/config` writes plaintext local configuration.

Use [Permissions and sandbox](permissions-and-sandbox.md) for controls, [Privacy](privacy.md) for data and credentials, [MCP](mcp.md) for external-server trust, and [Reference](reference.md) for local-service details.

<!-- public-doc-cta: configure-permissions -->
Next: [Configure permissions and sandbox limits](permissions-and-sandbox.md).
