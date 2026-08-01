<!-- public-doc-role: permissions-and-sandbox; authority: permission-network-sandbox-authority; sections: choose-a-permission-mode,review-before-an-action-runs,narrow-command-and-path-rules,network-and-web-tools,sandbox-expectations; cta: review-safety -->

# 权限与沙箱

[文档首页](README.md) · [配置](configuration.md) · [安全](safety.md) · [隐私](privacy.md) · [English](../en/permissions-and-sandbox.md)

本页说明本机权限、外部路径、网络访问与沙箱的实际边界。需要判断某项操作为什么被允许、询问或拒绝时，请以这里为准。

## 选择权限模式

```toml
[permission]
mode = "manual"
```

| 模式 | 用途 | 默认行为 |
| --- | --- | --- |
| `read-only` | 探索与评审 | 允许读取工作区和执行可识别的只读命令；拒绝写入，以及会改变状态或无法分类的命令。网络仍遵守独立策略。 |
| `manual` | 常规交互工作 | 读取可继续；变更和命令通常需要询问。 |
| `auto-edit` | 有监督的文件编辑 | 工作区内的文件编辑可以继续。只有当前执行后端能够证明所请求的全部隔离能力时，可识别的工作区验证才会直接运行；否则 Sigil 会询问。 |
| `danger-full-access` | 严密监督的自动化 | 本机访问较宽，但网络、受保护路径和其他硬限制仍然生效。 |

建议从 `manual` 开始。精确的拒绝规则始终比宽泛的模式设置更严格。

## 动作运行前检查

作出决定前，请检查摘要、路径或目标、命令和文件差异。计划或之前的审批不代表另一个操作已经获准。非交互式 `sigil run` 无法打开审批弹窗；仍处于 `ask` 状态的操作会失败。

交互式审批界面会展示经过安全投影的命令或工具输入、Sigil 检测到的副作用、受影响目标，以及当前后端无法证明的隔离能力。风险标签只解释操作可能造成什么影响，不会单独决定是否需要审批。因此，同一个中风险操作在隔离能力得到证明时可以自动运行，在隔离不足时则需要询问。

控制路由接受决定后，Desktop 与 TUI 会立即移除决定按钮并显示“正在恢复执行”；后续执行事件到达后才切换为“正在运行”。如果服务端无法确认决定是否已经送达运行实例，界面会显示“投递状态不确定”、禁用重复决定，并通过权威 run snapshot 收敛，而不会把它重新解释为待批准。临时中断后的重试会复用同一个精确 command id 与请求身份；请求过期，或命令、策略、执行配置发生变化时，必须重新检查。

只有 Sigil 能够为等价请求推导出有边界的语义授权时，才会显示 **本次会话允许**。该授权会绑定命令族与参数、目标、副作用上限、工作区、策略版本、执行后端、隔离配置和环境配置。它不会授权任意 Shell 命令、不同的验证步骤、改变后的目标、远端变更、破坏性或动态代码，也不会跨越风险类别。

Sigil 会逐个分析 POSIX 复合 Shell 命令的子命令。`&&`、`||`、`;` 和管道本身不会再把一条可识别的验证链整体变成“未知命令”。重定向、wrapper、危险参数、动态展开和嵌套执行器仍会分别检查；分析不完整或不支持时会 fail closed 为 `ask`（在无交互运行中为 `deny`）。

## 收窄命令与路径规则

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

优先使用少量、范围明确的规则。多个规则同时匹配时，`deny` 优先于 `ask`，`ask` 优先于 `allow`。原始命令 pattern 只表达用户意图，不是沙箱。`allow` pattern 不能覆盖受保护目标、动态或无效的 Shell 分析、无法解析的目标、提权操作或缺失的必要隔离能力。

<!-- public-doc-topic: external-directory -->

工作区外的路径默认不可访问：

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

启用这一配置区块，不代表所有外部路径都安全或可访问；每条路径仍需遵守自己的规则和受保护路径检查。命令需要临时文件时，优先使用 `$SIGIL_SCRATCH_DIR`。

## 网络与 Web 工具

<!-- public-doc-topic: network-control -->

网络策略与本机权限模式相互独立：

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"
```

`allow` 允许受支持的只读搜索和页面抓取继续，但仍会执行目标检查和各项限制。`ask` 可以选择仅允许一次，或在当前会话中允许同一工具。`deny` 会关闭 Web 访问。会话内的决定不会授权另一个工具、写入型请求或已拒绝的目标。选择第三方路由或发送敏感查询前，请阅读[隐私](privacy.md)。

远端 MCP 与 MCP OAuth 也遵守这条独立的网络边界。`auto-edit` 不会擅自授权 OAuth 元数据发现、令牌交换、刷新或撤销。一次登录可能同时访问 MCP 资源和另一个授权服务，因此 Sigil 可能展示多个目标提示。会话内审批不会暴露令牌内容，也不会授权另一类请求或绕过目标检查。

## 沙箱预期

<!-- public-doc-topic: sandbox-limit -->

权限策略决定 Sigil 是否可以尝试某项操作；沙箱是在此之后可选应用的操作系统边界。默认的本机执行方式不是操作系统沙箱，也不保证文件系统、网络、凭据或进程隔离。

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt" # 或 linux_bubblewrap / docker
profile = "workspace_write"
fallback = "deny"
```

可用性和保护范围取决于宿主系统、执行后端、沙箱配置和操作类型。每次自动执行或会话授权都会绑定执行后端实际返回的能力凭据；“请求了隔离”绝不会被当作“已经证明隔离”。尤其是当前 macOS Seatbelt 后端不声明网络隔离，因此 `cargo check`、`cargo test`、`cargo clippy` 等会执行工作区代码的命令仍需明确的一次性或会话授权，除非选择了能证明所需网络禁用的其他后端。在沙箱中运行一条命令，并不会自动保证远端服务、MCP 服务端、插件、容器或所有进程路径都安全。`fallback = "deny"` 会在后端不可用时停止操作，而不是悄悄改成本机直接执行。修改执行设置后，请运行 `sigil doctor`。

有限的检查与构建通过前台 Shell 工具执行，并只产生一个最终结果。常驻服务与交互程序必须使用明确的 terminal task。Terminal task 会主动向 Desktop 与 TUI 发布 readiness、输出 generation、退出、取消和中断变化；agent 需要等待时使用一次事件驱动 wait，不再反复读取日志。日志读取只用于明确的检查操作。

验证命令有独立的行为声明和审批要求。设置见[高级配置](advanced-configuration.md#验证)，字段默认值见[配置字段参考](configuration-reference.md#权限)。

<!-- public-doc-cta: review-safety -->
下一步：[查看安全决策清单](safety.md)。
