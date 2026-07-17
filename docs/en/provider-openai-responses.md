<!-- public-doc-role: provider-openai-responses; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI Responses Provider

[Provider guide](providers.md) · [OpenAI-compatible](provider-openai-compatible.md) · [简体中文](../zh-CN/provider-openai-responses.md)

## Minimal Setup

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "openai_responses"
model = "gpt-4.1"

[providers.openai_responses]
base_url = "https://api.openai.com/v1"
```

See [openai-responses.toml](../examples/config/openai-responses.toml) for a copyable file.

## Authentication

`SIGIL_OPENAI_RESPONSES_API_KEY` takes priority over `[providers.openai_responses].api_key`. `organization` and `project` are optional account fields.

## Options And Visible Limits

`SIGIL_OPENAI_RESPONSES_BASE_URL` temporarily overrides `base_url`. This provider uses the Responses route, not Chat Completions. Background requests and provider-hosted tools are not enabled.

Image attachments work only for model IDs Sigil recognizes as image-capable. Unknown names and aliases are rejected before sending. On the official endpoint and supported dated snapshot, one context-window rejection before output may trigger one compact-and-retry attempt; compatible endpoints, aliases, restored sessions, and repeated failures do not.

## Verify

Run `sigil doctor` and confirm `openai_responses`, the `/v1` base URL, model, and credential source.

## Common Problems

- 404: confirm the service exposes `/v1/responses`, not only Chat Completions.
- Authentication: check the environment variable or config fallback.
- Stream ends early: confirm the endpoint emits a completed Responses event.
- Tool or image input fails: confirm the selected model supports that input.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
