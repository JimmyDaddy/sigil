# Gemini Provider

[Docs home](README.md) · [Provider guide](providers.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [简体中文](../zh-CN/provider-gemini.md)

Use the Gemini provider when you want Sigil to call Gemini models through Google's `streamGenerateContent` API.

## Minimal Setup

For temporary local use:

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

For reusable config:

```toml
[agent]
provider = "gemini"
model = "gemini-2.5-pro"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com/v1beta"
model = "gemini-2.5-pro"
# Prefer SIGIL_GEMINI_API_KEY, GEMINI_API_KEY, or GOOGLE_API_KEY.
# api_key = "..."
```

A full starting template is available at [gemini.toml](../examples/config/gemini.toml).

## Authentication

Sigil resolves Gemini authentication in this order:

1. `SIGIL_GEMINI_API_KEY`
2. `GEMINI_API_KEY`
3. `GOOGLE_API_KEY`
4. `[providers.gemini].api_key`

Prefer `SIGIL_GEMINI_API_KEY` when you want Sigil-specific credentials without affecting other Google tooling in the same shell.

## Environment Overrides

| Variable | Overrides |
| --- | --- |
| `SIGIL_GEMINI_MODEL` | `[providers.gemini].model` |
| `SIGIL_GEMINI_BASE_URL` | `[providers.gemini].base_url` |

## Behavior Notes

Sigil maps provider-neutral messages, tool specs, function calls, function responses, usage, and block reasons into Gemini protocol details inside the provider crate. Gemini-specific `systemInstruction`, `functionDeclarations`, and `functionResponse` details stay out of `sigil-kernel`.

Gemini model names and endpoint availability can vary by account and region. Keep the model name explicit in config when using this provider in automation.

## Verify

Run:

```bash
sigil doctor
```

Check provider name, model, base URL and API key source.

## Common Problems

| Symptom | Check |
| --- | --- |
| Authentication fails | Confirm which of `SIGIL_GEMINI_API_KEY`, `GEMINI_API_KEY`, or `GOOGLE_API_KEY` is visible to the `sigil` process. |
| Model not found | Confirm the exact Gemini model name and endpoint version. |
| Tool/function calls fail | Confirm the model and endpoint support function calling for your account. |
| Requests time out | Check network access and consider `[model_request].stream_idle_timeout_secs`. |
