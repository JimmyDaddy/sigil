# AGENTS.md

本文件约束在 `sigil` 内协作的 coding agent、自动化脚本与辅助开发工具。

## 先读这些文档

在修改代码前，先对齐这三份文档：

1. 代码规范：[`dev/governance/code-standards.md`](dev/governance/code-standards.md)
2. 工程规范：[`dev/governance/engineering-standards.md`](dev/governance/engineering-standards.md)
3. 架构方案：[`dev/docs/sigil-rust-agent-core-technical-solution.md`](dev/docs/sigil-rust-agent-core-technical-solution.md)

其中 `dev/governance/code-standards.md` 是本仓库的直接编码约束；如果实现与习惯冲突，以该文档为准。

## 仓库定位

`sigil` 是一个 **TUI-first** 的 Rust AI coding agent。

协作时请守住这几个高优先级原则：

- 不要把项目继续推成“命令越做越多的 command-only 工具”
- 优先保护 TUI 作为第一用户表面的体验与信息架构
- `sigil-kernel` 必须保持通用，不引入 DeepSeek 专属术语到公共 API
- provider 专项行为留在 provider crate 内解释
- session / control state 必须是 append-only、可持久化、可审计的

## 修改代码前的最小检查

开始动手前请先确认：

- 目标变更属于哪个 crate
- 是否会影响 TUI、CLI、provider、tool 或 session 持久化行为
- 是否需要同步更新 `README.md`、`dev/governance/*` 或 `dev/docs/*`
- 是否需要补测试或更新现有测试断言

## 目录职责

- `crates/sigil-kernel`：通用领域契约、agent loop、approval、event、session
- `crates/sigil-provider-deepseek`：DeepSeek provider 与相关专项行为
- `crates/sigil-tools-builtin`：内置工具与 preview/diff 预览
- `crates/sigil-process`：跨 crate 的最小进程树 ownership 与平台 capability probe
- `crates/sigil-desktop`：桌面 Rust 后端的 launcher、私有 bearer 与 typed local HTTP client；不承载 UI 或 agent loop
- `crates/sigil-mcp`：stdio MCP client 与工具适配
- `crates/sigil-runtime`：CLI/TUI 共享的 provider、tool registry 与 run options 装配
- `crates/sigil-cli`：薄 CLI、调试入口、自动化入口
- `crates/sigil-tui`：第一用户入口
- `dev/governance`：开发规范
- `dev/docs`：架构与技术方案

## 具体协作要求

### 编码要求

- 遵守 [`dev/governance/code-standards.md`](dev/governance/code-standards.md)
- 变更公共接口前，先确认是否会把 provider 私有语义泄漏进 `kernel`
- 写工具相关代码时，优先考虑 preview、审批、可恢复性和结构化错误
- 改 TUI 时，不要只改 UI；同时检查状态模型、事件流和键位提示是否同步

### 工程要求

- 遵守 [`dev/governance/engineering-standards.md`](dev/governance/engineering-standards.md)
- 代码变更完成后至少跑相关 gate；默认优先跑：
  - `cargo fmt --all --check`
  - `cargo check`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
- docs-only 变更可以不跑全量 gate，但要确认链接、路径和命令没有写错

### 文档同步

出现以下情况时，必须同步更新文档：

- 新增或移除 crate / 入口命令
- TUI 用户流程变化
- tool 审批、session、provider 能力边界变化
- 新的代码约束或工程约束落地

## 不要这样做

- 不要把 DeepSeek 专属字段直接塞进 `sigil-kernel` 的公共类型
- 不要把隐藏调试入口写成 README 的主入口
- 不要绕过审批流直接让写工具静默生效
- 不要引入没有测试覆盖的会话恢复或持久化行为变更
- 不要让 `README`、`AGENTS`、`dev/governance` 和真实实现长期失同步
