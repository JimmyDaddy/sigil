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

## API Keys

优先使用环境变量：

```bash
export SIGIL_API_KEY="sk-..."
```

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
- 学习工具时保持 `permission.default_mode = "ask"`。
- MCP 保持 `allow_secrets = false`。
- External directory access 默认关闭。
- 允许文件变更前 review approval diff。
- 从预期 workspace root 启动 Sigil。
