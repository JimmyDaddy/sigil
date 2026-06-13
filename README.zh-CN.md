# Sigil

[English](README.md) | 简体中文

`sigil` 是一个 TUI-first 的 Rust AI coding agent。它把终端里的对话、工具调用、审批、diff 预览、运行状态和 session 恢复放在同一个交互界面里，而不是让用户记一组越来越多的子命令。

当前项目仍以源码运行和开发验证为主；普通使用路径优先走 `sigil-tui`。

## 快速开始

启动 TUI：

```bash
cargo run -p sigil-tui
```

如果没有可用配置，Sigil 会进入 Quick Setup。你只需要确认当前工作区、选择模型并填写认证信息。如果你更喜欢环境变量或手写配置文件，见 [docs/zh-CN/configuration.md](docs/zh-CN/configuration.md)。

自动化或脚本场景可以使用 CLI：

```bash
cargo run -p sigil-cli -- run "总结一下当前仓库"
```

配置或本地工具链看起来不对时，可以先跑 `doctor`：

```bash
cargo run -p sigil-cli -- doctor
```

在 TUI 内也可以用 `/doctor`，它会把同一份本地诊断报告渲染到 transcript。Doctor 会附带可执行的 `fix:` 建议；如果 API key 只明文保存在配置里，也会给出安全提示。

CLI 不是主要产品表面，默认只承担自动化和调试入口。

## 能做什么

- 在 TUI 里对当前仓库发起 coding task，并流式查看输出
- 在高风险工具调用前查看审批卡片、受影响文件和 diff 预览
- 运行后继续查看 tool activity、命令输出、文件变更和诊断摘要
- 重启后恢复最近 session，继续基于已有上下文工作
- 用 `/config` 调整常用配置，用 `/resume` 选择历史 session
- 用 `/doctor` 诊断配置、认证、MCP、LSP 和 terminal 就绪状态，并查看修复建议
- 用 `/model` 和 `/effort` 调整下一轮模型与 reasoning effort
- 用 `/compact` 压缩长会话上下文
- 在支持 mouse mode / OSC52 的终端里使用鼠标点击、滚动、transcript 拖选和复制
- 可选开启 code intelligence，让 agent 使用符号、定义、引用、诊断、code action 和 rename preview，并在 `/config` 查看就绪状态
- 可选接入 stdio MCP server，按信任策略暴露外部工具

## TUI 心智

Sigil 的主界面以 chat/composer 为中心。你输入任务，Sigil 在同一个界面里展示 assistant 回复、工具活动、审批请求、运行状态和 session 信息。

几个常用操作：

- `F1`：打开 keyboard help
- `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End`：回看 transcript
- `/`：打开 slash command selector
- `Shift-Tab`：切换默认权限模式
- `Ctrl-C` 或 `Esc`：取消当前运行或退出当前浮层
- `Ctrl-G`：聚焦最近 tool activity
- `Alt-J` / `Alt-K`：切换 activity
- `Ctrl-T`：展开或收起 thinking / activity

完整使用说明见 [docs/zh-CN/user-guide.md](docs/zh-CN/user-guide.md)。

## 配置

Sigil 会按顺序查找配置：

1. `--config <path>`
2. 当前工作目录下的 `./sigil.toml`
3. 标准用户配置目录里的 `sigil.toml`

常见用户配置目录：

- macOS：`~/Library/Application Support/sigil/sigil.toml`
- Linux：`$XDG_CONFIG_HOME/sigil/sigil.toml` 或 `~/.config/sigil/sigil.toml`
- Windows：`%APPDATA%\sigil\sigil.toml`

认证、provider、permission、memory、compaction、code intelligence、terminal 兼容性和环境变量 override 的完整配置示例见 [docs/zh-CN/configuration.md](docs/zh-CN/configuration.md)。真实终端里的鼠标和剪贴板 smoke checklist 见 [docs/zh-CN/terminal-compatibility.md](docs/zh-CN/terminal-compatibility.md)。

## Provider

Sigil 当前支持 DeepSeek 和 OpenAI-compatible Chat Completions provider。Quick Setup 仍默认走 DeepSeek；OpenAI-compatible endpoint 使用 `provider = "openai_compat"` 和 `[providers.openai_compat]` 配置。

## MCP

Sigil 可以通过 stdio MCP server 接入外部工具。MCP tools、resources 和 prompts 会被包装进同一个工具审批和 activity 展示体系，支持 eager/lazy 启动、required/optional server、trust policy、secret egress 阻断和 pinned identity 校验。

配置和安全说明见 [docs/zh-CN/mcp.md](docs/zh-CN/mcp.md)。

## 文档索引

用户文档：

- [TUI 使用指南](docs/zh-CN/user-guide.md) / [English](docs/en/user-guide.md)
- [配置指南](docs/zh-CN/configuration.md) / [English](docs/en/configuration.md)
- [MCP 接入指南](docs/zh-CN/mcp.md) / [English](docs/en/mcp.md)

开发者文档：

- [代码规范](dev/governance/code-standards.md)
- [工程规范](dev/governance/engineering-standards.md)
- [核心技术方案](dev/docs/sigil-rust-agent-core-technical-solution.md)
- [当前实现快照](dev/docs/current-implementation-notes.md) / [English](dev/docs/current-implementation-notes.en.md)
- [能力路线图](dev/docs/sigil-capability-roadmap.md)
- [仓库内协作说明](AGENTS.md)

## 开发验证

代码变更默认按仓库工程规范执行相关 gate：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

只改文档时可以不跑全量 Rust gate，但需要确认链接、路径和示例命令仍然成立。
