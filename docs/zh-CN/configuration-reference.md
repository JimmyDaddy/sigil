<!-- public-doc-role: configuration-reference; authority: configuration-field-authority; sections: workspace-storage-and-session,agent-and-providers,execution,verification,appearance,task,permission,web-memory-skills-and-compaction,code-intelligence-terminal-plugins-and-mcp; cta: return-configuration -->

# 配置字段参考

[文档首页](README.md) · [配置指南](configuration.md) · [权限与沙箱](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [English](../en/configuration-reference.md)

本文是公开 `sigil.toml` 表面的查阅参考。选择行为时请先阅读对应指南；在确认字段名、可接受的值或默认值时再回到本文。

## Workspace、Storage 与 Session

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[workspace].root` | `"."` | Workspace 目录；`"."` 跟随启动 `sigil` 的目录。 |
| `[storage].state_root` | `"auto"` | 每用户的持久 Sigil state。`SIGIL_STATE_HOME` 可覆盖。 |
| `[storage].cache_root` | `"auto"` | 可重建的每用户 cache。`SIGIL_CACHE_HOME` 可覆盖。 |
| `[session].log_dir` | workspace state 下的 `sessions` | session log 位置。相对值在 workspace state 下解析。 |
| `[session.retention].max_sessions` | `500` | 显式 cleanup 后最多保留的 ready session 数。 |
| `[session.retention].max_bytes` | `2147483648` | 显式 cleanup 后 ready session 的最大总字节数。 |
| `[session.retention].expire_older_than_ms` | `15552000000` | 显式 cleanup 时选择早于 180 天且未受保护的 session。 |
| `[storage.mutation_artifact_retention].max_artifacts` | `10000` | 显式 cleanup 可选择的 artifact 数量上限。 |
| `[storage.mutation_artifact_retention].max_bytes` | `536870912` | 显式 cleanup 可选择的 artifact 总字节上限。 |
| `[storage.mutation_artifact_retention].expire_older_than_ms` | `2592000000` | 显式 cleanup 时选择早于 30 天的 artifact。 |

路径选择与 retention 只允许显式执行的边界见[配置指南](configuration.md#存储与-session-路径)。

## Agent 与 Providers

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[agent].provider` | setup 选择 | `deepseek`、`openai_compat`、`openai_responses`、`anthropic` 或 `gemini`。 |
| `[agent].model` | provider setup 选择 | 默认 chat model。 |
| `[agent].tool_timeout_secs` | `30` | 工具超时秒数。 |
| `[agent].max_turns` | 禁用 | 未收敛工具循环的可选上限。 |
| `[model_request].request_timeout_secs` | `120` | 模型请求等待上限；单次启动可用 `SIGIL_MODEL_REQUEST_TIMEOUT_SECS` 覆盖。 |
| `[model_request].stream_idle_timeout_secs` | `180` | Stream item 之间的最长等待；可用 `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 覆盖。 |
| `[model_request].stream_total_timeout_secs` | 未设置 | 可选 stream 总时限；可用 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 覆盖。 |

Provider 区块与凭据见[Provider 指南](providers.md)。

## Execution

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[execution].strategy` | `"local"` | `local` 或 `sandbox`。 |
| `[execution.sandbox].backend` | 使用 `sandbox` 时必填 | `macos_seatbelt`、`linux_bubblewrap` 或 `docker`。 |
| `[execution.sandbox].profile` | `"workspace_write"` | 请求的 sandbox profile。 |
| `[execution.sandbox].fallback` | `"deny"` | 所选 sandbox 不可用时的行为。 |
| `[execution.sandbox].container_image` | 仅 Docker 必填 | Docker backend 使用的 image。 |

修改前请阅读[权限与沙箱](permissions-and-sandbox.md#沙箱预期)。

## Verification

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[verification.scope].profile` | `"auto"` | 粗粒度 verification scope preset。 |
| `[verification.scope].extra_excludes` | `[]` | 额外排除 glob。 |
| `[verification.scope].generated_roots` | `[]` | 不应成为验证证据的 generated 目录。 |
| `[[verification.checks]].id` | 必填 | 稳定检查名称。 |
| `[[verification.checks]].command` | 必填 | 可执行文件名。 |
| `[[verification.checks]].args` | `[]` | 命令参数。 |
| `[[verification.checks]].cwd` | workspace root | workspace 相对工作目录。 |
| `[[verification.checks]].effect` | `"read_only"` | 预期影响。 |
| `[verification].auto_run` | `"manual"` | `manual`、`trusted_only` 或 `never`。 |

## Appearance

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[appearance].info_rail` | `true` | 终端宽度允许时启动显示右侧 info rail。 |
| `[appearance].theme` | `"sigil_dark"` | TUI palette。 |
| `[appearance].syntax_theme` | `"auto"` | 代码高亮 palette。 |
| `[appearance].usage_cost_currency` | `"auto"` | `auto`、`usd` 或 `cny` 的显示货币。 |
| `[appearance.colors].<token>` | 内置主题 | `#RRGGBB` 语义颜色覆盖。 |

颜色 token group 包括 surfaces、borders、text、accents、selection/buttons、status、diff、approval/risk、markdown、modal/overlay 和 config/setup。可读性建议见[外观](appearance.md)。

## Task

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[task].enabled` | `true` | 开启 task planning。 |
| `[task].default_mode` | `"chat"` | composer 默认行为。 |
| `[task].max_plan_steps` | `12` | plan step 上限。 |
| `[task].max_replans` | `2` | replan 上限。 |
| `[task].max_subagents` | `8` | 活跃 child agent 上限。 |
| `[task].multi_agent_mode` | `"explicit_request_only"` | `none`、`explicit_request_only` 或 `proactive`。 |
| `[task].allow_write_subagents` | `true` | 符合资格的 child 是否可请求文件修改工作。 |
| `[task.<role>].provider` / `.model` / `.reasoning_effort` | 继承 `[agent]` | 可选 role 专用 model 选择。 |
| `[task.<role>.tools].names` / `.prefixes` / `.allow_all` | role 默认值 | 可选可见工具限制。 |

Role 包括 `planner`、`executor`、`subagent_read` 和 `subagent_write`。

## Permission

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[permission].mode` | `"manual"` | `read-only`、`manual`、`auto-edit` 或 `danger-full-access`。 |
| `[permission.commands].allow` / `.ask` / `.deny` | `[]` | Shell 命令 pattern。 |
| `[permission.tools].<tool>` | 未设置 | 按工具设置 `allow`、`ask` 或 `deny`。 |
| `[[permission.rules]].tool_name` / `.subject_glob` / `.mode` | `[]` | 按工具与目标内容设置细粒度规则。 |
| `[permission.external_directory].enabled` | `false` | 开启对 workspace 外路径的考虑。 |
| `[permission.external_directory].default_mode` | `"ask"` | 已启用外部路径的默认动作。 |
| `[[permission.external_directory.rules]].path_glob` / `.mode` | `[]` | 收窄的外部路径规则。 |

有效安全行为见[权限与沙箱](permissions-and-sandbox.md)。

## Web、Memory、Skills 与 Compaction

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[web].enabled` | `true` | 开启已配置的 Web 工具。 |
| `[web].network_mode` | `"allow"` | `allow`、`ask` 或 `deny`。 |
| `[web].allow_http` | `true` | 仅在 route 与目标检查允许时使用 HTTP。 |
| `[web].proxy_mode` / `.redirect_policy` | `"environment"` / `"same_origin"` | Proxy 来源和 redirect 边界；redirect 也可设为 `deny`。 |
| `[web].search_route` | `"auto"` | `auto`、`provider_hosted`、`mcp`、`bundled` 或 `disabled`。 |
| `[web].max_results` | `8` | 搜索结果上限。 |
| `[web].max_query_chars` / `.max_query_bytes` | `512` / `2048` | 查询限制。 |
| `[web.bundled_search].enabled` | `true` | 开启 bundled search route。 |
| `[web.search_mcp].server` / `.tool` | 未设置 | 你的兼容 MCP search binding。 |
| `[web].max_same_origin_redirects` | `5` | Same-origin redirect 开启时的次数上限。 |
| `[web].timeout_secs` / `.connect_timeout_secs` / `.max_url_bytes` / `.max_domains` | `15` / `5` / `2048` / `10` | 请求、连接、URL 和 domain list 上限。 |
| `[web].max_url_capabilities_per_session` / `.url_capability_ttl_secs` | `256` / `3600` | Session URL grant 数量和有效期上限。 |
| `[web].max_wire_response_bytes` / `.max_decoded_response_bytes` / `.max_model_content_bytes` / `.max_hosted_turn_buffer_bytes` | `2097152` / `1048576` / `24000` / `262144` | 单个 response 与 hosted turn 的字节上限。 |
| `[web].max_fetches_per_run` / `.max_client_searches_per_run` / `.max_hosted_enabled_provider_requests_per_run` / `.provider_hosted_max_uses_per_request` | `5` / `3` / `4` / `3` | 每次 run 的 Web 调用上限。 |
| `[web].max_network_attempts_per_run` / `.max_concurrent_requests` / `.per_host_rate_limit_per_minute` | `12` / `2` / `10` | 尝试、并发和每 host rate limit。 |
| `[web].max_total_wire_bytes_per_run` / `.max_total_decoded_bytes_per_run` / `.max_total_model_bytes_per_run` | `8388608` / `4194304` / `98304` | 每次 run 的累计字节上限。 |
| `[web].allowed_ports` | `[80, 443]` | 允许的目标端口。 |
| `[web].allowed_domains` / `.blocked_domains` / `.allowed_private_hosts` / `.allowed_private_cidrs` | `[]` | 可选目标 list；私有目标需要显式匹配。 |
| `[memory].enabled` | `true` | 加载 workspace 指令文件。 |
| `[skills].enabled` / `.user_skills` / `.user_agents` | `true` | 开启发现到的可复用资源。 |
| `[skills].compatibility_sources` | `[]` | 可选 `claude` 或 `reasonix` 导入。 |
| `[compaction].enabled` | `true` | 开启对话 compaction。 |
| `[compaction].soft_threshold_ratio` / `.hard_threshold_ratio` | `0.5` / `0.8` | 提醒与有限 idle 自动阈值；自动应用仍要求本地 review 显示 ready。 |
| `[compaction].fallback_context_window_tokens` | 未设置 | 后备 model window 值。 |
| `[compaction].tail_messages` | `6` | 原样保留的最近消息数。 |

## 代码智能、Terminal、Plugins 与 MCP

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[code_intelligence].enabled` | `false` | 开启代码导航和经过审查的编辑建议。 |
| `[code_intelligence].server_startup` | `"lazy"` | 配置的 language server 何时启动。 |
| `[code_intelligence].default_timeout_ms` | `5000` | 单次请求超时。 |
| `[code_intelligence].max_results` / `.max_payload_bytes` | `100` / `65536` | 结果限制。 |
| `[code_intelligence].auto_discover` / `.report_missing` | `true` | 自动发现与 readiness 报告。 |
| `[[code_intelligence.servers]].name` / `.command` | 显式 server 时必填 | Language server 标识与命令。 |
| `[[code_intelligence.servers]].languages` | `[]` | 可选 language identifier。 |
| `[[code_intelligence.servers]].args` / `.env` / `.initialization_options` | `[]` / `{}` / `{}` | Server 参数、显式环境和初始化数据。 |
| `[[code_intelligence.servers]].root_markers` / `.file_extensions` | `[]` | Workspace 与文件匹配。 |
| `[[code_intelligence.servers]].startup_timeout_ms` | `10000` | 启动超时。 |
| `[[code_intelligence.servers]].trust_required` | `true` | 需要匹配 workspace trust decision。 |
| `[terminal].keyboard_enhancement` | `"auto"` | `auto`、`on` 或 `off`。 |
| `[terminal].mouse_capture` / `.osc52_clipboard` | `true` | 鼠标与 OSC52 剪贴板行为。 |
| `[terminal].scroll_sensitivity` | `3` | 每次滚轮滚动的行数。 |
| `[terminal.notifications].enabled` | `false` | 在交互式 TUI 中启用有明确隐私边界的 attention signal。 |
| `[terminal.notifications].method` | `"auto"` | `auto`、`osc9`、`osc777` 或 `bell`。 |
| `[terminal.notifications].minimum_run_duration_ms` | `10000` | 长任务完成阈值，范围为 `1000` 到 `3600000`。 |
| `[[mcp_servers]].name` / `.transport` | 必填 | 稳定 server 名和显式 `stdio` 或 `streamable_http` transport。 |
| `[[mcp_servers]].command` / `.args` / `.inherit_env` | stdio 必填 / `[]` / `[]` | 本机 command、参数和仅用户根可用的环境变量名。 |
| `[[mcp_servers]].url` | HTTP 时必填 | HTTP(S) endpoint；使用环境变量 header、bearer 或 OAuth 凭据时必须为 HTTPS。 |
| `[[mcp_servers]].http_headers` / `.env_http_headers` | `{}` | 静态公开 header，或 header 到环境变量名的绑定。Secret 值保留在环境变量中。 |
| `[[mcp_servers]].bearer_token_env_var` | 未设置 | 包含一个静态 bearer token 的环境变量；与 OAuth 互斥。 |
| `[[mcp_servers]].client_capabilities` | `[]` | 可选的 `roots` 与 `elicitation` remote server capability。 |
| `[mcp_servers.oauth].client_id` | 未设置 | 可选 public client id；server 支持动态注册时可省略。 |
| `[mcp_servers.oauth].scopes` | `[]` | 可选 scope。OAuth 要求 HTTPS，且不能与静态 bearer 或 Authorization 凭据同时使用。 |
| `[[mcp_servers]].startup_timeout_secs` / `.required` / `.startup` | `10` / `true` / `"eager"` | 启动时限、严格启动要求，以及 `eager` 或 `lazy` 启动。 |
| `[mcp_servers.trust].trust_class` / `.approval_default` | `"self_hosted"` / `"ask"` | Trust label 与常规审批行为。 |
| `[mcp_servers.trust].egress_logging` / `.allow_secrets` / `.pin_version` | `true` / `false` / `false` | Egress log、secret access 与 identity pin 控制。 |
| `[mcp_servers.trust.pinned].transport_fingerprint` / `.protocol_version` / `.server_name` / `.server_version` | pin 开启时必填 | `pin_version = true` 时预期的 server identity。 |

设置示例见[高级配置](advanced-configuration.md)和[MCP 指南](mcp.md)。

<!-- public-doc-cta: return-configuration -->
下一步：[返回配置指南](configuration.md)。
