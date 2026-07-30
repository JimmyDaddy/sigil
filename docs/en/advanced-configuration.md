<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# Advanced Configuration

[Docs home](README.md) · [Configuration](configuration.md) · [Permissions](permissions-and-sandbox.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/advanced-configuration.md)

Use these settings only after the normal setup works. Change one area at a time and run `sigil doctor` when the result is unclear.

## Task Planning

<!-- public-doc-topic: task -->

```toml
[task]
enabled = true
routing_policy = "manual"
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_subagents = 8
max_parallel_read_steps = 4
max_parallel_changeset_steps = 2
max_planning_research_agents = 3
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true
```

The values above are the schema and migration-safe compatibility defaults.
Quick Setup for a missing configuration may instead save `auto + proactive`
only when the installed release carries a qualified sidecar whose exact
provider, model, official endpoint family, task-config digest, and binary build
all match. Existing configurations are never rewritten. Missing, invalid, stale,
or non-matching sidecars fail closed to the values shown above. `sigil doctor`
reports the rollout state.

For a coarse rollback, set `routing_policy = "manual"` and
`multi_agent_mode = "explicit_request_only"`. This disables automatic handoff
and proactive spawn without deleting durable Task history. A route-local
zero-tolerance invariant applies the same effective fallback to subsequent
input for the affected session and build.

`routing_policy` is separate from the composer `default_mode`. The compatibility default is `manual`, so ordinary input stays chat-first. TUI `auto` routing lets the model request a typed handoff from a complex chat turn into the durable planner/executor flow; simple prompts still remain chat, and the handoff never bypasses write, shell, network, or merge approval. Planner, executor, subagent, and final-synthesis transcripts stay in isolated child sessions, while the parent keeps bounded results and one host-committed final answer. Independent shared-read-only Task steps may execute concurrently; `max_parallel_read_steps` bounds that fan-out together with `max_subagents`, while the host commits their terminal results to the parent in stable plan order. Independent `ChangesetOnly` write-subagent steps may also run concurrently, bounded by `max_parallel_changeset_steps` and `max_subagents`. Every member uses the same immutable parent-workspace snapshot, produces a proposal without changing that workspace, and is accepted for review only after the parent revalidates the snapshot. Independent physical `Worktree` writers can also run as a bounded whole batch in supported Git repositories. Sigil freezes the exact clean or safe dirty/untracked baseline, rebinds each child to a separately owned checkout, extracts bounded proposals, and routes them through a deterministic conflict graph. Non-conflicting integration lanes may apply and verify concurrently, but final promotion still requires exact integration review and authoritative parent verification. Direct or effectful writes in the shared parent workspace remain sequential and exclusive. The TUI Task strip and info rail mark every active step, and cancelling the Task closes the whole active batch. Before accepting a plan, the isolated planner may request one host-owned batch of independent read-only Explore probes. `max_planning_research_agents` defaults to `3`, is hard-capped at `4`, and may be set to `0` to disable this planner-only fan-out. The host waits for terminal probe results and resumes the planner automatically; no model polling command is required. The production HTTP driver, including the Desktop-owned `sigil serve` child, shares the same typed Task pause/continue, guidance, integration-review, restart-control, and recovery contracts with the TUI. Use `/plan` for a read-only plan and `/task` for deterministic multi-step execution; a complete `sigil-plan-v2` DAG is promoted directly without replanning. When the model identifies independently meaningful outcomes, the plan may also propose typed intent definitions and bind steps to provider-local aliases. These remain unaccepted suggestions until you accept the Plan card; the host then derives all runtime identities and atomically persists the accepted Intent plan with the bound Task plan. The conservative agent mode uses child agents only when you or workspace instructions request delegation. Role-specific model and tool restrictions are listed in [Configuration Reference](configuration-reference.md#task).

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

`[memory].enabled = true` lets Sigil load workspace instruction files such as `SIGIL.md`, `AGENTS.md`, and `SIGIL.local.md`. Keep them short, current, and suitable for every session in the repository.

<!-- public-doc-topic: skills-agents -->

Sigil-native reusable workspace skills, commands, agents, and plugins live under `.sigil/skills`, `.sigil/commands`, `.sigil/agents`, and `.sigil/plugins`. By default, Sigil also discovers standard `.agents/skills`, Codex `.codex/agents`, OpenCode `.opencode/{skills,commands,agents}`, and Claude Code `.claude/{skills,commands,agents}` resources. Native Sigil resources win name conflicts. Compatible resources inherit workspace trust instead of requiring per-item review or enablement; compatible commands use `/name`, while compatible agents use `@name` and remain manual-only and read-only. Set `[skills].compatibility_auto_discover = false` to disable the default set, or use `compatibility_sources` to add or precisely select sources.

## Compaction And Code Intelligence

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
strategy = "cache_aware_v3"
native_carrier_enabled = false
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
tail_messages = 6
```

`cache_aware_v3` is the default. It keeps reusable prior input stable where supported, preserves current intent and complete recent turns, and starts a new cache period only when the conversation must fit or trusted cost evidence shows a benefit. Running `/compact` is the explicit request to generate, validate, and atomically activate one recoverable semantic checkpoint; it does not open a confirmation modal. On an admitted route this makes one additional LLM request on the current provider/model route, keeping the previous request as the cacheable prefix and appending a strict JSON summary instruction. This is not a child agent and cannot execute tools. The model contributes only untrusted narrative; objectives, constraints, authorization, completion, and verification remain grounded in saved session history. Sigil shows progress followed by an applied receipt or an actionable refusal. If generation, exact token proof, economics admission, or a concurrent conversation change prevents safe activation, the active context remains unchanged. Automatic V3 is enabled only on supported, trusted provider routes; unknown or compatible routes fall back to `legacy_v2`. A failed manual summary is not silently downgraded; only fit-required or overflow emergency paths may use an explicitly audited deterministic fallback. Large tool-output aging remains a separate deterministic maintenance path instead of being hidden behind a `/compact` review. The ratio and `tail_messages` fields remain readable for rollback and migration; V3 treats the tail value as a whole-turn minimum instead of cutting a raw message count. If a model window is unknown, set `fallback_context_window_tokens`.

`native_carrier_enabled` is a reserved, default-off migration flag. Setting it to `true` currently has no effect because Sigil does not yet reuse provider-specific compacted state in the next request on the same route. Portable continuity remains the only active compaction path.

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
