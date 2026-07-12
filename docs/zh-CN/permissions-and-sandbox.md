# 权限与沙箱

[文档首页](README.md) · [配置指南](configuration.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [English](../en/permissions-and-sandbox.md)

本文说明 Sigil 在 workspace 中可以做什么、何时先询问，以及审批之后仍然有效的限制。普通设置请从[配置指南](configuration.md)开始；准备修改权限模式或连接网络工具前，再阅读本文。

## 选择权限模式

```toml
[permission]
mode = "manual"
```

| 模式 | 适用场景 | 行为 |
| --- | --- | --- |
| `read-only` | 审查和探索 | 拒绝文件修改与本地命令执行；网络读取仍遵循独立的网络策略。 |
| `manual` | 日常交互工作 | 默认允许读取；文件修改和本地命令会询问，除非有更窄的规则。 |
| `auto-edit` | 快速但可审查的文件编辑 | 允许 workspace 文件编辑；本地命令默认仍会询问。 |
| `danger-full-access` | 受到密切监督的本地自动化 | 广泛允许本地访问；它不能覆盖网络 ask/deny、受保护路径或其他硬限制。 |

推荐默认使用 `manual`。切换模式只改变默认行为，不会覆盖每条具体规则。明确 deny、受保护路径和外部目录 gate 都比宽泛的本地模式更严格。

## 在动作执行前审查

当 Sigil 请求批准时，请检查动作摘要、涉及的路径或命令，以及 diff 预览，再选择 Allow 或 Deny。在非交互 `sigil run` 中，仍需批准的动作会返回“需要批准”的错误，不会静默执行。

请使用 TUI 的正常批准流程。计划、任务描述或此前的一次批准，都不是另一条命令或另一个网络地址的通行证。

## 收窄命令与路径规则

如果有稳定、可重复的需求，可以在 `sigil.toml` 中配置高级规则：

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

命令 pattern 匹配归一化后的命令文本，支持 `*` 和 `?` 通配符。优先写少量具体 allow，不要写过宽的 pattern。一个命令同时匹配多个分组时，deny 比 ask 严格，ask 比 allow 严格。

workspace 外的路径默认禁用。即使启用外部目录，它也不会变成不受限制的区域：每个匹配路径仍会遵循配置的动作和受保护路径规则。命令的临时文件请使用 `$SIGIL_SCRATCH_DIR`；系统临时目录仍属于外部路径，除非你明确允许。

完整字段列表和优先级见[配置字段参考](configuration-reference.md#permission)。

## 网络与 Web 工具

网络访问与本地文件/进程访问分开判断：

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"  # auto | provider_hosted | mcp | bundled | disabled
```

使用 `network_mode = "allow"` 时，只读 web search 和 fetch 不会每次询问，但 Sigil 仍会检查目标地址、记录请求并应用限制。使用 `ask` 时，批准界面提供 Allow once、Allow session 和 Deny。Allow session 只覆盖当前 session 里的同一只读 Web 工具；它不会授权其他工具、网络写入式动作或此前已拒绝的地址。

`deny` 会关闭 Web 访问。bundled search route 会把归一化后的查询发送给其声明的搜索服务；开启第三方工具或凭据前，请阅读[隐私指南](privacy.md)和[MCP 指南](mcp.md)。

## 沙箱的实际含义

权限决定 Sigil 是否可以尝试一项动作。沙箱是操作系统层面的边界，用于在动作获准后限制命令。两者互补，不能互相替代。

```toml
[execution]
strategy = "local"
```

`local` 保留普通本地 shell 行为，不宣称提供操作系统隔离。在受支持系统上，高级用户可以选择 sandbox 策略：

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
profile = "workspace_write"
fallback = "deny"
```

可用 backend 名称为 `macos_seatbelt`、`linux_bubblewrap` 和 `docker`。实际可用性和保证取决于主机、profile 与动作类型。如果要求的沙箱不可用，Sigil 会拒绝执行，而不是假装已隔离。一个受沙箱约束的本地命令，也不会自动让远程服务、容器或每种外部工具安全。

修改 execution 设置后运行 `sigil doctor`。更完整的信任模型见[安全指南](safety.md)，字段见[配置字段参考](configuration-reference.md#execution)。

## 验证检查

验证命令单独配置，因为检查可能只读、可能修改文件，也可能需要批准：

```toml
[verification]

[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

仓库里发现的建议检查不会自动执行；只提升你理解其影响的检查。一条会修改相关文件的检查，需要在之后再跑一次不写文件的检查，结果才能作为最终验证。

工作流说明见[高级配置](advanced-configuration.md#验证)，完整字段见[配置字段参考](configuration-reference.md#verification)。
