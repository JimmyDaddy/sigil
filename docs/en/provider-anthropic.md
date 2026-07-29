<!-- public-doc-role: provider-anthropic; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Anthropic Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-anthropic.md)

## Minimal Setup

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "anthropic-default"
model = "claude-sonnet-4-5"

[connections.anthropic-default]
label = "Anthropic"
provider = "anthropic"
protocol = "anthropic_messages"
base_url = "https://api.anthropic.com"
credential = { source = "environment", name = "SIGIL_ANTHROPIC_API_KEY" }

[connections.anthropic-default.options]
anthropic_version = "2023-06-01"
max_tokens = 4096
```

See [anthropic.toml](../examples/config/anthropic.toml) for a copyable file.

## Authentication

The example binds this connection to `SIGIL_ANTHROPIC_API_KEY`. Setup and `/config` can save the secret to the protected credential store instead; `sigil.toml` then contains only an opaque `stored` ID. The default `file` mode uses the owner-only credential file without system authentication prompts.

## Options And Visible Limits

`anthropic_version`, `max_tokens`, and `beta_headers` are provider-owned fields under `[connections.anthropic-default.options]`. Use `beta_headers` only when a known Anthropic feature requires them.

Images work only with recognized Claude model IDs and accepted dated variants. Unknown names and aliases are rejected before sending.

## Verify

Run `sigil doctor` and confirm `default=anthropic-default/claude-sonnet-4-5`, endpoint, credential source, and readiness.

## Common Problems

- Version/header rejection: check `anthropic_version` and `beta_headers`.
- Output stops early: review `max_tokens` and model limits.
- Authentication: check the bound environment variable or repair this connection in `/config`.
- Tool behavior differs: confirm the selected Claude model supports tool use.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
