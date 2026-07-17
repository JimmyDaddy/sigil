<!-- public-doc-role: mcp; authority: mcp-setup-and-use-authority; sections: minimal-config,streamable-http,process-environment-and-credentials,startup-and-refresh,compatibility-and-limits,trust-and-identity,roots-resources-prompts-and-input,troubleshooting; cta: open-troubleshooting -->

# MCP Guide

[Docs home](README.md) · [Configuration](configuration.md) · [Privacy](privacy.md) · [Troubleshooting](troubleshooting.md) · [简体中文](../zh-CN/mcp.md)

Sigil connects local stdio and user-root Streamable HTTP MCP servers. Start with one server, keep approval at `ask`, run `/doctor`, and inspect what it can read, change, or transmit.

## Minimal Config

```toml
[[mcp_servers]]
name = "filesystem"
transport = "stdio"
command = "node"
args = ["/absolute/path/to/server.js"]
startup = "eager"
required = true

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

Use an absolute command when possible. Exposed tool names look like `mcp__filesystem__read_file`; conflicts receive a stable suffix.

## Streamable HTTP

Remote MCP is allowed only in the user-root config. Prefer HTTPS; plain HTTP is accepted only without environment-backed headers, bearer credentials, or OAuth, and should be limited to a trusted local setup.

```toml
[[mcp_servers]]
name = "my-search"
transport = "streamable_http"
url = "https://mcp.example.com/mcp"
startup = "lazy"
env_http_headers = { "X-API-Key" = "MY_SEARCH_API_KEY" }
client_capabilities = ["roots", "elicitation"]

[mcp_servers.trust]
trust_class = "third_party"
approval_default = "ask"
allow_secrets = false
```

Use `bearer_token_env_var` for a static bearer token. Sigil checks every destination, does not follow redirects automatically, bounds response size, and shows safe origin and credential-source names without displaying values.

### OAuth Authentication

Use OAuth instead of a static Authorization or bearer credential:

```toml
[mcp_servers.oauth]
# client_id = "sigil-public-client" # optional when dynamic registration is supported
scopes = ["mcp:tools"]
```

OAuth requires HTTPS and an explicit **sign in** action. Eager or headless startup reports `authentication required` without opening a browser. In `/config`, select the server and open **Authentication**. From the modal you can sign in, open or copy the authorization URL, paste a complete callback URL when the browser callback cannot return automatically, refresh, revoke remotely, or explicitly clear only the local credential. The automatic callback listens only on the IPv4 loopback interface at a random port. Callback text stays transient. Tokens are stored in the native system credential store, not TOML; there is no plaintext fallback.

OAuth can contact separate HTTPS authorization endpoints, all subject to the configured Network controls. Redirects and automatic retries are disabled. A `401` marks authentication stale but does not replay the request.

Remote sign-out tries revocation and never deletes the local credential implicitly. If revocation fails, the modal reports an error and keeps the credential. You may then retry or choose **clear local only**; that explicit action makes no remote-revocation claim. After a successful revocation—or when the server advertises no revocation endpoint—the modal enters **remote handled, local retained** and still lets you clear or keep the local credential.

## Process Environment And Credentials

Local stdio servers start with a small runtime environment, not Sigil's full parent environment. Grant only required variable names from the user-root config:

```toml
inherit_env = ["MY_MCP_API_KEY"]
```

Every listed variable must exist when the server starts. Provider keys, cloud credentials, proxy settings, and other sensitive variables are not inherited automatically. `inherit_env` controls process startup; `allow_secrets` separately controls recognized secret-like data in later MCP calls.

## Startup And Refresh

- `startup = "eager"` connects and registers tools during startup.
- `startup = "lazy"` waits until you activate the server from `/config` or an allowed activation tool call.
- `required = true` makes startup failure fatal in strict/headless setup; optional TUI servers can fail without stopping built-in tools.

The TUI reports deferred, authentication required, activating, ready, stale, or failed. Use `/config` → MCP → **activate** to start or refresh a server after fixing it. OAuth servers open **Authentication** instead of pretending that an unauthenticated zero-tool connection is ready.

## Compatibility And Limits

Stdio servers must use newline-delimited JSON for MCP `2025-06-18`; `Content-Length` framing is not supported. Startup and calls have finite timeouts. Oversized, invalid, or timed-out input closes that connection and is reported as a failure. Tool, resource, and prompt results are redacted and shortened before model use.

## Trust And Identity

`trust_class` records whether a server is official, self-hosted, or third-party. `approval_default` controls its normal prompt behavior. Keep `allow_secrets = false` unless a trusted server genuinely needs sensitive data.

`pin_version = true` can bind the expected command and reported server identity. A stale or missing pin prevents startup. Pinning helps detect unexpected changes but is not protection against another same-user process that can replace an executable during launch.

## Roots, Resources, Prompts, And Input

Sigil exposes only the active workspace through `roots/list`. Resources and prompts are listed or read only through explicit MCP tools; they are not injected automatically. Elicitation forms appear in the TUI with the server, requested fields, and defaults. Headless use reports unsupported when interactive input is required. Progress updates refresh the live panel without flooding the transcript.

## Troubleshooting

- **Lazy tools missing:** activate the server from `/config`.
- **Startup failed:** check command path, arguments, required variables, timeout, and any pin.
- **Authentication required:** open **Authentication**; confirm HTTPS, scopes, and system credential-store availability.
- **Callback rejected:** paste the complete callback URL, and do not reuse an old tab or callback after cancelling or restarting sign-in.
- **Credential store unavailable:** unlock or enable the native platform credential store; Sigil will not fall back to a file.
- **Destination rejected or budget exhausted:** review the Network disclosure and Web policy; retry only after correcting the destination or limit.
- **Secret blocked:** keep the block unless you understand why this server needs the data.
- **Server stale:** refresh it after configuration or capability changes.

See [Troubleshooting](troubleshooting.md#mcp-server-is-missing-failed-or-deferred) for the symptom path and [Configuration Reference](configuration-reference.md#code-intelligence-terminal-plugins-and-mcp) for fields.

<!-- public-doc-cta: open-troubleshooting -->
Next: [Use the MCP troubleshooting path](troubleshooting.md).
