# Anthropic Provider

[Docs home](README.md) · [Provider guide](providers.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/provider-anthropic.md)

Use the Anthropic provider when you want Sigil to call Claude models through the Anthropic Messages streaming API.

## Minimal Setup

For temporary local use:

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

For reusable config:

```toml
[agent]
provider = "anthropic"
model = "claude-sonnet-4-5"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.anthropic]
base_url = "https://api.anthropic.com"
model = "claude-sonnet-4-5"
# Prefer SIGIL_ANTHROPIC_API_KEY or ANTHROPIC_API_KEY.
# api_key = "sk-ant-..."
anthropic_version = "2023-06-01"
max_tokens = 4096
beta_headers = []
```

A full starting template is available at [anthropic.toml](../examples/config/anthropic.toml).

## Authentication

Sigil resolves Anthropic authentication in this order:

1. `SIGIL_ANTHROPIC_API_KEY`
2. `ANTHROPIC_API_KEY`
3. `[providers.anthropic].api_key`

Prefer environment variables for local and CI use. Do not commit configs that contain plaintext `api_key` values.

## Environment Overrides

| Variable | Overrides |
| --- | --- |
| `SIGIL_ANTHROPIC_MODEL` | `[providers.anthropic].model` |
| `SIGIL_ANTHROPIC_BASE_URL` | `[providers.anthropic].base_url` |
| `SIGIL_ANTHROPIC_VERSION` | `[providers.anthropic].anthropic_version` |
| `SIGIL_ANTHROPIC_MAX_TOKENS` | `[providers.anthropic].max_tokens` |

## Behavior Notes

Sigil maps provider-neutral messages, tool specs, tool results, usage, and incremental tool arguments into Anthropic request and SSE events inside the provider crate. Anthropic-specific headers, versioning, and tool result shaping stay out of `sigil-kernel`.

Use `beta_headers` only when you know the Anthropic feature or endpoint requires them.

## Verify

Run:

```bash
sigil doctor
```

Check provider name, model, `anthropic_version`, base URL, and API key source.

## Common Problems

| Symptom | Check |
| --- | --- |
| Request rejected for version/header reasons | Confirm `anthropic_version` and `beta_headers`. |
| Output stops early | Review `max_tokens` and model limits. |
| Auth uses the wrong key | Check whether `SIGIL_ANTHROPIC_API_KEY` overrides `ANTHROPIC_API_KEY`. |
| Tool-use behavior differs from another provider | Compare provider support and keep permission policy unchanged while testing. |
