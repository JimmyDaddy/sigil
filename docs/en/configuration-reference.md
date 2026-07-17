<!-- public-doc-role: configuration-reference; authority: configuration-field-authority; sections: workspace-storage-and-session,agent-and-providers,execution,verification,appearance,task,permission,web-memory-skills-and-compaction,code-intelligence-terminal-plugins-and-mcp; cta: return-configuration -->

# Configuration Reference

[Docs home](README.md) · [Configuration](configuration.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Appearance](appearance.md) · [Advanced configuration](advanced-configuration.md) · [简体中文](../zh-CN/configuration-reference.md)

This page is a lookup reference for the public `sigil.toml` surface. Start with the focused guides when choosing a behavior; use this page to confirm a field name, accepted value, or default.

## Workspace, Storage, And Session

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[workspace].root` | `"."` | Workspace directory; `"."` follows the directory where `sigil` starts. |
| `[storage].state_root` | `"auto"` | Per-user durable Sigil state. `SIGIL_STATE_HOME` overrides it. |
| `[storage].cache_root` | `"auto"` | Rebuildable per-user cache. `SIGIL_CACHE_HOME` overrides it. |
| `[session].log_dir` | workspace state `sessions` child | Session-log location. A relative value resolves under workspace state. |
| `[session.retention].max_sessions` | `500` | Maximum retained ready sessions after an explicit cleanup. |
| `[session.retention].max_bytes` | `2147483648` | Maximum bytes retained across ready sessions after explicit cleanup. |
| `[session.retention].expire_older_than_ms` | `15552000000` | Select unprotected sessions older than 180 days during explicit cleanup. |
| `[storage.mutation_artifact_retention].max_artifacts` | `10000` | Maximum artifacts selected by explicit cleanup. |
| `[storage.mutation_artifact_retention].max_bytes` | `536870912` | Maximum artifact bytes selected by explicit cleanup. |
| `[storage.mutation_artifact_retention].expire_older_than_ms` | `2592000000` | Select artifacts older than 30 days during explicit cleanup. |

Use [Configuration](configuration.md#storage-and-session-paths) for path choices and the explicit-only retention boundary.

## Agent And Providers

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[agent].provider` | setup choice | `deepseek`, `openai_compat`, `openai_responses`, `anthropic`, or `gemini`. |
| `[agent].model` | provider setup choice | Default chat model. |
| `[agent].tool_timeout_secs` | `30` | Tool timeout in seconds. |
| `[agent].max_turns` | disabled | Optional limit for an unfinished tool loop. |
| `[model_request].request_timeout_secs` | `120` | Model-request wait; `SIGIL_MODEL_REQUEST_TIMEOUT_SECS` overrides it for one launch. |
| `[model_request].stream_idle_timeout_secs` | `180` | Maximum pause between streamed items; `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` overrides it. |
| `[model_request].stream_total_timeout_secs` | unset | Optional total stream limit; `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` overrides it. |

Provider blocks and credentials are documented in the [Provider guide](providers.md).

## Execution

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[execution].strategy` | `"local"` | `local` or `sandbox`. |
| `[execution.sandbox].backend` | required for `sandbox` | `macos_seatbelt`, `linux_bubblewrap`, or `docker`. |
| `[execution.sandbox].profile` | `"workspace_write"` | Requested sandbox profile. |
| `[execution.sandbox].fallback` | `"deny"` | Behavior if the selected sandbox cannot be used. |
| `[execution.sandbox].container_image` | required only for Docker | Image for the Docker backend. |

See [Permissions and sandbox](permissions-and-sandbox.md#sandbox-expectations) before changing these fields.

## Verification

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[verification.scope].profile` | `"auto"` | Coarse verification scope preset. |
| `[verification.scope].extra_excludes` | `[]` | Extra excluded globs. |
| `[verification.scope].generated_roots` | `[]` | Generated directories that should not become verification evidence. |
| `[[verification.checks]].id` | required | Stable check name. |
| `[[verification.checks]].command` | required | Executable name. |
| `[[verification.checks]].args` | `[]` | Command arguments. |
| `[[verification.checks]].cwd` | workspace root | Workspace-relative working directory. |
| `[[verification.checks]].effect` | `"read_only"` | Expected effect. |
| `[verification].auto_run` | `"manual"` | `manual`, `trusted_only`, or `never`. |

## Appearance

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[appearance].info_rail` | `true` | Show the right info rail at startup when terminal width allows it. |
| `[appearance].theme` | `"sigil_dark"` | TUI palette. |
| `[appearance].syntax_theme` | `"auto"` | Code-highlight palette. |
| `[appearance].usage_cost_currency` | `"auto"` | `auto`, `usd`, or `cny` display currency. |
| `[appearance.colors].<token>` | built-in theme | A `#RRGGBB` semantic color override. |

