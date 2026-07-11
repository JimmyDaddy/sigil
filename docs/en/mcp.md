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
# Add only the parent variables this server actually needs.
# inherit_env = ["MY_MCP_API_KEY"]

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

## Process Environment and Credentials

Local stdio MCP processes do not inherit Sigil's full environment. Sigil clears the parent environment before spawn, adds a small allowlisted runtime baseline such as `PATH`, locale, temporary-directory, and required Windows system variables, and then injects only names explicitly listed in the user root config:

```toml
[[mcp_servers]]
name = "credentialed-search"
command = "/absolute/path/to/search-mcp"
args = ["--stdio"]
inherit_env = ["MY_MCP_API_KEY"]
startup = "lazy"

[mcp_servers.trust]
approval_default = "ask"
allow_secrets = false
```

`inherit_env` entries must match `[A-Za-z_][A-Za-z0-9_]*`; Sigil de-duplicates and sorts them. Every listed variable must exist when the server is activated. A missing or non-UTF-8 value is a pre-spawn `configuration_invalid` error, so no child process receives a partial credential set.

Variables such as `HOME`, `SSH_AUTH_SOCK`, proxy settings, provider keys, and cloud credentials are not inherited automatically. Prefer an absolute `command` path for executables outside the baseline `PATH`.

Only user root `[[mcp_servers]]` entries may use `inherit_env`. Plugin manifests cannot request environment or credential grants; discovery rejects that field with `plugin_mcp_environment_grant_not_supported`. Move a credentialed plugin-declared server into the user root config instead.

Sigil stores and displays grant names, source metadata, and static/live fingerprint status, never the resolved value. The live fingerprint uses a process-random key and cannot be used as an offline secret verifier. If a granted value changes or disappears, the old MCP process binding is invalidated and the server must be restarted or refreshed.

`inherit_env` and `allow_secrets` are independent controls. The first authorizes a value only for child-process startup. The second controls whether later MCP tool/resource/prompt payloads may contain recognized secrets. Enabling either one does not enable the other.

## Startup Modes

`startup` supports:

- `eager`: start the server, list tools, and register them during startup.
- `lazy`: record the config only; do not start the server and do not register fake tools.

`required` controls failure behavior:

- `required = true`: startup or `tools/list` failure fails strict registry construction.
- `required = false`: an eager server failure can be skipped with a warning.

In the TUI, eager MCP servers are activated in the background after the core agent worker starts. If one MCP server is slow, missing, or times out, normal chat and `/plan` runs continue with built-in and code-intelligence tools; only that MCP server is marked `failed` until it is fixed or refreshed.

A lazy server can be activated manually from the TUI `/config` MCP section. The `Server` row follows the same cycle interaction as theme choices: `Enter` selects the next server for lifecycle inspection without modifying the config. `Down` moves to the footer; select `activate` and press `Enter` to activate or refresh that server. `PageUp/PageDown` remain compatibility aliases for cycling the inspected server. The model can also call `mcp_activate_server` to start a named lazy server on demand. After activation succeeds, real MCP tools are added to the current agent registry.

Model-triggered server activation is classified as local `Execute` plus `NetworkEffect::Unknown` and goes through the complete tool permission decision. Configured eager startup, direct lifecycle activation, and refresh keep their existing lifecycle/source semantics, but carry the current run-scoped network policy to the spawn boundary: network `Ask` without explicit approval does not silently start a process, and network `Deny` is admitted only when the selected backend proves both network and process-tree isolation. Once connected, a generic MCP tool call is local `Read` plus network `Unknown`; resource and prompt surfaces are local `Read` plus network `Read`. These local labels do not mean that data stays on the machine.

The TUI shows lifecycle states:

- `deferred`
- `activating`
- `refreshing`
- `stale <capability>`
- `ready`
- `failed`

## Stdio Compatibility And Deadlines

