# Permissions And Sandbox

[Docs home](README.md) · [Configuration](configuration.md) · [Appearance](appearance.md) · [Advanced configuration](advanced-configuration.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/permissions-and-sandbox.md)

This guide explains what Sigil may do in your workspace, when it asks first, and which limits still apply after you approve an action. For ordinary setup, start with the [Configuration guide](configuration.md); use this page before changing a permission mode or connecting a networked tool.

## Choose A Permission Mode

```toml
[permission]
mode = "manual"
```

| Mode | Best for | What happens |
| --- | --- | --- |
| `read-only` | Review and exploration | File changes and local command execution are denied. Network reads still follow the separate network policy. |
| `manual` | Normal interactive work | Reads are allowed; file changes and local commands ask unless you add a narrower rule. |
| `auto-edit` | Fast, reviewable file edits | Workspace file edits are allowed; local command execution still asks by default. |
| `danger-full-access` | Closely supervised local automation | Local access is broadly allowed. It does not override a network ask/deny, protected paths, or other hard safety limits. |

`manual` is the recommended default. Switching modes changes the default, not every individual rule. A specific deny, a protected path, and an external-directory gate remain stricter than a broad local mode.

## Review Before An Action Runs

When Sigil needs approval, inspect the action summary, affected paths or command, and diff preview before choosing Allow or Deny. In a non-interactive `sigil run`, an action that needs approval reports an approval-required error instead of running silently.

Use the normal TUI flow for approval. Do not treat a plan, a task description, or a previous approval as permission for an unrelated command or destination.

## Narrow Command And Path Rules

Advanced rules belong in `sigil.toml` when you have a stable, repeatable need:

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Command patterns match normalized command text and support `*` and `?` wildcards. Prefer a few specific allow rules over a broad pattern. If the same command matches several command groups, deny wins over ask, and ask wins over allow.

Paths outside the workspace are disabled by default. Enabling external directories does not make them unrestricted: each matching path still follows the configured action and any protected-path rule. Temporary files for a command should use `$SIGIL_SCRATCH_DIR`; system temporary directories remain external paths unless you explicitly allow them.

The complete field list and precedence table are in the [Configuration reference](configuration-reference.md#permission).

## Network And Web Tools

Network access is evaluated separately from local file and process access:

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"  # auto | provider_hosted | mcp | bundled | disabled
```

With `network_mode = "allow"`, read-only web searches and fetches can proceed without a prompt, but Sigil still applies destination checks, records the request, and applies limits. With `ask`, the approval surface offers Allow once, Allow session, and Deny. An Allow session decision covers only the same read-only web tool in the current session; it never grants another tool, a write-like network action, or a previously denied destination.

`deny` disables web access. The bundled search route sends your normalized query to its stated search service; read the [Privacy guide](privacy.md) and [MCP guide](mcp.md) before enabling third-party tools or credentials.

Remote MCP and MCP OAuth follow this independent network boundary too. `auto-edit` does not silently authorize OAuth discovery, token exchange, refresh, or revocation. One sign-in can contact the MCP resource and a separate authorization server, so Sigil may show more than one destination disclosure. Session grants remain bound to the admitted network effect; they never expose token values or bypass destination checks.

## Sandbox Expectations

Permission decides whether Sigil may attempt an action. A sandbox is the operating-system boundary that may constrain a command after it is allowed. They are complementary, not interchangeable.

```toml
[execution]
strategy = "local"
```

`local` preserves normal local shell behavior and does not claim operating-system isolation. On supported systems, advanced users can select a sandbox strategy:

On Windows, local commands are owned by a kill-on-close Job Object so cancellation, timeout, and output failures can reap the registered process tree. This is lifecycle control only: it does not restrict filesystem access, network access, credentials, or tokens, and `local` remains unconfined. PowerShell and `cmd.exe` commands also stay at execute-level approval unless a future dialect-specific analyzer can prove a narrower effect; Sigil never applies Bash read-only classification to them.

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
profile = "workspace_write"
fallback = "deny"
```

Available backend names are `macos_seatbelt`, `linux_bubblewrap`, and `docker`. Availability and guarantees depend on the host, the selected profile, and the kind of action. If a required sandbox is unavailable, Sigil fails closed rather than silently claiming isolation. A sandboxed local command does not automatically make remote services, containers, or every external tool safe.

Check `sigil doctor` after changing execution settings. The [Safety guide](safety.md) covers the broader trust model, and the [Configuration reference](configuration-reference.md#execution) lists the supported fields.

## Verification Checks

Verification commands are configured separately because a check may read files, change files, or require permission:

```toml
[verification]

[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

Suggested repository checks are not automatically executed merely because Sigil discovers them. Promote only checks you understand. A check that changes relevant files needs a later non-writing check before its result can count as final verification.

See [Advanced configuration](advanced-configuration.md#verification) for the workflow and [Configuration reference](configuration-reference.md#verification) for every field.
