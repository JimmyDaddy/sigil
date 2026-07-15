# 用户 Changelog

[文档首页](README.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页只列面向用户的 release notes。当前支持边界和 early-preview 说明见 [当前支持状态与未来工作](status.md)。

## Unreleased - main

### 调整

- 所选 profile 已安装 checksum-pinned tokenizer 且本地 exact target-fit proof 通过时，`/compact` 现在可以确认一次手动 Context Compaction V2 apply；完成的 hard-threshold 与 queued pre-turn request 也可以使用同一 owned verified path。pinned 官方 OpenAI Responses snapshot 可在精确 rejection evidence、两次受审计计数与 minimum-savings proof 通过后执行一次不递归的 overflow recovery。

## v0.0.1-alpha.2 - 2026-07-15

以下变更已包含在打包发布的 `v0.0.1-alpha.2` 中。

### 新增

- 通过 `[providers.openai_responses]` 增加 OpenAI Responses provider。
- 增加 stable `websearch` 与 capability-backed `webfetch` route，并使用独立 network policy 和来源 provenance。
- 增加任务 Verification card、`Alt-V` 聚焦、推荐检查，以及可检查的 snapshot 与 changeset 证据。
- 增加 `Ctrl-R` checkpoint 检查，并提供受控 restore 或 conversation fork 选择。
- 增加通过 `/compact` 打开的只读 Context Compaction V2 preview。

### 调整

- 本地 MCP 在 stdio server 之外增加用户根 Streamable HTTP server，并沿用同一套 trust、approval 和 secret-egress policy。
- 围绕 verification、recovery 和 context controls 更新用户文档与网站导航。

### 当前限制

- Context Compaction V2 apply（包括受控 overflow recovery）在修复正确性问题期间仍暂时冻结；`/compact` 仅用于 review preview。

## v0.0.1-alpha.1 - 2026-07-08

### 新增

- 发布 scoped npm package：`@sigil-ai/sigil@alpha`。
- 发布 Homebrew tap formula：`JimmyDaddy/sigil/sigil-ai`。
- 补齐 npm、Homebrew、Cargo git tag、源码构建和手动 release archive 安装路径。
- 生成 GitHub Pages 文档页，覆盖安装、配置、provider、安全、隐私、MCP、视觉导览、排障、参考和当前支持状态。

### 调整

- 明确 `v0.0.1-alpha.1` 是 early preview：核心 TUI 工作流已经可用，但配置、插件 API、高级 sandbox 行为和自动化入口仍可能调整。
- 把文档入口改成更清晰的任务路径：快速上手、安装、视觉导览、日常工作流、安全、排障和参考。
- 更新用户文档中的 provider 范围：DeepSeek、OpenAI-compatible、Anthropic 和 Gemini。

### 已知限制

- 暂不支持自更新。
- alpha 阶段暂不承诺稳定 plugin API 兼容。
- Sandbox 覆盖和 execution receipt 会随平台与后端不同而不同。
- Headless automation 不能展示交互式审批弹窗。

## v0.0.1-alpha - 2026-07-07

### 新增

- Sigil TUI 的首个公开 alpha release。
- 通过 `sigil` 命令进入 TUI。
- Quick Setup、`/config`、`sigil doctor` 和 `/doctor`。
- 通过 `/task` 和 `/plan` 使用 durable task 与 planning flow。
- 文件变更、shell execution、MCP 使用和 code-intelligence edit 通过 approval 控制。
- 从 append-only 本地状态恢复 session。

### 已知限制

- 这是初始 preview，已经被 `v0.0.1-alpha.1` 取代。
- 用户应优先使用 `alpha` 安装渠道或最新文档中的 release tag。
