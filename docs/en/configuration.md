# Sigil Configuration Guide

[Docs home](README.md) · [Quickstart](quickstart.md) · [Providers](providers.md) · [Troubleshooting](troubleshooting.md) · [Reference](reference.md) · [简体中文](../zh-CN/configuration.md)

This guide covers user-facing Sigil configuration. Most users should start with Quick Setup in the TUI; use this page when you need a repeatable config file, environment-variable setup, automation behavior, or advanced tool policy. If you are changing the config schema, also read `dev/governance/code-standards.md` and `dev/governance/engineering-standards.md`.

## Common User Paths

| Goal | Recommended path |
| --- | --- |
| First local setup | Run `sigil` and complete Quick Setup |
| Temporary local auth | Set `SIGIL_API_KEY` before launch |
| CI or script auth | Use environment variables, not plaintext config |
| Change model/provider from the TUI | Use `/config` |
| Keep one config that follows the launch directory | Use `workspace.root = "."` |
| Debug config/auth/provider state | Run `sigil doctor` or `/doctor` |

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

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"

[providers.deepseek]
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# Prefer SIGIL_API_KEY. If written here, the key is stored as plaintext.
# api_key = "sk-..."
```

`SIGIL_API_KEY` has higher priority than `api_key` in the config file. The legacy `DEEPSEEK_API_KEY` environment variable is still read as a fallback for the DeepSeek provider. `doctor` warns when auth only comes from plaintext config, but it does not block the run.

Copyable templates are available under [docs/examples/config](../examples/config):

- [deepseek-basic.toml](../examples/config/deepseek-basic.toml)
- [openai-compatible.toml](../examples/config/openai-compatible.toml)
- [anthropic.toml](../examples/config/anthropic.toml)
- [gemini.toml](../examples/config/gemini.toml)
- [mcp-safe-defaults.toml](../examples/config/mcp-safe-defaults.toml)
- [code-intelligence-rust.toml](../examples/config/code-intelligence-rust.toml)

Provider-specific setup now lives in focused pages:

| Provider | Use when | Details |
| --- | --- | --- |
| DeepSeek | You want the default Quick Setup path, DeepSeek chat, and FIM-related settings. | [DeepSeek provider](provider-deepseek.md) |
| OpenAI-compatible | You have a Chat Completions-compatible `/v1` endpoint such as OpenAI or a compatible gateway. | [OpenAI-compatible provider](provider-openai-compatible.md) |
| Anthropic | You want Anthropic Messages streaming and Claude model settings. | [Anthropic provider](provider-anthropic.md) |
| Gemini | You want Gemini `streamGenerateContent` with function calling support. | [Gemini provider](provider-gemini.md) |

See [Provider guide](providers.md) for a comparison and copyable provider blocks.

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

- `provider`: the runtime provider name. Supported values are `deepseek`, `openai_compat`, `anthropic`, and `gemini`.
- `model`: the default model.
- `tool_timeout_secs`: tool execution timeout.
- `max_turns`: optional guard. It is disabled by default; when set, a run stops recoverably if the model keeps requesting tools without producing a final answer.

## Appearance

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"

[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

`theme` controls the TUI color palette. Built-in values are `sigil_dark`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `nord`, and `high_contrast_dark`. The `/config` panel includes an `Appearance` section; pressing `Enter` on `Theme` cycles through the built-ins and previews the draft palette immediately with compare, syntax, page, shell, composer, tool-card, approval-modal, status, diff, and markdown samples. `Ctrl-S` saves the selection to `sigil.toml`.

`syntax_theme` controls syntect/two-face syntax highlighting for markdown code blocks, tool markdown previews, and approval preview summaries. The default `auto` maps to the selected TUI theme. Explicit values are `catppuccin_mocha`, `catppuccin_latte`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `gruvbox_light`, `nord`, `one_half_dark`, `one_half_light`, and `monokai`.

`[appearance.colors]` can override stable semantic color tokens with `#RRGGBB` values. Unknown tokens or non-hex values are reported by appearance diagnostics instead of becoming provider-visible state. Overrides affect TUI rendering only; they are not written to session history, approval records, tool payloads, or provider-visible context.

