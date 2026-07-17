<!-- public-doc-role: provider-anthropic; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Anthropic Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-anthropic.md)

## Minimal Setup

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

```toml
[agent]
provider = "anthropic"
model = "claude-sonnet-4-5"

[providers.anthropic]
base_url = "https://api.anthropic.com"
anthropic_version = "2023-06-01"
max_tokens = 4096
```

See [anthropic.toml](../examples/config/anthropic.toml) for a copyable file.

## Authentication

`SIGIL_ANTHROPIC_API_KEY` takes priority over `[providers.anthropic].api_key`. Prefer the environment; a saved key is plaintext.

## Options And Visible Limits

`SIGIL_ANTHROPIC_BASE_URL`, `SIGIL_ANTHROPIC_VERSION`, and `SIGIL_ANTHROPIC_MAX_TOKENS` override their config fields. Use `beta_headers` only when a known Anthropic feature requires them.

Images work only with recognized Claude model IDs and accepted dated variants. Unknown names and aliases are rejected before sending.

## Verify

Run `sigil doctor` and confirm provider, model, base URL, API version, token limit, and credential source.

## Common Problems

- Version/header rejection: check `anthropic_version` and `beta_headers`.
- Output stops early: review `max_tokens` and model limits.
- Authentication: check the environment variable or config fallback.
- Tool behavior differs: confirm the selected Claude model supports tool use.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
