# Sigil

<p align="center">
  <img src="assets/logo/sigil-full.png" alt="Sigil logo" width="560">
</p>

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil is a TUI-first Rust coding agent for working inside a real repository. It keeps chat, tool calls, approvals, diffs, diagnostics, planning, and session recovery in one terminal interface, while keeping the CLI as a thin automation surface.

Project site source lives in [site](site). After GitHub Pages is enabled for this repository, it publishes to [jimmydaddy.github.io/sigil](https://jimmydaddy.github.io/sigil/).

Sigil is still validated through local development workflows. The recommended path today is to install it from a checkout and launch the `sigil` binary from the workspace you want it to operate on.

## Quickstart

Prerequisites:

- A modern terminal emulator.
- A Rust toolchain compatible with this repository.
- A model provider credential. Quick Setup can collect it on first launch.

Install from the repository root:

```bash
cargo install --path crates/sigil --locked
```

Start Sigil in the project you want to work on:

```bash
cd /path/to/your/project
sigil
```

If Sigil cannot find a usable config, it opens Quick Setup. Confirm the workspace, choose a provider/model, and enter authentication there. For environment-variable and `sigil.toml` setups, see [configuration](docs/en/configuration.md).

Check the installed binary:

```bash
sigil --version
sigil doctor
```

## What Sigil Is Built For

Sigil is designed for coding sessions where you want the agent to understand the current repository, make tool-backed changes, and keep you in control before risky actions happen.

- Ask questions about the codebase and inspect streamed reasoning/output in the TUI.
- Let the agent read, search, edit, and run commands through structured tools.
- Review write operations with approval cards, affected files, and bounded diffs.
- Resume work from append-only session logs after restarting the TUI.
- Use `/plan` for durable multi-step tasks with planner, executor, and optional subagent roles.
- Connect stdio MCP servers under explicit trust, approval, and secret-egress policies.
- Optionally enable code intelligence for symbols, references, diagnostics, code actions, and rename previews.

## TUI Workflow

Run `sigil` with no subcommand to open the main interface. The default screen is chat-first: a transcript, a composer, live tool activity, and an info rail for session, permissions, agents, LSP, usage, and controls.

Common entry points:

- Type normally in the composer for ordinary chat or coding work.
- Use `/plan <task>` when a larger task should be planned before execution.
- Use `/new` to start a fresh session, `/resume` to switch sessions, and `/config` to update common settings.
- Use `/doctor` to render local diagnostics inside the transcript.
- Use `Shift-Tab` to cycle the default permission mode.
- Use `Ctrl-C` or `Esc` to cancel the current run or close an overlay.

The full keyboard, mouse, transcript selection, and OSC52 clipboard behavior is covered in the [TUI user guide](docs/en/user-guide.md) and [terminal compatibility checklist](docs/en/terminal-compatibility.md).

## Safety And State

Sigil treats tool execution as auditable state, not as hidden side effects.

- File writes, edits, deletes, command execution, MCP calls, and external data access go through the permission model.
- Write tools are designed to provide previews and diffs before approval.
- Session and control records are append-only JSONL under `.sigil/sessions/`.
- Interrupted tool executions are restored as interrupted results instead of being replayed silently.
- Provider-specific behavior stays in provider crates; `sigil-kernel` keeps generic agent, tool, session, approval, and event contracts.

## Automation

The CLI exists for scripts, CI, and diagnostics. It is not the primary product surface.

```bash
sigil run "summarize this repository"
sigil doctor
```

For local development without installing:

```bash
cargo run -p sigil
cargo run -p sigil -- doctor
```

## Providers And Integrations

Sigil currently supports:

- DeepSeek through `[providers.deepseek]`
- OpenAI-compatible Chat Completions through `[providers.openai_compat]`
- Anthropic Messages through `[providers.anthropic]`
- Gemini GenerateContent through `[providers.gemini]`
- stdio MCP servers through `[[mcp_servers]]`
- optional code intelligence through `[code_intelligence]`

DeepSeek remains the default Quick Setup path. Other providers are selected with `[agent].provider` and configured in their matching `[providers.*]` block.
See the [Provider guide](docs/en/providers.md) for provider-specific setup, key priority, and troubleshooting.

## Documentation

User documentation:

- [User docs home](docs/en/README.md) / [中文](docs/zh-CN/README.md)
- [Quickstart](docs/en/quickstart.md) / [中文](docs/zh-CN/quickstart.md)
- [Installation](docs/en/installation.md) / [中文](docs/zh-CN/installation.md)
- [Visual tour](docs/en/visual-tour.md) / [中文](docs/zh-CN/visual-tour.md)
- [Common workflows](docs/en/workflows.md) / [中文](docs/zh-CN/workflows.md)
- [Cookbook](docs/en/cookbook.md) / [中文](docs/zh-CN/cookbook.md)
- [TUI user guide](docs/en/user-guide.md) / [中文](docs/zh-CN/user-guide.md)
- [Safety and permissions](docs/en/safety.md) / [中文](docs/zh-CN/safety.md)
- [Configuration](docs/en/configuration.md) / [中文](docs/zh-CN/configuration.md)
- [Provider guide](docs/en/providers.md) / [中文](docs/zh-CN/providers.md)
- [Privacy and data handling](docs/en/privacy.md) / [中文](docs/zh-CN/privacy.md)
- [Troubleshooting](docs/en/troubleshooting.md) / [中文](docs/zh-CN/troubleshooting.md)
- [Command and key reference](docs/en/reference.md) / [中文](docs/zh-CN/reference.md)
- [MCP guide](docs/en/mcp.md) / [中文](docs/zh-CN/mcp.md)
- [Terminal compatibility](docs/en/terminal-compatibility.md) / [中文](docs/zh-CN/terminal-compatibility.md)
- [Supported today and future work](docs/en/status.md) / [中文](docs/zh-CN/status.md)
- [User changelog](docs/en/changelog.md) / [中文](docs/zh-CN/changelog.md)
- [Config examples](docs/examples/config)

Developer documentation:

- [Code standards](dev/governance/code-standards.md)
- [Engineering standards](dev/governance/engineering-standards.md)
- [Core technical solution](dev/docs/sigil-rust-agent-core-technical-solution.md)
- [Current implementation notes](dev/docs/current-implementation-notes.en.md) / [中文](dev/docs/current-implementation-notes.md)
- [Capability roadmap](dev/docs/sigil-capability-roadmap.md)
- [Release process](dev/docs/release-process.md)
- [Agent collaboration instructions](AGENTS.md)

## Brand Assets

Logo files live in [assets/logo](assets/logo). Use transparent `sigil-full.png` for README, release pages, and the website hero; use `sigil-mark-square-1024.png` for square package or social previews; use `*-on-white.png` variants when the target does not preserve transparency.

## Development

Code changes should run the relevant repository gates:

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

Docs-only changes do not need the full Rust gate, but links, paths, and example commands should still be checked.

Build a local release archive when validating distribution artifacts:

```bash
scripts/build-release-archive.sh
```
