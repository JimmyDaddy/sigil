# User Changelog

[Docs home](README.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page is a user-facing summary. Maintainer-facing implementation details still live under `dev/docs/*` and release automation scripts. The first public version is an early preview: expect the core TUI workflow to work, but do not treat config, plugin APIs, advanced sandbox behavior, or automation surfaces as stable compatibility contracts yet.

## Current Documentation Update

The user documentation has been reorganized around task paths:

- Quickstart for first-run setup.
- Workflows and Cookbook for practical prompts.
- Safety and privacy pages for permissions, secrets, MCP, and session logs.
- Troubleshooting with a decision-tree entrypoint.
- Reference page for commands, keys, paths, and environment variables.
- GitHub Pages site with a documentation hub and generated docs pages.

## Current Capability Snapshot

Sigil currently documents these user-facing capabilities:

- TUI-first workflow through `sigil`.
- npm, Homebrew tap, Cargo git-tag, GitHub release archive, and checkout install paths.
- Quick Setup and `/config`.
- `sigil doctor` and `/doctor`.
- Durable multi-step tasks through `/task`, with `/plan` kept read-only until the user explicitly accepts a plan-to-task handoff.
- Session restore from append-only logs.
- Approval-backed file changes, shell execution, MCP, and LSP edits.
- DeepSeek, OpenAI-compatible, Anthropic, and Gemini providers.
- stdio MCP servers.
- Optional code intelligence.
- Terminal mouse capture and OSC52 clipboard support.
- Verification status for task completion and explicit user-approved checks.
- Core execution sandbox receipts for supported local backends, with platform-specific limitations documented separately.

## Release Archive Notes

Release archive validation is available through:

```bash
scripts/build-release-archive.sh
```

Tagged releases build archives, checksums, GitHub provenance attestations, `sigil-ai.rb` for the Homebrew tap, and npm package tarballs derived from the archives. Self-update remains future packaging work unless a later release states otherwise.

## Where To Find More Detail

- User support status: [status.md](status.md)
- Install and update: [installation.md](installation.md)
- Full configuration: [configuration.md](configuration.md)
- Developer architecture and RFC details: `dev/docs/sigil-rust-agent-core-technical-solution.md` and `dev/docs/rfcs/`
