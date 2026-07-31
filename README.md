<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/sigil-lockup-dark-mode.svg">
    <img src="assets/logo/sigil-lockup.svg" alt="Sigil" width="520">
  </picture>
</p>

<p align="center"><strong>Reviewable edits. Resumable sessions. Desktop or terminal.</strong></p>
<p align="center">A Rust coding agent with first-class Desktop and TUI experiences.</p>

<p align="center">
  <a href="https://github.com/JimmyDaddy/sigil/releases"><img src="https://img.shields.io/github/v/release/JimmyDaddy/sigil?include_prereleases&amp;sort=semver&amp;style=flat-square&amp;color=C85B4B" alt="Release"></a>
  <a href="https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/JimmyDaddy/sigil/ci.yml?branch=main&amp;style=flat-square&amp;label=build" alt="Build status"></a>
  <a href="https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml"><img src="https://img.shields.io/github/actions/workflow/status/JimmyDaddy/sigil/pages.yml?branch=main&amp;style=flat-square&amp;label=docs" alt="Documentation status"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/JimmyDaddy/sigil?style=flat-square&amp;color=242932" alt="MIT License"></a>
</p>

<p align="center">
  <a href="https://sigil.corerobin.com/">Website</a> ·
  <a href="https://sigil.corerobin.com/docs/">Documentation</a> ·
  <a href="docs/en/quickstart.md">Quickstart</a> ·
  <a href="https://sigil.corerobin.com/docs/visual-tour/">Visual tour</a>
</p>

<p align="center">English · <a href="README.zh-CN.md">简体中文</a></p>

<p align="center">
  <a href="https://sigil.corerobin.com/#demo">
    <img src="assets/demo/sigil-desktop-demo-poster.png" alt="Sigil Desktop workbench with plan, tool activity, and approval" width="900">
  </a>
</p>

<p align="center"><a href="https://github.com/JimmyDaddy/sigil/releases">View macOS prereleases</a> · <a href="https://sigil.corerobin.com/#demo">Watch Desktop + TUI demos</a> · <a href="docs/en/changelog.md">Changelog</a></p>

> [!WARNING]
> Sigil is under active development and continuous iteration, so features may be unstable and change without notice. The website and user docs follow `main`; packaged releases can lag behind. Check [Installation](docs/en/installation.md) and the [Changelog](docs/en/changelog.md) before relying on a newly documented feature.

## Why Sigil

| Work in context | Stay in control |
| --- | --- |
| **Desktop or terminal**<br>Use the surface that fits your workflow while keeping the same conversation, task, approval, and recovery semantics. | **Review before risk**<br>Inspect approvals and diffs before writes, commands, network access, or external integrations proceed. |
| **Resumable sessions**<br>Return to saved work and recover interrupted tasks without silently rerunning an unfinished tool. | **Models and tools, your way**<br>Choose among supported providers, add MCP integrations, and enable repository-aware assistance when you need it. |
| **Large outputs stay inspectable**<br>Sigil keeps bounded conversation views while preserving policy-safe tool output as session-scoped artifacts for precise, paged follow-up reads. | **Cache-stable context**<br>Historical tool output ages deterministically before semantic compaction, so long sessions shed token pressure without rewriting the active cached prefix. |

## Start in under a minute

```bash
npm install -g @sigil-ai/sigil@alpha
cd /path/to/your/project
sigil
```

Quick Setup opens when configuration is missing or invalid. No values are reused or migrated from an invalid file: Desktop Settings and the TUI offer an explicit reviewed replacement that is published only while the live file is still invalid. Choose a provider and model, add authentication, and run `sigil doctor` if anything looks incomplete. The [Quickstart](docs/en/quickstart.md) takes you from a first read-only task to a small reviewed change.

Prefer a native app? The [GitHub prerelease](https://github.com/JimmyDaddy/sigil/releases) provides signed and Apple-notarized macOS DMGs for Apple Silicon and Intel. See [Installation](docs/en/installation.md) for the exact asset names and update path.

A release may enable automatic Task routing and proactive read-only Explore agents for a new installation only when it ships an exact-route qualification manifest for its own binary. Other routes, releases without that sidecar, and every existing configuration keep the conservative `manual + explicit_request_only` behavior. This changes orchestration only; it never grants file, shell, network, MCP, external-directory, or merge permission. See [Advanced Configuration](docs/en/advanced-configuration.md#task-planning).

## Go deeper

| Guide | What it covers |
| --- | --- |
| [Visual tour](docs/en/visual-tour.md) and [TUI user guide](docs/en/user-guide.md) | Desktop workspaces plus TUI controls, approvals, sessions, and recovery. |
| [Configuration](docs/en/configuration.md) | Common setup paths and exact fields. |
| [Providers](docs/en/providers.md) and [MCP](docs/en/mcp.md) | Models, authentication, and integrations. |
| [Safety](docs/en/safety.md), [permissions](docs/en/permissions-and-sandbox.md), and [privacy](docs/en/privacy.md) | Decisions, limits, and data handling. |
| [Troubleshooting](docs/en/troubleshooting.md) | Symptoms, checks, and recovery actions. |
| [Reference](docs/en/reference.md) | Commands, keys, paths, and exit behavior. |

## Project

[Project status](https://sigil.corerobin.com/docs/status/) · [Contributing](CONTRIBUTING.md) · [Developer docs](dev/docs/index.md) · [Security](SECURITY.md) · [MIT License](LICENSE)
