<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# 高级配置

[文档首页](README.md) · [配置](configuration.md) · [权限](permissions-and-sandbox.md) · [字段参考](configuration-reference.md) · [English](../en/advanced-configuration.md)

请先完成普通设置并确认 Sigil 可以正常工作，再使用本页选项。一次只修改一个区域；结果不清楚时运行 `sigil doctor`。

## 任务规划

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
max_planning_research_agents = 3
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true
```

`routing_policy` 与输入框的 `default_mode` 是两件事。兼容默认值为 `manual`，因此普通输入仍从对话开始。TUI 设为 `auto` 后，模型可以把复杂普通输入通过 typed handoff 交给 durable planner/executor；简单问题仍直接回答，而且 handoff 不会绕过写文件、shell、网络或 merge 审批。Planner、Executor、Subagent 与最终 Synthesis transcript 均保存在隔离 child session，parent 只保留 bounded result 和一个由 host 提交的正式 final。相互独立且已证明为 shared-read-only 的 Task step 可以并发执行；`max_parallel_read_steps` 与 `max_subagents` 共同限制 fan-out，host 仍按稳定的 plan 顺序向 parent 提交终态结果。写入或带副作用的 Task step 继续串行。Planner 在接受计划前可以请求一次由 host 托管的独立只读 Explore 批次；`max_planning_research_agents` 默认是 `3`、硬上限是 `4`，设为 `0` 可关闭这个 planner-only fan-out。Host 会等待所有 probe 进入终态后自动恢复 Planner，不需要模型轮询命令。HTTP/Desktop application surface 在接入同一 task executor 前仍强制使用 manual routing，避免创建无人执行的 task。只读计划使用 `/plan`，需要确定进入多步骤执行时使用 `/task`；字段完整的 `sigil-plan-v2` DAG 会直接 promotion，不再二次规划。在保守的子智能体模式下，只有你或工作区指令明确要求委派时，Sigil 才会启动子智能体。不同角色使用的模型与工具限制见[配置字段参考](configuration-reference.md#任务)。

## 验证

```toml
[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

只添加你理解的检查。仓库提示可以被建议，但不会仅因存在而运行。会修改相关文件的检查必须再由不写入的检查跟进，结果才是当前的。

## 记忆、技能与子智能体

<!-- public-doc-topic: memory -->

`[memory].enabled = true` 允许 Sigil 加载 `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` 等工作区指令文件。请保持内容简短、及时更新，并确保这些说明适用于仓库中的每个会话。

<!-- public-doc-topic: skills-agents -->

可复用的工作区技能、命令、子智能体和插件分别位于 `.sigil/skills`、`.sigil/commands`、`.sigil/agents` 和 `.sigil/plugins`。用户资源和兼容格式导入由 `[skills]` 控制。允许导入的指令参与工作前，请先检查其内容。

## 上下文精简与代码智能

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
tail_messages = 6
```

当预览显示可以应用时，上下文精简会压缩较早的对话内容。手动入口是 `/compact`。无法确定模型上下文窗口大小时，可以设置 `fallback_context_window_tokens`；精简失败不会改变当前对话。

<!-- public-doc-topic: code-intelligence -->

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
auto_discover = true
```

启用后，Sigil 可以使用已经安装的语言服务器提供代码导航、诊断和经过检查的编辑。按 `Alt-D` 检查已修改源码。缺少语言服务器不会阻止普通对话或文件工具继续工作。

## 终端与模型请求环境变量覆盖

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

终端、远程环境或终端复用器不支持某项能力时，请将其关闭。通知默认关闭，并使用不含提示词、路径、工具详情、模型服务、具体模型或会话 ID 的固定文本。可以按照[终端兼容性](terminal-compatibility.md)检查实际效果。

<!-- public-doc-topic: model-request-env -->

`SIGIL_MODEL_REQUEST_TIMEOUT_SECS`、`SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 和 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 可以临时覆盖共享的模型请求超时。模型服务凭据和端点设置仍放在各服务的专用页面。

## 插件与 MCP

<!-- public-doc-topic: plugins -->

Sigil 会从 `.sigil/plugins/<id>/plugin.toml` 发现插件，并在 `/config` 中等待你检查。插件发生变化后，需要重新检查才能允许运行。插件入口不能请求继承凭据环境变量。

<!-- public-doc-topic: mcp -->

使用 `[[mcp_servers]]` 配置 MCP。本机服务端会从清空的环境启动；只有通过用户级配置中的 `inherit_env`，才能授予确实需要的环境变量。远端认证、信任与兼容性见 [MCP 指南](mcp.md)，精确字段见[配置字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
