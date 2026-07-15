# 当前支持状态与未来工作

[文档首页](README.md) · [安装](installation.md) · [Changelog](changelog.md) · [English](../en/status.md)

这一页把用户今天可以依赖的能力，与 experimental、limited 或 future packaging work 分开说明。当前 alpha 仍是 early preview，不承诺稳定 API 或插件兼容性。Release 版本与安装命令统一由[安装](installation.md)和 [Changelog](changelog.md)维护。

**版本边界：** 本页与 GitHub Pages 网站跟随 `main`。已打包发布的 alpha 是 `v0.0.1-alpha.3`，可能晚于下方能力；依赖新功能前请先检查 [Unreleased](changelog.md#unreleased-main)。

## 当前支持

| 领域 | 状态 |
| --- | --- |
| TUI 入口 | `sigil` 打开 TUI，是主要产品表面。 |
| 分发 | 当前提供 npm alpha、Homebrew tap、Cargo git-tag、源码和 release archive 路径；最新命令和渠道细节见[安装](installation.md)。 |
| Quick Setup | 首次运行 setup 可以创建可用本地配置。 |
| Doctor | `sigil doctor` 和 `/doctor` 报告 config、auth、workspace、MCP、code intelligence 和 terminal readiness。 |
| 自动化输出 | `sigil run --output json` 输出一条带版本的结果；`--output jsonl` 输出有序的带版本事件，并以唯一 terminal result 或 error 结束。 |
| Chat workflow | 用户可以通过 composer 工作，并查看可见 tool activity。 |
| Tool approvals | 文件变更、shell execution、外部路径和外部工具可在执行前 review。 |
| Session recovery | Session 和 control records 是 append-only；当前 V2 session log 可在重启后恢复可见状态。旧 raw session log 会明确提示为不受支持并保持原样。 |
| Checkpoint recovery | `Ctrl-R` 预览绑定证据的 checkpoint，并提供受控文件 restore，或保持文件不变的 conversation fork。 |
| Planning | `/plan` 运行只读 planning prompt，并可在用户显式接受后交接为 durable `/task` 执行；`/task <task>` 直接创建 durable 多步骤任务，`/task continue` 继续最新任务。 |
| 任务验证 | Verification card 展示 readiness、推荐检查，以及可检查的 snapshot 和 changeset 证据；`Alt-V` 用于聚焦。 |
| 上下文控制 | 界面持续显示 context pressure；manual、完全 idle 的 hard-threshold 与 queued pre-turn portable apply 要求本地 exact admission。窄 OpenAI Responses overflow path 要求受审计 server count 与 exact economics。owned preparation 与 source/queue/rejection CAS 会阻止 stale dispatch。 |
| DeepSeek provider | DeepSeek 是默认 Quick Setup 路径。 |
| OpenAI-compatible provider | 通过 `[providers.openai_compat]` 支持兼容 Chat Completions endpoint。 |
| OpenAI Responses provider | 通过 `[providers.openai_responses]` 支持 Responses streaming endpoint；官方 pinned snapshot 可在精确、无 output 的 context rejection 后执行一次受控且不递归的 overflow recovery。 |
| Anthropic provider | 通过 `[providers.anthropic]` 支持 Anthropic Messages streaming。其原生 compaction beta driver 仅记录加密候选，不是用户操作，也不会自动改变上下文。 |
| Gemini provider | 通过 `[providers.gemini]` 支持 Gemini `streamGenerateContent` streaming。 |
| Web data tools | Stable `websearch` 与 capability-backed `webfetch` route 使用独立 network policy、durable egress disclosure 和 external-source provenance。 |
| MCP server | 通过 `[[mcp_servers]]` 支持本地 stdio 与用户根 Streamable HTTP server，并带 trust、approval 和 secret-egress policy。 |
| Code intelligence | 可选，默认关闭，支持 LSP discovery 和 Rust fallback 行为。 |
| Terminal controls | Mouse capture、OSC52 copy、scroll sensitivity 和 terminal diagnostics 已有文档和配置。 |

## 有限制或高级用法

| 领域 | 当前预期 |
| --- | --- |
| Release archives | 已在 tagged GitHub releases 提供，用于手动安装；日常优先使用包管理器安装。 |
| 包管理器渠道 | Alpha 阶段的 package name 和可用性仍可能调整；当前事实以[安装](installation.md)为准。 |
| OpenAI-compatible 差异 | 该 provider 有意不暴露 DeepSeek-only prefix/FIM/beta 行为。 |
| Provider 专项选项 | 每个 provider 页面说明可用的 setup 与选项；正常的 tool approval、隐私和 session 行为保持一致。 |
| Code intelligence | 依赖本地 language servers 和环境；普通 chat 不依赖它。 |
| MCP lazy startup | Lazy server 会记录配置，但激活前不会注册假工具。 |
| External directories | 默认关闭，应保持窄范围和 approval-backed。 |
| Headless automation | `sigil run` 可用于脚本，但不能展示交互 approval modal。 |
| 本地服务 | `sigil serve` 会启动支持 retained event replay 和 graceful shutdown 的高级本机 HTTP/SSE 服务。V1 只允许 loopback，除 health 外所有 route 都要求 bearer auth；它不是远程或多用户服务。 |
| Execution sandbox | macOS、Linux、Docker、PTY、MCP stdio 和受信任 plugin hook 路径在支持的平台上已有 core coverage 与 receipt，但不同平台、远端工具和容器/daemon 场景的覆盖并不等价。 |
| 上下文帮助 | Sigil 可以使用相关的 session/task 信息和少量 workspace 文件；全面自动代码库分析仍是后续工作。 |
| 模型质量报告 | 已有内部自动化检查，但可重复的最终用户模型对比与 release 趋势还不是支持的产品功能。 |

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
- 面向最终用户的内置模型质量对比；
- 对生成的终端截图做跨 release visual regression review。

## 如何理解文档

用户文档描述当前行为，除非某节明确写了 "future work"、"limited" 或 "advanced"。alpha 阶段适合试用，不代表稳定兼容承诺。
