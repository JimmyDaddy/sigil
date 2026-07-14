# Sigil

<p align="center">
  <img src="assets/logo/sigil-full-staff-glow.svg" alt="Sigil 标志" width="560">
</p>

[English](README.md) | 简体中文

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil 是一个 TUI-first 的 Rust coding agent，用来在真实仓库里协助开发。它把对话、工具调用、审批、diff、诊断、计划任务和 session 恢复放进同一个终端界面里；CLI 只保留为轻量自动化入口。

[网站](https://jimmydaddy.github.io/sigil/zh-CN/) · [文档站](https://jimmydaddy.github.io/sigil/zh-CN/docs/) · [快速上手](https://jimmydaddy.github.io/sigil/zh-CN/docs/quickstart/) · [视觉导览](https://jimmydaddy.github.io/sigil/zh-CN/docs/visual-tour/) · [支持状态](https://jimmydaddy.github.io/sigil/zh-CN/docs/status/)

Sigil 的首个 alpha release 已通过 npm、Homebrew tap、Cargo git-tag 安装和 GitHub release archive 发布。`v0.0.1-alpha.2` 是 early preview：核心 TUI 工作流已经可用，但配置、插件 API、高级 sandbox 覆盖和自动化入口仍可能调整。自更新仍属于后续 packaging 工作。

网站和用户文档跟随 `main`，可能比已发布 alpha 包更新。依赖新能力前请先查看 [Unreleased](docs/zh-CN/changelog.md#unreleased-main)；如果能力尚未进入 tagged release，请从源码安装。

## 快速开始

前置要求：

- 一个现代终端模拟器。
- 一种安装器：npm、Homebrew，或与本仓库兼容的 Rust toolchain。
- 一个模型 provider 凭据。首次启动时可以通过 Quick Setup 填写。

使用首发包管理器路径之一安装 Sigil：

```bash
npm install -g @sigil-ai/sigil@alpha
```

```bash
brew install JimmyDaddy/sigil/sigil-ai
```

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.2 --locked sigil
```

如果你希望从源码安装，可以在 checkout 中运行：

```bash
git clone https://github.com/JimmyDaddy/sigil.git
cd sigil
cargo install --path crates/sigil --locked
```

进入希望 Sigil 操作的仓库并启动：

```bash
cd /path/to/your/project
sigil
```

如果 Sigil 找不到可用配置，会进入 Quick Setup。确认 workspace、选择 provider/model，并在界面里填写认证信息。可重复配置文件和环境变量方式见 [配置指南](docs/zh-CN/configuration.md)。

检查本地 setup：

```bash
sigil --version
sigil doctor
```

## Sigil 做什么

- 把 coding 工作留在 TUI：transcript、composer、live tool activity、approval、status、usage 和 controls。
- 让 agent 通过结构化工具读取、搜索、编辑文件和运行命令。
- 在高风险写操作前展示 approval card、受影响文件和有边界的 diff。
- 从 Sigil 用户态 state 目录下的 append-only JSONL 恢复 session。
- 用 `/plan` 执行只读规划，并在用户显式接受后交接为 durable `/task` 任务，进入 planner、executor 和可选 subagent 流程。
- 普通 chat 明确要求子 agent 时，会在最终回答前强制等待有效 agent 结果。
- 受信任的 agent profile 可通过 `@profile <prompt>` 或受信任的 profile slash name 直接调用。
- 按显式 trust、approval 和 secret-egress policy 接入本地 stdio 与用户根 Streamable HTTP MCP server。
- 提供 capability-backed `webfetch` 与 stable `websearch` route，并执行独立 network policy、durable egress disclosure 和 external-source provenance。
- 可选开启 code intelligence，支持符号、引用、诊断、code action 和 rename preview。

## 日常工作流

正常使用时直接运行无子命令的 `sigil`。常用 TUI 入口：

| 需求 | 使用 |
| --- | --- |
| 普通提问或编辑 | 直接在 composer 输入 |
| 粘贴多行文本或代码 | 粘贴到 composer；大段粘贴会折叠展示，但会完整提交 |
| 编辑较长 composer 草稿 | `Ctrl-A/E`、`Alt-B/F`、`Ctrl-K/Y`、`Ctrl-Z` |
| 规划后执行 | `/plan` 后输入 prompt，或 `/plan <prompt>`；接受 plan card 后创建并运行 durable task |
| 执行 durable 多步骤任务 | `/task <任务>`；未完成任务用 `/task continue` |
| 验证任务完成情况并查看证据 | 用 `Alt-V` 聚焦 Verification card；运行推荐检查，或查看 snapshot 与 changeset 证据 |
| 检查安全恢复点 | 按 `Ctrl-R` 预览受控 checkpoint restore 或 fork，再决定是否变更文件 |
| 预览长上下文压缩 | 用 `/compact` 打开只读 Context Compaction V2 preview；apply 仍暂时冻结 |
| Sigil 忙碌时追加后续消息 | 在当前 run 进行中提交普通 chat；Sigil 会显示在 Follow-ups，并在下一次安全 turn 派发时追加用户消息 |
| 查看待处理 follow-ups | `Tab` 聚焦 follow-up panel；`/queue show`、`/queue next`、`/queue interrupt`、`/queue edit` 和 `/queue delete` 是高级控制 |
| 要求普通 chat 使用子 agent | 明确说明“使用子 agent ...” |
| 直接调用受信任 agent profile | `@profile <prompt>` 或 `/review-agent <prompt>` 这类受信任 profile slash name |
| 把前台子 agent 移到后台 | Sigil 等待该 agent 时按 `Ctrl-B` |
| 切换或重命名主/子 agent transcript | composer 下方 agent 面板（`Down`、`Up/Down`、`Enter`）、`Alt-A`、`Shift-Alt-A`、`/agent` 或 `/agent rename <child-id|current> <name>` |
| 查看较长子 agent 结果 | 切到子 agent transcript，或让 `read_agent_result` 分页读取子 agent final answer |
| 新建或切换 session | `/new`、`/resume`，或退出后用 `sigil resume <session-id>` |
| 修改常用设置 | `/config` |
| 诊断 setup/auth/MCP/LSP | `/doctor` |
| 在紧凑和详细信息栏之间切换 | `F2` |
| 切换默认权限模式 | `Shift-Tab` |
| 取消当前运行或关闭浮层 | `Ctrl-C` 或 `Esc` |

完整键位、鼠标、transcript 选择和 OSC52 剪贴板行为见 [TUI 使用指南](docs/zh-CN/user-guide.md) 和 [terminal 兼容性检查清单](docs/zh-CN/terminal-compatibility.md)。

## 安全与状态

Sigil 把工具执行视为可审计状态，而不是隐藏副作用。

- 文件写入、编辑、删除、命令执行、MCP 调用和外部数据访问都经过 permission model。
- 写工具围绕 preview 和 diff 审批体验设计。
- 中断的工具执行在恢复时会投影为 interrupted result，不会静默重放。
- Provider 专项选项由各 provider 页面维护；tool approval、session recovery 与安全规则在支持的服务之间保持一致。

## Provider 与集成

| 能力 | 配置入口 | 适合场景 | 详情 |
| --- | --- | --- | --- |
| DeepSeek | `[providers.deepseek]` | 默认 Quick Setup 路径和 DeepSeek 专项选项。 | [DeepSeek 指南](docs/zh-CN/provider-deepseek.md) |
| OpenAI-compatible | `[providers.openai_compat]` | 兼容 Chat Completions 的 `/v1` endpoint。 | [OpenAI-compatible 指南](docs/zh-CN/provider-openai-compatible.md) |
| OpenAI Responses | `[providers.openai_responses]` | OpenAI Responses streaming endpoint。 | [OpenAI Responses 指南](docs/zh-CN/provider-openai-responses.md) |
| Anthropic | `[providers.anthropic]` | 通过 Anthropic Messages streaming 使用 Claude 模型。 | [Anthropic 指南](docs/zh-CN/provider-anthropic.md) |
| Gemini | `[providers.gemini]` | 通过 `streamGenerateContent` 使用 Gemini 模型。 | [Gemini 指南](docs/zh-CN/provider-gemini.md) |
| Web data tools | `[web]` | Provider-hosted、configured 或 bundled search，以及读取已选择的来源。 | [权限与沙箱](docs/zh-CN/permissions-and-sandbox.md#网络与-web-工具) |
| MCP server | `[[mcp_servers]]` | 带显式 trust 与 egress policy 的外部 stdio 或用户根 Streamable HTTP 工具。 | [MCP 指南](docs/zh-CN/mcp.md) |
| Code intelligence | `[code_intelligence]` | LSP-backed 符号、引用、诊断、action 和 rename preview。 | [配置指南](docs/zh-CN/configuration.md) |

## 按任务找文档

| 我想要... | 阅读 |
| --- | --- |
| 第一次试用 Sigil | [快速上手](docs/zh-CN/quickstart.md) |
| 看产品界面大概长什么样 | [视觉导览](docs/zh-CN/visual-tour.md) |
| 学习 TUI、命令、键位、session 和 approval | [TUI 使用指南](docs/zh-CN/user-guide.md) |
| 配置 provider、权限、memory、planning、terminal 或 LSP | [配置指南](docs/zh-CN/configuration.md) |
| 选择或排查模型 provider | [Provider 指南](docs/zh-CN/providers.md) |
| 理解 approval、workspace、MCP 和数据边界 | [安全](docs/zh-CN/safety.md) 和 [隐私](docs/zh-CN/privacy.md) |
| 修复 setup、认证、terminal、MCP 或 LSP 问题 | [排障](docs/zh-CN/troubleshooting.md) |
| 查找所有命令、键位、路径和环境变量 | [参考](docs/zh-CN/reference.md) |

## 项目

欢迎贡献。开始前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 和
[开发者文档索引](dev/docs/index.md)。安全漏洞请按
[SECURITY.md](SECURITY.md) 中的说明私下报告。Sigil 使用
[MIT License](LICENSE) 发布。
