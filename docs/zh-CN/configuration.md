<!-- public-doc-role: configuration; authority: configuration-router; sections: choose-the-right-page,resolution-order,minimal-path,workspace,storage-and-session-paths,task-rollout-and-defaults,use-doctor-when-setup-looks-wrong; cta: open-configuration-reference -->

# Sigil 配置指南

[文档首页](README.md) · [权限](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [English](../en/configuration.md)

常规配置从这里开始。模型服务凭据和各服务的专用设置统一在[模型服务指南](providers.md)维护。

## 选择正确的页面

| 目标 | 页面 |
| --- | --- |
| 找到配置、选择工作区或设置存储位置 | 本指南 |
| 修改审批、网络、外部路径或沙箱 | [权限与沙箱](permissions-and-sandbox.md) |
| 修改主题、代码配色或信息栏 | [外观](appearance.md) |
| 配置任务、验证、记忆、子智能体、上下文、终端、插件或 MCP | [高级配置](advanced-configuration.md) |
| 查询精确字段或默认值 | [配置字段参考](configuration-reference.md) |

## 解析顺序

提供 `--config <path>` 时，Sigil 加载该文件；否则使用用户配置：

```text
~/.sigil/sigil.toml
```

快速设置会写入用户配置。工作区中的 `sigil.toml` 不会自动加载；只有明确需要时，才通过参数指定该文件。

## 最小配置路径

进入仓库并运行 `sigil`。快速设置会处理工作区、模型服务、具体模型与认证，保存时也会显式信任启动目录。Provider 是快速设置的第一个决定；在 `/config` 中，对 **Connection** 按 Enter 会打开明确的已保存连接/Provider 模板选择器，`A` 会开始新增 Provider。最小的手写配置如下：

```toml
config_version = 2

[composition]
profile = "core"
enhancements = []

[workspace]
root = "."

[agent]
connection = "my-connection"
model = "my-model"
tool_timeout_secs = 30

[model_request]
# 可选；省略后由所选 Provider 自动决定输出预算。
max_output_tokens = 8192

[appearance]
info_rail = true
theme = "sigil_dark"
```

然后从所选模型服务的页面加入匹配的 `[connections.my-connection]` 区块。可以直接复制的起点
位于 [`docs/examples/config`](../examples/config)。Sigil 只接受当前 schema；旧配置需要直接
替换为当前模板。在 TUI 中，通过 `/config` 选择 connection 或模型并保存时，当前对话会保持打开，
下一轮切换到该 route，同时把同一 route 保存为未来会话的默认值。如果字段校验或文件发布失败，
配置面板会保留未保存草稿并持续显示 **保存失败**；能够识别具体字段时还会自动聚焦该字段。再次编辑
会清除已经过期的错误状态，让下一次保存结果保持明确。

如果当前配置有效，但后续 authority 启动失败，设置页会保留具体启动错误，并默认选中
**Retry Start**。修复所显示的启动问题后直接重试即可，无需重复保存连接或重新输入凭据。

默认不限制模型轮数。需要为每次 agent run 设置可选上限时，在配置文件已有的 `[agent]`
区块中加入正整数 `max_turns`，然后重启 Sigil：

```toml
[agent]
max_turns = 100
```

它累计本次运行的模型总轮数，包含请求工具的轮次；同一轮可以请求多个工具。省略 `max_turns`
即可取消轮数上限；不要用 `0` 表示不限，它会立即停止。达到上限时，Sigil 返回部分结果并保留对话，之后可以发送新消息继续。

`[model_request].max_output_tokens` 是普通 agent run 的 provider-neutral 可选默认值。省略时保留
所选 Provider 自动决定的输出预算。在 TUI `/config` 中，**Max output tokens** 接受正整数或 `8K`、
`1M` 这类 K/M 后缀；留空即为 Automatic。显式指定的单次运行约束优先于此默认值。
当所选模型的上下文窗口已知时，Sigil 会额外预留 8,192 token 用于输入封装和 provider framing；
如果输出上限会把输入预算挤没，保存前就会拒绝。留空时，provider 的自动输出上限也会自动收敛到
仍能容纳输入的范围。

Connection ID 用于标识保存的 route，但本身不等于 trust 授权。只修正同一 endpoint origin 内的路径时，
恢复的 session 可以自动 rebind；origin、Provider 协议或账户/tenant 边界变化时，发送任何历史前都必须
经过精确的用户确认或选择 replacement route。

核心组合保留模型对话、文件读写与搜索、短命令、审批、取消和持久会话。快速设置为新配置选择
`profile = "core"`。需要增强能力时，在 `[composition].enhancements` 中逐项加入，例如
`["skills", "memory"]`，并配置相应模块。`profile = "standard"` 选择所有可选模块，再由各模块的
`enabled` 设置决定是否启用；手写配置省略 `[composition]` 时使用 `standard`。

未选择的模块不会装配运行组件，其配置内容会保留到下次保存，并在选择该模块后才校验。
能力组合在会话首次运行时固定；修改组合后请重新启动并创建新会话。缺少组合记录的旧执行会话不能继续，
已有文件不会自动删除或迁移。完整能力名称见[字段参考](configuration-reference.md)。

## 工作区

`workspace.root = "."` 表示使用 `sigil` 的启动目录。文件工具会留在这个工作区内；只有明确配置了范围足够小的外部目录规则时才例外。修改前请阅读[权限与沙箱](permissions-and-sandbox.md)。

Shell 选择和终端行为见[终端兼容性](terminal-compatibility.md)；可移植读写优先使用文件工具。

## 存储与会话路径

`[storage].state_root` 存放用户会话和变更记录；`[storage].cache_root` 存放可以重建的数据。`SIGIL_STATE_HOME` 和 `SIGIL_CACHE_HOME` 会覆盖对应的根目录。`[session].log_dir` 只改变当前工作区的会话日志位置。

保留期限只会在 `/config` → **Storage** 中经过预览和确认后应用。普通启动、恢复、运行和 `sigil serve` 不会自动删除会话。见[管理已保存的会话](user-guide.md#管理已保存的会话)。

## Task Rollout 与默认值

选择 Task 增强能力后，配置默认使用 `routing_policy = "auto"` 与 `multi_agent_mode = "explicit_request_only"`。模型依据实际 provider 与执行器能力选择 Chat、PlanReview 或 Task；显式 `manual` 关闭自动交接。

Quick Setup 只有在所选 provider、model、官方 endpoint、task config 与 build 匹配 qualified release manifest 时才保存 `auto + proactive`。该证据影响安装默认值；证据缺失或无效不阻断已配置的 Task 执行器。已有配置不会被重写。`sigil doctor` 分别报告配置和发布评测；设置 `routing_policy = "manual"` 会保留 Task 与 agent 历史。


## 设置异常时使用 Doctor

运行 `sigil doctor`，或在 TUI 中运行 `/doctor`。它会检查配置、工作区、会话位置、模型服务凭据来源、MCP、代码智能和终端支持，但不会打印密钥内容。使用备用配置时，请带上相同的 `--config <path>` 参数。

接下来按需要进入[权限](permissions-and-sandbox.md)、[外观](appearance.md)、[高级配置](advanced-configuration.md)或[字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
