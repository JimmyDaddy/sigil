# 用户 Changelog

[文档首页](README.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页只列面向用户的 release notes。当前支持边界和 early-preview 说明见 [当前支持状态与未来工作](status.md)。

## Unreleased - main

### 调整

- Windows shell 与 terminal 工具现在默认使用 native PowerShell，在 Doctor 和 tool card 中显示解析后的 dialect，对非 POSIX 命令保持保守批准分析，并通过 Job Object 持有取消与超时的进程树；本地执行仍明确标记为 unconfined。
- 远端 Streamable HTTP MCP server 现在支持显式、TUI-first 的 OAuth 登录，包括 PKCE、loopback 或手动 callback、原生凭据存储、有边界的 refresh/revoke 与 typed authentication failure。每个 OAuth 目标仍经过常规 network disclosure 与 destination check；headless 启动不会打开浏览器。
- 远端 MCP activation/refresh 现在以 transaction 方式替换 tool generation；Windows stdio MCP process tree 使用原生 Job Object ownership 进行有边界清理。

## v0.0.1-alpha.4 - 2026-07-16

以下变更已包含在打包发布的 `v0.0.1-alpha.4` 中。

### 新增

- 增加默认关闭且有隐私边界的 terminal attention notification，用于长任务完成、等待审批、执行失败和需要用户输入，并可自动选择 OSC 9、OSC 777 或 BEL。
- 增加适用于 Rust、Python、JavaScript/TypeScript 与 Go 的有边界 request-local 仓库上下文：优先复用相关的 warm LSP snapshot，否则回退到内置 Tree-sitter adapter。
- 增加 TUI 图片附件：可通过本地路径或系统图片剪贴板输入有边界的 PNG、JPEG 与 WebP，提供可删除的 metadata chip、受控 cache、安全 session projection 和精确 provider/model 准入。
- 增加 `sigil doctor --output json`，为支持请求提供带版本且脱敏的本地诊断格式。
- 增加 `/feedback`：先预览包含和排除的数据，再显式导出仅保存在本机的 JSON；报告绝不会自动上传。
- 增加 bug、feature request 和 documentation issue 的结构化 GitHub 表单。

### 调整

- 补全 `/feedback` 交接流程：导出后可在 TUI 内检查报告、在文件管理器中定位，或显式打开 Bug 表单；只有用户自行附加时报告才会离开本机。

## v0.0.1-alpha.3 - 2026-07-15

以下变更已包含在打包发布的 `v0.0.1-alpha.3` 中。

### 新增

- 为脚本增加稳定的 `sigil run --output json` 与 `--output jsonl` 格式，并增加只监听本机、要求 bearer 认证的高级 `sigil serve` 接口。
- 增加显式的已保存 session 操作：安全导出、conversation fork、pin、精确删除 review，以及 retention 清理 preview 与确认。

### 调整

- 所选模型具备已安装的本地计数支持，且压缩后请求已证明可以装入上下文时，`/compact` 现在可以确认一次手动上下文压缩。已完成的长对话与排队请求可以使用同一检查路径。一个固定的官方 OpenAI Responses 模型也可以在确认尚未产生输出的上下文超限后，经过独立计数和节省量检查，只恢复一次。

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
