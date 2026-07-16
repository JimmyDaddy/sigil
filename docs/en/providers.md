# Sigil Provider Guide

[Docs home](README.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [OpenAI Responses](provider-openai-responses.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/providers.md)

Sigil separates provider choice from the rest of the user workflow. This guide and the linked provider pages are the source of truth for provider selection, authentication variables, model endpoints, and provider-specific options. Shared workspace, permission, task, terminal, and tool settings remain in [Configuration](configuration.md).

## Choose A Provider

| Provider | Best fit | Image input V1 | Config value | Guide |
| --- | --- | --- | --- | --- |
| DeepSeek | Default Quick Setup path, DeepSeek chat, FIM, and DeepSeek-specific endpoint options. | Not supported | `deepseek` | [DeepSeek provider](provider-deepseek.md) |
| OpenAI-compatible | OpenAI or a compatible Chat Completions `/v1` gateway. | Not supported | `openai_compat` | [OpenAI-compatible provider](provider-openai-compatible.md) |
| OpenAI Responses | OpenAI Responses `/v1/responses` models and event stream. | Explicit recognized model IDs | `openai_responses` | [OpenAI Responses provider](provider-openai-responses.md) |
| Anthropic | Claude models through Anthropic Messages streaming. | Explicit recognized Claude IDs | `anthropic` | [Anthropic provider](provider-anthropic.md) |
| Gemini | Gemini models through `streamGenerateContent` and function calling. | Explicit recognized Gemini IDs | `gemini` | [Gemini provider](provider-gemini.md) |

For first use, run `sigil` and complete Quick Setup. Use manual config when you need repeatable local defaults, CI automation, or a provider not exposed by your current Quick Setup path.

## Provider Selection

Set `[agent].provider` and configure the matching `[providers.*]` block:

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[providers.deepseek]
# Provider block contains endpoint/auth/provider-specific fields.
```

`[agent].model` is the single chat-model setting. Role-specific task settings can still override the inherited agent provider/model for planner, executor, or subagent roles.

## Authentication Priority

Prefer environment variables for credentials. Plaintext `api_key` fields are supported for local-only configs, but `doctor` warns when a key is resolved only from config.

| Provider | Environment key | Config fallback |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `[providers.deepseek].api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `[providers.openai_compat].api_key` |
| OpenAI Responses | `SIGIL_OPENAI_RESPONSES_API_KEY` | `[providers.openai_responses].api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `[providers.anthropic].api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `[providers.gemini].api_key` |

Run this after changing credentials:

```bash
sigil doctor
```

Inside the TUI, `/doctor` shows the same provider and key-source checks in the transcript without printing secret values.

## Copyable Starting Points

Config templates live in [docs/examples/config](../examples/config):

- [deepseek-basic.toml](../examples/config/deepseek-basic.toml)
- [openai-compatible.toml](../examples/config/openai-compatible.toml)
- [openai-responses.toml](../examples/config/openai-responses.toml)
- [anthropic.toml](../examples/config/anthropic.toml)
- [gemini.toml](../examples/config/gemini.toml)

Use them as starting points, then review model names, base URLs, key sources, and permission policy before running in a real workspace.

## Behavior Boundaries

Provider-specific options stay on the matching provider page. The shared Sigil workflow remains consistent:

- Tool calls still go through the same approval and preview flow.
- Session and control records remain append-only.
- MCP trust and secret-egress policy do not change by provider.
- Provider-only options should not change your normal approval, privacy, or session workflow.

## Troubleshooting Path

If a provider fails:

1. Run `sigil doctor` and check provider name, model, base URL, and API key source.
2. Confirm `[agent].provider` matches the configured `[providers.*]` block.
3. Confirm the expected environment variable is visible in the same shell where `sigil` starts.
4. Check the provider-specific page for endpoint and timeout fields.
5. Keep `[permission].mode = "manual"` while diagnosing so write actions remain visible.
