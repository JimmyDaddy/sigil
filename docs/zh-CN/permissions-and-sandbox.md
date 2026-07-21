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
| `auto-edit` | 有监督的文件编辑 | 工作区内的文件编辑可以继续；命令通常仍需询问。 |
| `danger-full-access` | 严密监督的自动化 | 本机访问较宽，但网络、受保护路径和其他硬限制仍然生效。 |

建议从 `manual` 开始。精确的拒绝规则始终比宽泛的模式设置更严格。

## 动作运行前检查

作出决定前，请检查摘要、路径或目标、命令和文件差异。计划或之前的审批不代表另一个操作已经获准。非交互式 `sigil run` 无法打开审批弹窗；仍处于 `ask` 状态的操作会失败。

交互式审批界面会展示经过安全投影的命令或工具输入，并在作出决定后更新已记录卡片。只有策略能够为同类请求推导出有边界的授权时，才会显示 **本次会话允许**；它不会授权无关命令、其他目标或不同风险类别。能够识别的只读 shell 结构可以按读取操作处理；会改变状态或无法分类的 shell 语法仍遵守已配置的命令策略。

## 收窄命令与路径规则

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

优先使用少量、范围明确的规则。多个规则同时匹配时，`deny` 优先于 `ask`，`ask` 优先于 `allow`。

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

可用性和保护范围取决于宿主系统、执行后端、沙箱配置和操作类型。在沙箱中运行一条命令，并不会自动保证远端服务、MCP 服务端、插件、容器或所有进程路径都安全。`fallback = "deny"` 会在后端不可用时停止操作，而不是悄悄改成本机直接执行。修改执行设置后，请运行 `sigil doctor`。

验证命令有独立的行为声明和审批要求。设置见[高级配置](advanced-configuration.md#验证)，字段默认值见[配置字段参考](configuration-reference.md#权限)。

<!-- public-doc-cta: review-safety -->
下一步：[查看安全决策清单](safety.md)。
