<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# Advanced Configuration

[Docs home](README.md) · [Configuration](configuration.md) · [Permissions](permissions-and-sandbox.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/advanced-configuration.md)

Use these settings only after the normal setup works. Change one area at a time and run `sigil doctor` when the result is unclear.

Workspace changes or an unavailable workspace snapshot do not prevent running or saving a reviewed Plan. Execution reads the current files. Model catalogs are optional references: an explicit model ID remains selectable even when the list omits it. Missing credentials can be repaired after saving a default model; they are checked when starting a run. Verification reruns check current files, including after edits; an incomplete snapshot produces inconclusive evidence.

## Task Planning

<!-- public-doc-topic: task -->

```toml
[task]
enabled = true
routing_policy = "auto"
max_subagents = 8
max_concurrent_provider_routes = 4
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true
```

Task uses the ordinary model tool loop. The model chooses whether to start a Task, how to split it, and when to delegate, run work in parallel, or wait. The host enforces permissions, approvals, budgets, execution, and durable records; it does not create steps or schedule business work. `max_subagents` limits child agents, and `max_concurrent_provider_routes` limits simultaneous provider requests from those agents. These values are the current schema defaults. Quick Setup saves `auto + explicit_request_only`; a matching qualified release sidecar may instead recommend `auto + proactive`. Provider, model, endpoint, task-config digest and build must match for that installation recommendation. Missing or stale release evidence does not disable a configured executor. Runtime routing uses actual provider/tool/executor availability and the user's policy. `sigil doctor` reports configuration and release evaluation separately.

For a coarse rollback, set `routing_policy = "manual"` and
`multi_agent_mode = "explicit_request_only"`. This disables automatic handoff
and proactive spawn without deleting durable Task history. A route-local
zero-tolerance invariant applies the same effective fallback to subsequent
input for the affected session and build.

`routing_policy` defaults to `auto`. An ordinary new request can answer or use
actual tools on its first turn. The model may call `request_plan_review`,
`start_task`, or an exact-bound Task continuation when useful. Existing Plans
and Tasks do not take over a new turn; without an operation call, they remain
unchanged. All tools retain write, shell, network, and merge approval.
Setting `routing_policy = "manual"` disables automatic handoff tools for
ordinary input; `/plan` and `/task` remain available explicitly.
An approved Plan's full text becomes the direct Task objective. Child agents use isolated sessions, and the root model decides whether to read or integrate their results. The host does not create a second Task scheduler from Plan steps. Child writes remain subject to isolation, permissions, and merge-review authority. Role-specific model and tool restrictions are listed in [Configuration Reference](configuration-reference.md#task).

## Verification

```toml
[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

Add only checks you understand. Repository hints can be suggested but do not run merely because they exist. A check that changes relevant files must be followed by a non-writing check before the result is current.

## Memory, Skills, And Agents

<!-- public-doc-topic: memory -->

```toml
[memory]
enabled = true
writable = true
```

`enabled` lets Sigil load workspace instruction files such as `SIGIL.md`, `AGENTS.md`, and `SIGIL.local.md`. Keep them short, current, and suitable for every session in the repository.

`writable` is enabled by default and can be set to `false` to opt out. Sigil exposes `remember_user_preference` and `remember_project_fact` tools, plus `inspect_memory` and `forget_memory`. Durable mutations ask for approval in ordinary permission modes; `danger-full-access` treats that `Ask` as already authorized, while explicit `Deny` rules and secret-like-content rejection remain authoritative. The model decides from the user's meaning whether durable memory is intended; Sigil does not classify prompts by matching phrases. A successful write returns a durable receipt with `scope`, `memory_id`, and `version`. Until that receipt exists, Sigil must not claim the information was remembered beyond the current session. User preferences are shared across local workspaces; project facts are isolated to the canonical current workspace.

Forgetting stops future retrieval and physically deletes the Sigil-controlled memory sidecar. It cannot retract context already sent to a provider or erase independent session and audit evidence.

<!-- public-doc-topic: skills-agents -->

Sigil-native reusable workspace skills, commands, agents, and plugins live under `.sigil/skills`, `.sigil/commands`, `.sigil/agents`, and `.sigil/plugins`. By default, Sigil also discovers standard `.agents/skills`, Codex `.codex/agents`, OpenCode `.opencode/{skills,commands,agents}`, and Claude Code `.claude/{skills,commands,agents}` resources. Native Sigil resources win name conflicts. Compatible resources inherit workspace trust instead of requiring per-item review or enablement; compatible commands use `/name`, while compatible agents use `@name` and remain manual-only and read-only. Set `[skills].compatibility_auto_discover = false` to disable the default set, or use `compatibility_sources` to add or precisely select sources.

## Compaction And Code Intelligence

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
strategy = "cache_aware_v3"
native_carrier_enabled = false
```

