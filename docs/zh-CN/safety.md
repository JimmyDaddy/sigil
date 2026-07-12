# 安全与权限

[文档首页](README.md) · [配置](configuration.md) · [排障](troubleshooting.md) · [English](../en/safety.md)

Sigil 的设计目标是让 tool-backed coding 可见、可 review。模型可以提出读取、搜索、编辑、shell 命令、MCP 调用、code-intelligence action 和计划任务步骤；Sigil 决定这些动作能直接执行、需要审批，还是必须拒绝。

## 最短说明

- 仓库内只读检查通常可以直接运行。
- 文件写入、编辑、删除、shell 执行、external directory、MCP tools 和 LSP edit tools 可以要求审批。
- Approval card 会在动作运行前展示即将发生什么。
- Headless `sigil run` 不能交互询问；最终 `ask` 决策会变成结构化 `approval_required` tool error。
- Session 和 control records 是 append-only，后续恢复和审计可以解释发生了什么。

## Permission Modes

Sigil 的 permission layer 常见结果：

| Outcome | 含义 |
| --- | --- |
| `allow` | tool call 不弹 approval modal，直接运行。 |
| `ask` | TUI 展示 approval card。 |
| `deny` | tool call 被拒绝，模型收到结构化 denial。 |

推荐默认配置：

```toml
[permission]
mode = "manual"
```

这让普通仓库检查可以继续，同时保留对修改性或高风险动作的 review。

## 通常不需要审批的动作

当只读工具留在 workspace 内时，通常可以直接运行：

- list files；
- read files；
- search text；
- code intelligence 启用时的 symbol 或 diagnostic 检查；
- MCP resources/prompts list，但前提是 trust 和 approval policy 允许。

具体行为仍由 config、tool category、trust class 和 permission rules 决定。

## 通常需要 review 的动作

这些动作应预期出现 approval card：

- 写入、编辑或删除文件；
- 运行非简单可信读取的 shell command；
- 访问 workspace 外路径；
- 运行外部 MCP tool；
- 接受 MCP elicitation request；
- 应用 LSP code action 或 rename edit；
- 配置 trust policy 要求 `ask` 的任何操作。

## 如何阅读 Approval Card

允许工具前检查：

1. Summary：工具即将执行的动作。
2. Subject：涉及的文件路径、命令、MCP server 或外部资源。
3. Files：受影响文件。
4. Diff：新增、删除或修改的行。
5. Trust context：尤其是 MCP server trust class 和 secret-egress 行为。
6. Action：只有摘要和 diff 符合你的意图时才 allow。

如果 diff 太大，deny 并要求 Sigil 拆小改动。

## Workspace Confinement

文件工具会限制在已解析的 workspace root 内，并拒绝：

- workspace 外绝对路径；
- 使用 `..` 逃逸 workspace 的路径；
- 解析后指向 workspace 外的 symlink。

常规配置：

```toml
[workspace]
root = "."
```

`.` 会解析成启动 `sigil` 时所在目录。

## External Directories

External-directory access 默认关闭：

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

只在明确需要时启用，并保持 `external_directory.default_mode = "ask"`，除非外部路径低风险且稳定。临时 shell scratch 文件应使用 `bash` 或 `terminal_start` 提供的 `$SIGIL_SCRATCH_DIR`；系统 temp 目录（如 `/tmp`、macOS `/private/tmp`、Windows `%TEMP%`）仍需要 external-directory 权限。

## Shell Commands

默认情况下，`bash` 使用本地执行后端，不提供 OS sandbox。Sigil 保守处理 shell execution：

- 简单 read-like command 只有匹配安全模式时才可能允许；
- 写入、重定向、包管理器、网络访问、未知命令、变量或复杂 shell syntax 应保持可 review；
- command output 进入模型前会有边界限制。

审批前检查 command、working directory 和预期副作用。

