<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# Provider Guide

[Docs home](README.md) · [Configuration](configuration.md) · [简体中文](../zh-CN/providers.md)

Choose the model service here, then create a named connection for the account or endpoint you
actually use. A connection owns its provider protocol, endpoint, credential source, and model
catalog. The saved default and every running session refer to the compound
`connection-id/model-id` identity, so changing providers cannot reuse another connection's model
fallback.

## Choose A Provider

| Provider | Use it for | Image input | Config value |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | Default Quick Setup path and DeepSeek-specific options | No | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | Chat Completions-compatible `/v1` gateways | No | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | OpenAI Responses models | Recognized model IDs | `openai_responses` |
| [Anthropic](provider-anthropic.md) | Claude through Anthropic Messages | Recognized Claude IDs | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini and function calling | Recognized Gemini IDs | `gemini` |

Quick Setup is the shortest first-use path: choose provider, credential source, and model, then
review and save. Use a manual current-schema config for repeatable local or CI defaults.
In `/config` → **Provider**, Enter on **Connection** opens an explicit chooser for saved
connections and provider templates; `A` opens the add-provider group directly. Up/Down works on
standard macOS keyboards, and adding never guesses the next provider.
The model chooser is scoped to the selected connection: it starts with that provider's bundled
default and refreshes a remote list only when discovery is supported. `M` is offered only after
an authoritative remote/fresh-cache response, a confirmed empty catalog, or an explicit
unsupported-discovery result; transport, authentication, TLS, protocol, malformed-response, and
stale-cache failures must be repaired or retried first. Loading, authenticated remote results, confirmed empty results, authentication
rejection, offline/TLS failure, unsupported discovery, and malformed responses are distinct
states; Sigil does not fill another provider's models into the list. A confirmed empty remote
catalog clears the candidates and permits acknowledged manual entry.
After a successful catalog load, leaving and reopening the picker reuses the exact
connection/fingerprint view for ten minutes. An older in-process view remains visible as
unverified while Sigil refreshes it in the background, so menu navigation does not repeatedly
replace the list with a blocking loading state.

## Authentication Priority

The current schema never writes a newly entered API key to `sigil.toml`. Choose one credential source per
connection:

| Source | Use it for | Stored in config |
| --- | --- | --- |
| Protected credential store | Normal local use | Random `source = "stored"` reference only; `file` and `auto` write an owner-only credential file |
| Environment | CI or an already managed shell secret | Allowlisted variable name only |
| No authentication | Explicit loopback custom endpoints | `source = "none"`; rejected for credentialed remote HTTP |

Provider environment names are `SIGIL_API_KEY`, `SIGIL_OPENAI_COMPATIBLE_API_KEY`,
`SIGIL_OPENAI_RESPONSES_API_KEY`, `SIGIL_ANTHROPIC_API_KEY`, and `SIGIL_GEMINI_API_KEY`.
`[storage].credential_store` accepts `file`, `auto`, or `keyring`. The default `file` and
non-interactive `auto` modes use only the owner-only `~/.sigil/credentials.json`. If the file does not contain the credential, reopen
`/config` and enter the key once. Strict `keyring` mode explicitly uses macOS Keychain, Windows
Credential Manager, or Linux Secret Service and may show platform authentication UI. The
dedicated file contains protected plaintext credential material; it is not encryption. No mode
writes a newly pasted secret to `sigil.toml`, workspace data, sessions, model cache, logs,
snapshots, or support output. Run `sigil doctor` after changing a credential; it reports source
and readiness without printing the value or identifier.

## Copyable Starting Points

Templates are available under [`docs/examples/config`](../examples/config). Review the model, base URL, credential source, and permission settings before use.

## Troubleshooting Path

Check, in order: `[agent].connection`, `[agent].model`, the matching
`[connections.<id>]` block, endpoint, credential-source readiness, and provider-specific limits.
`/config` shows the current session route separately from the saved default. Incompatible
configuration is rejected rather than migrated; replace it with a current template. Keep
`permission.mode = "manual"` while diagnosing, then use
[Troubleshooting](troubleshooting.md) for shared symptoms.

<!-- public-doc-cta: open-provider-guide -->
Next: [Set up DeepSeek or choose another provider](provider-deepseek.md).
