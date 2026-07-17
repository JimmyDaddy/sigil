<!-- public-doc-role: provider-gemini; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Gemini Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-gemini.md)

## Minimal Setup

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

```toml
[agent]
provider = "gemini"
model = "gemini-2.5-pro"

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com/v1beta"
```

See [gemini.toml](../examples/config/gemini.toml) for a copyable file.

## Authentication

`SIGIL_GEMINI_API_KEY` takes priority over `[providers.gemini].api_key` and avoids changing credentials used by other Google tools.

## Options And Visible Limits

`SIGIL_GEMINI_BASE_URL` temporarily overrides `base_url`. Keep `[agent].model` explicit because model availability can vary by account and region.

Images work only with recognized Gemini model IDs. Floating `latest` names, unknown IDs, and aliases are rejected before sending.

## Verify

Run `sigil doctor` and confirm provider, model, base URL, and credential source.

## Common Problems

- Authentication: check `SIGIL_GEMINI_API_KEY` in the launching shell.
- Model not found: confirm the model name, endpoint version, account, and region.
- Function call fails: confirm model and endpoint support function calling.
- Timeout: check network access and model-request timeouts.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
