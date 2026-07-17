<!-- public-doc-role: provider-openai-compatible; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI-Compatible Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-openai-compatible.md)

## Minimal Setup

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
```

See [openai-compatible.toml](../examples/config/openai-compatible.toml) for a copyable file.

## Authentication

`SIGIL_OPENAI_COMPATIBLE_API_KEY` takes priority over `[providers.openai_compat].api_key`. `organization` and `project` are optional account fields.

## Options And Visible Limits

`SIGIL_OPENAI_COMPATIBLE_BASE_URL` temporarily overrides `base_url`. The endpoint and model must support streamed Chat Completions and tool calls.

Generic compatible endpoints do not accept image attachments through Sigil, even if a specific service offers its own multimodal extension. DeepSeek-only FIM and strict-tool settings also do not apply here.

## Verify

Run `sigil doctor` and confirm `openai_compat`, the expected `/v1` base URL, model, and credential source.

## Common Problems

- 404: point `base_url` at the compatible `/v1` root.
- Authentication: check the environment variable or config fallback.
- Tool calls fail: confirm endpoint and model support streamed tool calls.
- Wrong account: review `organization`, `project`, and provider dashboard settings.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