macOS 上可以在 `~/.sigil/sigil.toml` 中为非交互命令显式启用 `macos_seatbelt` backend：

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
```

这个 backend 会通过 `/usr/bin/sandbox-exec` 运行命令，允许读取文件系统、限制写入到命令工作目录，并且不在 sandbox profile 中开放网络访问。当前本地 handoff 路径可以在所选 backend 支持时，为非交互 shell、PTY、MCP stdio 和受信任 plugin hook command 记录 sandbox coverage。它不会让远端工具、所有容器/daemon 场景或不支持的平台获得等价 sandbox。

如需让非交互命令通过容器执行，可以显式配置 Docker：

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "docker"
profile = "build_offline"
container_image = "rust:1.94.1"
```

Sigil 不会隐式选择或拉取容器镜像。选择 Docker backend 但未配置 `execution.sandbox.container_image` 时，配置解析、启动和 doctor 检查会 fail closed。Docker backend 会 bind mount 命令工作目录，将 offline profile 映射为 `--network none`，并且只报告它预期能强制的 capability。PTY、MCP、plugin、远端和 daemon-style 路径使用各自的 coverage label；不支持时应 fail closed，而不是静默退回本地执行。

## MCP Trust

MCP server 可以暴露 tools、resources、prompts 和 elicitation requests。每个 server 都应显式配置 trust policy：

```toml
[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

从 `approval_default = "ask"` 和 `allow_secrets = false` 开始。只有确认 server 能读取、写入和发送什么之后，再放宽这些设置。

## Web Search 安全

`websearch` 属于本地 `Read` 加网络 `Read`。Configured 与 bundled client route 会经过普通 tool permission 和 query pre-egress disclosure；remote MCP 的每条 HTTP message 还会单独获得 transport disclosure。Provider-hosted search 按 provider request 独立授权，并在该 run 内隐藏 client `websearch` tool。Configured search binding 一旦被选择就具有权威性：失败时不会静默回退到 bundled Exa。

公网 `NetworkEndpoint` 与 workspace 外文件路径是不同 subject，不进入 `permission.external_directory`。网络 `Allow` 下只读 `websearch` / `webfetch` 不逐次确认；网络 `Ask` 下，`Allow session` 只放宽同一 tool 的 `NetworkEffect::Read`，不会覆盖 source trust、网络写入/Unknown、不同 tool 或 `Deny`。无论是否复用 grant，destination guard、逐消息 disclosure、durable audit 与 budget 都继续执行。

搜索结果必须按不可信输入处理，其中可能包含 prompt injection、恶意 markup、误导性陈述或危险 URL。Sigil 会清洗并限制返回内容，但模型仍应验证重要事实，并且只 fetch 当前 session 已证明的 URL/source capability。搜索摘要已足够时不得默认对多个结果批量 fetch；只有用户明确要求读取页面或缺少一个具体必要事实时才进入 `webfetch`。

## Secrets

Provider 凭据优先使用环境变量。先选择 provider，再使用 [provider 认证映射](providers.md#认证优先级)中的准确变量；不存在对所有 provider 通用的 API key 环境变量。

通过 Quick Setup 或 `/config` 保存 API key 会把它以明文写入 `sigil.toml`。私有本地配置可以这样做，但不要提交真实 secret。

`doctor` 会报告 credential 来源，不会打印 secret 值。

## Recovery And Audit

默认 session 和 control records 是 Sigil 用户态 state 目录下的 append-only JSONL。

用户需要知道：

- 已完成 tool calls 会留在 history；
- started-but-unfinished tools 会恢复为 interrupted；
- 恢复不会静默重放 tools；
- compaction 追加 records，不改写旧 history；
- planned task state 从 durable control records 重建。

## 建议默认值

从这里开始：

```toml
[permission]
mode = "manual"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

MCP：

```toml
[mcp_servers.trust]
approval_default = "ask"
egress_logging = true
allow_secrets = false
```

然后只调整你确实需要的窄行为。
