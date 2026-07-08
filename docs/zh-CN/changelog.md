# 用户 Changelog

[文档首页](README.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页只列面向用户的 release notes。当前支持边界和 early-preview 说明见 [当前支持状态与未来工作](status.md)。

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
