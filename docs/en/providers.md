<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# Provider Guide

[Docs home](README.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/providers.md)

Choose the model service here, then use its page for setup and visible limits. Shared permissions, sessions, and tools do not need to be relearned for each provider.

## Choose A Provider

| Provider | Use it for | Image input | Config value |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | Default Quick Setup path and DeepSeek-specific options | No | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | Chat Completions-compatible `/v1` gateways | No | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | OpenAI Responses models | Recognized model IDs | `openai_responses` |
| [Anthropic](provider-anthropic.md) | Claude through Anthropic Messages | Recognized Claude IDs | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini and function calling | Recognized Gemini IDs | `gemini` |

Quick Setup is the shortest first-use path. Use manual config for repeatable local or CI defaults.

## Authentication Priority

Prefer environment variables. A config `api_key` fallback is plaintext in `sigil.toml`.

| Provider | Environment variable | Config fallback |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `[providers.deepseek].api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `[providers.openai_compat].api_key` |
| OpenAI Responses | `SIGIL_OPENAI_RESPONSES_API_KEY` | `[providers.openai_responses].api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `[providers.anthropic].api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `[providers.gemini].api_key` |

Run `sigil doctor` after changing a credential. It reports the source without printing the value.

## Copyable Starting Points

Templates are available under [`docs/examples/config`](../examples/config). Review the model, base URL, credential source, and permission settings before use.

## Troubleshooting Path

Check, in order: the `[agent].provider` value, selected model, provider block, base URL, credential visibility in the launching shell, and provider-specific limits. Keep `permission.mode = "manual"` while diagnosing, then use [Troubleshooting](troubleshooting.md) for shared symptoms.

<!-- public-doc-cta: open-provider-guide -->
Next: [Set up DeepSeek or choose another provider](provider-deepseek.md).
