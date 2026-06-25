# Privacy And Data Handling

[Docs home](README.md) · [Safety](safety.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/privacy.md)

Sigil runs locally, but it can send prompt context and tool results to the configured model provider. It can also call configured MCP servers. This page explains what users should review before using Sigil on sensitive repositories.

## What Can Leave Your Machine

Data can leave your machine when:

- the provider request includes your prompt, system context, selected session history, and model-visible tool results;
- the model asks for tool results that include file excerpts, search matches, diagnostics, or command output;
- an MCP tool/resource/prompt call is approved and sent to an MCP server;
- an MCP elicitation response is accepted;
- a shell command you approve transmits data over the network.

Sigil does not automatically publish repository data, but provider and MCP configuration determine where approved context can go.

## What Stays Local

These are local by default:

- per-user `sigil.toml`;
- per-user session logs and input history under the Sigil state directory;
- per-user terminal and changeset artifacts under the Sigil state directory;
- per-user scratch/cache files under the Sigil cache directory;
- local memory files such as `SIGIL.md`, `AGENTS.md`, and `SIGIL.local.md`;
- release archives and checksums you build locally;
- doctor output unless you copy it elsewhere.

## API Keys

Prefer environment variables:

```bash
export SIGIL_API_KEY="sk-..."
```

If you save an API key through Quick Setup or `/config`, it is stored as plaintext in the per-user `sigil.toml`. Do not copy real config files containing secrets into a repository.

`sigil doctor` reports where the key came from, but it does not print the key value.

## Session Logs

Session and control state are append-only JSONL under the per-user Sigil state directory by default. They can contain:

- prompts and assistant responses;
- tool call summaries;
- tool result previews;
- approval and execution records;
- interrupted tool records;
- compaction records;
- task planning state.

Treat session logs as sensitive local artifacts. Review them before sharing.

## MCP And Secret Egress

MCP servers are external tool providers. Configure trust explicitly:

```toml
[mcp_servers.trust]
approval_default = "ask"
egress_logging = true
allow_secrets = false
```

With `allow_secrets = false`, Sigil blocks recognized secret-like egress for MCP calls and resources. Keep this default unless the server genuinely needs secret material and you trust it.

## Doctor Output

Doctor reports:

- config resolution;
- workspace path;
- session log location;
- provider/auth source;
- MCP command and trust state;
- code-intelligence readiness;
- terminal profile and compatibility risk.

It should not print secret values, but paths, provider names, and local environment facts can still be sensitive.

## Before Sharing Logs Or Reports

Remove:

- API keys and tokens;
- private repository paths;
- proprietary source excerpts;
- provider request IDs if they identify private usage;
- session logs that include sensitive prompts or file snippets;
- MCP server arguments that contain internal URLs or credentials.

## Recommended Defaults

- Keep real secrets in environment variables.
- Keep `permission.default_mode = "ask"` while learning the tool.
- Keep MCP `allow_secrets = false`.
- Keep external directory access disabled unless needed.
- Review approval diffs before allowing file changes.
- Run Sigil from the intended workspace root.
