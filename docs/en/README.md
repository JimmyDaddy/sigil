# Sigil User Docs

[简体中文](../zh-CN/README.md)

Sigil is a TUI-first coding agent. The normal path is to open a repository, run `sigil`, chat in the terminal interface, review tool activity, and approve risky actions before they change files or run commands.

## Start Here

Use this path if you are new to Sigil:

1. [Quickstart](quickstart.md): install, launch, complete Quick Setup, and run your first useful session.
2. [Installation](installation.md): supported install, update, uninstall, and release archive notes.
3. [Visual tour](visual-tour.md): screenshot-style walkthrough of the main session, approval card, and config panel.
4. [TUI user guide](user-guide.md): screen layout, controls, sessions, approvals, planning, and code intelligence.
5. [Safety and permissions](safety.md): what can run, what needs approval, and how to review risky actions.
6. [Troubleshooting](troubleshooting.md): setup, auth, terminal, MCP, code intelligence, and recovery problems.

If you already have Sigil installed, the shortest loop is:

```bash
cd /path/to/workspace
sigil
```

Then ask a concrete task in the composer, for example:

```text
Explain the repository structure and point out the main entrypoints.
```

## What To Read By Task

| I want to... | Read |
| --- | --- |
| Try Sigil for the first time | [Quickstart](quickstart.md) |
| See what the product looks like | [Visual tour](visual-tour.md) |
| Install, update, or uninstall the binary | [Installation](installation.md) |
| Learn the TUI layout, keys, slash commands, and session behavior | [TUI user guide](user-guide.md) |
| See practical prompts for real coding tasks | [Common workflows](workflows.md) |
| Use copyable prompt patterns | [Cookbook](cookbook.md) |
| Understand approvals, workspace boundaries, and MCP trust | [Safety and permissions](safety.md) |
| Configure provider credentials, permissions, memory, planning, or code intelligence | [Configuration](configuration.md) |
| Choose DeepSeek, OpenAI-compatible, Anthropic, or Gemini | [Provider guide](providers.md) |
| Understand privacy, provider context, session logs, and secrets | [Privacy and data handling](privacy.md) |
| Add external tools through MCP | [MCP guide](mcp.md) |
| Fix setup, auth, terminal, MCP, or LSP issues | [Troubleshooting](troubleshooting.md) |
| Check every command, key, path, and environment variable in one place | [Reference](reference.md) |
| Validate mouse capture, OSC52 copy, tmux, SSH, or WSL behavior | [Terminal compatibility](terminal-compatibility.md) |
| Check current support commitments | [Supported today and future work](status.md) |
| Read user-facing release notes | [User changelog](changelog.md) |

## Product Model

Sigil is built around a few user-facing ideas:

- **The TUI is the product surface.** Run `sigil` without a subcommand for normal work. Subcommands such as `sigil doctor` and `sigil run` are for diagnostics and automation.
- **The launch directory matters.** With the normal `workspace.root = "."` setup, the directory where you start Sigil is the workspace the agent can inspect and modify.
- **Tools are visible work.** Reads, searches, edits, shell commands, MCP calls, and code-intelligence actions appear as activities in the transcript.
- **Risky actions require control.** File changes, shell execution, deletes, and external tools can require approval with summaries and diffs.
- **Sessions are durable.** Session and control records are append-only JSONL under the per-user Sigil state directory by default, so restart and recovery do not silently replay interrupted tools.

## Current Distribution Status

The first release is prepared for npm, Homebrew tap, Cargo git-tag installs, and GitHub release archives. `v0.0.1-alpha` is an early preview: the core TUI workflow is usable, while config, plugin APIs, advanced sandbox coverage, and automation surfaces may still change.

```bash
npm install -g @sigil-ai/sigil@alpha
brew install JimmyDaddy/sigil/sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha --locked sigil
```

Local checkout installs remain useful for development:

```bash
cargo install --path crates/sigil --locked
```

Self-update remains future packaging work.

## Configuration At A Glance

Most users should start with Quick Setup in the TUI. Manual configuration is useful when you need repeatable local defaults or CI automation.

Common choices:

- DeepSeek default provider: use `SIGIL_API_KEY` or `[providers.deepseek]`.
- OpenAI-compatible provider: use `[agent].provider = "openai_compat"` and `[providers.openai_compat]`.
- Anthropic provider: use `[agent].provider = "anthropic"` and `[providers.anthropic]`.
- Gemini provider: use `[agent].provider = "gemini"` and `[providers.gemini]`.
- Permission mode: keep `[permission].mode = "manual"` until you know which actions you want to allow automatically.
- Terminal compatibility: tune `[terminal].mouse_capture`, `[terminal].osc52_clipboard`, and `[terminal].scroll_sensitivity`.
- Code intelligence: enable `[code_intelligence].enabled = true` when you want LSP-backed symbol, reference, diagnostic, code action, and rename tools.

See [configuration.md](configuration.md) for shared config and [providers.md](providers.md) for provider-specific setup and environment variable priority.
Copyable config templates live in [docs/examples/config](../examples/config).

## Help Path

When something looks wrong:

```bash
sigil doctor
```

Inside the TUI:

```text
/doctor
```

The doctor report checks config loading, workspace resolution, session log location, provider authentication, MCP command/trust settings, code intelligence readiness, terminal profile, mouse capture, and OSC52 clipboard risk.
