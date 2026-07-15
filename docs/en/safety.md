# Safety And Permissions

[Docs home](README.md) · [Configuration](configuration.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/safety.md)

Sigil is designed to make tool-backed coding visible and reviewable. The model can propose reads, searches, edits, shell commands, MCP calls, code-intelligence actions, and planned task steps; Sigil decides whether those actions can run directly, need approval, or must be denied.

## The Short Version

- Read-only repository inspection is usually allowed.
- File writes, edits, deletes, shell execution, external directories, MCP tools, and LSP edit tools can require approval.
- Approval cards show what is about to happen before the action runs.
- Headless `sigil run` cannot ask interactively; final `ask` decisions become structured `approval_required` tool errors.
- `sigil serve` is loopback-only in V1 and requires bearer authentication for every route except `GET /health`.
- Session and control records are append-only so later recovery and review can explain what happened.

## Permission Modes

Sigil's permission layer has three common outcomes:

| Outcome | Meaning |
| --- | --- |
| `allow` | The tool call runs without an approval modal. |
| `ask` | The TUI shows an approval card. |
| `deny` | The tool call is rejected and the model receives a structured denial. |

The recommended default is:

```toml
[permission]
mode = "manual"
```

This lets ordinary repository inspection proceed while keeping mutating or risky actions reviewable.

## What Usually Runs Without Approval

Read-only tools can run directly when they stay inside the workspace:

- list files;
- read files;
- search text;
- inspect symbols or diagnostics when code intelligence is enabled;
- list MCP resources or prompts only when their trust and approval policy allows it.

The exact behavior is still controlled by config, tool category, trust class, and permission rules.

## What Usually Needs Review

Expect an approval card for:

- writing, editing, or deleting files;
- running shell commands that are not simple trusted reads;
- accessing paths outside the workspace;
- running external MCP tools;
- accepting MCP elicitation requests;
- applying LSP code actions or rename edits;
- any operation where the configured trust policy says `ask`.

## How To Read An Approval Card

Before allowing a tool, check:

1. Summary: the action the tool is about to perform.
2. Subject: file path, command, MCP server, or external resource involved.
3. Files: affected files, if any.
4. Diff: added, removed, or changed lines.
5. Trust context: especially MCP server trust class and secret-egress behavior.
6. Action: allow only if the summary and diff match your intent.

If the diff is too large, deny and ask Sigil to split the change.

## Workspace Confinement

File tools are confined to the resolved workspace root. They reject:

- absolute paths outside the workspace;
- paths that escape with `..`;
- symlinks that resolve outside the workspace.

With the normal setup:

```toml
[workspace]
root = "."
```

`.` resolves to the directory where you launched `sigil`.

## External Directories

External-directory access is disabled by default:

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Only enable it for narrow, intentional use cases. Keep `external_directory.default_mode = "ask"` unless the external path is low risk and stable. Temporary shell scratch files should use `$SIGIL_SCRATCH_DIR` from `bash` or `terminal_start`; OS temp directories such as `/tmp`, macOS `/private/tmp`, or Windows `%TEMP%` still require external-directory access.

## Shell Commands

By default, `bash` uses the local execution backend and does not provide an OS sandbox. Sigil treats shell execution conservatively:

- simple read-like commands may be allowed only when they match safe patterns;
- commands with writes, redirects, package managers, network access, unknown commands, variables, or complex shell syntax should stay reviewable;
- command output is bounded before it becomes model-visible.

Review the command, working directory, and expected side effects before approving.

On macOS, `~/.sigil/sigil.toml` can opt into the `macos_seatbelt` backend for non-interactive commands:

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
```

This backend runs commands through `/usr/bin/sandbox-exec`, allows filesystem reads, restricts writes to the command working directory, and omits network access from the sandbox profile. Current local handoff paths can record sandbox coverage for non-interactive shell, PTY, MCP stdio, and trusted plugin hook command execution where the selected backend supports that mode. It does not make remote tools, every container/daemon scenario, or unsupported platforms equivalent.

For container-backed non-interactive commands, configure Docker explicitly:

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "docker"
profile = "build_offline"
container_image = "rust:1.94.1"
```

