# Sigil Configuration Guide

[简体中文](../zh-CN/configuration.md)

This guide covers user-facing Sigil configuration. If you are changing the config schema, also read `dev/governance/code-standards.md` and `dev/governance/engineering-standards.md`.

## Resolution Order

The TUI and CLI resolve configuration in this order:

1. `--config <path>`
2. `./sigil.toml` in the current working directory
3. `sigil.toml` in the standard per-user config directory

Common per-user paths:

- macOS: `~/Library/Application Support/sigil/sigil.toml`
- Linux: `$XDG_CONFIG_HOME/sigil/sigil.toml` or `~/.config/sigil/sigil.toml`
- Windows: `%APPDATA%\sigil\sigil.toml`

Do not commit a real repository-local `sigil.toml`; it may contain secrets.

## Minimal Path

For normal use, start the TUI and complete Quick Setup:

```bash
sigil
```

For temporary use or CI, provide authentication through an environment variable before launch:

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

Quick Setup creates a usable config when no config file exists. Later, use `/config` for common settings.

## Troubleshooting With Doctor

Run `doctor` when setup, authentication, MCP, or local LSP tooling looks wrong:

```bash
sigil doctor
```

Inside the TUI, use `/doctor` to render the same report in the transcript. The TUI version starts with a status summary and a `needs attention` remediation list before the full check list.

Use the same config override if you launch Sigil with a non-default config:

```bash
sigil --config ./sigil.toml doctor
```

The report checks config loading, workspace resolution, session log location, provider settings, API key source, configured MCP commands and trust settings, code intelligence language-server availability, and the current `TERM`. It reports where the API key was resolved from, but never prints the secret value. Warning and error checks include `fix:` remediation lines; a key resolved only from plaintext config is a warning so users can move it to an environment variable or keep the local config private intentionally.

## Minimal Config Example

If you want to write config manually, start here:

```toml
[workspace]
root = "."

[session]
log_dir = ".sigil/sessions"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[terminal]
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[providers.deepseek]
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# Prefer SIGIL_API_KEY. If written here, the key is stored as plaintext.
# api_key = "sk-..."
```

`SIGIL_API_KEY` has higher priority than `api_key` in the config file. The legacy `DEEPSEEK_API_KEY` environment variable is still read as a fallback for the DeepSeek provider. `doctor` warns when auth only comes from plaintext config, but it does not block the run.

For an OpenAI-compatible endpoint, switch the provider and use `[providers.openai_compat]`:

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"
tool_timeout_secs = 30

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
# Prefer SIGIL_OPENAI_COMPATIBLE_API_KEY or OPENAI_API_KEY.
# api_key = "sk-..."
```

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` is special: it resolves to the directory where you launched `sigil`. This allows one user-level config to follow the repository you opened.

File tools are confined to the workspace root. They reject `..`, absolute paths, and symlinks that point outside the workspace. `bash` does not provide a full process sandbox.

## Agent

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30
# max_turns = 20
```

- `provider`: the runtime provider name. Supported values are `deepseek` and `openai_compat`.
- `model`: the default model.
- `tool_timeout_secs`: tool execution timeout.
- `max_turns`: optional guard. It is disabled by default; when set, a run stops recoverably if the model keeps requesting tools without producing a final answer.

## Task Planning

```toml
[task]
enabled = true
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_child_sessions = 8
allow_parallel_readonly_subagents = false
allow_write_subagents = true

[task.planner]
# provider = "deepseek"
# model = "deepseek-v4-flash"
# reasoning_effort = "high"

[task.executor]
# model = "deepseek-v4-pro"

[task.subagent_read]
# Read-only by default.

[task.subagent_write]
# Uses the full tool surface only when allow_write_subagents = true.
```

Planned tasks are started from the TUI with `/plan <task>`. `default_mode = "chat"` keeps normal composer submits chat-first; switch it only when a build intentionally wants planned tasks as the default flow.

Role-specific provider/model settings inherit `[agent]` when omitted. Planner and subagent-read default to read-only file/search/code-intelligence tools. Executor can see the full runtime registry. Subagent-write can see the full runtime registry only when `allow_write_subagents = true`; otherwise it falls back to the read-only scope. Mutating tools still go through the normal approval policy.

Each role can override visible tools:

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

Use explicit names and stable prefixes carefully. A scoped role registry gates tool specs, previews, execution, permission hooks, and egress hooks, so hidden tools are not merely omitted from the prompt.

## DeepSeek Provider

```toml
[providers.deepseek]
base_url = "https://api.deepseek.com"
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# api_key = "sk-..."
user_id_strategy = "stable_per_end_user"
strict_tools_mode = "auto"
request_timeout_secs = 120
```

The TUI `/config` surface exposes only high-frequency fields such as `model`, `api_key`, `base_url`, and `fim_model`. Saving `api_key` through `/config` writes plaintext to `sigil.toml`; prefer `SIGIL_API_KEY` for temporary or CI use. Lower-frequency provider-specific fields, including `beta_base_url`, `anthropic_base_url`, `user_id_strategy`, `request_timeout_secs`, and `strict_tools_mode`, remain file/env configuration.

## OpenAI-compatible Provider

```toml
[providers.openai_compat]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
request_timeout_secs = 120
```

This provider uses the Chat Completions streaming shape with text deltas, streamed tool calls, usage, and optional `system_fingerprint`. It does not expose DeepSeek-only prefix/FIM, reasoning replay, strict tools mode, or beta endpoint settings.

`SIGIL_OPENAI_COMPATIBLE_API_KEY` has the highest priority for this provider. `OPENAI_API_KEY` is read as a fallback. `SIGIL_OPENAI_COMPATIBLE_MODEL`, `SIGIL_OPENAI_COMPATIBLE_BASE_URL`, and `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS` override the matching config fields.

## Permission

Default shape:

```toml
[permission]
default_mode = "ask"

