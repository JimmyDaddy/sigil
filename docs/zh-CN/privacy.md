# 隐私与数据处理

[文档首页](README.md) · [安全](safety.md) · [配置](configuration.md) · [English](../en/privacy.md)

Sigil 在本地运行，但它可以把 prompt context 和 tool results 发送给配置的模型 provider，也可以调用已配置的 MCP servers。使用 Sigil 处理敏感仓库前，请先理解这些边界。

## 什么可能离开本机

以下情况数据可能离开本机：

- provider request 包含你的 prompt、system context、选中的 session history 和 model-visible tool results；
- 模型请求包含文件片段、搜索结果、diagnostics 或 command output 的 tool results；
- 已审批的 MCP tool/resource/prompt call 被发送给 MCP server；
- 已接受的 MCP elicitation response；
- 你审批的 shell command 通过网络发送数据。

Sigil 不会自动发布仓库数据，但 provider 和 MCP 配置决定了已审批 context 可以发送到哪里。

## 默认留在本地的内容

这些默认在本地：

- 用户配置目录中的 `sigil.toml`；
- Sigil 用户态 state 目录中的 session logs 和 input history；
- Sigil 用户态 state 目录中的 terminal 和 changeset artifacts；
- Sigil 用户态 cache 目录中的 scratch/cache 文件；
- `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` 等 local memory files；
- 本地构建的 release archives 和 checksums；
- doctor output，除非你主动复制到别处。
- `/feedback` 支持报告，除非你主动附加或复制到别处。

## API Keys

优先使用环境变量。先选择 provider，再使用 [provider 认证映射](providers.md#认证优先级)中的准确变量；Sigil 不会让所有 provider 共用一个 API key 环境变量。

如果通过 Quick Setup 或 `/config` 保存 API key，它会以明文保存到用户配置目录中的 `sigil.toml`。不要把包含 secret 的真实 config 复制进仓库。

`sigil doctor` 会报告 key 来源，但不会打印 key 值。

## Session Logs

默认 session 和 control state 是 Sigil 用户态 state 目录下的 append-only JSONL。里面可能包含：

- prompts 和 assistant responses；
- tool call summaries；
- tool result previews；
- approval 和 execution records；
- interrupted tool records；
- compaction records；
- task planning state。

请把 session logs 当作敏感本地 artifacts，分享前先 review。

## MCP And Secret Egress

MCP server 是外部工具 provider。明确配置 trust：

```toml
[mcp_servers.trust]
approval_default = "ask"
egress_logging = true
allow_secrets = false
```

`allow_secrets = false` 时，Sigil 会阻断识别到的 MCP secret-like egress。除非 server 确实需要 secret material 且你信任它，否则保持默认。

## Web Search 数据

Provider-hosted search 由所选模型 provider 生成并执行 query；Sigil 可能只能在执行后收到 provider 上报的 query。Configured MCP search 会把规范化 query 发送到精确 server/tool binding。Bundled anonymous route 会在不使用 Sigil-supplied API key 的情况下发送到 `https://mcp.exa.ai/mcp`。“Anonymous”只描述认证方式：Exa 与网络路径仍可观察 Query Data 以及源 IP/代理出口 IP。截至 2026-07-12，Exa 的公开[隐私政策](https://exa.ai/privacy-policy)说明 Query Data 可能用于改进产品，包括训练和微调其模型；其公开[安全文档](https://exa.ai/docs/reference/security)把 Zero Data Retention 列为企业/定制能力。因此 Sigil 不假设该匿名 route 具有 ZDR，也不承诺 free-tier quota、retention、处理 region、availability 或 SLA。

Client-side query 出站前，Sigil 会阻止已识别的配置 secret，以及高置信 email、phone、SSN 和 payment-card pattern。返回文本按 external/untrusted 处理，并在持久化或进入模型前清洗。可通过 `[web].enabled = false`、`search_route = "disabled"` 或 `network_mode = "deny"` 关闭；使用 `[web.search_mcp]` 选择自有兼容 MCP binding。

## Doctor Output

Doctor 会报告：

- config resolution；
- workspace path；
- session log location；
- provider/auth source；
- MCP command 和 trust state；
- code-intelligence readiness；
- terminal profile 和 compatibility risk。

它不应该打印 secret 值，但 path、provider name 和本地环境事实仍可能敏感。

需要带版本的脱敏结构、而不是默认文本报告时，使用 `sigil doctor --output json`。它仍然只在本机离线运行。

## 私密反馈报告

`/feedback` 会先打开预览，此时不会写入任何内容。预览会说明包含和排除的诊断类别。按 `Enter` 后只会在 Sigil cache 目录下写入一份 JSON 报告；它不会写进 workspace、改变 session log、联系 provider 或上传报告。在 Unix 上，报告目录只允许当前用户访问，文件也只允许 owner 读写。

报告可能包含 build、操作系统和架构信息、脱敏 doctor checks、provider 与 model 标签、MCP alias，以及 capability 或 sandbox 状态。它会排除对话、tool input/output、文件内容和 diff、配置文件正文、credential 与环境变量名称及值、私有 endpoint、本地路径和 session log 内容。

导出后，TUI 会显示准确的本地目录和文件名。按 `Enter` 可在弹窗中检查实际写盘的脱敏 JSON，按 `O` 定位文件，按 `B` 打开结构化 GitHub Bug 表单。`C` 复制本地报告路径，`U` 复制表单链接。这些显式动作都不会上传或附加报告；决定分享前仍应完成检查。

## 分享 Log 或 Report 前

移除：

- API keys 和 tokens；
- 私有仓库路径；
- proprietary source excerpts；
- 能识别 private usage 的 provider request IDs；
- 包含敏感 prompts 或 file snippets 的 session logs；
- 包含内部 URL 或 credential 的 MCP server arguments。

## 推荐默认值

- 真实 secret 放在环境变量。
- 学习工具时保持 `permission.mode = "manual"`。
- MCP 保持 `allow_secrets = false`。
- External directory access 默认关闭。
- 允许文件变更前 review approval diff。
- 从预期 workspace root 启动 Sigil。
