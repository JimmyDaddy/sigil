# Sigil

<p align="center">
  <img src="assets/logo/sigil-wordmark-header.svg" alt="Sigil logo" width="420">
</p>

English | [简体中文](README.zh-CN.md)

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil is a TUI-first Rust coding agent for real repository work. It keeps chat, tool calls, approvals, diffs, diagnostics, planning, and session recovery inside one terminal interface, while keeping the CLI as a thin automation surface.

[Website](https://jimmydaddy.github.io/sigil/) · [Docs site](https://jimmydaddy.github.io/sigil/docs/) · [Quickstart](https://jimmydaddy.github.io/sigil/docs/quickstart/) · [Visual tour](https://jimmydaddy.github.io/sigil/docs/visual-tour/) · [Status](https://jimmydaddy.github.io/sigil/docs/status/)

Sigil's first alpha release is available through npm, Homebrew tap, Cargo git-tag installs, and GitHub release archives. `v0.0.1-alpha.1` is an early preview: core TUI workflows are usable, while configuration, plugin APIs, advanced sandbox coverage, and automation surfaces may still change. Self-update remains future packaging work.

## Quickstart

Prerequisites:

- A modern terminal emulator.
- One installer: npm, Homebrew, or a Rust toolchain compatible with this repository.
- A model provider credential. Quick Setup can collect it on first launch.

Install Sigil with one of the first-release package paths:

```bash
npm install -g @sigil-ai/sigil@alpha
```

```bash
brew install JimmyDaddy/sigil/sigil-ai
```

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil
```

If you prefer a source install, run this from a checkout:

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
- Restores sessions from append-only JSONL records under the per-user Sigil state directory.
- Supports `/plan` for read-only planning, then an explicit create-task handoff into durable `/task` work with planner, executor, and optional subagent roles.
- Honors explicit ordinary-chat requests to delegate to a subagent before accepting a final answer.
- Lets trusted agent profiles be invoked directly with `@profile <prompt>` or trusted profile slash names.
- Connects stdio MCP servers under explicit trust, approval, and secret-egress policy.
- Optionally enables code intelligence for symbols, references, diagnostics, code actions, and rename previews.

## Daily Workflow

Run `sigil` with no subcommand for normal work. Common TUI entry points:

| Need | Use |
| --- | --- |
| Ask or edit normally | Type in the composer |
| Paste multiline text or code | Paste into the composer; large pastes fold visually and submit in full |
| Edit long composer drafts | `Ctrl-A/E`, `Alt-B/F`, `Ctrl-K/Y`, `Ctrl-Z` |
| Plan before editing | `/plan` then type a prompt, or `/plan <prompt>`; accept a structured Plan ready card to create and run a durable task |
| Run a durable multi-step task | `/task <task>`; use `/task continue` for unfinished tasks |
| Add a follow-up while Sigil is busy | Submit ordinary chat while a run is active; Sigil shows it in Follow-ups and adds the user message when it dispatches at the next safe turn |
| Review pending follow-ups | `Tab` focuses the follow-up panel; `/queue show`, `/queue next`, `/queue interrupt`, `/queue edit`, and `/queue delete` are advanced controls |
| Require a child agent from chat | Say so explicitly, for example "use a sub-agent for ..." |
| Invoke a trusted agent profile directly | `@profile <prompt>` or a trusted profile slash name such as `/review-agent <prompt>` |
| Move a foreground child agent to background | Press `Ctrl-B` while Sigil is waiting for that agent |
| Switch or rename visible parent/child agent transcript | Composer agent panel (`Down`, `Up/Down`, `Enter`), `Alt-A`, `Shift-Alt-A`, `/agent`, or `/agent rename <child-id|current> <name>` |
| Inspect long child-agent results | Switch to the child transcript, or let `read_agent_result` explicitly page through the child final answer when extra detail is needed |
| Start or switch sessions | `/new`, `/resume`, or `sigil resume <session-id>` after exit |
| Change common settings | `/config` |
| Diagnose setup/auth/MCP/LSP | `/doctor` |
| Toggle the right info rail between compact and detail | `F2` |
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