Sigil does not choose or pull a container image implicitly. If Docker is selected without `execution.sandbox.container_image`, config parsing, startup, and doctor checks fail closed. The Docker backend bind-mounts the command working directory, maps offline profiles to `--network none`, and reports only the capabilities the backend is expected to enforce. PTY, MCP, plugin, remote, and daemon-style paths use their own coverage labels and may fail closed instead of silently falling back to local execution.

## MCP Trust

MCP servers can expose tools, resources, prompts, and elicitation requests. Configure each server with an explicit trust policy:

```toml
[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

Start with `approval_default = "ask"` and `allow_secrets = false`. Only loosen those settings after confirming what the server can read, write, and transmit.

## Web Search Safety

`websearch` is local `Read` plus network `Read`. Configured and bundled client routes pass ordinary tool permission and a pre-egress query disclosure; every remote MCP HTTP message also receives its own transport disclosure. Provider-hosted search is authorized per provider request and suppresses the client `websearch` tool for that run. A configured search binding is authoritative when selected: failure never silently falls back to bundled Exa.

A public `NetworkEndpoint` is distinct from a workspace-external file path and does not enter `permission.external_directory`. With network `Allow`, read-only `websearch` / `webfetch` calls do not ask per request. With network `Ask`, `Allow session` relaxes only `NetworkEffect::Read` for the same tool; it never overrides source trust, network mutation/Unknown, another tool, or `Deny`. Destination guard, per-message disclosure, durable audit, and budgets still run when a grant is reused.

Treat search results as untrusted input. They may contain prompt injection, hostile markup, misleading claims, or unsafe URLs. Sigil sanitizes and bounds returned content, but the model must still verify important claims and should fetch only session-proven URL/source capabilities. It must not fan out across multiple results when search snippets are sufficient; `webfetch` is reserved for an explicit request to read a page or one specific necessary fact missing from the snippets.

## Secrets

Prefer environment variables for provider credentials. Choose the provider first, then use its exact variable from the [provider authentication map](providers.md#authentication-priority); there is no provider-neutral API key variable.

Saving an API key through Quick Setup or `/config` writes plaintext to `sigil.toml`. That may be acceptable for a private local config, but never commit real secrets.

`doctor` reports credential source, not secret values.

## Local HTTP/SSE Service

Treat `sigil serve` as a privileged local control surface. V1 accepts loopback addresses only and rejects disabled or missing bearer authentication before it binds. Keep `SIGIL_HTTP_TOKEN` in the environment, use a high-entropy value, pass it only in the `Authorization` header, and do not put it in URLs, config committed to a repository, logs, or screenshots. Sigil prints the token environment-variable name, never its value.

Only `GET /health` is unauthenticated and it returns no session data. OpenAPI, disclosure replay, session/run queries, commands, cancellation, approval, and SSE all require the bearer token. The server does not use cookies or wildcard CORS and does not claim remote-access or multi-user isolation. Per-run approval mode remains explicit: `deny` blocks gated tools, `allow_readonly` only auto-allows read-only work, and `ask` waits for the authenticated approval route.

## Recovery And Audit

Session and control records are append-only JSONL under Sigil's per-user state directory by default.

Recovery rules users should know:

- finished tool calls stay in history;
- started-but-unfinished tools restore as interrupted;
- restore does not silently replay tools;
- compaction appends records instead of rewriting old history;
- planned task state is rebuilt from durable control records.

## Practical Safety Defaults

Start with:

```toml
[permission]
mode = "manual"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

For MCP:

```toml
[mcp_servers.trust]
approval_default = "ask"
egress_logging = true
allow_secrets = false
```

Then adjust only the narrow behavior you actually need.
