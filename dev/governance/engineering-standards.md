# Termquill 工程规范

本文档定义 `termquill` 的工程协作、变更流程和交付约束。

## 1. 文档分工

仓库里有两套开发文档面，职责不要混淆：

- `dev/governance/*`：直接约束当前工程协作的规范文档
- `dev/docs/*`：技术方案、架构草案、设计演进文档

简单说：

- “应该怎么写代码、怎么做工程” 看 `dev/governance`
- “为什么这样设计、未来怎么扩” 看 `dev/docs`

## 2. 仓库分层职责

### 2.1 产品层

- `crates/termquill-tui` 是第一用户入口
- `crates/termquill-cli` 是自动化和调试入口

### 2.2 内核层

- `crates/termquill-kernel` 是领域核心
- 任何跨入口共享的能力，优先先沉到 kernel

### 2.3 基础能力层

- `crates/termquill-provider-deepseek`：DeepSeek provider
- `crates/termquill-tools-builtin`：内置工具
- `crates/termquill-mcp`：MCP 接入
- `crates/termquill-runtime`：跨 TUI / CLI 的 provider、tool registry、run options 装配

## 3. 变更流程

### 3.1 开工前

先回答这几个问题：

- 这是产品表面变化、内核变化，还是 provider / tool 变化
- 是否会影响 TUI 用户流程
- 是否会影响 session 持久化、恢复或审批安全边界
- 是否需要同步文档

### 3.2 实施时

- 优先做最小闭环，不做“大而全”半成品
- 优先保护现有运行链路，不要为新能力破坏已有 TUI 体验
- 跨 crate 变更时，优先先定边界，再改代码

### 3.3 收尾时

- 跑必要 gate
- 更新必要文档
- 在说明里明确哪些能力已完成，哪些只是扩展点

## 4. 文档同步规范

以下变更默认要同步文档：

- 新入口、新 crate、新配置块
- TUI 主流程变化
- tool 审批 / diff / session 行为变化
- provider 能力边界变化
- 代码约束、工程约束变化

同步目标通常包括：

- `README.md`
- `AGENTS.md`
- `dev/governance/*`
- `dev/docs/*` 中被影响的设计方案

## 5. 验证规范

### 5.1 默认质量门

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

### 5.2 何时需要更强验证

以下情况建议至少做一次针对性人工冒烟：

- TUI 交互流程变更
- 审批逻辑变更
- session 恢复变更
- provider streaming / retry / reasoning replay 变更
- MCP 工具调用变更

### 5.3 docs-only 变更

如果只改文档，可以不跑全量 Rust gate；但要确认：

- 所有引用路径真实存在
- 示例命令可在当前仓库语境下成立
- 文档没有继续宣传已经隐藏或不再推荐的产品心智

## 6. 配置管理规范

- 示例配置以仓库根目录的 `termquill.toml` 为准
- 新增配置项时，要同时考虑默认值、兼容性和文档说明
- 用户主心智相关配置要谨慎暴露，不要把调试开关直接产品化

## 7. TUI 产品规范

- 新能力默认先想清楚如何进入 TUI，而不是先加 CLI 子命令
- 键位、状态栏、面板提示必须同步更新
- 审批体验优先级很高：写工具要尽量有 preview / diff / 导航
- session 体验要可持续使用，而不只是“一次性跑完一轮”
- `AppState` 应保持 façade；可独立演进的输入、审批、session、slash、timeline、provider status 和渲染块应拆入独立模块，避免 `app.rs` / `ui.rs` 重新膨胀

## 8. Provider 与工具工程规范

### 8.1 Provider

- 保持 `DeepSeek-first`，但不要把仓库做窄成 `DeepSeek-only`
- provider 专项能力写在 provider crate，不反向污染 kernel
- Beta-only 能力要明确标识，避免误导成默认稳定能力
- chat 主链路必须保持真实 streaming；只有首个 chunk yield 前允许透明 retry，yield 后的错误应作为 stream error 暴露

### 8.2 Tool

- 工具必须考虑 workspace confinement
- 文件类工具必须拒绝绝对路径、`..` 和指向 workspace 外的 symlink；新增路径要校验最近存在父目录仍在 workspace root 内
- 写工具要考虑 preview、审批、失败回灌、恢复一致性
- shell 工具要特别注意工作目录、超时和错误输出结构化

## 9. 会话与持久化规范

- session log 采用 append-only JSONL
- control state 不能只存在运行内存
- response handle、continuation state、prefix snapshot、compaction record 等 durable control state 要有显式查询/恢复路径
- 任何恢复相关设计，都要优先考虑“进程重启后是否还能正确继续”

## 10. 评审标准

一个变更准备合入前，至少应该满足：

- 架构边界没有被破坏
- TUI 用户心智没有被命令式入口反向带偏
- 文档和实现一致
- 关键验证已完成
- 后续扩展点没有被临时实现锁死
