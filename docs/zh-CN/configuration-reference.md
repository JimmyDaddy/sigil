# 配置字段参考

[文档首页](README.md) · [配置指南](configuration.md) · [权限与沙箱](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [English](../en/configuration-reference.md)

本文是公开 `sigil.toml` 表面的查阅参考。选择行为时请先阅读对应指南；在确认字段名、可接受的值或默认值时再回到本文。

## Workspace、Storage 与 Session

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[workspace].root` | 手写配置时必填 | workspace 目录。`"."` 跟随启动 `sigil` 的目录。 |
| `[storage].state_root` | `"auto"` | 每用户的持久 Sigil state。`SIGIL_STATE_HOME` 可覆盖。 |
| `[storage].cache_root` | `"auto"` | 可重建的每用户 cache。`SIGIL_CACHE_HOME` 可覆盖。 |
| `[session].log_dir` | workspace state 下的 `sessions` | session log 位置。相对值在 workspace state 下解析。 |
| `[session.retention].max_sessions` | `500` | 显式 cleanup 后最多保留的 ready session 数。 |
| `[session.retention].max_bytes` | `2147483648` | 显式 cleanup 后 ready session 的最大总字节数。 |
| `[session.retention].expire_older_than_ms` | `15552000000` | 显式 cleanup 时选择早于 180 天且未受保护的 session。 |

路径选择与 retention 只允许显式执行的边界见[配置指南](configuration.md#storage-与-session-路径)。

## Agent 与 Providers

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[agent].provider` | setup 选择 | `deepseek`、`openai_compat`、`openai_responses`、`anthropic` 或 `gemini`。 |
| `[agent].model` | provider setup 选择 | 默认 chat model。 |
| `[agent].tool_timeout_secs` | `30` | 工具超时秒数。 |
| `[agent].max_turns` | 禁用 | 未收敛工具循环的可选上限。 |

Provider 区块与凭据见[Provider 指南](providers.md)。

## Execution

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[execution].strategy` | `"local"` | `local` 或 `sandbox`。 |
| `[execution.sandbox].backend` | 使用 `sandbox` 时必填 | `macos_seatbelt`、`linux_bubblewrap` 或 `docker`。 |
| `[execution.sandbox].profile` | 取决于 backend | 请求的 sandbox profile。 |
| `[execution.sandbox].fallback` | 推荐 `"deny"` | 所选 sandbox 不可用时的行为。 |
| `[execution.sandbox].container_image` | 仅 Docker 必填 | Docker backend 使用的 image。 |

修改前请阅读[权限与沙箱](permissions-and-sandbox.md#沙箱的实际含义)。

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
| `[[verification.checks]].effect` | 必填 | 预期影响；普通检查使用 `read_only`。 |

## Appearance

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
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
| `[permission.external_directory].enabled` | `false` | 开启对 workspace 外路径的考虑。 |
| `[permission.external_directory].default_mode` | `"ask"` | 已启用外部路径的默认动作。 |
| `[permission.external_directory].rules` | `[]` | 收窄的外部路径规则。 |

有效安全行为见[权限与沙箱](permissions-and-sandbox.md)。

## Web、Memory、Skills 与 Compaction

| 区块 / 字段 | 默认值 | 用途 |
| --- | --- | --- |
| `[web].enabled` | `true` | 开启已配置的 Web 工具。 |
| `[web].network_mode` | `"allow"` | `allow`、`ask` 或 `deny`。 |
| `[web].search_route` | `"auto"` | `auto`、`provider_hosted`、`mcp`、`bundled` 或 `disabled`。 |
| `[web].max_results` | `8` | 搜索结果上限。 |
| `[web].max_query_chars` / `.max_query_bytes` | `512` / `2048` | 查询限制。 |
| `[web.bundled_search].enabled` | `true` | 开启 bundled search route。 |
| `[web.search_mcp].server` / `.tool` | 未设置 | 你的兼容 MCP search binding。 |
| `[memory].enabled` | `true` | 加载 workspace 指令文件。 |
| `[skills].enabled` / `.user_skills` / `.user_agents` | `true` | 开启发现到的可复用资源。 |
| `[skills].compatibility_sources` | `[]` | 可选 `claude` 或 `reasonix` 导入。 |
| `[compaction].enabled` | `true` | 开启对话 compaction。 |
| `[compaction].soft_threshold_ratio` / `.hard_threshold_ratio` | `0.5` / `0.8` | 提醒与有限 idle 自动阈值；自动应用仍要求本地 target admission。 |
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
| `[[code_intelligence.servers]].name` / `.languages` / `.command` | 显式 server 时必填 | Language server 标识与命令。 |
| `[[code_intelligence.servers]].root_markers` / `.file_extensions` | `[]` | Workspace 与文件匹配。 |
| `[[code_intelligence.servers]].startup_timeout_ms` | `5000` | 启动超时。 |
| `[[code_intelligence.servers]].trust_required` | `true` | 需要匹配 workspace trust decision。 |
| `[terminal].keyboard_enhancement` | `"auto"` | `auto`、`on` 或 `off`。 |
| `[terminal].mouse_capture` / `.osc52_clipboard` | `true` | 鼠标与 OSC52 剪贴板行为。 |
| `[terminal].scroll_sensitivity` | `3` | 每次滚轮滚动的行数。 |
| `[terminal.notifications].enabled` | `false` | 在交互式 TUI 中启用有明确隐私边界的 attention signal。 |
| `[terminal.notifications].method` | `"auto"` | `auto`、`osc9`、`osc777` 或 `bell`。 |
| `[terminal.notifications].minimum_run_duration_ms` | `10000` | 长任务完成阈值，范围为 `1000` 到 `3600000`。 |
| `[[mcp_servers]].inherit_env` | `[]` | 传递给本地 MCP server 的仅根配置凭据名列表。 |

设置示例见[高级配置](advanced-configuration.md)和[MCP 指南](mcp.md)。
