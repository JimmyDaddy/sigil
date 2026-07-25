<!-- public-doc-role: provider-openai-compatible; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI-Compatible Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-openai-compatible.md)

## Minimal Setup

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "custom-default"
model = "gpt-4.1"

[connections.custom-default]
label = "Custom endpoint"
provider = "custom"
protocol = "chat_completions"
base_url = "https://api.openai.com/v1"
credential = { source = "environment", name = "SIGIL_OPENAI_COMPATIBLE_API_KEY" }
```

See [openai-compatible.toml](../examples/config/openai-compatible.toml) for a copyable file.

## Authentication

The example binds only this connection to `SIGIL_OPENAI_COMPATIBLE_API_KEY`. The secure credential store is also available in setup and `/config`; plaintext `api_key` connection fields are rejected in V2. `organization` and `project` are optional connection options.

## Options And Visible Limits

The endpoint and model must support streamed Chat Completions and tool calls. Each custom endpoint owns its exact URL, protocol, credential, and model catalog; Sigil never borrows a model or credential from another connection.

Generic compatible endpoints do not accept image attachments through Sigil, even if a specific service offers its own multimodal extension. DeepSeek-only FIM and strict-tool settings also do not apply here.

## Verify

Run `sigil doctor` and confirm `default=custom-default/gpt-4.1`, the `chat_completions` protocol, expected `/v1` endpoint, credential source, and readiness.

## Common Problems

- 404: point `base_url` at the compatible `/v1` root.
- Authentication: check the bound environment variable or repair this connection in `/config`.
- Tool calls fail: confirm endpoint and model support streamed tool calls.
- Wrong account: review `organization`, `project`, and provider dashboard settings.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