In `/config` Appearance, `Syntax theme` cycles between `auto` and explicit code-highlighting themes. `Color group` narrows the color editor to one token group, `Color token` selects a semantic token inside that group, and `Override` edits the selected token value. Press `Enter` on `Color group` or `Color token` to cycle choices, type or paste `#RRGGBB` on `Override` to set the override, press `Backspace` or `Delete` on a token or override to clear the selected token, press `Backspace` or `Delete` on a group to clear that group, and press `Ctrl-R` to clear all color overrides in the draft.

`sigil doctor`, TUI `/doctor`, and the live `/config` Appearance diagnostics validate appearance overrides after config load or draft edits. Low-contrast text/surface pairs, indistinct semantic colors, and weak structural cues are reported as warnings with remediation text; invalid override values appear under `appearance:colors`.

Supported color tokens are stable semantic names. Prefer overriding the smallest group that expresses the desired change; for example, change `accent_info` before changing every status or tool-card color individually.

| Group | Tokens | Used for | Guidance |
| --- | --- | --- | --- |
| Surfaces | `surface_base`, `surface_rail`, `surface_panel`, `surface_panel_alt`, `surface_input`, `surface_agent_panel`, `surface_overlay`, `surface_overlay_shadow`, `surface_badge`, `surface_selection`, `surface_user_message`, `surface_code` | Shell background, info rail, composer, agent panel, overlays, badges, selected rows, user bubbles, code blocks | Keep `text_primary` readable on `surface_base`, `surface_panel`, `surface_input`, and `surface_user_message`. |
| Borders | `border_subtle`, `border_strong`, `border_focus`, `border_danger` | Panel dividers, focused borders, danger borders | Keep subtle borders visible without competing with focus/danger borders. |
| Text | `text_primary`, `text_secondary`, `text_muted`, `text_inverse`, `text_disabled` | Body text, secondary details, hints, selected button text, disabled text | Keep `text_primary` high contrast; use `text_muted` only for nonessential labels. |
| Accents | `accent_primary`, `accent_secondary`, `accent_info`, `accent_success`, `accent_warning`, `accent_danger`, `accent_streaming`, `accent_idle` | Composer state, section labels, info/success/warning/danger semantics, streaming/idle state | Keep success, warning, danger, and info visually distinct. |
| Selection and buttons | `selection_fg`, `selection_bg`, `button_selected_fg`, `button_selected_bg`, `button_inactive_fg` | Active rows, selected footer/config actions, button-like chips | Keep selected foreground readable on both `selection_bg` and button backgrounds. |
| Status | `status_idle`, `status_thinking`, `status_tool`, `status_streaming`, `status_success`, `status_warning`, `status_error`, `status_pending` | Live status, doctor results, task/agent indicators, info rail markers | Keep success, warning, error, and pending indicators distinct at a glance. |
| Diff | `diff_header_fg`, `diff_hunk_fg`, `diff_added_fg`, `diff_added_bg`, `diff_removed_fg`, `diff_removed_bg`, `diff_context_fg`, `diff_gutter_fg`, `diff_current_hunk_bg` | Tool previews and approval diff panes | Keep added and removed colors separable, including their backgrounds. |
| Approval and risk | `approval_bg`, `approval_backdrop_bg`, `approval_border`, `approval_shadow`, `risk_low`, `risk_medium`, `risk_high`, `approval_allow_bg`, `approval_deny_bg`, `approval_selected_bg` | Tool approval modal, risk badges, allow/deny actions | Make allow and deny backgrounds distinct; keep `risk_high` visibly stronger than `risk_low`. |
| Markdown | `markdown_heading`, `markdown_quote_bar`, `markdown_quote_text`, `markdown_rule`, `markdown_code_fg`, `markdown_code_bg`, `markdown_link` | Timeline markdown, tool-card markdown previews, approval summary markdown | Keep inline code readable on `markdown_code_bg`; keep links distinguishable from headings. |
| Modal and overlay | `modal_bg`, `modal_border`, `modal_shadow`, `modal_command_bg`, `modal_selected_bg`, `overlay_bg`, `overlay_shadow` | Modal dialogs and slash-command overlays | Keep command chips readable on `modal_command_bg`; keep selected rows visible. |
| Config and setup | `config_bg`, `config_border`, `config_primary`, `config_detail`, `config_warning`, `config_danger`, `config_tab_bg`, `config_section_bg`, `config_selected_bg`, `setup_bg` | `/config`, setup flow, config preview, config footer/actions | Keep `config_selected_bg` distinct from `config_bg`; keep warning/danger colors separate. |

