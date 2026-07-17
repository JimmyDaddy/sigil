<!-- public-doc-role: permissions-and-sandbox; authority: permission-network-sandbox-authority; sections: choose-a-permission-mode,review-before-an-action-runs,narrow-command-and-path-rules,network-and-web-tools,sandbox-expectations; cta: review-safety -->

# 权限与沙箱

[文档首页](README.md) · [配置](configuration.md) · [安全](safety.md) · [隐私](privacy.md) · [English](../en/permissions-and-sandbox.md)

本页是本机权限、外部路径、网络访问与沙箱预期的操作权威页。

## 选择 Permission Mode

```toml
[permission]
mode = "manual"
```

| Mode | 用途 | 默认行为 |
| --- | --- | --- |
| `read-only` | 探索与评审 | 允许 workspace 读取和可识别的只读命令；拒绝写入以及会变更状态或无法分类的命令。网络仍遵守独立策略。 |
| `manual` | 常规交互工作 | 读取可继续；变更和命令通常需要询问。 |
| `auto-edit` | 有监督的文件编辑 | Workspace 编辑可以继续；命令通常仍需询问。 |
| `danger-full-access` | 严密监督的自动化 | 本机访问较宽，但网络、受保护路径和其他硬限制仍然生效。 |

建议从 `manual` 开始。精确 deny 始终比宽泛 mode 更严格。

## 动作运行前检查

作出决定前，检查摘要、路径或目标、命令与 diff。Plan 或早先审批不代表另一个动作已获许可。Headless `sigil run` 不能打开审批浮层；仍为 `ask` 的动作会失败。

## 收窄命令与路径规则

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

优先使用少量窄规则。多个规则同时匹配时，deny 优先于 ask，ask 优先于 allow。

<!-- public-doc-topic: external-directory -->

Workspace 外路径默认关闭：

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

启用该 section 不代表所有外部路径都安全或可访问；每条路径仍遵守自身 rule 和受保护路径检查。命令临时文件优先使用 `$SIGIL_SCRATCH_DIR`。

## 网络与 Web 工具

<!-- public-doc-topic: network-control -->

网络策略与本机 permission mode 相互独立：

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"
```

`allow` 允许受支持的只读 search 与 fetch 继续，但仍执行目标检查和限制。`ask` 提供单次或同工具 session 决定。`deny` 关闭 Web 访问。Session 决定不会授权另一个工具、写入型请求或已拒绝目标。选择第三方 route 或发送敏感查询前请阅读[隐私](privacy.md)。

远端 MCP 与 MCP OAuth 也遵守这条独立网络边界。`auto-edit` 不会静默授权 OAuth discovery、token exchange、refresh 或 revoke。一次登录可能同时访问 MCP resource 与另一个 authorization server，因此 Sigil 可能展示多个目标提示。Session approval 不会暴露 token 值、授权另一类请求或绕过目标检查。

## 沙箱预期

<!-- public-doc-topic: sandbox-limit -->

Permission 决定 Sigil 是否可以尝试动作；sandbox 是之后可选应用的操作系统边界。默认 local strategy 不是 OS sandbox，也不保证文件系统、网络、凭据或进程隔离。

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt" # 或 linux_bubblewrap / docker
profile = "workspace_write"
fallback = "deny"
```

可用性和保护取决于 host、backend、profile 与动作类型。Sandboxed command 不会让远端服务、MCP server、plugin、container 或所有进程路径自动安全。`fallback = "deny"` 会在 backend 不可用时停止动作，而不是静默改用 local。修改 execution 后运行 `sigil doctor`。

Verification command 有独立行为声明和审批要求。设置见[高级配置](advanced-configuration.md#验证)，字段默认值见[配置字段参考](configuration-reference.md#permission)。

<!-- public-doc-cta: review-safety -->
下一步：[查看安全决策清单](safety.md)。
