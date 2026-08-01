<!-- public-doc-role: safety; authority: risk-model-authority; sections: risk-model,review-an-approval,hard-limits-to-remember; cta: configure-permissions -->

# Safety

[Docs home](README.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Privacy](privacy.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/safety.md)

Safety in Sigil is a decision process: understand the proposed action, inspect the relevant preview, and grant only the access needed for this task.

## Risk Model

<!-- public-doc-topic: approval-risk-model -->

Repository reads are usually lower risk. Writes, deletes, commands, external paths, network access, MCP calls, code edits from language tools, and secret-bearing requests deserve more scrutiny. Sigil derives structured effects, targets, analysis completeness, and required execution containment before policy runs. A risk label explains what the action may do; it does not decide whether approval is required. Configuration, explicit rules, bounded session authority, and containment evidence decide whether an action runs, asks, or is denied; none makes an approved action correct.

## Review An Approval

Before allowing an action, confirm:

1. The goal matches your request.
2. The files, command, server, or destination are expected.
3. The diff or request preview is narrow enough.
4. A one-time decision is sufficient; use a session decision only when Sigil offers the same semantic scope under the same execution boundary and repeated access is intentional.
5. You know how to verify the result.

Deny and restate the scope when the preview is surprising or too broad.

## Hard Limits To Remember

- Headless runs cannot ask interactively; unresolved approvals fail.
- Permission is not a sandbox. The default local command strategy does not provide OS isolation.
- Unknown, dynamic, destructive, remote, privileged, and credential-access effects cannot become broad session grants.
- External-directory, network, and sandbox behavior must be configured separately; none is a blanket guarantee.
- A file restore does not undo shell commands, remote services, MCP effects, or other outside changes.
- An interrupted tool is shown as interrupted after restore and is not silently run again.
- `sigil serve` is for a trusted local client: it listens on loopback and requires authentication for privileged routes.
- Saving a pasted credential through Quick Setup or `/config` writes it to the configured protected credential store and keeps only an opaque reference in `sigil.toml`. Default `file` and non-interactive `auto` use owner-only `~/.sigil/credentials.json`; that dedicated file is protected plaintext, not encryption. Only explicit `keyring` may show system authentication UI.
- Automatic Task routing and proactive Explore agents do not grant tool permission. File, Shell, network, MCP, external-directory, and merge decisions remain independent.
- A zero-tolerance orchestration invariant disables only the affected provider/model/build route for that session. Subsequent input falls back to `manual + explicit_request_only`; accepted Task-plan recovery remains available, and durable Task history is retained.

Use [Permissions and sandbox](permissions-and-sandbox.md) for controls, [Privacy](privacy.md) for data and credentials, [MCP](mcp.md) for external-server trust, and [Reference](reference.md) for local-service details.

<!-- public-doc-cta: configure-permissions -->
Next: [Configure permissions and sandbox limits](permissions-and-sandbox.md).
