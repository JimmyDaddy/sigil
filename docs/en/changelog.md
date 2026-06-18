# User Changelog

[Docs home](README.md) · [Supported status](status.md) · [简体中文](../zh-CN/changelog.md)

This page is a user-facing summary. Maintainer-facing implementation details still live under `dev/docs/*` and release automation scripts.

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
- Source install from repository checkout.
- Quick Setup and `/config`.
- `sigil doctor` and `/doctor`.
- Durable planning through `/plan`.
- Session restore from append-only logs.
- Approval-backed file changes, shell execution, MCP, and LSP edits.
- DeepSeek, OpenAI-compatible, Anthropic, and Gemini providers.
- stdio MCP servers.
- Optional code intelligence.
- Terminal mouse capture and OSC52 clipboard support.

## Release Archive Notes

Release archive validation is available through:

```bash
scripts/build-release-archive.sh
```

Tagged releases can build archives and checksums. Package-manager distribution and self-update remain future packaging work unless a later release states otherwise.

## Where To Find More Detail

- User support status: [status.md](status.md)
- Install and update: [installation.md](installation.md)
- Full configuration: [configuration.md](configuration.md)
- Developer implementation snapshot: `dev/docs/current-implementation-notes.en.md`
