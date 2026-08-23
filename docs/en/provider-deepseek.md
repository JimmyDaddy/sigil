<!-- public-doc-role: provider-deepseek; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# DeepSeek Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-deepseek.md)

## Minimal Setup

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.deepseek-default.options]
fim_model = "deepseek-v4-pro"
```

See [deepseek-basic.toml](../examples/config/deepseek-basic.toml) for a copyable file.

## Authentication

The example binds this connection to `SIGIL_API_KEY`. You can instead choose the **protected credential store** in setup or `/config`; `sigil.toml` stores only an opaque `stored` ID. The default `file` mode uses owner-only `~/.sigil/credentials.json` without system authentication prompts. Pasted secrets are never valid connection fields.

## Options And Visible Limits

`base_url` belongs to this exact connection. `beta_base_url`, `anthropic_base_url`, `fim_model`, `strict_tools_mode`, and `user_id_strategy` belong under `[connections.deepseek-default.options]` and apply only to this DeepSeek route.

[`deepseek-v4-flash-vision-exp`](https://api-docs.deepseek.com/guides/vision) is bundled as an
experimental model and is the only DeepSeek model for which Sigil enables image input. With that
exact model ID, locally attached PNG, JPEG, and WebP files are sent as OpenAI-compatible image
content parts. Other DeepSeek model IDs reject attachments before dispatch. Sigil does not fetch
remote image URLs or claim support for additional formats merely because the provider accepts them.

Use `/model deepseek-v4-flash-vision-exp` for the current idle conversation. For a newly released
or private model ID not listed by discovery, type the full ID after `/model`; the selector shows a
**Use exact model ID** candidate instead of replacing it with a similarly named catalog model.

## Verify

Run `sigil doctor` and confirm `default=deepseek-default/deepseek-v4-flash`, the endpoint, credential source, and readiness.

## Common Problems

- Authentication: export `SIGIL_API_KEY` in the same shell that launches Sigil.
- Wrong model: check the exact `[agent].connection` plus `[agent].model` route and any task-role override.
- FIM unavailable: confirm `fim_model` and endpoint support.
- Slow stream: check network access and model-request timeouts.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
