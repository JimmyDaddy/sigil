<!-- public-doc-role: status; authority: maturity-and-limit-authority; sections: supported-today,limited-or-advanced,not-supported-yet; cta: open-changelog -->

# Supported Today And Future Work

[Docs home](README.md) · [Installation](installation.md) · [Changelog](changelog.md) · [简体中文](../zh-CN/status.md)

Sigil is an early preview. Core TUI work is usable, but config, plugins, advanced sandbox behavior, and automation interfaces can still change. Release numbers and install commands live in [Installation](installation.md) and the [Changelog](changelog.md).

## Supported Today

| Area | Current support |
|---|---|
| Providers | DeepSeek, OpenAI-compatible Chat Completions, OpenAI Responses, Anthropic, and Gemini; see [Providers](providers.md) |
| Non-interactive interfaces | Headless `run` supports text, JSON, and JSONL; advanced integrations can use authenticated local-only `serve` |
| Platforms | macOS and Linux are the main tested paths; Windows uses native PowerShell and reports its limits in Doctor |

## Limited Or Advanced

- Headless mode cannot ask for interactive approval; policy must decide in advance.
- The local service listens only on the local machine and requires bearer authentication.
- Code intelligence depends on language tools available in the launch environment.
- External-directory access is off by default, and sandbox strength varies by platform and backend.
- Deferred MCP servers must be activated before their tools are available.
- Image input is limited to supported formats, sources, providers, and model capabilities.
- Context compaction is offered only when Sigil can safely apply it for the selected model.
- An opt-in desktop shell can be built from `main` for contributor dogfood. It uses the same local server, approval, session, and verification contracts as the TUI, but it is not a supported install channel.

## Not Supported Yet

Self-update, a stable plugin API, signed or notarized desktop installers, a desktop update channel, uniform sandbox guarantees across platforms, and resuming an in-flight child process after restart are not promised today.

For exact commands and keys, use [Reference](reference.md). For configuration fields, use [Configuration Reference](configuration-reference.md). For problems, use [Troubleshooting](troubleshooting.md).

<!-- public-doc-cta: open-changelog -->
Next: [Read the Changelog](changelog.md).
