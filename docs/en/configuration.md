# Sigil Configuration Guide

[Docs home](README.md) · [Quickstart](quickstart.md) · [Providers](providers.md) · [Troubleshooting](troubleshooting.md) · [Reference](reference.md) · [简体中文](../zh-CN/configuration.md)

This guide owns shared Sigil configuration: config resolution, workspace, permissions, tasks, tools, appearance, terminal behavior, plugins, and MCP. Provider selection, provider blocks, and authentication variables belong to the [Provider guide](providers.md) and its linked provider pages.

## Common User Paths

| Goal | Recommended path |
| --- | --- |
| First local setup | Run `sigil` and complete Quick Setup |
| Temporary local auth | Choose a provider, then use its [environment key](providers.md#authentication-priority) |
| CI or script auth | Follow the chosen [provider page](providers.md) and use its environment variable |
| Change model/provider from the TUI | Follow the [provider selection guide](providers.md#provider-selection), then use `/config` |
| Keep one config that follows the launch directory | Use `workspace.root = "."` |
| Debug config/auth/provider state | Run `sigil doctor` or `/doctor` |

## Resolution Order

The TUI and CLI resolve configuration in this order:

1. `--config <path>`
2. `sigil.toml` in the user-visible Sigil config directory

Default user config path:

- `~/.sigil/sigil.toml`

Quick Setup writes the per-user config path. On startup, if `~/.sigil/sigil.toml` does not exist but an older platform-specific user config exists, Sigil copies it to `~/.sigil/sigil.toml` and uses the new path. A workspace-root `sigil.toml` is not loaded by default; pass it explicitly with `--config <path>` if you need one for a local experiment.

## Minimal Path

For normal use, start the TUI and complete Quick Setup:

```bash
sigil
```

For temporary use or CI, choose a provider and export its variable from the [provider authentication map](providers.md#authentication-priority) before launch. Provider pages contain copyable shell commands; there is no provider-neutral API key variable.

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

## Shared Config Fragment

If you want to write config manually, start with this provider-neutral fragment:

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

Combine these shared settings with the provider selection and authentication setup from the [Provider guide](providers.md). Provider pages contain the copyable provider blocks and environment-variable commands.

Copyable templates are available under [docs/examples/config](../examples/config):

- [mcp-safe-defaults.toml](../examples/config/mcp-safe-defaults.toml)
- [code-intelligence-rust.toml](../examples/config/code-intelligence-rust.toml)

Provider-specific templates are indexed from the [Provider guide](providers.md), next to the selection and authentication rules they depend on.

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` is special: it resolves to the directory where you launched `sigil`. This allows one user-level config to follow the repository you opened.

File tools are confined to the workspace root. They reject `..`, absolute paths, and symlinks that point outside the workspace. `bash` does not provide a full process sandbox.

## Storage and Session Paths

```toml
[storage]
state_root = "auto"
cache_root = "auto"

[session]
# log_dir = "sessions"
```

These settings control different path responsibilities. They are not alternate names for the same storage location.

| Setting | Responsibility | Default / resolution |
| --- | --- | --- |
| `storage.state_root` | Durable per-user Sigil state. Sigil derives each workspace's state directory under `state_root/workspaces/<workspace-id>` and stores session-adjacent records such as input history, artifacts, changesets, and terminal task records there. | `auto` uses the platform user state directory. `SIGIL_STATE_HOME` overrides the configured value. Prefer an absolute path when you override it in a config file. |
| `storage.cache_root` | Rebuildable per-user cache. Sigil derives each workspace's cache directory under `cache_root/workspaces/<workspace-id>` and uses it for scratch data such as `$SIGIL_SCRATCH_DIR`. | `auto` uses the platform user cache directory. `SIGIL_CACHE_HOME` overrides the configured value. Prefer an absolute path when you override it in a config file. |
| `session.log_dir` | Append-only session JSONL logs for the current workspace. This changes only where session logs are written; it does not replace `storage.state_root`. | When omitted, Sigil writes logs under the workspace state directory's `sessions` child. Relative overrides resolve under the workspace state directory. |

Repo-local Sigil assets are fixed under the workspace `.sigil` directory and are not user-editable root settings:

| Path | Responsibility |
| --- | --- |
| `.sigil/skills` | Sigil-native workspace skills. |
| `.sigil/commands` | Sigil-native Markdown slash commands. Each `*.md` file is discovered as a user-invocable inline command. |
| `.sigil/agents` | Sigil-native workspace agent profiles. |
| `.sigil/plugins` | Workspace plugin manifests and plugin-owned assets. |

Derived paths such as workspace state/cache roots, artifacts, changesets, terminal task records, input history, scratch, and `.sigil/*` project assets are intentionally not separate user-facing root settings. Use the root above that matches the data's lifecycle: state for durable audit/recovery data, cache for disposable scratch data, fixed `.sigil/*` paths for repo-local reusable assets, and `session.log_dir` only for session JSONL placement.

The TUI `/config` Storage page is read-only for these paths. It shows resolved paths, artifact retention, and the cleanup action; edit only state/cache roots in `sigil.toml` or with `SIGIL_STATE_HOME` / `SIGIL_CACHE_HOME`. Project assets remain fixed under `.sigil/*`.

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

## Execution Backend

```toml
[execution]
strategy = "local"
```

`[execution]` is a file-only advanced section. The default `strategy = "local"` preserves normal local shell behavior and does not claim OS sandbox isolation.

On macOS, advanced users can opt into the first sandbox backend MVP:

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
profile = "workspace_write"
fallback = "deny"
```

`macos_seatbelt` runs commands through `/usr/bin/sandbox-exec` with full filesystem reads and writes limited to the command working directory. It cannot prove network isolation. User shell paths report that limitation truthfully; extension processes such as MCP/plugin hooks fail before spawn when the selected profile denies network, and may use this backend only with a network-allowed profile. Supported local paths include non-interactive shell execution and the current PTY/MCP/plugin-hook handoff surfaces that report sandbox coverage receipts. It remains macOS-only, does not make remote tools or every container/daemon scenario sandboxed, and fails closed if `sandbox-exec` is unavailable. `sandbox-exec` is deprecated by Apple, so this backend is an enforcement MVP rather than the final cross-platform sandbox strategy.

Legal combinations are intentionally narrow: `strategy = "local"` must not include `[execution.sandbox]`; `strategy = "sandbox"` requires `[execution.sandbox]`; sandbox backends are `macos_seatbelt`, `linux_bubblewrap`, or `docker`; Docker requires `container_image`; non-Docker backends must not set `container_image`. `isolation` is derived from the strategy and is not a user-facing key.

Sandbox capability and network receipts assume the locally installed enforcement executable or daemon is trusted. Sigil currently discovers `bwrap` and `docker` through its startup `PATH` and verifies availability/conformance, but does not attest the binary supply chain, owner, or mode. Use an administrator-controlled installation and a trusted startup `PATH`; a hostile wrapper can invalidate the receipt model.

## Verification

```toml
[verification]

[verification.scope]
profile = "auto"
# extra_excludes = ["tmp/generated/**"]
# generated_roots = ["generated"]

[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

`[verification]` is a file-only section for explicit user-approved checks. Current task runs materialize these entries into verification policy records before evaluating completion. Sigil also has kernel support for discovering repository-local candidate checks from `.sigil/verification.toml`, CI `run:` steps, `package.json`, `Cargo.toml`, and `Makefile`, but discovery never means execution. Repository-local candidates stay suggested checks until they are promoted through explicit approval, a satisfying sandbox decision, or a global policy; trusting a workspace alone does not make every discovered CI/Cargo/Makefile check block ordinary tasks.

`[verification.scope]` is the single user-facing place for verification scope. `profile` chooses the coarse preset, `extra_excludes` adds project-specific excluded globs, and `generated_roots` marks generated directories that should not become verification evidence.

On first workspace entry, the TUI records a coarse workspace trust decision before normal use. That decision allows repository-local instructions and check discovery, but it does not promote discovered checks by itself and does not grant shell, plugin, MCP, or file-write permissions.

Each `[[verification.checks]]` entry defines a trusted check from user config:

- `id`: stable check id used by verification policy and audit records.
- `command`: executable command name.
- `args`: optional argv list.
- `cwd`: optional workspace-relative working directory.
- `effect`: expected tool effect. Use `read_only` for ordinary build/test/lint checks that do not modify verification-scoped files. Mutating checks are treated as mutation evidence and must be followed by a non-writing verification run before the result can be `Passed`.

Project-shaped commands from user config are only applied when they match the current workspace. For example, `cargo` checks require a `Cargo.toml` at the workspace root or in the configured `cwd` chain, package-manager checks such as `npm` require `package.json`, and `make` / `just` checks require their corresponding project files. This keeps a global `~/.sigil/sigil.toml` from making an unrelated scratch directory fail verification just because it lacks the configured project type.

## Appearance

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"

[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

`theme` controls the TUI color palette. Built-in values are `sigil_dark`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `nord`, and `high_contrast_dark`. The `/config` panel includes an `Appearance` section; pressing `Enter` on `Theme` cycles through the built-ins and previews the draft palette immediately with compare, syntax, page, shell, composer, tool-card, approval-modal, status, diff, and markdown samples. `Ctrl-S` saves the selection to `sigil.toml`.

`syntax_theme` controls syntect/two-face syntax highlighting for markdown code blocks, tool markdown previews, and approval preview summaries. The default `auto` maps to the selected TUI theme. Explicit values are `catppuccin_mocha`, `catppuccin_latte`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `gruvbox_light`, `nord`, `one_half_dark`, `one_half_light`, and `monokai`.

`usage_cost_currency` controls the TUI currency used for usage cost estimates. The default `auto` follows the provider balance currency when available and otherwise displays USD. Explicit values are `usd` and `cny`. This is display-only; provider pricing and session usage accounting remain USD-based estimates.

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
max_subagents = 8
multi_agent_mode = "explicit_request_only"
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

Planned tasks are started from the TUI with `/task <task>`. `/plan` remains read-only and creates/runs durable task state only after the user explicitly accepts the plan-ready handoff. `default_mode = "chat"` keeps normal composer submits chat-first even when the current session has unfinished task state; use `/task continue` or a task UI action for explicit continuation. Switch the default mode only when a build intentionally wants planned tasks as the default flow.

Role-specific provider/model settings inherit `[agent]` when omitted. Planner and subagent-read default to read-only file/search/code-intelligence tools. Executor can see the full runtime registry. Subagent-write can see the full runtime registry only when `allow_write_subagents = true`; otherwise it falls back to the read-only scope. Mutating tools still go through the normal approval policy.

Agent concurrency is controlled by `[task].max_subagents`: the default permits up to 8 active child agents across foreground, background, read-only, and write-capable roles. Token usage is recorded in agent results for reporting, but it is not a hard spawn-denial budget.

`multi_agent_mode` controls when model-visible agent tools should be used. The default, `explicit_request_only`, keeps `spawn_agent` available but instructs the model to use subagents only when the user or active repo/skill instructions explicitly request delegation, parallel agent work, or subagents. `none` disables ordinary model delegation guidance, while `proactive` lets the model spawn non-overlapping child agents when parallelism clearly improves speed or quality. Write-capable `worker` runs are still constrained by runtime policy: foreground and join-before-final use changeset-only merge review, and background worker writes are rejected until isolation is available.

Each role can override visible tools:

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

Use explicit names and stable prefixes carefully. A scoped role registry gates tool specs, previews, execution, permission hooks, and egress hooks, so hidden tools are not merely omitted from the prompt.

## Providers

Use the [Provider guide](providers.md) to choose a provider, set `[agent].provider` and `[agent].model`, configure the matching `[providers.*]` block, and select the correct authentication environment variable. This page covers only configuration shared across providers.

## Permission

Default shape:

```toml
[permission]
mode = "manual"

[permission.commands]
allow = []
ask = []
deny = []

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Modes:

| Mode | User meaning | Semantics |
| --- | --- | --- |
| `read-only` | Inspect only | Local reads are allowed; local writes and process execution are denied even if a lower-level local override tries to allow them. Network effects are evaluated separately. |
| `manual` | Confirm manually | Local reads are allowed by default; local writes and execution ask unless a specific tool/rule/external-directory policy says otherwise. |
| `auto-edit` | Edit files automatically | Workspace file edits are allowed; local process execution still asks by default. |
| `danger-full-access` | High-risk local access | Local access is allowed by default. The explicit `danger` name is intentional; it does not override an independent network ask or deny. |

Meaning:

- `mode = "manual"` is the default interactive safety posture.
- `commands`, `tools`, `rules`, and `external_directory` are advanced policy-file overrides for specific commands, tools, subjects, or external paths. They are not a second default permission baseline.
- `permission.commands` is the recommended advanced shell-command override. Patterns match normalized command text and only treat `*` and `?` as wildcards. Exact duplicate patterns across `allow`, `ask`, and `deny` are rejected.
- When `permission.commands` matches, approval cards and session audit entries record `permission.commands.<allow|ask|deny>`, the pattern, and the command text so the decision remains explainable.
- Paths outside the workspace are disabled by default; if external directories are enabled, they still go through the external-directory gate.
- Temporary shell scratch files should use `$SIGIL_SCRATCH_DIR` from `bash` or `terminal_start`. It is backed by Sigil's per-user cache root and shown to the model as `cache/tmp`; OS temp directories such as `/tmp`, macOS `/private/tmp`, or Windows `%TEMP%` are still external paths and are not allowed by default.
- In headless `run`, final `ask` decisions are returned to the model as structured `approval_required` tool errors instead of being executed silently.
- Local access and network effect are independent axes. A tool declares local `Read` / `Write` / `Execute` plus an optional network `Read` / `Mutate` / `Unknown` effect. `read-only` permits a network read only when the effective `NetworkPolicy` permits it, while network mutation or an unknown network effect remains denied in `read-only`. `danger-full-access` cannot override network `Ask` or `Deny`.
- This release does not expose a new `[web]` or remote-MCP network configuration and does not enable WebFetch/WebSearch. Existing generic MCP calls are classified conservatively as local `Read` plus `NetworkEffect::Unknown`; their existing source/tool approval still participates in the final `Deny > Ask > Allow` decision.

Precedence:

| Order | Source | Responsibility |
| --- | --- | --- |
| 1 | Local `mode` baseline | The user-facing top-level mode sets the local Read/Write/Execute posture; `read-only` is a hard local write/execute cap. |
| 2 | Independent network policy | The runtime evaluates the declared/dynamic network effect separately; network `Ask` or `Deny` cannot be widened by local danger mode, plan approval, or a session grant. |
| 3 | Tool/source-provided default | Runtime/tool-specific source policy, such as MCP trust approval or a trusted read-only command downgrade. |
| 4 | `tools.<tool_name>` | Tool-name override. |
| 5 | `rules[]` | Matching tool/subject rules; the last matching rule wins, preserving file-order specificity. |
| 6 | `commands.allow/ask/deny` | Matching command patterns for shell commands. Within command groups, `deny > ask > allow`; command `allow` can widen the default `manual` shell ask, but it cannot override explicit tool/rule ask or deny. |
| 7 | `external_directory` | Extra gate for workspace-external subjects: disabled means deny; enabled uses matching external rules or `external_directory.default_mode`. |
| 8 | Effective policy cap and risk overlays | Runtime caps, local `read-only`, protected paths, destructive operations, and external-directory denial remain hard safety boundaries. The final result is the strictest local, network, and source decision. |

## Memory

```toml
[memory]
enabled = true
```

When enabled, Sigil loads stable workspace memory files such as `SIGIL.md`, `AGENTS.md`, `CLAUDE.md`, and `SIGIL.local.md`. A memory file can also import another file with a single-line `@path`.

## Skills and Agents

```toml
[skills]
enabled = true
user_skills = true
user_agents = true
compatibility_sources = []
```

Skill and agent discovery has three separate source classes:

| Setting | Responsibility |
| --- | --- |
| `.sigil/skills` | Fixed Sigil-native reusable skills for the current workspace. |
| `.sigil/commands` | Fixed Sigil-native Markdown slash commands for the current workspace. Each `*.md` file runs as inline skill context through `/command-id`. |
| `.sigil/agents` | Fixed Sigil-native workspace agent profiles. Agents run as child sessions rather than inline skill context. |
| `user_skills` / `user_agents` | Whether to include per-user skills and agents from the user config directory. These do not change workspace discovery roots. |
| `compatibility_sources` | Explicit imports from foreign layouts. Supported values are `claude` and `reasonix`; the default is empty so Sigil-native `.sigil/*` remains the ordinary workspace source. |

Compatibility sources are marked by source/trust in the Agents and Skills browsers and still go through the same trust lifecycle before model or user invocation. The TUI `/config` Agents and Skills sections browse discovered entries, show source/trust/hash/run mode, and expose trust/use actions. Workspace discovery roots are fixed under `.sigil/*`.

Workspace agent profiles can define OpenCode-style permissions in `.sigil/agents/<id>/agent.toml` or `.sigil/agents/<id>/AGENT.md`. Use `permission` for what the agent may do, and use `tool_scope` / `allowed_tools` only to narrow which tools are visible to that profile:

```toml
description = "Focused implementation worker"
trust = "trusted"
invocation_policy = "model_allowed"
result_policy = "foreground_merge_required"

[permission]
read = "allow"
glob = "allow"
grep = "allow"
edit = "ask"

[permission.commands]
allow = ["cargo test *", "git status*", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

Agent permissions are merged after the global `[permission]` config. Agent command groups use the same `allow` / `ask` / `deny` semantics as the root config. The global `read-only` mode remains a hard cap, and protected paths, destructive operations, external-directory gates, and write-subagent isolation still fail closed. Write-capable subagents still use foreground changeset-only merge review until a stronger write isolation mode is available.

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

## Code Intelligence

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
default_timeout_ms = 5000
max_results = 100
max_payload_bytes = 65536
auto_discover = true
report_missing = true
```

When enabled, the runtime registers code-query tools plus LSP edit tools for code actions and symbol rename. Edit tools are `Write` tools and require a diff approval before files are changed. Workspace trust only controls whether a configured LSP process may start; it does not bypass tool permission or diff approval. The TUI can use `Alt-D` to run diagnostics over git changed source files.

With `auto_discover = true`, Sigil discovers common languages and safe LSP servers available on `PATH`. Explicit `code_intelligence.servers` entries are advanced overrides or additions.

The TUI `/config` panel includes a `Code Intel` section for `enabled`, `server_startup`, `auto_discover`, the LSP process trust boundary, write approval requirements, and readiness checks. The readiness rows reuse the same local doctor facts, so missing LSP commands show remediation before any language server is started.

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

`trust_required` defaults to `true`. Such a server starts only when the current session contains a matching durable `Trusted` decision for this exact workspace. `Unknown`, `Restricted`, and `Denied` fail closed before command resolution or process spawn. A fresh `sigil run` session therefore cannot start a trust-required LSP; set `trust_required = false` only as an explicit opt-out from this process-start gate. Rust Tree-sitter fallback remains available without an LSP process, and LSP write tools still require their normal diff approval in either mode.

## Terminal

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

`keyboard_enhancement` controls crossterm keyboard enhancement. The default `auto` probes the current terminal at TUI startup and requests enhanced key reporting only when supported. Use `on` to force the request, or `off` if the terminal, multiplexer, SSH layer, or embedded PTY mishandles the enhanced protocol.

`mouse_capture` lets the TUI request terminal mouse events for clicks, scrolling, approval controls, setup/config/session selection, and transcript drag selection. It defaults on for the normal interactive TUI. Set it to `false` explicitly if a terminal, multiplexer, SSH layer, or embedded PTY mishandles mouse mode; keyboard controls remain available.

`osc52_clipboard` lets `Ctrl-C` copy selected transcript text by writing an OSC52 clipboard sequence. Turn it off if your terminal blocks OSC52 or shows the sequence as text. When disabled, Sigil reports `clipboard unavailable` instead of writing to the terminal.

`scroll_sensitivity` sets how many rows a mouse wheel tick moves in transcript and approval diff views. The default is `3`; use a smaller value for high-resolution wheels and a larger value for slower terminal scroll events.

The TUI `/config` panel includes a read-only `Terminal` section for these controls. Edit `sigil.toml` for compatibility overrides. `keyboard_enhancement` is resolved on the next launch; `mouse_capture` applies on the next launch; `osc52_clipboard` is checked for each copy action; `scroll_sensitivity` applies to the running config after it is saved and reloaded.

`doctor` reports the configured switches, `TERM`, common terminal profile variables, tmux/screen, SSH, WSL, and clipboard bridge risk. For a repeatable manual checklist across iTerm2, Terminal.app, WezTerm, kitty, tmux, and SSH, see [Terminal Compatibility Checklist](terminal-compatibility.md).

## Model Request Environment Overrides

- `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS`

These override `[model_request]` for every provider. Use them when a shell or CI job needs a different transport timeout without editing `sigil.toml`. Provider-specific endpoint and authentication variables are documented only in the [Provider guide](providers.md) and the linked provider pages.

## Plugins

Workspace plugin manifests are discovered from `.sigil/plugins/<id>/plugin.toml`. They are reviewed from the TUI rather than edited in `sigil.toml`.

Open `/config`, move to `Plugins`, and use `PgUp/PgDn` to select a discovered manifest. The detail view shows the trust state, relative manifest path, full manifest hash, skill paths, hook commands with args and approval mode, and MCP server commands with args, startup, and required status. Footer `approve` trusts only the displayed manifest hash; footer `deny` disables that hash. Sigil reloads the manifest before recording the decision, so a changed hash must be reviewed again. Plugin MCP entries cannot declare `inherit_env`; credentialed stdio servers belong in the user root config.

## MCP

MCP servers are configured with `[[mcp_servers]]`. Local stdio servers start with a cleared environment; use the additive root-only `inherit_env = ["ENV_NAME"]` field for explicit credential grants. `/doctor` and `/config` show grant names, missing status, and live-fingerprint readiness without displaying values. See [Sigil MCP Guide](mcp.md).
