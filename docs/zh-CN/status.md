# 当前支持状态与未来工作

[文档首页](README.md) · [安装](installation.md) · [Changelog](changelog.md) · [English](../en/status.md)

这一页把用户今天可以依赖的能力，与 experimental、limited 或 future packaging work 分开说明。`v0.0.1` 是 early preview，不承诺稳定 API 或插件兼容性。

## 当前支持

| 领域 | 状态 |
| --- | --- |
| TUI 入口 | `sigil` 打开 TUI，是主要产品表面。 |
| npm 安装 | `npm install -g @jimmydaddy/sigil` 是首个 release 的 scoped npm package 路径。 |
| Homebrew tap | `brew install JimmyDaddy/sigil/sigil-ai` 安装 tap formula，binary 名仍然是 `sigil`。 |
| Cargo git 安装 | `cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1 --locked sigil` 从 release tag 安装。 |
| 源码安装 | `cargo install --path crates/sigil --locked` 仍支持本地 checkout 开发。 |
| Quick Setup | 首次运行 setup 可以创建可用本地配置。 |
| Doctor | `sigil doctor` 和 `/doctor` 报告 config、auth、workspace、MCP、code intelligence 和 terminal readiness。 |
| Chat workflow | 用户可以通过 composer 工作，并查看可见 tool activity。 |
| Tool approvals | 文件变更、shell execution、外部路径和外部工具可在执行前 review。 |
| Session recovery | Session 和 control records 是 append-only，可在重启后恢复可见状态。 |
| Planning | `/plan` 运行只读 planning prompt，并可在用户显式接受后交接为 durable `/task` 执行；`/task <task>` 直接创建 durable 多步骤任务，`/task continue` 继续最新任务。 |
| DeepSeek provider | DeepSeek 是默认 Quick Setup 路径。 |
| OpenAI-compatible provider | 通过 `[providers.openai_compat]` 支持兼容 Chat Completions endpoint。 |
| Anthropic provider | 通过 `[providers.anthropic]` 支持 Anthropic Messages streaming。 |
| Gemini provider | 通过 `[providers.gemini]` 支持 Gemini `streamGenerateContent` streaming。 |
| MCP stdio servers | 通过 `[[mcp_servers]]` 支持，并带 trust 和 approval policy。 |
| Code intelligence | 可选，默认关闭，支持 LSP discovery 和 Rust fallback 行为。 |
| Terminal controls | Mouse capture、OSC52 copy、scroll sensitivity 和 terminal diagnostics 已有文档和配置。 |

## 有限制或高级用法

| 领域 | 当前预期 |
| --- | --- |
| Release archives | 可以本地构建，也可由 tag release workflow 构建；包管理器 artifacts 从这些 archives 派生。 |
| Homebrew formula asset | 为 tap maintainer 生成 `sigil-ai.rb`；tap 仓库更新在本仓库外执行。 |
| npm package tarballs | 从 release archives 生成，用于 registry 发布和 release asset 检查。 |
| OpenAI-compatible 差异 | 该 provider 有意不暴露 DeepSeek-only prefix/FIM/beta 行为。 |
| Provider-specific 语义 | Anthropic 和 Gemini 的 request/event 细节留在 provider crate；`sigil-kernel` 只暴露 provider-neutral capabilities 和 chunks。 |
| Code intelligence | 依赖本地 language servers 和环境；普通 chat 不依赖它。 |
| MCP lazy startup | Lazy server 会记录配置，但激活前不会注册假工具。 |
| External directories | 默认关闭，应保持窄范围和 approval-backed。 |
| Headless automation | `sigil run` 可用于脚本，但不能展示交互 approval modal。 |
| HTTP/SSE adapter | `sigil serve` 会校验 local bind/token 默认值并输出 preflight plan；HTTP routing 和 listener startup 仍是后续工作。 |
| Execution sandbox | macOS、Linux、Docker、PTY、MCP stdio 和受信任 plugin hook 路径在支持的平台上已有 core coverage 与 receipt，但不同平台、远端工具和容器/daemon 场景的覆盖并不等价。 |
| Context retrieval | Context V0 支持 session/task memory 和有边界的 repo-file candidates。完整 semantic repo graph、impact graph 和向量检索仍是证据触发的未来工作。 |
| Model evals | 已有 deterministic eval 基础设施。真实模型 eval runner、重复运行策略和 release/nightly 趋势报告还不是当前用户支持路径。 |

## 未来工作

除非后续 release 明确说明，否则这些不是当前支持路径：

- self-update；
- desktop shell；
- hosted documentation search；
- 更丰富的自动 release notes；
- 更多 provider-specific setup assistants；
- 默认完整 semantic repo graph 或 vector retrieval；
- 对运行中的 shell/agent 进程做透明原地 crash resume；
- 把并行写 agent worktree isolation 作为默认工作流；
- 稳定 plugin API 兼容性；
- 全平台等价 OS sandbox 行为；
- 面向最终用户的内置真实模型 eval runner；
- 用于 release docs 的全自动真实终端截图生成。

## 如何理解文档

用户文档描述当前行为，除非某节明确写了 "future work" 或 "advanced"。`dev/docs/*` 下的开发文档可以描述架构方向和实现快照，不总是稳定用户支持承诺。
