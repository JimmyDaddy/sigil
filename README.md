# Sigil

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/sigil-full-staff-glow-dark-mode.svg">
    <img src="assets/logo/sigil-full-staff-glow.svg" alt="Sigil logo" width="560">
  </picture>
</p>

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil is a TUI-first coding agent for real repository work. It keeps conversation, edits, approvals, diffs, diagnostics, and recovery in one terminal workspace, with a small CLI surface for automation.

[Website](https://sigil.corerobin.com/) · [Documentation](https://sigil.corerobin.com/docs/) · [Visual tour](https://sigil.corerobin.com/docs/visual-tour/) · [Project status](https://sigil.corerobin.com/docs/status/)

Sigil is an early preview. The website and user docs follow `main`; packaged releases can lag behind. Check [Installation](docs/en/installation.md) for supported install and update paths, and [Changelog](docs/en/changelog.md) before relying on a newly documented feature.

## Start

Install the preview package:

```bash
npm install -g @sigil-ai/sigil@alpha
```

Then open the repository you want to work in:

```bash
cd /path/to/your/project
sigil
```

Quick Setup appears when configuration is missing. Choose a provider and model, add authentication, and run `sigil doctor` if anything looks incomplete. The [Quickstart](docs/en/quickstart.md) walks through a first read-only task and a small reviewed change.

## Why Sigil

- **TUI-first work:** follow the conversation, tool activity, changes, and next action without leaving the terminal.
- **Review before risk:** inspect approvals and diffs before writes, commands, network access, or external integrations proceed.
- **Work that can resume:** return to saved sessions and recover interrupted work without silently rerunning an unfinished tool.
- **Flexible models and tools:** choose among supported providers, add MCP integrations, and enable repository-aware assistance when you need it.

## Documentation

- [TUI user guide](docs/en/user-guide.md) — daily controls, approvals, sessions, and recovery.
- [Configuration](docs/en/configuration.md) — common setup paths and links to exact fields.
- [Providers](docs/en/providers.md) and [MCP](docs/en/mcp.md) — models, authentication, and integrations.
- [Safety](docs/en/safety.md), [permissions](docs/en/permissions-and-sandbox.md), and [privacy](docs/en/privacy.md) — decisions, limits, and data handling.
- [Troubleshooting](docs/en/troubleshooting.md) — symptoms, checks, and recovery actions.
- [Reference](docs/en/reference.md) — exact commands, keys, paths, and exit behavior.

## Project

Contributions are welcome; start with [CONTRIBUTING.md](CONTRIBUTING.md) and the [developer documentation index](dev/docs/index.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md). Sigil is distributed under the [MIT License](LICENSE).
