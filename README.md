# Sigil

<p align="center">
  <img src="assets/logo/sigil-full-on-white.png" alt="Sigil logo" width="560">
</p>

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil is a TUI-first Rust coding agent for real repository work. It keeps chat, tool calls, approvals, diffs, diagnostics, planning, and session recovery inside one terminal interface, while keeping the CLI as a thin automation surface.

[Website](https://jimmydaddy.github.io/sigil/) · [Docs](docs/en/README.md) · [Quickstart](docs/en/quickstart.md) · [Visual tour](docs/en/visual-tour.md) · [Provider guide](docs/en/providers.md)

Sigil is currently best installed from a checkout. Package-manager distribution and self-update remain future packaging work.

## Quickstart

Prerequisites:

- A modern terminal emulator.
- A Rust toolchain compatible with this repository.
- A model provider credential. Quick Setup can collect it on first launch.

Install Sigil:

```bash
git clone https://github.com/JimmyDaddy/sigil.git
cd sigil
cargo install --path crates/sigil --locked
```

Start Sigil in the repository you want it to work on:

```bash
cd /path/to/your/project
sigil
```

If Sigil cannot find a usable config, it opens Quick Setup. Confirm the workspace, choose a provider/model, and enter authentication there. For repeatable config files and environment variables, see [Configuration](docs/en/configuration.md).

Check local setup:

```bash
sigil --version
sigil doctor
```

## What Sigil Does

- Keeps coding work in the TUI: transcript, composer, live tool activity, approvals, status, usage, and controls.
- Lets the agent read, search, edit, and run commands through structured tools.
- Shows risky write operations through approval cards, affected files, and bounded diffs.
- Restores sessions from append-only JSONL records under `.sigil/sessions/`.
- Supports `/plan` for durable multi-step work with planner, executor, and optional subagent roles.
- Connects stdio MCP servers under explicit trust, approval, and secret-egress policy.
- Optionally enables code intelligence for symbols, references, diagnostics, code actions, and rename previews.

## Daily Workflow

Run `sigil` with no subcommand for normal work. Common TUI entry points:

| Need | Use |
| --- | --- |
| Ask or edit normally | Type in the composer |
| Edit long composer drafts | `Ctrl-A/E`, `Alt-B/F`, `Ctrl-K/Y`, `Ctrl-Z` |
| Plan a larger task | `/plan <task>` |
| Switch or rename visible parent/child agent transcript | Composer agent panel (`Down`, `Up/Down`, `Enter`), `Alt-A`, `Shift-Alt-A`, `/agent`, or `/agent rename <child-id|current> <name>` |
| Start or switch sessions | `/new`, `/resume` |
| Change common settings | `/config` |
| Diagnose setup/auth/MCP/LSP | `/doctor` |
| Cycle default permission mode | `Shift-Tab` |
| Cancel a run or close an overlay | `Ctrl-C` or `Esc` |

The full keyboard, mouse, transcript selection, and OSC52 clipboard behavior is covered in the [TUI user guide](docs/en/user-guide.md) and [terminal compatibility checklist](docs/en/terminal-compatibility.md).

## Safety And State

Sigil treats tool execution as auditable state, not hidden side effects.

- File writes, edits, deletes, command execution, MCP calls, and external data access go through the permission model.
- Write tools are designed around previews and diff approval.
- Interrupted tool executions are restored as interrupted results instead of being replayed silently.
- Provider-specific behavior stays in provider crates; `sigil-kernel` keeps generic agent, tool, session, approval, and event contracts.

## Providers And Integrations

| Capability | Config surface | Best for | Details |
| --- | --- | --- | --- |
| DeepSeek | `[providers.deepseek]` | Default Quick Setup path and DeepSeek-specific options. | [DeepSeek guide](docs/en/provider-deepseek.md) |
| OpenAI-compatible | `[providers.openai_compat]` | Chat Completions-compatible `/v1` endpoints. | [OpenAI-compatible guide](docs/en/provider-openai-compatible.md) |
| Anthropic | `[providers.anthropic]` | Claude models through Anthropic Messages streaming. | [Anthropic guide](docs/en/provider-anthropic.md) |
| Gemini | `[providers.gemini]` | Gemini models through `streamGenerateContent`. | [Gemini guide](docs/en/provider-gemini.md) |
| MCP servers | `[[mcp_servers]]` | External stdio tools with explicit trust policy. | [MCP guide](docs/en/mcp.md) |
| Code intelligence | `[code_intelligence]` | LSP-backed symbols, references, diagnostics, actions, and rename previews. | [Configuration](docs/en/configuration.md) |

## Find The Right Doc

| I want to... | Read |
| --- | --- |
| Try Sigil for the first time | [Quickstart](docs/en/quickstart.md) |
| See what the product looks like | [Visual tour](docs/en/visual-tour.md) |
| Learn the TUI, commands, keys, sessions, and approvals | [TUI user guide](docs/en/user-guide.md) |
| Configure providers, permissions, memory, planning, terminal, or LSP | [Configuration](docs/en/configuration.md) |
| Choose or troubleshoot a model provider | [Provider guide](docs/en/providers.md) |
| Understand approval, workspace, MCP, and data boundaries | [Safety](docs/en/safety.md) and [Privacy](docs/en/privacy.md) |
| Fix setup, auth, terminal, MCP, or LSP issues | [Troubleshooting](docs/en/troubleshooting.md) |
| Look up every command, key, path, and environment variable | [Reference](docs/en/reference.md) |
| Work on Sigil itself | [Code standards](dev/governance/code-standards.md), [engineering standards](dev/governance/engineering-standards.md), and [core technical solution](dev/docs/sigil-rust-agent-core-technical-solution.md) |

## Project Maintenance

Project site source lives in [site](site). The generated Pages site is validated by:

```bash
scripts/check-pages-site.sh
```

Code changes should run the relevant repository gates:

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

Docs-only changes do not need the full Rust gate, but links, paths, and example commands should still be checked. Logo files live in [assets/logo](assets/logo).
