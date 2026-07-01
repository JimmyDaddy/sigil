# DeepSeek Provider

[Docs home](README.md) · [Provider guide](providers.md) · [Configuration](configuration.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/provider-deepseek.md)

Use the DeepSeek provider when you want Sigil's default Quick Setup path, DeepSeek chat models, and DeepSeek-specific FIM or endpoint settings.

## Minimal Setup

For temporary local use, set the API key before launch:

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

For repeatable config, use:

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.deepseek]
base_url = "https://api.deepseek.com"
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# Prefer SIGIL_API_KEY. If written here, the key is stored as plaintext.
# api_key = "sk-..."
user_id_strategy = "stable_per_end_user"
strict_tools_mode = "auto"
```

A shorter template is available at [deepseek-basic.toml](../examples/config/deepseek-basic.toml).

## Authentication

Sigil resolves DeepSeek authentication in this order:

1. `SIGIL_API_KEY`
2. `DEEPSEEK_API_KEY`
3. `[providers.deepseek].api_key`

Prefer `SIGIL_API_KEY` for local shells and CI. If you save `api_key` through `/config`, it is written as plaintext to `sigil.toml`; keep that file private and out of commits.

## Common Fields

| Field | Purpose |
| --- | --- |
| `model` | Main chat model used by the agent unless a role overrides it. |
| `fim_model` | Model used for DeepSeek FIM-related flows when available. |
| `base_url` | Main DeepSeek API endpoint. |
| `beta_base_url` | DeepSeek beta endpoint for features that require it. |
| `anthropic_base_url` | DeepSeek Anthropic-compatible endpoint when a build uses that route. |
| `strict_tools_mode` | DeepSeek-specific tool strictness behavior. Use `auto` unless you need a known override. |
| `user_id_strategy` | How Sigil provides a stable user identifier to the provider. |

The TUI `/config` surface focuses on high-frequency fields such as `model`, `api_key`, `base_url`, and `fim_model`. Lower-frequency DeepSeek fields remain file or environment configuration.

## Verify

Run:

```bash
sigil doctor
```

Check that the report shows `deepseek`, the expected model, and the intended API key source. The report never prints the key value.

## Common Problems

| Symptom | Check |
| --- | --- |
| Sigil asks for setup again | Confirm the expected `sigil.toml` is found by the config resolution order. |
| Authentication fails | Confirm `SIGIL_API_KEY` is set in the shell that launches `sigil`. |
| Wrong model is used | Check both `[agent].model` and `[providers.deepseek].model`. |
| Slow or interrupted responses | Check network access and consider `[model_request].stream_idle_timeout_secs`. |
| FIM behavior is unavailable | Confirm `fim_model` is configured and supported by the selected endpoint. |
