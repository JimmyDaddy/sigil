<!-- public-doc-role: privacy; authority: data-and-credential-authority; sections: what-can-leave-your-machine,what-stays-local,api-keys,session-logs,mcp-and-web-data,doctor-and-feedback-output,before-sharing-logs-or-reports; cta: review-safety -->

# Privacy And Data Handling

[Docs home](README.md) · [Safety](safety.md) · [Permissions](permissions-and-sandbox.md) · [简体中文](../zh-CN/privacy.md)

Sigil runs locally, but model providers, Web routes, MCP servers, and approved commands can receive data. Review those destinations before using a sensitive repository.

## What Can Leave Your Machine

<!-- public-doc-topic: data-egress -->

Depending on your configuration and approvals, outbound data can include prompts, selected conversation context, workspace instructions such as `AGENTS.md` or `SIGIL.md`, file excerpts, search matches, diagnostics, command output, Web queries, MCP inputs, and accepted elicitation responses. Enabled workspace instructions become part of model requests. Sigil does not publish a repository by itself, but an allowed provider, tool, or command can transmit what you give it.

Use the provider's policy for model requests and the selected Web or MCP service's policy for external tools. Disable an unnecessary route or deny the action when its destination is unclear.

## What Stays Local

Per-user configuration, input history, session logs, change artifacts, cache files, the on-disk copies of workspace instruction files, and `/feedback` exports stay stored locally by default. Their contents can still leave through model context or another allowed action; workspace instructions are sent to the configured model provider when memory is enabled.

## API Keys

<!-- public-doc-topic: credentials-plaintext -->

Prefer the provider-specific environment variable listed in [Providers](providers.md#authentication-priority). A key saved through Quick Setup or `/config` is plaintext in the per-user `sigil.toml`; never commit or share a real config file. `sigil doctor` reports the credential source without printing the value.

OAuth credentials for a configured remote MCP server are kept in the native system credential store rather than TOML. See [MCP](mcp.md) for sign-in, sign-out, and local-clear behavior.

## Session Logs

<!-- public-doc-topic: session-log-local -->

Local session logs can contain prompts, assistant replies, tool summaries and previews, approval decisions, interrupted activity, task state, and context-management records. Treat them as sensitive even when the source repository is public. Review any export before sharing it.

## MCP And Web Data

MCP servers are external tool providers. Keep secret access disabled unless the server needs it and you trust the destination. Web search sends the query to the selected provider-hosted, configured MCP, or bundled route; the network service can observe the query and connection metadata. Returned content remains external and untrusted. Route and opt-out controls are in [Permissions and sandbox](permissions-and-sandbox.md#network-and-web-tools).

## Doctor And Feedback Output

Doctor output is redacted but can still include paths, provider labels, and local-environment facts. `/feedback` previews its included categories, writes one local JSON report, and uploads nothing automatically. Review the exported JSON before attaching it to an issue.

## Before Sharing Logs Or Reports

Remove credentials, private paths, proprietary source, sensitive prompts or tool previews, internal endpoints, and identifiers tied to private usage. When in doubt, reproduce the problem in a non-sensitive workspace and share that output instead.

<!-- public-doc-cta: review-safety -->
Next: [Review the Safety guide](safety.md).