Color-token groups are surfaces, borders, text, accents, selection/buttons, status, diff, approval/risk, markdown, modal/overlay, and config/setup. See [Appearance](appearance.md) for readable override guidance.

## Task

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[task].enabled` | `true` | Enables task planning. |
| `[task].default_mode` | `"chat"` | Default composer behavior. |
| `[task].max_plan_steps` | `12` | Plan-step limit. |
| `[task].max_replans` | `2` | Replanning limit. |
| `[task].max_subagents` | `8` | Active child-agent limit. |
| `[task].multi_agent_mode` | `"explicit_request_only"` | `none`, `explicit_request_only`, or `proactive`. |
| `[task].allow_write_subagents` | `true` | Whether an eligible child may request file-changing work. |
| `[task.<role>].provider` / `.model` / `.reasoning_effort` | inherits `[agent]` | Optional role-specific model choice. |
| `[task.<role>.tools].names` / `.prefixes` / `.allow_all` | role default | Optional visible-tool restriction. |

Roles are `planner`, `executor`, `subagent_read`, and `subagent_write`.

## Permission

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[permission].mode` | `"manual"` | `read-only`, `manual`, `auto-edit`, or `danger-full-access`. |
| `[permission.commands].allow` / `.ask` / `.deny` | `[]` | Shell-command patterns. |
| `[permission.tools].<tool>` | unset | Per-tool `allow`, `ask`, or `deny` override. |
| `[[permission.rules]].tool_name` / `.subject_glob` / `.mode` | `[]` | Fine-grained tool and subject rule. |
| `[permission.external_directory].enabled` | `false` | Enables consideration of workspace-external paths. |
| `[permission.external_directory].default_mode` | `"ask"` | Fallback action for an enabled external path. |
| `[[permission.external_directory.rules]].path_glob` / `.mode` | `[]` | Narrow external-path rules. |

See [Permissions and sandbox](permissions-and-sandbox.md) for the effective safety behavior.

