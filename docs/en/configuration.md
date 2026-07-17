<!-- public-doc-role: configuration; authority: configuration-router; sections: choose-the-right-page,resolution-order,minimal-path,workspace,storage-and-session-paths,use-doctor-when-setup-looks-wrong; cta: open-configuration-reference -->

# Sigil Configuration Guide

[Docs home](README.md) · [Permissions](permissions-and-sandbox.md) · [Appearance](appearance.md) · [Advanced](advanced-configuration.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/configuration.md)

Start here for the normal configuration path. Provider credentials and service-specific settings belong in [Providers](providers.md).

## Choose The Right Page

| Goal | Page |
| --- | --- |
| Find config, choose a workspace, or set storage | This guide |
| Change approvals, network, external paths, or sandboxing | [Permissions and sandbox](permissions-and-sandbox.md) |
| Change the theme, syntax colors, or info rail | [Appearance](appearance.md) |
| Configure tasks, checks, memory, agents, context, terminal, plugins, or MCP | [Advanced configuration](advanced-configuration.md) |
| Look up an exact field or default | [Configuration Reference](configuration-reference.md) |

## Resolution Order

Sigil loads `--config <path>` when supplied; otherwise it uses the per-user config:

```text
~/.sigil/sigil.toml
```

Quick Setup writes the user config. A workspace `sigil.toml` is not loaded automatically; pass it explicitly when you intend to use it.

## Minimal Path

Open the repository and run `sigil`. Quick Setup handles the workspace, provider, model, and authentication. A minimal hand-written base is:

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[appearance]
info_rail = true
theme = "sigil_dark"
```

Add one provider block from the chosen provider page. Copyable starting points are under [`docs/examples/config`](../examples/config).

## Workspace

`workspace.root = "."` follows the directory where `sigil` starts. File tools stay within that workspace unless you deliberately enable a narrow external-directory rule. Review [Permissions and sandbox](permissions-and-sandbox.md) before doing so.

Shell choice and terminal behavior are covered by [Terminal compatibility](terminal-compatibility.md); prefer file tools for portable reads and edits.

## Storage And Session Paths

`[storage].state_root` stores per-user sessions and artifacts; `[storage].cache_root` stores rebuildable data. `SIGIL_STATE_HOME` and `SIGIL_CACHE_HOME` override those roots. `[session].log_dir` changes only the session-log location for the current workspace.

Retention limits are applied only through an explicit preview and confirmation under `/config` → **Storage**. Normal startup, resume, runs, and `sigil serve` do not delete sessions automatically. See [Manage saved sessions](user-guide.md#manage-saved-sessions).

## Use Doctor When Setup Looks Wrong

Run `sigil doctor`, or `/doctor` in the TUI. It checks configuration, workspace, session location, provider credential source, MCP, code intelligence, and terminal support without printing secret values. With an alternate config, use the same `--config <path>` argument.

Continue with [Permissions](permissions-and-sandbox.md), [Appearance](appearance.md), [Advanced configuration](advanced-configuration.md), or the [Field reference](configuration-reference.md) for the setting you need.

<!-- public-doc-cta: open-configuration-reference -->
Next: [Look up exact configuration fields](configuration-reference.md).
