<!-- public-doc-role: provider-openai-responses; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI Responses Provider

[Provider guide](providers.md) · [OpenAI-compatible](provider-openai-compatible.md) · [简体中文](../zh-CN/provider-openai-responses.md)

## Minimal Setup

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "openai-default"
model = "gpt-4.1"

[connections.openai-default]
label = "OpenAI"
provider = "openai"
protocol = "responses"
base_url = "https://api.openai.com/v1"
credential = { source = "environment", name = "SIGIL_OPENAI_RESPONSES_API_KEY" }
```

See [openai-responses.toml](../examples/config/openai-responses.toml) for a copyable file.

## Authentication

The example binds only this connection to `SIGIL_OPENAI_RESPONSES_API_KEY`. You can instead choose the secure credential store; `sigil.toml`, model cache, and session files contain no secret value. `organization` and `project` are optional connection options.

## Options And Visible Limits

This connection uses the Responses route, not Chat Completions. Keep endpoint and account options on this connection so another OpenAI or compatible account cannot supply a fallback. Background requests and provider-hosted tools are not enabled.

Image attachments work only for model IDs Sigil recognizes as image-capable. Unknown names and aliases are rejected before sending. On the official endpoint and supported dated snapshot, one context-window rejection before output may trigger one compact-and-retry attempt; compatible endpoints, aliases, restored sessions, and repeated failures do not.

## Verify

Run `sigil doctor` and confirm `default=openai-default/gpt-4.1`, the `responses` protocol, `/v1` endpoint, credential source, and readiness.

## Common Problems

- 404: confirm the service exposes `/v1/responses`, not only Chat Completions.
- Authentication: check the environment variable or repair this connection in `/config`; Sigil does not fall back to another connection.
- Stream ends early: confirm the endpoint emits a completed Responses event.
- Tool or image input fails: confirm the selected model supports that input.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
