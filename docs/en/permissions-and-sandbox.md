<!-- public-doc-role: permissions-and-sandbox; authority: permission-network-sandbox-authority; sections: choose-a-permission-mode,review-before-an-action-runs,narrow-command-and-path-rules,network-and-web-tools,sandbox-expectations; cta: review-safety -->

# Permissions And Sandbox

[Docs home](README.md) · [Configuration](configuration.md) · [Safety](safety.md) · [Privacy](privacy.md) · [简体中文](../zh-CN/permissions-and-sandbox.md)

This page is the operational authority for local permissions, external paths, network access, and sandbox expectations.

## Choose A Permission Mode

```toml
[permission]
mode = "manual"
```

| Mode | Use | Default behavior |
| --- | --- | --- |
| `read-only` | Exploration and review | Workspace reads and recognized read-only commands can run; writes and mutating or unclassified commands are denied. Network still follows its own policy. |
| `manual` | Normal interactive work | Reads proceed; changes and commands usually ask. |
| `auto-edit` | Supervised file editing | Workspace edits can proceed; commands still usually ask. |
| `danger-full-access` | Closely supervised automation | Local access is broad, but network, protected paths, and other hard limits still apply. |

`manual` is the recommended starting point. A specific deny always remains stricter than a broad mode.

## Review Before An Action Runs

Check the summary, path or destination, command, and diff before choosing a decision. A plan or earlier approval is not permission for a different action. Headless `sigil run` cannot open an approval modal; an unresolved `ask` action fails.

Interactive approval surfaces show the safely projected command or tool input and update the recorded card after a decision. **Allow for session** appears only when the policy can derive a bounded grant for equivalent requests; it does not authorize unrelated commands, destinations, or risk classes. Recognized read-only shell structures may run as reads, while mutating or unclassified shell syntax remains subject to the configured command policy.

## Narrow Command And Path Rules

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

Prefer a few narrow patterns. When several rules match, deny wins over ask, and ask wins over allow.

<!-- public-doc-topic: external-directory -->

Workspace-external paths are disabled by default:

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Enabling this section does not make every external path safe or accessible; each path still follows its rule and protected-path checks. Use `$SIGIL_SCRATCH_DIR` for command scratch files when possible.

## Network And Web Tools

<!-- public-doc-topic: network-control -->

Network policy is independent of local permission mode:

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"
```

`allow` lets supported read-only search and fetch calls proceed while destination checks and limits still run. `ask` offers a one-time or same-tool session decision. `deny` disables Web access. A session decision never grants another tool, a write-like request, or a denied destination. Read [Privacy](privacy.md) before choosing a third-party route or sending sensitive queries.

Remote MCP and MCP OAuth follow this independent network boundary too. `auto-edit` does not silently authorize OAuth discovery, token exchange, refresh, or revocation. One sign-in can contact the MCP resource and a separate authorization server, so Sigil can show more than one destination disclosure. A session approval does not expose token values, authorize another kind of request, or bypass destination checks.

## Sandbox Expectations

<!-- public-doc-topic: sandbox-limit -->

Permission answers whether Sigil may attempt an action. A sandbox is an optional operating-system boundary applied afterward. The default local strategy is not an OS sandbox and does not guarantee filesystem, network, credential, or process isolation.

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt" # or linux_bubblewrap / docker
profile = "workspace_write"
fallback = "deny"
```

Availability and protection depend on the host, backend, profile, and action. A sandboxed command does not make remote services, MCP servers, plugins, containers, or every process path safe. With `fallback = "deny"`, an unavailable backend stops the action instead of silently running it locally. Run `sigil doctor` after changing execution settings.

Verification commands have their own declared behavior and approval needs. Configure them through [Advanced configuration](advanced-configuration.md#verification); field defaults are in [Configuration Reference](configuration-reference.md#permission).

<!-- public-doc-cta: review-safety -->
Next: [Review the Safety decision checklist](safety.md).