## Web, Memory, Skills, And Compaction

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[web].enabled` | `true` | Enables configured web tools. |
| `[web].network_mode` | `"allow"` | `allow`, `ask`, or `deny`. |
| `[web].allow_http` | `true` | Allows HTTP only where route and destination checks permit it. |
| `[web].proxy_mode` / `.redirect_policy` | `"environment"` / `"same_origin"` | Proxy source and redirect boundary; redirect can also be `deny`. |
| `[web].search_route` | `"auto"` | `auto`, `provider_hosted`, `mcp`, `bundled`, or `disabled`. |
| `[web].max_results` | `8` | Search-result limit. |
| `[web].max_query_chars` / `.max_query_bytes` | `512` / `2048` | Query limits. |
| `[web.bundled_search].enabled` | `true` | Enables the bundled search route. |
| `[web.search_mcp].server` / `.tool` | unset | Your compatible MCP search binding. |
| `[web].max_same_origin_redirects` | `5` | Redirect limit when same-origin redirects are enabled. |
| `[web].timeout_secs` / `.connect_timeout_secs` / `.max_url_bytes` / `.max_domains` | `15` / `5` / `2048` / `10` | Request, connection, URL, and domain-list bounds. |
| `[web].max_url_capabilities_per_session` / `.url_capability_ttl_secs` | `256` / `3600` | Session URL grant count and lifetime bounds. |
| `[web].max_wire_response_bytes` / `.max_decoded_response_bytes` / `.max_model_content_bytes` / `.max_hosted_turn_buffer_bytes` | `2097152` / `1048576` / `24000` / `262144` | Per-response and hosted-turn byte limits. |
| `[web].max_fetches_per_run` / `.max_client_searches_per_run` / `.max_hosted_enabled_provider_requests_per_run` / `.provider_hosted_max_uses_per_request` | `5` / `3` / `4` / `3` | Per-run Web call limits. |
| `[web].max_network_attempts_per_run` / `.max_concurrent_requests` / `.per_host_rate_limit_per_minute` | `12` / `2` / `10` | Attempt, concurrency, and per-host rate limits. |
| `[web].max_total_wire_bytes_per_run` / `.max_total_decoded_bytes_per_run` / `.max_total_model_bytes_per_run` | `8388608` / `4194304` / `98304` | Aggregate per-run byte limits. |
| `[web].allowed_ports` | `[80, 443]` | Permitted destination ports. |
| `[web].allowed_domains` / `.blocked_domains` / `.allowed_private_hosts` / `.allowed_private_cidrs` | `[]` | Optional destination lists; private destinations require an explicit match. |
| `[memory].enabled` | `true` | Loads workspace instruction files. |
| `[skills].enabled` / `.user_skills` / `.user_agents` | `true` | Enables discovered reusable resources. |
| `[skills].compatibility_sources` | `[]` | Optional `claude` or `reasonix` imports. |
| `[compaction].enabled` | `true` | Enables conversation compaction. |
| `[compaction].soft_threshold_ratio` / `.hard_threshold_ratio` | `0.5` / `0.8` | Warning and limited idle-auto threshold; automatic apply still requires a ready local review. |
| `[compaction].fallback_context_window_tokens` | unset | Fallback model-window value. |
| `[compaction].tail_messages` | `6` | Recent messages retained verbatim. |

## Code Intelligence, Terminal, Plugins, And MCP

| Section / field | Default | Purpose |
| --- | --- | --- |
| `[code_intelligence].enabled` | `false` | Enables code navigation and reviewed edit suggestions. |
| `[code_intelligence].server_startup` | `"lazy"` | When configured language servers start. |
| `[code_intelligence].default_timeout_ms` | `5000` | Per-request timeout. |
| `[code_intelligence].max_results` / `.max_payload_bytes` | `100` / `65536` | Result limits. |
| `[code_intelligence].auto_discover` / `.report_missing` | `true` | Discovery and readiness reporting. |
| `[[code_intelligence.servers]].name` / `.command` | required for explicit server | Language-server identity and command. |
| `[[code_intelligence.servers]].languages` | `[]` | Optional language identifiers. |
| `[[code_intelligence.servers]].args` / `.env` / `.initialization_options` | `[]` / `{}` / `{}` | Server arguments, explicit environment, and initialization data. |
| `[[code_intelligence.servers]].root_markers` / `.file_extensions` | `[]` | Workspace and file matching. |
| `[[code_intelligence.servers]].startup_timeout_ms` | `10000` | Startup timeout. |
| `[[code_intelligence.servers]].trust_required` | `true` | Requires a matching workspace-trust decision. |
| `[terminal].keyboard_enhancement` | `"auto"` | `auto`, `on`, or `off`. |
| `[terminal].mouse_capture` / `.osc52_clipboard` | `true` | Mouse and OSC52 clipboard behavior. |
| `[terminal].scroll_sensitivity` | `3` | Rows per mouse-wheel tick. |
| `[terminal.notifications].enabled` | `false` | Enables privacy-bounded attention signals in the interactive TUI. |
| `[terminal.notifications].method` | `"auto"` | `auto`, `osc9`, `osc777`, or `bell`. |
| `[terminal.notifications].minimum_run_duration_ms` | `10000` | Long-run completion threshold, from `1000` through `3600000`. |
| `[[mcp_servers]].name` / `.transport` | required | Stable server name and explicit `stdio` or `streamable_http` transport. |
| `[[mcp_servers]].command` / `.args` / `.inherit_env` | required for stdio / `[]` / `[]` | Local command, arguments, and root-only environment names. |
| `[[mcp_servers]].url` | required for HTTP | HTTP(S) endpoint. HTTPS is required for environment-backed headers, bearer credentials, or OAuth. |
| `[[mcp_servers]].http_headers` / `.env_http_headers` | `{}` | Static public headers or header-to-environment-name bindings. Secret values stay in environment variables. |
| `[[mcp_servers]].bearer_token_env_var` | unset | Environment variable containing one static bearer token. Mutually exclusive with OAuth. |
| `[[mcp_servers]].client_capabilities` | `[]` | Optional `roots` and `elicitation` capabilities advertised to a remote server. |
| `[mcp_servers.oauth].client_id` | unset | Optional public client id; omit when the server supports dynamic registration. |
| `[mcp_servers.oauth].scopes` | `[]` | Optional requested scopes. OAuth requires HTTPS and cannot be combined with a static bearer or Authorization credential. |
| `[[mcp_servers]].startup_timeout_secs` / `.required` / `.startup` | `10` / `true` / `"eager"` | Startup limit, strict-start requirement, and `eager` or `lazy` start. |
| `[mcp_servers.trust].trust_class` / `.approval_default` | `"self_hosted"` / `"ask"` | Trust label and normal approval behavior. |
| `[mcp_servers.trust].egress_logging` / `.allow_secrets` / `.pin_version` | `true` / `false` / `false` | Egress log, secret access, and identity-pin controls. |
| `[mcp_servers.trust.pinned].transport_fingerprint` / `.protocol_version` / `.server_name` / `.server_version` | required when pinned | Expected server identity when `pin_version = true`. |

See [Advanced configuration](advanced-configuration.md) and the [MCP guide](mcp.md) for setup examples.

<!-- public-doc-cta: return-configuration -->
Next: [Return to Configuration](configuration.md).
