# OpenAI-Compatible Provider

[Docs home](README.md) · [Provider guide](providers.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/provider-openai-compatible.md)

Use the OpenAI-compatible provider when your endpoint implements the Chat Completions streaming shape. This includes OpenAI's `/v1` endpoint and compatible gateways.

## Minimal Setup

For temporary local use:

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

For a reusable config:

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
# Prefer SIGIL_OPENAI_COMPATIBLE_API_KEY.
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
```

A full starting template is available at [openai-compatible.toml](../examples/config/openai-compatible.toml).

## Authentication

Sigil resolves OpenAI-compatible authentication in this order:

1. `SIGIL_OPENAI_COMPATIBLE_API_KEY`
2. `[providers.openai_compat].api_key`

Optional `organization` and `project` fields are only needed for endpoints/accounts that require them.

## Environment Overrides

| Variable | Overrides |
| --- | --- |
| `SIGIL_OPENAI_COMPATIBLE_BASE_URL` | `[providers.openai_compat].base_url` |

These overrides are useful for CI and local experiments where you do not want to edit `sigil.toml`.

## Behavior Notes

This provider uses a Chat Completions-compatible API and supports Sigil conversations, tool calls, streaming replies, and usage reporting. Optional provider diagnostics may be shown when the service supplies them.

Image attachments are not enabled on the generic OpenAI-compatible route in V1. A compatible text/tool wire shape is not sufficient evidence that an endpoint accepts the same multimodal contract, so an attached-image request fails before provider transport. Use an explicitly supported native provider from [Image Attachments](user-guide.md#image-attachments).

It does not expose DeepSeek-only prefix/FIM, reasoning replay, strict tools mode, or beta endpoint settings. If you need those, use [DeepSeek provider](provider-deepseek.md).

## Verify

Run:

```bash
sigil doctor
```

Check that `[agent].provider` is `openai_compat`, the base URL includes the expected `/v1` path, and the key source is the intended environment variable.

## Common Problems

| Symptom | Check |
| --- | --- |
| 404 or route errors | Confirm `base_url` points to the Chat Completions-compatible `/v1` root. |
| Auth fails | Confirm `SIGIL_OPENAI_COMPATIBLE_API_KEY` or `[providers.openai_compat].api_key` is set. |
| Tool calls are not accepted | Confirm the selected endpoint/model supports streamed tool calls. |
| Wrong account/project is billed | Check `organization`, `project`, and provider dashboard settings. |