`cache_aware_v3` is the only strategy. It keeps reusable prior input stable where supported, preserves current intent and complete recent turns, and starts a new cache period only when the conversation must fit or trusted cost evidence shows a benefit. Running `/compact` is the explicit request to generate, validate, and atomically activate one recoverable semantic checkpoint; it does not open a confirmation modal. On an admitted route this makes one additional LLM request on the current provider/model route, keeping the previous request as the cacheable prefix and appending a strict JSON summary instruction. This is not a child agent and cannot execute tools. The model contributes only untrusted narrative; objectives, constraints, authorization, completion, and verification remain grounded in saved session history. Sigil shows progress followed by an applied receipt or an actionable refusal. If generation, exact token proof, economics admission, or a concurrent conversation change prevents safe activation, the active context remains unchanged. Unsupported routes remain unavailable rather than selecting an older algorithm. A failed manual summary is not silently downgraded; only fit-required or overflow emergency paths may use an explicitly audited deterministic fallback. Large tool-output aging remains a separate deterministic maintenance path instead of being hidden behind `/compact`. Context-window resolution uses the exact connection/model value first, then provider-owned metadata, then `fallback_context_window_tokens`. Normal TUI settings expose Automatic, 64K, 128K, 256K, and 1M presets; Automatic leaves the exact override unset. Existing custom values remain valid and can still be maintained in `sigil.toml`.

`native_carrier_enabled` is a default-off provider-native acceleration flag. Setting it to `true` currently has no effect because Sigil does not yet reuse provider-specific compacted state in the next request on the same route. Portable continuity remains the only active compaction path.

<!-- public-doc-topic: code-intelligence -->

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
auto_discover = true
```

When enabled, Sigil can use installed language servers for navigation, diagnostics, and reviewed edits. `Alt-D` checks changed source files. Missing language-server support does not block ordinary chat or file tools.

## Terminal And Model Request Overrides

<!-- public-doc-topic: terminal -->

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[terminal.notifications]
enabled = false
method = "auto"
minimum_run_duration_ms = 10000
```

Disable a feature when a terminal, remote layer, or multiplexer does not support it. Notifications are off by default and use fixed text without prompts, paths, tool details, provider, model, or session id. Use [Terminal compatibility](terminal-compatibility.md) to test the result.

<!-- public-doc-topic: model-request-env -->

`SIGIL_MODEL_REQUEST_TIMEOUT_SECS`, `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`, and `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` temporarily override shared model-request timeouts. Provider credentials and endpoint settings stay on provider pages.

## Plugins And MCP

<!-- public-doc-topic: plugins -->

Plugins are discovered at `.sigil/plugins/<id>/plugin.toml` and reviewed in `/config`. Review a changed plugin again before allowing it to run. Plugin entries cannot request inherited credential variables.

<!-- public-doc-topic: mcp -->

Configure MCP servers with `[[mcp_servers]]`. Local servers start with a cleared environment; grant only required variable names through root-user `inherit_env`. Remote authentication, trust, and compatibility belong in the [MCP guide](mcp.md). Exact fields are in [Configuration Reference](configuration-reference.md).

<!-- public-doc-cta: open-configuration-reference -->
Next: [Look up exact configuration fields](configuration-reference.md).
