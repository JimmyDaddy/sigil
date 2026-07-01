# Supported Today And Future Work

[Docs home](README.md) · [Installation](installation.md) · [Changelog](changelog.md) · [简体中文](../zh-CN/status.md)

This page separates what users can rely on today from what is experimental, limited, or future packaging work. `v0.0.1` is an early preview, not a stable API or plugin compatibility promise.

## Supported Today

| Area | Status |
| --- | --- |
| TUI entrypoint | `sigil` opens the TUI and is the primary product surface. |
| npm install | `npm install -g @jimmydaddy/sigil` is the scoped first-release npm package path. |
| Homebrew tap | `brew install JimmyDaddy/sigil/sigil-ai` installs the tap formula while keeping the binary name `sigil`. |
| Cargo git install | `cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1 --locked sigil` installs from the release tag. |
| Source install | `cargo install --path crates/sigil --locked` remains supported for local checkout development. |
| Quick Setup | First-run setup can create a usable local config. |
| Doctor | `sigil doctor` and `/doctor` report config, auth, workspace, MCP, code intelligence, and terminal readiness. |
| Chat workflow | Users can work through the composer with visible tool activity. |
| Tool approvals | File writes, shell execution, external paths, and external tools can be reviewed before execution. |
| Session recovery | Session and control records are append-only and can restore visible state after restart. |
| Planning | `/plan` runs read-only planning prompts and can explicitly hand an accepted plan to durable `/task` execution; `/task <task>` creates durable multi-step task state directly, and `/task continue` resumes the latest task. |
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
| Release archives | Can be built locally and by tagged release workflows; package-manager artifacts are derived from them. |
| Homebrew formula asset | `sigil-ai.rb` is generated for tap maintainers; the tap repository update is performed outside this repository. |
| npm package tarballs | Generated from release archives for registry publishing and release-asset inspection. |
| OpenAI-compatible differences | The provider intentionally omits DeepSeek-only prefix/FIM/beta behaviors. |
| Provider-specific semantics | Anthropic and Gemini request/event details stay in provider crates; `sigil-kernel` only exposes provider-neutral capabilities and chunks. |
| Code intelligence | Depends on installed language servers and local environment; normal chat does not require it. |
| MCP lazy startup | Lazy servers are configured but do not register fake tools until activated. |
| External directories | Disabled by default and should stay narrow and approval-backed. |
| Headless automation | `sigil run` is useful for scripts but cannot show interactive approval modals. |
| HTTP/SSE adapter | `sigil serve` validates local bind/token defaults and prints a preflight plan; HTTP routing and listener startup remain future work. |
| Execution sandbox | macOS, Linux, Docker, PTY, MCP stdio, and trusted plugin-hook paths have core coverage and receipts where supported, but coverage is not equivalent across all platforms and remote/container daemon scenarios. |
| Context retrieval | Context V0 supports session/task memory and bounded repo-file candidates. Full semantic repo graph, impact graph, and vector retrieval remain evidence-gated future work. |
| Model evals | Deterministic eval infrastructure exists. Real-model eval runner, repeat policy, and release/nightly trend reports are not part of the supported user path yet. |

## Future Work

These are not the current supported path unless a later release says otherwise:

- self-update;
- desktop shell;
- hosted documentation search;
- richer generated release notes;
- broader provider-specific setup assistants;
- full semantic repo graph or vector retrieval by default;
- transparent in-flight crash resume for running shell/agent processes;
- parallel write-agent worktree isolation as a default workflow;
- stable plugin API compatibility;
- all-platform equivalent OS sandbox behavior;
- built-in real-model eval runner for end users;
- fully automated terminal screenshot generation for release docs.

## How To Read The Docs

User docs describe current behavior unless a section explicitly says "future work" or "advanced". Developer docs under `dev/docs/*` can describe architecture direction and implementation snapshots; they are not always the same as stable user support commitments.
