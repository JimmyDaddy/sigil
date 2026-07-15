# 高级配置

[文档首页](README.md) · [配置指南](configuration.md) · [权限与沙箱](permissions-and-sandbox.md) · [外观](appearance.md) · [字段参考](configuration-reference.md) · [English](../en/advanced-configuration.md)

当普通设置和 `/config` 不够用时，再使用本文。始终从[配置指南](configuration.md)开始，一次只修改一个聚焦的设置。

## 任务规划

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

普通 composer 输入保持 chat-first。多步骤工作使用 `/task <目标>`，用 `/task continue` 继续；如果想先做只读规划，使用 `/plan <目标>`，并在明确决定后从 plan 创建任务。

`max_subagents` 限制活跃 child agent 数量。默认的 `multi_agent_mode = "explicit_request_only"` 更保守：只有你或 workspace 指令明确要求 delegation 时，Sigil 才会使用 child agent。设置 `none` 可关闭普通 delegation 提示；只有独立的并行工作确实合适时才使用 `proactive`。涉及文件修改的 child work 仍会经过正常审查和批准。

可以为 planner、executor 或 child role 指定不同 model，或收窄可见工具；只有能明确解释为什么某个 role 应比主会话更受限时才建议这样做。精确字段见[配置字段参考](configuration-reference.md#task)。

## 验证

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

配置中的检查是你为 workspace 明确认可的检查。仓库提示可以被建议，但不会仅因存在而自动运行。普通测试、构建和 lint 使用 `read_only`；会修改相关文件的检查必须在之后由不写文件的检查跟随，结果才是当前的。

## Memory、Skills 与 Agents

```toml
[memory]
enabled = true

[skills]
enabled = true
user_skills = true
user_agents = true
compatibility_sources = []
```

开启 memory 后，Sigil 可加载 `SIGIL.md`、`AGENTS.md`、`CLAUDE.md` 和 `SIGIL.local.md` 等 workspace 指令文件。请保持仓库指令简洁、最新，并适用于每次打开该 workspace 的会话。

workspace 资源固定在 `.sigil/` 下：可复用 skills 在 `.sigil/skills`，slash commands 在 `.sigil/commands`，agent profiles 在 `.sigil/agents`，plugin manifests 在 `.sigil/plugins`。兼容来源需要显式启用。允许任何导入的 skill、agent 或 plugin 操作 workspace 前，请先完成审查。

## Compaction 与代码智能

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

compaction 管理长对话。manual、完全 idle 的 hard-threshold 与 queued pre-turn apply 仍由本地 exact target proof 门控；窄 OpenAI Responses overflow route 使用独立的受审计 server-count proof。不受支持的 profile 会 fail closed，不改变 active boundary。代码智能默认关闭。开启后，它可使用已安装的 language server，提供代码导航、诊断和经过审查的编辑建议。开启它不会绕过 workspace trust、文件批准或 diff review。

在 TUI 中用 `Alt-D` 查看 changed source files 的 diagnostics。即使缺少 language server，普通 chat 和文件工具仍可使用。

## 终端与模型请求环境变量覆盖

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

如果终端或 multiplexer 无法处理增强按键，将 `keyboard_enhancement` 设为 `off`。如果 mouse mode 与终端冲突，将 `mouse_capture` 设为 `false`。如果终端阻止剪贴板序列，将 `osc52_clipboard` 设为 `false`。[终端兼容性检查清单](terminal-compatibility.md)提供人工 checklist。

`SIGIL_MODEL_REQUEST_TIMEOUT_SECS`、`SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 与 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 可临时覆盖共享模型请求超时。Provider 凭据与 endpoint 选项仍在 [Provider 指南](providers.md)及其专页维护。

## Plugins 与 MCP

Plugins 从 `.sigil/plugins/<id>/plugin.toml` 发现，并在 `/config` 中审查。plugin 改变后，请再次审查再允许它运行。plugin 条目不能请求继承环境凭据；有凭据的本地 MCP server 应配置在用户配置中。

本地 MCP server 使用 `[[mcp_servers]]` 配置，启动时会清空环境。如果 server 需要凭据，请只通过仅允许写在用户根配置中的 `inherit_env = ["ENV_NAME"]` 授予必要的变量名。`/doctor` 和 `/config` 会显示 grant 是否可用，但不展示值。

服务器设置和信任决策见[MCP 指南](mcp.md)，完整高级字段见[配置字段参考](configuration-reference.md)。
