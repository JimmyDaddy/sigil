<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# 高级配置

[文档首页](README.md) · [配置](configuration.md) · [权限](permissions-and-sandbox.md) · [字段参考](configuration-reference.md) · [English](../en/advanced-configuration.md)

普通 setup 已经工作后再使用这些设置。一次只修改一个区域；结果不清楚时运行 `sigil doctor`。

## 任务规划

<!-- public-doc-topic: task -->

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

普通输入保持 chat-first。只读计划使用 `/plan`，多步骤执行使用 `/task`。保守 agent mode 只会在你或 workspace 指令要求 delegation 时使用 child agent。Role 专项 model 与 tool 限制见[配置字段参考](configuration-reference.md#task)。

## 验证

```toml
[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

只添加你理解的检查。仓库提示可以被建议，但不会仅因存在而运行。会修改相关文件的检查必须再由不写入的检查跟进，结果才是当前的。

## Memory、Skills 与 Agents

<!-- public-doc-topic: memory -->

`[memory].enabled = true` 允许 Sigil 加载 `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` 等 workspace 指令文件。保持内容简短、最新，并适合仓库中的每个 session。

<!-- public-doc-topic: skills-agents -->

可复用 workspace skill、command、agent 和 plugin 分别位于 `.sigil/skills`、`.sigil/commands`、`.sigil/agents` 和 `.sigil/plugins`。用户资源和兼容导入由 `[skills]` 控制。允许导入指令工作前请先检查。

## Compaction 与代码智能

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
tail_messages = 6
```

Compaction 会在预览显示 ready 时精简较早的对话上下文。手动入口是 `/compact`。Model window 未知时设置 `fallback_context_window_tokens`；失败不会改变活动对话。

<!-- public-doc-topic: code-intelligence -->

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
auto_discover = true
```

启用后，Sigil 可以使用已安装 language server 提供导航、诊断和经过检查的编辑。`Alt-D` 检查已修改源码。缺少 language server 不会阻止普通 chat 或文件工具。

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

终端、远程层或 multiplexer 不支持某项能力时，将其关闭。Notification 默认关闭，并使用不含 prompt、路径、工具详情、provider、model 或 session id 的固定文本。用[终端兼容性](terminal-compatibility.md)测试结果。

<!-- public-doc-topic: model-request-env -->

`SIGIL_MODEL_REQUEST_TIMEOUT_SECS`、`SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 和 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 可以临时覆盖共享 model-request timeout。Provider 凭据与 endpoint 设置仍留在 provider 页面。

## Plugins 与 MCP

<!-- public-doc-topic: plugins -->

Plugin 从 `.sigil/plugins/<id>/plugin.toml` 发现，并在 `/config` 中 review。Plugin 改变后要重新检查再允许运行。Plugin entry 不能请求继承 credential variable。

<!-- public-doc-topic: mcp -->

使用 `[[mcp_servers]]` 配置 MCP。Local server 会从清空的环境启动；只通过用户根配置中的 `inherit_env` 授予必要变量名。远端认证、trust 与兼容性见 [MCP 指南](mcp.md)，精确字段见[配置字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