[permission.access]
read = "allow"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Meaning:

- Tool calls without an explicit override default to approval.
- Read-only file and search tools are allowed by default.
- Paths outside the workspace are disabled by default; if external directories are enabled, they still go through rules and approval.
- In headless `run`, final `ask` decisions are returned to the model as structured `approval_required` tool errors instead of being executed silently.

## Memory

```toml
[memory]
enabled = true
```

When enabled, Sigil loads stable workspace memory files such as `SIGIL.md`, `AGENTS.md`, `CLAUDE.md`, and `SIGIL.local.md`. A memory file can also import another file with a single-line `@path`.

## Compaction

```toml
[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
# fallback_context_window_tokens = 128000
tail_messages = 6
```

If Sigil can resolve the current provider/model context window, it uses that value. `fallback_context_window_tokens` is used only when the model window cannot be resolved.

Older configs using `context_window_tokens` still load; saved configs use the fallback field.

## Code Intelligence

```toml
[code_intelligence]
enabled = false
startup = "lazy"
default_timeout_ms = 5000
max_results = 100
max_payload_bytes = 65536

[code_intelligence.discovery]
enabled = true
report_missing = true
```

When enabled, the runtime registers read-only code intelligence tools plus LSP edit tools for code actions and symbol rename. Edit tools are `Write` tools and require a diff approval before files are changed. The TUI can use `Alt-D` to run diagnostics over git changed source files.

With `discovery.enabled = true`, Sigil discovers common languages and safe LSP servers available on `PATH`. Explicit `code_intelligence.servers` entries are advanced overrides or additions.

The TUI `/config` panel includes a `Code Intel` section for `enabled`, `startup`, discovery, the read-only trust boundary, and readiness checks. The readiness rows reuse the same local doctor facts, so missing LSP commands show remediation before any language server is started.

Language server example:

```toml
[[code_intelligence.servers]]
name = "rust-analyzer"
languages = ["rust"]
command = "rust-analyzer"
root_markers = ["Cargo.toml"]
file_extensions = ["rs"]
startup_timeout_ms = 5000
trust_required = true
```

## Terminal

```toml
[terminal]
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

`mouse_capture` lets the TUI request terminal mouse events for clicks, scrolling, approval controls, setup/config/session selection, and transcript drag selection. Turn it off if your terminal or multiplexer mishandles mouse mode; keyboard controls remain available.

`osc52_clipboard` lets `Ctrl-C` copy selected transcript text by writing an OSC52 clipboard sequence. Turn it off if your terminal blocks OSC52 or shows the sequence as text. When disabled, Sigil reports `clipboard unavailable` instead of writing to the terminal.

`scroll_sensitivity` sets how many rows a mouse wheel tick moves in transcript and approval diff views. The default is `3`; use a smaller value for high-resolution wheels and a larger value for slower terminal scroll events.

The TUI `/config` panel includes a `Terminal` section for these controls. `mouse_capture` is applied on the next launch; `osc52_clipboard` is checked for each copy action; `scroll_sensitivity` applies to the running config after it is saved and reloaded.

`doctor` reports the configured switches, `TERM`, common terminal profile variables, tmux/screen, SSH, WSL, and clipboard bridge risk. For a repeatable manual checklist across iTerm2, Terminal.app, WezTerm, kitty, tmux, and SSH, see [terminal-compatibility.md](terminal-compatibility.md).

## Provider Environment Overrides

Supported variables:

DeepSeek:

- `SIGIL_MODEL`
- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_REQUEST_TIMEOUT_SECS`
- `SIGIL_STRICT_TOOLS_MODE`

`SIGIL_API_KEY` has the highest priority. `DEEPSEEK_API_KEY` remains a fallback source for the DeepSeek provider. If only `[providers.deepseek].api_key` is present, Sigil treats it as plaintext config auth and `doctor` reports a warning with remediation.

OpenAI-compatible:

- `SIGIL_OPENAI_COMPATIBLE_MODEL`
- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`
- `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS`

`OPENAI_API_KEY` remains a fallback source for the OpenAI-compatible provider.

## MCP

MCP servers are configured with `[[mcp_servers]]`. See [mcp.md](mcp.md).
