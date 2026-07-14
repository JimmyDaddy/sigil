# OpenAI Responses Provider

[Docs home](README.md) · [Provider guide](providers.md) · [Configuration](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [简体中文](../zh-CN/provider-openai-responses.md)

Use this provider when your selected model is served by the OpenAI Responses API at `/v1/responses`. It is separate from the [OpenAI-compatible provider](provider-openai-compatible.md), which speaks the Chat Completions protocol and is intended for compatible gateways.

## Minimal Setup

For temporary local use:

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

For a reusable config:

```toml
[agent]
provider = "openai_responses"
model = "gpt-4.1"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.openai_responses]
base_url = "https://api.openai.com/v1"
# Prefer SIGIL_OPENAI_RESPONSES_API_KEY.
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
```

A full starting template is available at [openai-responses.toml](../examples/config/openai-responses.toml).

## Authentication

Sigil resolves Responses authentication in this order:

1. `SIGIL_OPENAI_RESPONSES_API_KEY`
2. `[providers.openai_responses].api_key`

Optional `organization` and `project` fields are only needed for accounts that require them.

## Environment Overrides

| Variable | Overrides |
| --- | --- |
| `SIGIL_OPENAI_RESPONSES_BASE_URL` | `[providers.openai_responses].base_url` |

## Behavior Notes

The provider streams text, supported reasoning deltas, tool calls, and usage from the Responses event stream. It preserves each completed native output-item array as opaque provider state and reuses it unchanged for the matching assistant turn on later requests. This protects provider-specific items such as encrypted reasoning content without changing the Chat Completions provider contract.

Normal requests use the full local session context and do not use remote response-handle continuation. Background requests and provider-hosted tools are not enabled on this provider. The native compact endpoint is not a user action.

There is one guarded recovery exception: only the official `https://api.openai.com/v1` endpoint with the exact `gpt-4.1-2025-04-14` model snapshot can recover once from a provider-confirmed context-window rejection that happened before any output or side effect. Sigil counts the exact compacted target, records the checkpoint lifecycle, and retries that frozen target once. Aliases, compatible endpoints, ordinary errors, count failures, and restored sessions do not enter this path.

## Verify

Run:

```bash
sigil doctor
```

Check that `[agent].provider` is `openai_responses`, the base URL includes the expected `/v1` path, and the key source is `SIGIL_OPENAI_RESPONSES_API_KEY` or your local config.

## Common Problems

| Symptom | Check |
| --- | --- |
| 404 or route errors | Confirm that the service exposes the Responses route under the configured `/v1` root, not only Chat Completions. |
| Auth fails | Confirm `SIGIL_OPENAI_RESPONSES_API_KEY` or `[providers.openai_responses].api_key`. |
| Stream stops before a reply is final | The provider requires the terminal `response.completed` event; inspect the endpoint or gateway's SSE compatibility. |
| Tool calls are not accepted | Confirm the selected model supports Responses function tools. |
