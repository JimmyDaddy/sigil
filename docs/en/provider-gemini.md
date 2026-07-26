<!-- public-doc-role: provider-gemini; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Gemini Provider

[Provider guide](providers.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/provider-gemini.md)

## Minimal Setup

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

```toml
config_version = 2

[agent]
connection = "gemini-default"
model = "gemini-2.5-pro"

[connections.gemini-default]
label = "Google Gemini"
provider = "gemini"
protocol = "generate_content"
base_url = "https://generativelanguage.googleapis.com/v1beta"
credential = { source = "environment", name = "SIGIL_GEMINI_API_KEY" }
```

See [gemini.toml](../examples/config/gemini.toml) for a copyable file.

## Authentication

The example binds only this connection to `SIGIL_GEMINI_API_KEY`, avoiding credentials used by other Google tools. You can choose the secure credential store instead; `sigil.toml` stores only an opaque credential reference.

## Options And Visible Limits

Keep the exact `[agent].connection` and `[agent].model` route explicit because model availability can vary by account and region. A second Gemini account is a second connection with its own endpoint, credential, and catalog.

Images work only with recognized Gemini model IDs. Floating `latest` names, unknown IDs, and aliases are rejected before sending.

## Verify

Run `sigil doctor` and confirm `default=gemini-default/gemini-2.5-pro`, endpoint, credential source, and readiness.

## Common Problems

- Authentication: check `SIGIL_GEMINI_API_KEY` in the launching shell.
- Model not found: confirm the model name, endpoint version, account, and region.
- Function call fails: confirm model and endpoint support function calling.
- Timeout: check network access and model-request timeouts.

<!-- public-doc-cta: return-providers -->
Next: [Return to Providers](providers.md).
