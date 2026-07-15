# Supported Today And Future Work

[Docs home](README.md) · [Installation](installation.md) · [Changelog](changelog.md) · [简体中文](../zh-CN/status.md)

This page separates what users can rely on today from what is experimental, limited, or future packaging work. The current alpha line is an early preview, not a stable API or plugin compatibility promise. Release versions and install commands are maintained in [Installation](installation.md) and the [Changelog](changelog.md).

**Version boundary:** this page and the GitHub Pages site track `main`. The packaged alpha remains `v0.0.1-alpha.2` and can lag behind the capabilities below; check [Unreleased](changelog.md#unreleased-main) before relying on a newer feature.

## Supported Today

| Area | Status |
| --- | --- |
| TUI entrypoint | `sigil` opens the TUI and is the primary product surface. |
| Distribution | npm alpha, Homebrew tap, Cargo git-tag, source, and release-archive paths are available; use [Installation](installation.md) for current commands and channel details. |
| Quick Setup | First-run setup can create a usable local config. |
| Doctor | `sigil doctor` and `/doctor` report config, auth, workspace, MCP, code intelligence, and terminal readiness. |
| Chat workflow | Users can work through the composer with visible tool activity. |
| Tool approvals | File writes, shell execution, external paths, and external tools can be reviewed before execution. |
| Session recovery | Session and control records are append-only and restore visible state after restart for current V2 session logs. Older raw session logs are reported as unsupported and left unchanged. |
| Checkpoint recovery | `Ctrl-R` previews an evidence-bound checkpoint and offers controlled file restore or a conversation fork that leaves files unchanged. |
| Planning | `/plan` runs read-only planning prompts and can explicitly hand an accepted plan to durable `/task` execution; `/task <task>` creates durable multi-step task state directly, and `/task continue` resumes the latest task. |
| Task verification | The Verification card exposes readiness, recommended checks, and inspectable snapshot and changeset evidence; `Alt-V` focuses it. |
| Context controls | Context pressure stays visible. Manual, fully idle hard-threshold, and queued pre-turn portable apply are enabled only after exact local admission. Owned preparation and source/queue CAS prevent stale dispatch. Overflow recovery remains temporarily frozen. |
| DeepSeek provider | DeepSeek is the default Quick Setup path. |
| OpenAI-compatible provider | Supported through `[providers.openai_compat]` for compatible Chat Completions endpoints. |
| OpenAI Responses provider | Supported through `[providers.openai_responses]` for Responses streaming endpoints. Guarded overflow recovery remains temporarily frozen while its owned preparation path is completed. |
| Anthropic provider | Supported through `[providers.anthropic]` for Anthropic Messages streaming. Its native-compaction beta driver records encrypted candidates only; it is not a user action and does not automatically change context. |
| Gemini provider | Supported through `[providers.gemini]` for Gemini `streamGenerateContent` streaming. |
| Web data tools | Stable `websearch` and capability-backed `webfetch` routes use separate network policy, durable egress disclosure, and external-source provenance. |
| MCP servers | Local stdio and user-root Streamable HTTP servers are supported through `[[mcp_servers]]` with trust, approval, and secret-egress policy. |
| Code intelligence | Optional, disabled by default, with LSP discovery and Rust fallback behavior. |
| Terminal controls | Mouse capture, OSC52 copy, scroll sensitivity, and terminal diagnostics are documented and configurable. |

## Limited Or Advanced

| Area | Current expectation |
| --- | --- |
| Release archives | Available on tagged GitHub releases for manual installs; package-manager installs are preferred. |
| Package-manager channels | Packaging names and availability may evolve during alpha; use [Installation](installation.md) as the current source of truth. |
| OpenAI-compatible differences | The provider intentionally omits DeepSeek-only prefix/FIM/beta behaviors. |
| Provider-specific options | Each provider page explains its available setup and options; normal tool approvals, privacy, and session behavior stay consistent. |
| Code intelligence | Depends on installed language servers and local environment; normal chat does not require it. |
| MCP lazy startup | Lazy servers are configured but do not register fake tools until activated. |
| External directories | Disabled by default and should stay narrow and approval-backed. |
| Headless automation | `sigil run` is useful for scripts but cannot show interactive approval modals. |
| Local server | `sigil serve` currently checks local server settings but does not start a service. |
| Execution sandbox | macOS, Linux, Docker, PTY, MCP stdio, and trusted plugin-hook paths have core coverage and receipts where supported, but coverage is not equivalent across all platforms and remote/container daemon scenarios. |
| Context help | Sigil can use relevant session/task information and a small set of workspace files. Comprehensive automatic codebase analysis remains future work. |
| Model quality reports | Internal automated checks exist, but repeatable end-user model comparisons and release trends are not a supported product feature yet. |

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
- built-in end-user model quality benchmarking;
- release-to-release visual regression review for generated terminal screenshots.

## How To Read The Docs

User docs describe current behavior unless a section explicitly says "future work", "limited", or "advanced". Treat the alpha line as usable for trials, not as a stable compatibility promise.
