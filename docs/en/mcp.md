# Sigil MCP Guide

[Docs home](README.md) · [Configuration](configuration.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/mcp.md)

Sigil can connect stdio MCP servers as external tool providers. Connected MCP tools, resources, and prompts enter the same tool registry and use the same approval, activity, session control, and secret egress rules as built-in tools.

Start conservative: configure one server, keep `approval_default = "ask"`, run `/doctor`, and only loosen trust settings after you understand what the server can read or mutate.

## Minimal Config

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup_timeout_secs = 5
required = true
startup = "eager"

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

Remote tools are exposed to the provider with sanitized names, for example:

```text
mcp__filesystem__read_file
```

Name conflicts or overly long names get a stable hash suffix.

## Startup Modes

`startup` supports:

- `eager`: start the server, list tools, and register them during startup.
- `lazy`: record the config only; do not start the server and do not register fake tools.

`required` controls failure behavior:

- `required = true`: startup or `tools/list` failure fails strict registry construction.
- `required = false`: an eager server failure can be skipped with a warning.

In the TUI, eager MCP servers are activated in the background after the core agent worker starts. If one MCP server is slow, missing, or times out, normal chat and `/plan` runs continue with built-in and code-intelligence tools; only that MCP server is marked `failed` until it is fixed or refreshed.

A lazy server can be activated manually from the TUI `/config` MCP section. The model can also call `mcp_activate_server` to start a named lazy server on demand. After activation succeeds, real MCP tools are added to the current agent registry.

The TUI shows lifecycle states:

- `deferred`
- `activating`
- `refreshing`
- `stale <capability>`
- `ready`
- `failed`

## Trust Policy

```toml
[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

Fields:

- `trust_class`: server trust class, one of `official`, `self_hosted`, or `third_party`.
- `approval_default`: default approval mode for tools from this server; explicit tool/rule overrides still win.
- `egress_logging`: after approval and before execution, append a safe summary of server, trust class, remote tool, and argument shape to control state.
- `allow_secrets`: when `false`, blocks MCP tool/resource/prompt arguments, `roots/list` payloads, or elicitation responses that contain resolved secrets or secret-like fields.
- `pin_version`: when `true`, validates the pinned server identity at startup.

MCP tool permission subjects include `mcp_trust_class:<class>`, so permission rules can match trust class.

## Pinned Identity

When `pin_version` is enabled, provide the expected identity:

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup = "eager"

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = true

[mcp_servers.trust.pinned]
command_fingerprint = "sha256:..."
protocol_version = "2025-06-18"
server_name = "filesystem"
server_version = "1.0.0"
```

If pinned identity is missing, startup fails and prints the observed pin so you can write it into config.

## Roots

Sigil exposes only the resolved workspace root through MCP `roots/list`. Do not infer workspace from the config file path.

If `allow_secrets = false`, secret-like content in the `roots/list` payload is blocked.

## Resources

When a server declares the MCP `resources` capability during `initialize`, Sigil registers two read-only provider-visible tools:

```text
mcp__<server>__resources_list
mcp__<server>__resources_read
```

`resources_list` calls MCP `resources/list`. It accepts an optional `cursor` string for pagination.

`resources_read` calls MCP `resources/read`. It requires a `uri` string returned by `resources_list`.

Resource tools use the same MCP trust policy as remote tools:

- permission subjects include `mcp_trust_class:<class>`;
- `approval_default` is applied per call;
- `egress_logging = true` records only a safe argument-shape summary;
- `allow_secrets = false` blocks secret-like resource arguments before they leave Sigil;
- returned resource content is redacted locally before it is shown to the model.

Sigil does not inject MCP resources into the system prompt. The model must explicitly list and read resources through these tools.

## Prompts

When a server declares the MCP `prompts` capability during `initialize`, Sigil registers two read-only provider-visible tools:

```text
mcp__<server>__prompts_list
mcp__<server>__prompts_get
```

`prompts_list` calls MCP `prompts/list`. It accepts an optional `cursor` string for pagination.

`prompts_get` calls MCP `prompts/get`. It requires a `name` returned by `prompts_list` and accepts an optional `arguments` object.

Prompt tools use the same MCP trust policy, approval defaults, egress logging, and `allow_secrets = false` gate as other MCP surfaces. Sigil does not inject MCP prompts into the system prompt; the model must explicitly list and get prompts through these tools.

## Output Limits

MCP tool, resource, and prompt results are redacted locally and then bounded before becoming model-visible. Large outputs are truncated with metadata such as `truncated`, `limit_bytes`, `limit_lines`, `returned_bytes`, and MCP details including server, remote tool/surface, trust class, operation, and observed server identity.

## Elicitation

The TUI runtime declares and handles `elicitation/create`. When a server requests user input, Sigil shows a modal with the server, requested fields, and defaults.

User actions map to:

- accept: send only the flat primitive object fields confirmed in the modal.
- decline: return `decline`.
- cancel: return `cancel`.

TUI elicitation decisions are appended to control state. Audit records include server, request message/schema hash, field names, and action, but not user-provided values.

The non-TUI default runtime returns explicit unsupported responses. It does not hang and does not fake user input.

## Progress Notifications

`notifications/progress` updates the TUI live panel instead of writing repeated timeline entries. `notifications/tools/list_changed`, `notifications/resources/list_changed`, and `notifications/prompts/list_changed` mark the server as stale and trigger a safe refresh at the next idle worker boundary.

## FAQ

### A lazy server is configured but tools are not visible

This is expected. `startup = "lazy"` does not register fake tools during startup. Activate it from TUI `/config`, or let the model call `mcp_activate_server`.

### Server startup fails

Check:

- Whether `command` is available on `PATH`, or use an absolute path.
- Whether paths inside `args` exist.
- Whether `required` should be `false` for optional servers in strict/headless registry construction.
- Whether pinned identity matches the observed pin when `pin_version = true`.

In the TUI, this should not stop ordinary tasks. The failing server appears as `failed` in MCP status, and built-in tools remain available.

### Secret egress is blocked

When `allow_secrets = false`, Sigil blocks recognized secret egress. This means the safety policy is working. Only adjust the server trust policy after confirming the server really needs that secret.