Recommended constraints:

- Use only `#RRGGBB` values. Named colors and alpha values are rejected.
- Treat tokens as semantic roles, not component-private CSS variables. A token may affect several TUI surfaces.
- Run `sigil doctor` after changing overrides; warnings mean the override is accepted but likely hard to read.
- Start from a built-in theme and override a few tokens. A fully custom palette is possible but harder to keep readable.

## Task Planning

```toml
[task]
enabled = true
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_child_sessions = 8
max_parallel_readonly = 3
max_parallel_write = 1
max_background_threads = 2
max_spawn_fanout_per_turn = 4
max_agent_tokens_per_task = 200000
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

Planned tasks are started from the TUI with `/task <task>`. `/plan` is reserved for read-only planning prompts and does not create durable task state. `default_mode = "chat"` keeps normal composer submits chat-first even when the current session has unfinished task state; use `/task continue` or a task UI action for explicit continuation. Switch the default mode only when a build intentionally wants planned tasks as the default flow.

Role-specific provider/model settings inherit `[agent]` when omitted. Planner and subagent-read default to read-only file/search/code-intelligence tools. Executor can see the full runtime registry. Subagent-write can see the full runtime registry only when `allow_write_subagents = true`; otherwise it falls back to the read-only scope. Mutating tools still go through the normal approval policy.

Agent fan-out limits live in `[task]`: the default permits up to 3 parallel read-only child agents and up to 2 background agents; set `max_parallel_readonly = 1` only when you want to serialize read-only child agents. Keep `max_spawn_fanout_per_turn` no higher than the intended per-turn spawn fan-out. The old `allow_parallel_readonly_subagents` field is retained only for compatibility; current budgeting follows `max_parallel_readonly`.

Each role can override visible tools:

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

Use explicit names and stable prefixes carefully. A scoped role registry gates tool specs, previews, execution, permission hooks, and egress hooks, so hidden tools are not merely omitted from the prompt.

## Providers

`[agent].provider` selects the runtime provider. The matching `[providers.*]` block controls endpoint, model, authentication, and provider-specific options.

| Provider value | Config block | Primary API key env | Guide |
| --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | `SIGIL_API_KEY` | [DeepSeek provider](provider-deepseek.md) |
| `openai_compat` | `[providers.openai_compat]` | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | [OpenAI-compatible provider](provider-openai-compatible.md) |
| `anthropic` | `[providers.anthropic]` | `SIGIL_ANTHROPIC_API_KEY` | [Anthropic provider](provider-anthropic.md) |
| `gemini` | `[providers.gemini]` | `SIGIL_GEMINI_API_KEY` | [Gemini provider](provider-gemini.md) |

Keep provider-specific behavior in provider configuration. The shared `sigil-kernel` contract stays provider-neutral: messages, tools, usage, approvals, and session state should not contain provider-only terms.

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

Anthropic:

- `SIGIL_ANTHROPIC_MODEL`
- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`
- `SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS`

`ANTHROPIC_API_KEY` remains a fallback source for the Anthropic provider.

Gemini:

- `SIGIL_GEMINI_MODEL`
- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_BASE_URL`
- `SIGIL_GEMINI_REQUEST_TIMEOUT_SECS`

`GEMINI_API_KEY` and `GOOGLE_API_KEY` remain fallback sources for the Gemini provider.

## Plugins

Workspace plugin manifests are discovered from `.sigil/plugins/<id>/plugin.toml`. They are reviewed from the TUI rather than edited in `sigil.toml`.

Open `/config`, move to `Plugins`, and use `PgUp/PgDn` to select a discovered manifest. The detail view shows the trust state, relative manifest path, full manifest hash, skill paths, hook commands with args and approval mode, and MCP server commands with args, startup, and required status. Footer `approve` trusts only the displayed manifest hash; footer `deny` disables that hash. Sigil reloads the manifest before recording the decision, so a changed hash must be reviewed again.

## MCP

MCP servers are configured with `[[mcp_servers]]`. See [mcp.md](mcp.md).
