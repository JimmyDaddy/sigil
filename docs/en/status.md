<!-- public-doc-role: status; authority: maturity-and-limit-authority; sections: supported-today,limited-or-advanced,not-supported-yet; cta: open-changelog -->

# Supported Today And Future Work

[Docs home](README.md) · [Installation](installation.md) · [Changelog](changelog.md) · [简体中文](../zh-CN/status.md)

Sigil is an early preview. Core Desktop and TUI workflows are usable, but config, plugins, advanced sandbox behavior, and automation interfaces can still change. Release numbers and install commands live in [Installation](installation.md) and the [Changelog](changelog.md).

## Supported Today

| Area | Current support |
|---|---|
| Providers | DeepSeek, OpenAI-compatible Chat Completions, OpenAI Responses, Anthropic, and Gemini; see [Providers](providers.md) |
| Non-interactive interfaces | Headless `run` supports text, JSON, and JSONL; advanced integrations can use authenticated local-only `serve` |
| Platforms | Desktop beta ships signed, Apple-notarized DMGs for Apple Silicon and Intel Macs; TUI primarily tests macOS and Linux, while Windows uses native PowerShell and reports its limits in Doctor |

## Limited Or Advanced

- Headless mode cannot ask for interactive approval; policy must decide in advance.
- The local service listens only on the local machine and requires bearer authentication.
- Code intelligence depends on language tools available in the launch environment.
- External-directory access is off by default, and sandbox strength varies by platform and backend.
- Deferred MCP servers must be activated before their tools are available.
- Image input is limited to supported formats, sources, providers, and model capabilities.
- Context compaction is offered only when Sigil can safely apply it for the selected model.
- Desktop Settings and the TUI/CLI can check the current release channel. Update installation is explicit, independently verifies signed/checksummed release metadata, and delegates npm, Homebrew, Cargo, or source-managed installations to their owning installer.
- Desktop and TUI are first-class product surfaces that reuse the same runtime semantics. The macOS beta is a public signed installation channel with Apple Silicon and Intel DMGs; TUI beta is published through npm, Homebrew, source tags, and release archives. Desktop offers bounded saved conversation history, run reattachment and control while the workspace service stays open, and dedicated tool, diff, approval, and verification surfaces. Its compact navigation keeps workspace selection in the top bar, makes each conversation row directly selectable, and shows verification only when evidence exists. The **Appearance** menu (`Cmd/Ctrl+,`) can follow the system or persist an app-wide light or dark choice without interrupting the active conversation.

## Not Supported Yet

Unattended background update installation or automatic restart, a stable plugin API, uniform sandbox guarantees across platforms, and resuming an in-flight child process after the desktop app or its workspace service restarts are not promised today. Desktop beta publishes architecture-specific signed updater bundles next to its DMGs, but the user remains in control of installing and restarting after an update.

For exact commands and keys, use [Reference](reference.md). For configuration fields, use [Configuration Reference](configuration-reference.md). For problems, use [Troubleshooting](troubleshooting.md).

<!-- public-doc-cta: open-changelog -->
Next: [Read the Changelog](changelog.md).