Sigil speaks the newline-delimited JSON stdio transport defined by MCP `2025-06-18`. MCP servers that use LSP-style `Content-Length` headers are not compatible and must be updated or wrapped with a standards-compliant adapter. Sigil does not sniff or switch framing on a live stream.

Startup has one absolute budget covering `initialize`, `initialized`, and the first `tools/list`. Each tool, resource, or prompt call uses the active tool timeout. A zero timeout uses the finite 30-second project default, and larger configured values are clamped to a 24-hour hard maximum. A timeout or invalid/oversized frame permanently closes that client generation, attempts to terminate its process group/tree, and reaps the direct child; incomplete cleanup is reported rather than presented as successful. On Windows, Sigil keeps the stdio connection open while it runs bounded `taskkill /T /F`, so a cooperative stdin-EOF exit cannot race ahead of tree cleanup; if the leader was already gone before teardown began, tree cleanup is reported as unconfirmed. Limit failures use structured resource-limit details, including the applicable limit and observed lower bound.

Each NDJSON frame is limited to 4 MiB; one operation may consume at most 256 inbound messages and 8 MiB of framed input, while MCP stderr has an 8 MiB hard limit. Tool, resource, and prompt content is redacted before JSON escaping and bounded to 32 KiB or 2,000 lines before it reaches the kernel model-content cap. Truncation is reported only through structured metadata, so no generated marker can accidentally reintroduce a configured secret carrier.

Use `/config` → MCP → `activate` to refresh the server. Registry ownership uses the exact unsanitized server identity plus a unique process-generation id, never a provider-visible name prefix, so sanitized or hashed name collisions cannot retire another server. Explicit activation and refresh require a callable replacement even when the server is optional. Refresh reports success only after the replacement is registered and every distinct retired generation has been explicitly shut down; if replacement startup fails, the old generation is restored, and if retirement cleanup fails, the replacement is removed and shut down as a fail-closed rollback. Multi-server registration is transactional and rolls back generations started earlier in the same failed operation. Duplicate exact server names are rejected before launch. The closed generation is never reused, so a late response cannot become the next call's response.

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
- `pin_version`: when `true`, validates the command/args/environment-grant fingerprint before spawn, then validates protocol and server identity after initialize. For a credentialed server, the pre-spawn fingerprint also binds the canonical execution base and the bytes of the executable resolved through the isolated baseline `PATH`.

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

If pinned identity is missing or the command fingerprint is stale, startup fails before the server receives environment grants and prints the pre-spawn command fingerprint. After that fingerprint matches, Sigil initializes the server and validates the remaining protocol/name/version fields. Existing pins for servers with no `inherit_env` keep their previous command fingerprint; adding or changing grant names intentionally requires a new pin.

For a server with `inherit_env`, replacing the resolved executable at the same path changes the pre-spawn fingerprint. Command arguments are bound as exact text, but Sigil does not interpret them or attest files named inside them. In particular, `command = "python3"` with a script path in `args` pins the Python executable and the argument string, not the script contents. Prefer a dedicated executable for credentialed servers, or separately review and protect interpreter scripts and modules.

This fingerprint detects the executable bytes observed during pre-spawn validation; it is not a hostile same-user host attestation. Sigil ultimately starts the executable by path, so another process that can rewrite that file concurrently may race validation and launch. Keep credentialed MCP executables and their parent directories outside untrusted write scope. A future OS-specific handle-bound execution primitive would be required to remove that host-level race.

## Roots

Sigil exposes only the resolved workspace root through MCP `roots/list`. Do not infer workspace from the config file path.

If `allow_secrets = false`, secret-like content in the `roots/list` payload is blocked.

## Resources

When a server declares the MCP `resources` capability during `initialize`, Sigil registers two provider-visible tools with local `Read` access and network `Read` effect:

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

When a server declares the MCP `prompts` capability during `initialize`, Sigil registers two provider-visible tools with local `Read` access and network `Read` effect:

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
