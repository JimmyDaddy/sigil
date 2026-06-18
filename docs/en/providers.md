# Sigil Provider Guide

[Docs home](README.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/providers.md)

Sigil separates provider choice from the rest of the user workflow. The TUI, approvals, tools, sessions, MCP, and code-intelligence behavior stay the same; provider pages only explain model endpoint, authentication, and provider-specific options.

## Choose A Provider

| Provider | Best fit | Config value | Guide |
| --- | --- | --- | --- |
| DeepSeek | Default Quick Setup path, DeepSeek chat, FIM, and DeepSeek-specific endpoint options. | `deepseek` | [DeepSeek provider](provider-deepseek.md) |
| OpenAI-compatible | OpenAI or a compatible Chat Completions `/v1` gateway. | `openai_compat` | [OpenAI-compatible provider](provider-openai-compatible.md) |
| Anthropic | Claude models through Anthropic Messages streaming. | `anthropic` | [Anthropic provider](provider-anthropic.md) |
| Gemini | Gemini models through `streamGenerateContent` and function calling. | `gemini` | [Gemini provider](provider-gemini.md) |

For first use, run `sigil` and complete Quick Setup. Use manual config when you need repeatable local defaults, CI automation, or a provider not exposed by your current Quick Setup path.

## Provider Selection

Set `[agent].provider` and configure the matching `[providers.*]` block:

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[providers.deepseek]
model = "deepseek-v4-flash"
```

Provider-level `model` should normally match `[agent].model`. Role-specific task settings can still override the inherited agent provider/model for planner, executor, or subagent roles.

## Authentication Priority

Prefer environment variables for credentials. Plaintext `api_key` fields are supported for local-only configs, but `doctor` warns when a key is resolved only from config.

| Provider | Highest-priority key | Fallbacks |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `DEEPSEEK_API_KEY`, then `api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `OPENAI_API_KEY`, then `api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `ANTHROPIC_API_KEY`, then `api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `GEMINI_API_KEY`, `GOOGLE_API_KEY`, then `api_key` |

Run this after changing credentials:

```bash
sigil doctor
```

Inside the TUI, `/doctor` shows the same provider and key-source checks in the transcript without printing secret values.

## Copyable Starting Points

Config templates live in [docs/examples/config](../examples/config):

- [deepseek-basic.toml](../examples/config/deepseek-basic.toml)
- [openai-compatible.toml](../examples/config/openai-compatible.toml)
- [anthropic.toml](../examples/config/anthropic.toml)
- [gemini.toml](../examples/config/gemini.toml)

Use them as starting points, then review model names, base URLs, key sources, and permission policy before running in a real workspace.

## Behavior Boundaries

Provider-specific behavior stays inside provider configuration and provider crates. The shared Sigil workflow remains provider-neutral:

- Tool calls still go through the same approval and preview flow.
- Session and control records remain append-only.
- MCP trust and secret-egress policy do not change by provider.
- `sigil-kernel` should not gain DeepSeek, OpenAI, Anthropic, or Gemini-only public API terms.

## Troubleshooting Path

If a provider fails:

1. Run `sigil doctor` and check provider name, model, base URL, and API key source.
2. Confirm `[agent].provider` matches the configured `[providers.*]` block.
3. Confirm the expected environment variable is visible in the same shell where `sigil` starts.
4. Check the provider-specific page for endpoint and timeout fields.
5. Keep `[permission].default_mode = "ask"` while diagnosing so write actions remain visible.
