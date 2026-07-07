# Sigil 用户文档

[English](../en/README.md)

Sigil 是一个 TUI-first coding agent。常规使用方式是进入一个仓库，运行 `sigil`，在终端界面里对话，查看工具活动，并在高风险操作真正修改文件或执行命令前进行审批。

## 从这里开始

第一次使用 Sigil 时，建议按这个顺序读：

1. [快速上手](quickstart.md)：安装、启动、完成 Quick Setup，并跑完第一个有用 session。
2. [安装](installation.md)：当前支持的安装、更新、卸载和 release archive 说明。
3. [视觉导览](visual-tour.md)：用 screenshot-style walkthrough 了解主界面、approval card 和 config panel。
4. [TUI 使用指南](user-guide.md)：界面布局、键位、session、审批、计划任务和 code intelligence。
5. [安全与权限](safety.md)：什么能运行、什么需要审批，以及如何 review 高风险动作。
6. [排障](troubleshooting.md)：setup、认证、终端、MCP、code intelligence 和恢复问题。

如果你已经安装好了 Sigil，最短路径是：

```bash
cd /path/to/workspace
sigil
```

然后在 composer 里输入一个具体任务，例如：

```text
解释这个仓库的结构，并指出主要入口文件。
```

## 按任务选择文档

| 我想要... | 阅读 |
| --- | --- |
| 第一次试用 Sigil | [快速上手](quickstart.md) |
| 看产品界面大概长什么样 | [视觉导览](visual-tour.md) |
| 安装、更新或卸载 binary | [安装](installation.md) |
| 学习 TUI 布局、键位、slash command 和 session 行为 | [TUI 使用指南](user-guide.md) |
| 参考真实 coding task 的提示词和流程 | [常见工作流](workflows.md) |
| 使用可复制 prompt 模式 | [Cookbook](cookbook.md) |
| 理解审批、workspace 边界和 MCP trust | [安全与权限](safety.md) |
| 配置 provider 凭据、权限、memory、计划任务或 code intelligence | [配置](configuration.md) |
| 选择 DeepSeek、OpenAI-compatible、Anthropic 或 Gemini | [Provider 指南](providers.md) |
| 理解隐私、provider context、session logs 和 secrets | [隐私与数据处理](privacy.md) |
| 通过 MCP 增加外部工具 | [MCP 接入指南](mcp.md) |
| 修复 setup、认证、终端、MCP 或 LSP 问题 | [排障](troubleshooting.md) |
| 在一页里查看命令、键位、路径和环境变量 | [参考](reference.md) |
| 验证 mouse capture、OSC52、tmux、SSH 或 WSL 行为 | [Terminal 兼容性](terminal-compatibility.md) |
| 查看当前支持承诺 | [当前支持状态与未来工作](status.md) |
| 阅读用户可见 release notes | [用户 Changelog](changelog.md) |

## 产品心智

Sigil 围绕几个用户可感知的概念工作：

- **TUI 是主要产品表面。** 日常使用直接运行无子命令的 `sigil`。`sigil doctor`、`sigil run` 这类子命令用于诊断和自动化。
- **启动目录很重要。** 常规 `workspace.root = "."` 配置下，启动 Sigil 时所在目录就是 agent 可以读取和修改的 workspace。
- **工具执行是可见工作。** 读取、搜索、编辑、shell 命令、MCP 调用和 code-intelligence action 都会作为 activity 出现在 transcript 中。
- **高风险动作要保留控制权。** 文件修改、命令执行、删除和外部工具可要求审批，并展示摘要和 diff。
- **Session 是持久状态。** 默认 session 和 control records 以 append-only JSONL 写入 Sigil 用户态 state 目录，重启和恢复不会静默重放中断工具。

## 当前分发状态

首个 release 已准备面向 npm、Homebrew tap、Cargo git-tag 安装和 GitHub release archive 分发。`v0.0.1` 是 early preview：核心 TUI 工作流已经可用，但配置、插件 API、高级 sandbox 覆盖和自动化入口仍可能调整。

```bash
npm install -g @sigil-ai/sigil
brew install JimmyDaddy/sigil/sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1 --locked sigil
```

从 checkout 安装仍适合本地开发：

```bash
cargo install --path crates/sigil --locked
```

自更新仍属于后续 packaging 工作。

## 配置速览

大多数用户应该先用 TUI 中的 Quick Setup。手写配置适合需要可重复本地默认值或 CI 自动化的场景。

常见选择：

- DeepSeek 默认 provider：使用 `SIGIL_API_KEY` 或 `[providers.deepseek]`。
- OpenAI-compatible provider：使用 `[agent].provider = "openai_compat"` 和 `[providers.openai_compat]`。
- Anthropic provider：使用 `[agent].provider = "anthropic"` 和 `[providers.anthropic]`。
- Gemini provider：使用 `[agent].provider = "gemini"` 和 `[providers.gemini]`。
- 默认权限：在明确知道哪些动作可以自动允许前，保持 `[permission].mode = "manual"`。
- 终端兼容性：调整 `[terminal].mouse_capture`、`[terminal].osc52_clipboard` 和 `[terminal].scroll_sensitivity`。
- Code intelligence：需要 LSP-backed 符号、引用、诊断、code action 和 rename 工具时，启用 `[code_intelligence].enabled = true`。

共享配置见 [configuration.md](configuration.md)，provider 专项设置和环境变量优先级见 [providers.md](providers.md)。
可复制配置模板位于 [docs/examples/config](../examples/config)。

## 遇到问题时

先运行：

```bash
sigil doctor
```

在 TUI 内：

```text
/doctor
```

Doctor report 会检查 config 加载、workspace 解析、session log、provider 认证、MCP command/trust、code intelligence readiness、terminal profile、mouse capture 和 OSC52 clipboard 风险。
