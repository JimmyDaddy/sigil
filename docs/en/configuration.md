# Sigil Configuration Guide

[Docs home](README.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Appearance](appearance.md) · [Advanced configuration](advanced-configuration.md) · [Field reference](configuration-reference.md) · [Providers](providers.md) · [简体中文](../zh-CN/configuration.md)

This is the recommended starting point for shared Sigil configuration. It covers where configuration lives, the smallest useful setup, workspace and storage choices, and where to find a focused setting. Provider credentials and provider-specific options are on the [Provider guide](providers.md).

## Choose The Right Page

| You want to… | Start here |
| --- | --- |
| Set up Sigil, select a workspace, or find the config file | This page |
| Change approvals, network access, external paths, or sandboxing | [Permissions and sandbox](permissions-and-sandbox.md) |
| Change the TUI theme, code highlighting, or colors | [Appearance](appearance.md) |
| Configure tasks, checks, memory, code intelligence, terminal, plugins, or MCP | [Advanced configuration](advanced-configuration.md) |
| Look up an exact `sigil.toml` field or value | [Configuration reference](configuration-reference.md) |
| Select a model service, endpoint, or credential | [Provider guide](providers.md) |

## Resolution Order

Sigil resolves configuration in this order:

1. `--config <path>`
2. `sigil.toml` in the user-visible Sigil configuration directory

The default user config is:

```text
~/.sigil/sigil.toml
```

Quick Setup writes this user config. A `sigil.toml` in the workspace is not loaded automatically; pass `--config <path>` when you deliberately want to use a local experimental config.

## Minimal Path

For normal interactive use, start Sigil in the project you want to work on and complete Quick Setup:

```bash
cd /path/to/workspace
sigil
```

For temporary use or CI, choose a provider and export its provider-specific credential before launch. The [Provider guide](providers.md#authentication-priority) contains the correct variable and copyable examples for each service; there is no one provider-neutral API-key variable.

If you prefer a small hand-written shared config, start here:

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

Then add the provider block from the provider page you selected. Copyable examples are available under [docs/examples/config](../examples/config).

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` resolves to the directory where you launched `sigil`, which lets one user config follow the repository you opened. File tools are limited to this workspace. They reject parent-path escapes, absolute paths, and symlinks that point outside it.

Use the [Permissions and sandbox](permissions-and-sandbox.md) guide before allowing a path outside the workspace or changing local command behavior.

## Storage And Session Paths

```toml
[storage]
state_root = "auto"
cache_root = "auto"

[session]
# log_dir = "sessions"
```

`state_root` holds durable per-user Sigil state such as session-adjacent records and artifacts. `cache_root` holds rebuildable scratch data. `session.log_dir` changes only session-log placement for the current workspace; it does not replace the state root.

`SIGIL_STATE_HOME` and `SIGIL_CACHE_HOME` override their corresponding roots. Prefer an absolute path for an override in `sigil.toml`. Repository-local reusable resources stay under the fixed `.sigil/` directory; use [Advanced configuration](advanced-configuration.md#memory-skills-and-agents) for those resources.

## Use Doctor When Setup Looks Wrong

Run:

```bash
sigil doctor
```

Inside the TUI, `/doctor` shows the same report. It checks configuration loading, workspace resolution, session location, provider and credential source, configured MCP servers, code-intelligence readiness, and terminal compatibility. It never prints a secret value and includes remediation guidance for warnings and errors.

Use the same config override when you launch with a non-default config:

```bash
sigil --config ./sigil.toml doctor
```

## Next

- Choose a model service in the [Provider guide](providers.md).
- Choose a safe editing and network posture in [Permissions and sandbox](permissions-and-sandbox.md).
- Customize the TUI in [Appearance](appearance.md).
- Set up tasks, checks, MCP, or terminal behavior in [Advanced configuration](advanced-configuration.md).
- Look up a field in the [Configuration reference](configuration-reference.md).
