<!-- public-doc-role: provider-deepseek; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# DeepSeek Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-deepseek.md)

## Minimal Setup

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
base_url = "https://api.deepseek.com"
fim_model = "deepseek-v4-pro"
```

See [deepseek-basic.toml](../examples/config/deepseek-basic.toml) for a copyable file.

## Authentication

`SIGIL_API_KEY` takes priority over `[providers.deepseek].api_key`. Prefer the environment for local and CI use; a saved config key is plaintext.

## Options And Visible Limits

`base_url`, `beta_base_url`, `anthropic_base_url`, `fim_model`, `strict_tools_mode`, and `user_id_strategy` are DeepSeek-specific. Environment overrides use `SIGIL_BASE_URL`, `SIGIL_BETA_BASE_URL`, `SIGIL_ANTHROPIC_BASE_URL`, `SIGIL_FIM_MODEL`, `SIGIL_STRICT_TOOLS_MODE`, and `SIGIL_USER_ID_STRATEGY`.

DeepSeek image input is not enabled. An attached image is rejected before a request is sent; choose a supported image provider instead.

## Verify

Run `sigil doctor` and confirm provider, model, base URL, and credential source.

## Common Problems

- Authentication: export `SIGIL_API_KEY` in the same shell that launches Sigil.
- Wrong model: check `[agent].model` and any task-role override.
- FIM unavailable: confirm `fim_model` and endpoint support.
- Slow stream: check network access and model-request timeouts.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
