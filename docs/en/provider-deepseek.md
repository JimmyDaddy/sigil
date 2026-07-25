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

The example binds this connection to `SIGIL_API_KEY`. You can instead choose the **secure credential store** in setup or `/config`; `sigil.toml` stores only an opaque `stored` ID. Default `auto` prefers the system store and may use owner-only `~/.sigil/credentials.json` only when it is unavailable. Pasted secrets are never valid connection fields.

## Options And Visible Limits

`base_url` belongs to this exact connection. `beta_base_url`, `anthropic_base_url`, `fim_model`, `strict_tools_mode`, and `user_id_strategy` belong under `[connections.deepseek-default.options]` and apply only to this DeepSeek route.

DeepSeek image input is not enabled. An attached image is rejected before a request is sent; choose a supported image provider instead.

## Verify

Run `sigil doctor` and confirm `default=deepseek-default/deepseek-v4-flash`, the endpoint, credential source, and readiness.

## Common Problems

- Authentication: export `SIGIL_API_KEY` in the same shell that launches Sigil.
- Wrong model: check the exact `[agent].connection` plus `[agent].model` route and any task-role override.
- FIM unavailable: confirm `fim_model` and endpoint support.
- Slow stream: check network access and model-request timeouts.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
