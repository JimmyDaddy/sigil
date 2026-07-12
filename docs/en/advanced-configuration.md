# Advanced Configuration

[Docs home](README.md) · [Configuration](configuration.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Appearance](appearance.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/advanced-configuration.md)

Use this page when the normal setup and `/config` choices are not enough. Keep the [Configuration guide](configuration.md) as the starting point, and make one focused change at a time.

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
```

Normal composer input stays chat-first. Use `/task <goal>` for a durable multi-step task, `/task continue` to continue it, and `/plan <goal>` for a read-only planning pass before you explicitly create a task from the plan.

`max_subagents` limits active child agents. `multi_agent_mode = "explicit_request_only"` is the conservative default: Sigil uses child agents only when you or the workspace instructions explicitly ask for delegation. Set `none` to disable ordinary delegation guidance, or `proactive` only when independent parallel work is appropriate. File-changing child work still follows the normal review and approval flow.

You can give planner, executor, or child roles a different model or a narrower tool list. Use this only when you can explain why a role should be more limited than the main session. See the exact role fields in the [Configuration reference](configuration-reference.md#task).

## Verification

```toml
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

Configured checks are explicit checks you approve for your workspace. Repository hints can be suggested, but are not run simply because they exist. Use `read_only` for ordinary test, build, and lint commands; a check that changes relevant files must be followed by a non-writing check before the result is current.

## Memory, Skills, And Agents

```toml
[memory]
enabled = true

[skills]
enabled = true
user_skills = true
user_agents = true
compatibility_sources = []
```

When memory is enabled, Sigil can load workspace instruction files such as `SIGIL.md`, `AGENTS.md`, `CLAUDE.md`, and `SIGIL.local.md`. Keep repository instructions short, current, and appropriate for every session that opens the workspace.

Workspace resources live under `.sigil/`: reusable skills in `.sigil/skills`, slash commands in `.sigil/commands`, agent profiles in `.sigil/agents`, and plugin manifests in `.sigil/plugins`. Compatibility sources are opt-in. Review any imported skill, agent, or plugin before allowing it to act on a workspace.

## Compaction And Code Intelligence

```toml
[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
tail_messages = 6

[code_intelligence]
enabled = false
server_startup = "lazy"
auto_discover = true
```

Compaction manages long conversations; the default thresholds provide an early warning and a later automatic limit. Code intelligence is off by default. When enabled, it can use installed language servers and can provide code navigation, diagnostics, and reviewed edit suggestions. Enabling it does not bypass workspace trust, file approval, or diff review.

Use `Alt-D` in the TUI to inspect diagnostics for changed source files. If a language server is missing, ordinary chat and file tools remain available.

## Terminal And Model Request Overrides

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

Set `keyboard_enhancement = "off"` if a terminal or multiplexer mishandles enhanced keys. Set `mouse_capture = false` if mouse mode conflicts with your terminal. Set `osc52_clipboard = false` if your terminal blocks clipboard sequences. The [Terminal compatibility guide](terminal-compatibility.md) provides a manual checklist.

The environment variables `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`, `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`, and `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` temporarily override shared model-request timeouts. Provider credentials and endpoint options stay on the [Provider guide](providers.md) and its provider pages.

## Plugins And MCP

Plugins are discovered under `.sigil/plugins/<id>/plugin.toml` and reviewed from `/config`. Review a changed plugin again before allowing it to run. Plugin entries cannot request inherited environment credentials; configure credentialed local MCP servers in your user configuration instead.

Configure local MCP servers with `[[mcp_servers]]`. They start with a cleared environment. If a server needs a credential, grant only the required variable name with the root-only `inherit_env = ["ENV_NAME"]` setting. `/doctor` and `/config` show whether a grant is available without showing its value.

See the [MCP guide](mcp.md) for server setup and trust decisions, and use the [Configuration reference](configuration-reference.md) for the complete advanced field list.
