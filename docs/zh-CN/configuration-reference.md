<!-- public-doc-role: configuration-reference; authority: configuration-field-authority; sections: workspace-storage-and-session,agent-and-providers,execution,verification,appearance,task,permission,web-memory-skills-and-compaction,code-intelligence-terminal-plugins-and-mcp; cta: return-configuration -->

# 配置字段参考

[文档首页](README.md) · [配置指南](configuration.md) · [权限与沙箱](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [English](../en/configuration-reference.md)

本文汇总 `sigil.toml` 的公开配置字段。决定怎么配置时请先阅读对应指南；需要确认字段名、可选值或默认值时，再回到这里查阅。

## 工作区、存储与会话

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[workspace].root` | `"."` | 工作区目录；`"."` 表示启动 `sigil` 时所在的目录。 |
| `[storage].state_root` | `"auto"` | 当前用户的 Sigil 持久状态目录。可用 `SIGIL_STATE_HOME` 覆盖。 |
| `[storage].cache_root` | `"auto"` | 当前用户可重新生成的缓存目录。可用 `SIGIL_CACHE_HOME` 覆盖。 |
| `[session].log_dir` | 工作区状态目录下的 `sessions` | 会话日志位置。相对路径从工作区状态目录开始解析。 |
| `[session.retention].max_sessions` | `500` | 手动清理后最多保留多少个状态为 `ready` 的会话。 |
| `[session.retention].max_bytes` | `2147483648` | 手动清理后，状态为 `ready` 的会话最多占用多少字节。 |
| `[session.retention].expire_older_than_ms` | `15552000000` | 手动清理时，选中早于 180 天且未受保护的会话。 |
| `[storage.mutation_artifact_retention].max_artifacts` | `10000` | 一次手动清理最多选择多少个变更记录。 |
| `[storage.mutation_artifact_retention].max_bytes` | `536870912` | 一次手动清理最多选择多少字节的变更记录。 |
| `[storage.mutation_artifact_retention].expire_older_than_ms` | `2592000000` | 手动清理时，选中早于 30 天的变更记录。 |

路径选择和保留期限的显式执行边界见[配置指南](configuration.md#存储与会话路径)。

## 智能体与模型服务

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[agent].provider` | 快速设置中的选择 | `deepseek`、`openai_compat`、`openai_responses`、`anthropic` 或 `gemini`。 |
| `[agent].model` | 模型服务设置中的选择 | 默认对话模型。 |
| `[agent].tool_timeout_secs` | `30` | 工具超时秒数。 |
| `[agent].max_turns` | 禁用 | 未收敛工具循环的可选上限。 |
| `[model_request].request_timeout_secs` | `120` | 模型请求等待上限；单次启动可用 `SIGIL_MODEL_REQUEST_TIMEOUT_SECS` 覆盖。 |
| `[model_request].stream_idle_timeout_secs` | `180` | 两个流式响应事件之间的最长等待时间；可用 `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 覆盖。 |
| `[model_request].stream_total_timeout_secs` | 未设置 | 整个流式响应的可选时限；可用 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 覆盖。 |

各模型服务的配置区块与凭据说明见[模型服务指南](providers.md)。

## 执行

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[execution].strategy` | `"local"` | `local` 或 `sandbox`。 |
| `[execution.sandbox].backend` | 使用 `sandbox` 时必填 | `macos_seatbelt`、`linux_bubblewrap` 或 `docker`。 |
| `[execution.sandbox].profile` | `"workspace_write"` | 需要使用的沙箱配置。 |
| `[execution.sandbox].fallback` | `"deny"` | 所选沙箱不可用时的处理方式。 |
| `[execution.sandbox].container_image` | 仅 Docker 必填 | Docker 后端使用的镜像。 |

修改前请阅读[权限与沙箱](permissions-and-sandbox.md#沙箱预期)。

## 验证

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[verification.scope].profile` | `"auto"` | 粗粒度的验证范围预设。 |
| `[verification.scope].extra_excludes` | `[]` | 额外排除的 glob 模式。 |
| `[verification.scope].generated_roots` | `[]` | 不应作为验证证据的生成目录。 |
| `[[verification.checks]].id` | 必填 | 稳定检查名称。 |
| `[[verification.checks]].command` | 必填 | 可执行文件名。 |
| `[[verification.checks]].args` | `[]` | 命令参数。 |
| `[[verification.checks]].cwd` | 工作区根目录 | 相对于工作区的运行目录。 |
| `[[verification.checks]].effect` | `"read_only"` | 预期影响。 |
| `[verification].auto_run` | `"manual"` | `manual`、`trusted_only` 或 `never`。 |

## 外观

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[appearance].info_rail` | `true` | 终端宽度允许时，启动后显示右侧信息栏。 |
| `[appearance].theme` | `"sigil_dark"` | TUI 配色主题。 |
| `[appearance].syntax_theme` | `"auto"` | 代码高亮配色。 |
| `[appearance].usage_cost_currency` | `"auto"` | `auto`、`usd` 或 `cny` 的显示货币。 |
| `[appearance.colors].<token>` | 内置主题 | `#RRGGBB` 语义颜色覆盖。 |

可以覆盖界面背景、边框、文字、强调色、选择与按钮、状态、差异、审批与风险、Markdown、弹窗与遮罩，以及配置与设置界面的颜色。可读性建议见[外观](appearance.md)。

## 任务

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[task].enabled` | `true` | 开启任务规划。 |
| `[task].routing_policy` | `"manual"` | 普通对话路由策略：`manual` 或由 coordinator 接管的 `auto`；不会授予工具权限。 |
| `[task].default_mode` | `"chat"` | 输入框的默认工作模式。 |
| `[task].max_plan_steps` | `12` | 单个计划最多包含多少步。 |
| `[task].max_replans` | `2` | 最多允许重新规划多少次。 |
| `[task].max_subagents` | `8` | 最多同时运行多少个子智能体。 |
| `[task].max_parallel_read_steps` | `4` | 单批最多启动多少个相互独立的 shared-read-only Task step；parent 终态仍按稳定 plan 顺序提交。 |
| `[task].max_planning_research_agents` | `3` | 每次 Planner attempt 最多使用多少个只读 Explore probe；`0` 表示关闭，超过硬上限 `4` 的值会被截断。 |
| `[task].multi_agent_mode` | `"explicit_request_only"` | `none`、`explicit_request_only` 或 `proactive`。 |
| `[task].allow_write_subagents` | `true` | 符合条件的子智能体能否请求修改文件。 |
| `[task.<role>].provider` / `.model` / `.reasoning_effort` | 继承 `[agent]` | 可按角色单独选择模型服务、模型和推理强度。 |
| `[task.<role>.tools].names` / `.prefixes` / `.allow_all` | 角色默认值 | 可按角色限制能够看到的工具。 |

可配置的角色包括 `planner`、`executor`、`subagent_read` 和 `subagent_write`。

## 权限

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[permission].mode` | `"manual"` | `read-only`、`manual`、`auto-edit` 或 `danger-full-access`。 |
| `[permission.commands].allow` / `.ask` / `.deny` | `[]` | 匹配 Shell 命令的模式。 |
| `[permission.tools].<tool>` | 未设置 | 按工具设置 `allow`、`ask` 或 `deny`。 |
| `[[permission.rules]].tool_name` / `.subject_glob` / `.mode` | `[]` | 按工具与目标内容设置细粒度规则。 |
| `[permission.external_directory].enabled` | `false` | 允许规则匹配工作区之外的路径。 |
| `[permission.external_directory].default_mode` | `"ask"` | 已启用外部路径的默认动作。 |
| `[[permission.external_directory.rules]].path_glob` / `.mode` | `[]` | 收窄的外部路径规则。 |

有效安全行为见[权限与沙箱](permissions-and-sandbox.md)。

## Web、记忆、技能与上下文精简

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[web].enabled` | `true` | 开启已配置的 Web 工具。 |
| `[web].network_mode` | `"allow"` | `allow`、`ask` 或 `deny`。 |
| `[web].allow_http` | `true` | 只有路由和目标检查均允许时才使用 HTTP。 |
| `[web].proxy_mode` / `.redirect_policy` | `"environment"` / `"same_origin"` | 代理来源和重定向边界；重定向策略也可设为 `deny`。 |
| `[web].search_route` | `"auto"` | `auto`、`provider_hosted`、`mcp`、`bundled` 或 `disabled`。 |
| `[web].max_results` | `8` | 搜索结果上限。 |
| `[web].max_query_chars` / `.max_query_bytes` | `512` / `2048` | 查询限制。 |
| `[web.bundled_search].enabled` | `true` | 开启内置搜索路由。 |
| `[web.search_mcp].server` / `.tool` | 未设置 | 用于搜索的兼容 MCP 服务和工具。 |
| `[web].max_same_origin_redirects` | `5` | 允许同源重定向时的次数上限。 |
| `[web].timeout_secs` / `.connect_timeout_secs` / `.max_url_bytes` / `.max_domains` | `15` / `5` / `2048` / `10` | 请求、连接、URL 长度和域名数量上限。 |
| `[web].max_url_capabilities_per_session` / `.url_capability_ttl_secs` | `256` / `3600` | 每个会话可授权的 URL 数量和授权有效期上限。 |
| `[web].max_wire_response_bytes` / `.max_decoded_response_bytes` / `.max_model_content_bytes` / `.max_hosted_turn_buffer_bytes` | `2097152` / `1048576` / `24000` / `262144` | 单次响应、解码内容、模型内容和托管轮次缓冲区的字节上限。 |
| `[web].max_fetches_per_run` / `.max_client_searches_per_run` / `.max_hosted_enabled_provider_requests_per_run` / `.provider_hosted_max_uses_per_request` | `5` / `3` / `4` / `3` | 每次运行允许的抓取、客户端搜索和模型服务托管搜索次数。 |
| `[web].max_network_attempts_per_run` / `.max_concurrent_requests` / `.per_host_rate_limit_per_minute` | `12` / `2` / `10` | 每次运行的网络尝试次数、并发请求数，以及单个主机每分钟的请求上限。 |
| `[web].max_total_wire_bytes_per_run` / `.max_total_decoded_bytes_per_run` / `.max_total_model_bytes_per_run` | `8388608` / `4194304` / `98304` | 每次运行的网络响应、解码内容和模型内容累计字节上限。 |
| `[web].allowed_ports` | `[80, 443]` | 允许的目标端口。 |
| `[web].allowed_domains` / `.blocked_domains` / `.allowed_private_hosts` / `.allowed_private_cidrs` | `[]` | 可选的目标列表；私有网络目标必须显式匹配。 |
| `[memory].enabled` | `true` | 加载工作区指令文件。 |
| `[skills].enabled` / `.user_skills` / `.user_agents` | `true` | 开启发现到的可复用资源。 |
| `[skills].compatibility_sources` | `[]` | 可选 `claude` 或 `reasonix` 导入。 |
| `[compaction].enabled` | `true` | 开启对话上下文精简。 |
| `[compaction].soft_threshold_ratio` / `.hard_threshold_ratio` | `0.5` / `0.8` | 提醒阈值和有限的空闲自动处理阈值；自动应用前仍要求本地检查结果为 `ready`。 |
| `[compaction].fallback_context_window_tokens` | 未设置 | 无法获知模型上下文窗口时使用的备用值。 |
| `[compaction].tail_messages` | `6` | 原样保留的最近消息数。 |

## 代码智能、终端、插件与 MCP

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[code_intelligence].enabled` | `false` | 开启代码导航和经过审查的编辑建议。 |
| `[code_intelligence].server_startup` | `"lazy"` | 已配置的语言服务器何时启动。 |
| `[code_intelligence].default_timeout_ms` | `5000` | 单次请求超时。 |
| `[code_intelligence].max_results` / `.max_payload_bytes` | `100` / `65536` | 结果限制。 |
| `[code_intelligence].auto_discover` / `.report_missing` | `true` | 是否自动发现语言服务器，并报告尚未就绪的状态。 |
| `[[code_intelligence.servers]].name` / `.command` | 显式配置服务时必填 | 语言服务器的标识与启动命令。 |
| `[[code_intelligence.servers]].languages` | `[]` | 可选的语言标识。 |
| `[[code_intelligence.servers]].args` / `.env` / `.initialization_options` | `[]` / `{}` / `{}` | 语言服务器参数、显式环境变量和初始化数据。 |
| `[[code_intelligence.servers]].root_markers` / `.file_extensions` | `[]` | 用于匹配工作区和文件的标记。 |
| `[[code_intelligence.servers]].startup_timeout_ms` | `10000` | 启动超时。 |
| `[[code_intelligence.servers]].trust_required` | `true` | 启动前需要工作区信任决定与配置相符。 |
| `[terminal].keyboard_enhancement` | `"auto"` | `auto`、`on` 或 `off`。 |
| `[terminal].mouse_capture` / `.osc52_clipboard` | `true` | 鼠标与 OSC52 剪贴板行为。 |
| `[terminal].scroll_sensitivity` | `3` | 每次滚轮滚动的行数。 |
| `[terminal.notifications].enabled` | `false` | 在交互式 TUI 中启用通知；通知内容受明确的隐私边界限制。 |
| `[terminal.notifications].method` | `"auto"` | `auto`、`osc9`、`osc777` 或 `bell`。 |
| `[terminal.notifications].minimum_run_duration_ms` | `10000` | 长任务完成阈值，范围为 `1000` 到 `3600000`。 |
| `[[mcp_servers]].name` / `.transport` | 必填 | 稳定的服务名，以及明确的 `stdio` 或 `streamable_http` 传输方式。 |
| `[[mcp_servers]].command` / `.args` / `.inherit_env` | stdio 必填 / `[]` / `[]` | 本机命令、参数，以及仅用户级配置可用的继承环境变量名。 |
| `[[mcp_servers]].url` | HTTP 时必填 | HTTP(S) 地址；使用环境变量请求头、Bearer 令牌或 OAuth 凭据时必须为 HTTPS。 |
| `[[mcp_servers]].http_headers` / `.env_http_headers` | `{}` | 静态公开请求头，或请求头与环境变量名的对应关系。敏感值继续保存在环境变量中。 |
| `[[mcp_servers]].bearer_token_env_var` | 未设置 | 保存静态 Bearer 令牌的环境变量；不能与 OAuth 同时使用。 |
| `[[mcp_servers]].client_capabilities` | `[]` | 可选的远端服务能力：`roots` 与 `elicitation`。 |
| `[mcp_servers.oauth].client_id` | 未设置 | 可选的公开客户端 ID；服务支持动态注册时可以省略。 |
| `[mcp_servers.oauth].scopes` | `[]` | 可选的授权范围。OAuth 要求 HTTPS，且不能与静态 Bearer 或 `Authorization` 凭据同时使用。 |
| `[[mcp_servers]].startup_timeout_secs` / `.required` / `.startup` | `10` / `true` / `"eager"` | 启动时限、严格启动要求，以及 `eager` 或 `lazy` 启动。 |
| `[mcp_servers.trust].trust_class` / `.approval_default` | `"self_hosted"` / `"ask"` | 信任分类与默认审批行为。 |
| `[mcp_servers.trust].egress_logging` / `.allow_secrets` / `.pin_version` | `true` / `false` / `false` | 出站日志、敏感凭据访问与服务版本固定策略。 |
| `[mcp_servers.trust.pinned].transport_fingerprint` / `.protocol_version` / `.server_name` / `.server_version` | 开启固定版本时必填 | `pin_version = true` 时预期的服务身份。 |

设置示例见[高级配置](advanced-configuration.md)和[MCP 指南](mcp.md)。

<!-- public-doc-cta: return-configuration -->
下一步：[返回配置指南](configuration.md)。
