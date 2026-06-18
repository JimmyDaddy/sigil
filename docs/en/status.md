# Supported Today And Future Work

[Docs home](README.md) · [Installation](installation.md) · [Changelog](changelog.md) · [简体中文](../zh-CN/status.md)

This page separates what users can rely on today from what is experimental, limited, or future packaging work.

## Supported Today

| Area | Status |
| --- | --- |
| TUI entrypoint | `sigil` opens the TUI and is the primary product surface. |
| Source install | `cargo install --path crates/sigil --locked` is the recommended install path. |
| Quick Setup | First-run setup can create a usable local config. |
| Doctor | `sigil doctor` and `/doctor` report config, auth, workspace, MCP, code intelligence, and terminal readiness. |
| Chat workflow | Users can work through the composer with visible tool activity. |
| Tool approvals | File writes, shell execution, external paths, and external tools can be reviewed before execution. |
| Session recovery | Session and control records are append-only and can restore visible state after restart. |
| Planning | `/plan <task>` creates durable multi-step task state, and `/plan continue` resumes the latest task. |
| DeepSeek provider | DeepSeek is the default Quick Setup path. |
| OpenAI-compatible provider | Supported through `[providers.openai_compat]` for compatible Chat Completions endpoints. |
| Anthropic provider | Supported through `[providers.anthropic]` for Anthropic Messages streaming. |
| Gemini provider | Supported through `[providers.gemini]` for Gemini `streamGenerateContent` streaming. |
| MCP stdio servers | Supported through `[[mcp_servers]]` with trust and approval policy. |
| Code intelligence | Optional, disabled by default, with LSP discovery and Rust fallback behavior. |
| Terminal controls | Mouse capture, OSC52 copy, scroll sensitivity, and terminal diagnostics are documented and configurable. |

## Limited Or Advanced

| Area | Current expectation |
| --- | --- |
| Release archives | Can be built locally and by tagged release workflows; source install remains the main path. |
| Homebrew formula asset | Generated for tap maintainers, but independent tap publishing is separate work. |
| OpenAI-compatible differences | The provider intentionally omits DeepSeek-only prefix/FIM/beta behaviors. |
| Provider-specific semantics | Anthropic and Gemini request/event details stay in provider crates; `sigil-kernel` only exposes provider-neutral capabilities and chunks. |
| Code intelligence | Depends on installed language servers and local environment; normal chat does not require it. |
| MCP lazy startup | Lazy servers are configured but do not register fake tools until activated. |
| External directories | Disabled by default and should stay narrow and approval-backed. |
| Headless automation | `sigil run` is useful for scripts but cannot show interactive approval modals. |

## Future Work

These are not the current supported path unless a later release says otherwise:

- package-manager distribution as the main install path;
- self-update;
- desktop shell;
- hosted documentation search;
- richer generated release notes;
- broader provider-specific setup assistants;
- fully automated terminal screenshot generation for release docs.

## How To Read The Docs

User docs describe current behavior unless a section explicitly says "future work" or "advanced". Developer docs under `dev/docs/*` can describe architecture direction and implementation snapshots; they are not always the same as stable user support commitments.
