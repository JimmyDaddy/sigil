# Sigil

English | [简体中文](README.zh-CN.md)

`sigil` is a TUI-first Rust AI coding agent. It brings chat, tool calls, approvals, diff review, run status, and session recovery into one terminal interface instead of asking users to learn an expanding command surface.

The project is still primarily run from source and validated through local development workflows. The main user-facing entrypoint is `sigil-tui`.

## Quick Start

Start the TUI:

```bash
cargo run -p sigil-tui
```

If no usable config exists, Sigil opens Quick Setup. You confirm the workspace, choose a model, and provide authentication there. If you prefer environment variables or a hand-written config file, see [docs/en/configuration.md](docs/en/configuration.md).

Use the CLI for automation or scripts:

```bash
cargo run -p sigil-cli -- run "summarize this repository"
```

Use `doctor` when setup or local tooling looks wrong:

```bash
cargo run -p sigil-cli -- doctor
```

Inside the TUI, use `/doctor` to render the same local diagnostics report in the transcript. Doctor includes actionable `fix:` lines and warns when the API key is only stored as plaintext in config.

The CLI is not the primary product surface. It is intentionally kept as an automation and debugging entrypoint.

## What It Does

- Run coding tasks inside the TUI and stream model output.
- Review approvals, affected files, and diff previews before risky tool calls.
- Inspect tool activities, command output, file changes, and diagnostics after a run.
- Restore the latest session after restarting the TUI.
- Use `/config` for common settings and `/resume` for session selection.
- Use `/doctor` to diagnose config, authentication, MCP, LSP, and terminal readiness with suggested fixes.
- Use `/model` and `/effort` to adjust the next model run.
- Use `/compact` to compact long-session context.
- Use mouse clicks, scrolling, transcript drag selection, and OSC52 copy when your terminal supports them.
- Optionally enable code intelligence for symbols, definitions, references, diagnostics, code actions, rename previews, and `/config` readiness checks.
- Optionally connect stdio MCP servers under explicit trust and approval policies.

## TUI Model

The TUI is centered on chat and the composer. You type a task, and Sigil shows assistant responses, tool activity, approval requests, run status, and session information in the same interface.

Common controls:

- `F1`: open keyboard help
- `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End`: scroll transcript history
- `/`: open the slash command selector
- `Shift-Tab`: cycle the default permission mode
- `Ctrl-C` or `Esc`: cancel the current run or leave the current overlay
- `Ctrl-G`: focus the latest tool activity
- `Alt-J` / `Alt-K`: move between activities
- `Ctrl-T`: expand or collapse thinking / activity content

See [docs/en/user-guide.md](docs/en/user-guide.md) for the full TUI guide.

## Configuration

Sigil resolves configuration in this order:

1. `--config <path>`
2. `./sigil.toml` in the current working directory
3. `sigil.toml` in the standard per-user config directory

Common per-user config paths:

- macOS: `~/Library/Application Support/sigil/sigil.toml`
- Linux: `$XDG_CONFIG_HOME/sigil/sigil.toml` or `~/.config/sigil/sigil.toml`
- Windows: `%APPDATA%\sigil\sigil.toml`

For examples covering authentication, provider settings, permissions, memory, compaction, code intelligence, terminal compatibility, and environment variable overrides, see [docs/en/configuration.md](docs/en/configuration.md). For real-terminal mouse and clipboard smoke checks, see [docs/en/terminal-compatibility.md](docs/en/terminal-compatibility.md).

## Providers

Sigil currently supports DeepSeek and OpenAI-compatible Chat Completions providers. DeepSeek remains the default Quick Setup path; OpenAI-compatible endpoints are configured with `provider = "openai_compat"` and `[providers.openai_compat]`.

## MCP

Sigil can connect stdio MCP servers as external tool providers. MCP tools, resources, and prompts use the same approval, activity, session control, secret egress, and trust policy surfaces as built-in tools.

See [docs/en/mcp.md](docs/en/mcp.md) for configuration and safety notes.

## Documentation

User documentation:

- [TUI user guide](docs/en/user-guide.md) / [中文](docs/zh-CN/user-guide.md)
- [Configuration guide](docs/en/configuration.md) / [中文](docs/zh-CN/configuration.md)
- [MCP guide](docs/en/mcp.md) / [中文](docs/zh-CN/mcp.md)

Developer documentation:

- [Code standards](dev/governance/code-standards.md)
- [Engineering standards](dev/governance/engineering-standards.md)
- [Core technical solution](dev/docs/sigil-rust-agent-core-technical-solution.md)
- [Current implementation notes](dev/docs/current-implementation-notes.en.md) / [中文](dev/docs/current-implementation-notes.md)
- [Capability roadmap](dev/docs/sigil-capability-roadmap.md)
- [Agent collaboration instructions](AGENTS.md)

## Development Checks

Code changes should run the relevant repository gates:

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

Docs-only changes do not need the full Rust gate, but links, paths, and example commands should still be checked.
