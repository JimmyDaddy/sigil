# Termquill 代码规范

本文档定义 `termquill` 的编码约束。`AGENTS.md` 会直接引用本文件；如果个人习惯与本文冲突，以本文为准。

## 1. 总体原则

### 1.1 TUI-first，不是 command-first

- 面向普通用户的主要产品表面是 `termquill-tui`
- CLI 可以存在，但默认只承担自动化、调试、脚本入口
- 新能力优先考虑如何进入 TUI 交互，而不是先加顶层命令

### 1.2 kernel 保持通用

- `crates/termquill-kernel` 只能承载通用概念
- 不要把 `DeepSeek`、`beta endpoint`、`reasoning_content` 这类 provider 私有术语直接做成 kernel 公共 API
- provider-specific 行为应留在 `crates/termquill-provider-deepseek`

### 1.3 append-only 与可审计

- session log、control state、provider continuation state 必须可持久化
- 不要依赖“只存在进程内存里”的隐式状态完成关键链路
- 写入审计面状态时，优先追加记录，不做难以追踪的就地改写

## 2. Rust 编码规则

### 2.1 风格

- 使用 Rust 2024 edition
- 统一走 `rustfmt`，遵守仓库中的 [`rustfmt.toml`](../../rustfmt.toml)
- 单行宽度默认按现有格式化结果维护，不手工对抗格式化器

### 2.2 Lint

- 默认以 `clippy -D warnings` 为目标
- 禁止保留 `dbg!`、`todo!`、`unwrap()`
- 如果确实需要放宽 lint，优先局部、最小化、并写明原因

### 2.3 错误处理

- 对外层流程使用 `anyhow::Result`
- 对稳定领域错误优先定义结构化错误类型
- 公共或跨 crate 传播的错误不要使用 `()`、裸 `String` 或无语义 JSON 充当错误类型
- 需要稳定错误面的场景优先使用 `thiserror` 等方式实现 `std::error::Error + Send + Sync`
- 错误消息必须带上下文，尤其是文件路径、provider 操作、session 恢复、MCP 调用
- 错误文案默认用小写、无句号，优先描述失败事实和定位信息
- 不要吞错，不要 silent fallback

### 2.4 注释

- 只为“读代码不容易一眼看懂”的状态机、边界约束、协议映射写简短注释
- 不要写重复代码字面意思的注释

### 2.5 Async 边界

- async 代码路径中不要直接执行阻塞式标准库 I/O 或长时间 CPU 计算
- 能使用 Tokio 的异步 API 时，优先使用异步 API，而不是把同步实现直接塞进 `async fn`
- 必须桥接阻塞逻辑时，短时阻塞工作使用 `tokio::task::spawn_blocking`
- `spawn_blocking` 只用于会自行结束的阻塞工作；长期常驻的阻塞 loop / worker 优先使用专用线程，并提供明确的退出路径
- 后台任务不能“放飞不管”；需要有明确 owner、`JoinHandle`、取消信号或收尾路径

### 2.6 公开接口与 Rustdoc

- 新增跨 crate 的公共类型、trait、函数时，至少写清楚它的职责和边界
- 会返回错误、可能 panic、或有调用前提的公共接口，应补 `Errors`、`Panics`、`Safety` 等对应说明
- 示例代码或文档示例默认不要用 `unwrap()` 掩盖错误路径

## 3. 分层与模块约束

### 3.1 `termquill-kernel`

- 负责：agent loop、session、approval、event、provider/tool 契约
- 不负责：DeepSeek 私有协议细节、具体 HTTP 端点拼装、UI 展示逻辑
- 公共类型修改时，必须先判断是否仍适合未来多 provider 复用

### 3.2 `termquill-provider-deepseek`

- 负责：DeepSeek 请求构造、SSE 解析、retry、reasoning replay、Beta 扩展点
- 不要把 kernel 可以表达的通用能力重新定义一套
- provider 特有 quirk 应集中表达，不要散落在多个 crate 的临时判断里

### 3.3 `termquill-tools-builtin`

- 工具必须有稳定 `ToolSpec`
- 写工具默认提供 `preview`，尤其是 `write_file`、`edit_file`、`bash`
- 所有路径操作必须限制在 workspace root 内
- workspace confinement 必须基于 canonicalized root 和路径组件判断；文件、目录和父目录链上的 symlink 指向 workspace 外时必须拒绝
- 工具失败必须结构化返回，不能 panic

### 3.4 `termquill-tui`

- 优先分离“状态模型”和“渲染”
- `app.rs` 保持 `AppState` façade、字段定义、bootstrap、顶层 key routing 和跨状态编排；具体状态流维护在 `app/input_flow.rs`、`app/slash_flow.rs`、`app/modal_flow.rs`、`app/config_flow.rs`、`app/session_flow.rs`、`app/timeline_flow.rs`、`app/tool_focus.rs`、`app/approval_flow.rs`、`app/worker_bridge.rs`、`app/command_dispatch.rs`
- `app/tests/*_tests.rs` 必须和对应 flow 同域维护；新增 TUI 状态机测试不要再回填到 `app.rs` 的 inline test module
- `app/formatting.rs` 只放跨 flow 复用的无副作用格式化 helper，单 flow 私有 helper 优先留在对应 flow 模块
- renderer 优先读取 ViewModel 或明确 render options，不要为了展示逻辑直接扩散完整 `AppState` 依赖
- setup/config 状态模型维护在 `setup.rs` / `config_panel.rs`，`app.rs` 只保留入口协调、持久化和跨状态行为
- `runner.rs` 只作为 worker façade 和必要 re-export；worker protocol、spawn 装配、运行 loop、event/approval bridge、session/compaction flow 分别维护在 `runner/*`，runner 状态机测试维护在 `runner/tests/*`
- `ui.rs` 只作为 `ui/*` 模块入口和必要 re-export；顶层 shell layout 放在 `ui/shell.rs`
- theme、geometry、text、primitives 等共享 renderer 底座分别维护在 `ui/theme.rs`、`ui/geometry.rs`、`ui/text.rs`、`ui/primitives.rs`
- markdown、timeline、tool card、approval、main screen、setup/config、modal 等渲染块分别维护在对应 `ui/*` 模块，不再回填到 `ui.rs`
- markdown 功能增强只能落在 `ui/markdown.rs` 和 `MarkdownRenderOptions`；assistant timeline、tool preview、approval modal 不应各自维护解析分支
- 新增或修改快捷键 / slash command 时必须同步 `commands.rs` metadata、info rail controls、README 和状态转换测试
- 能用 TUI 焦点和快捷键自然表达的能力，不优先新增 slash command；hidden command 必须有明确退场计划和删除条件

### 3.5 `termquill-runtime`

- 负责 TUI、CLI 和未来入口共享的 provider、tool registry、run options 装配
- 入口层不应各自硬编码 DeepSeek provider、built-in tools 或 MCP 注册流程
- runtime 只依赖 kernel、provider、tools、MCP 等下层 crate；kernel 不得反向依赖 runtime

## 4. 数据与状态规则

### 4.1 Session / Control

- `SessionLogEntry` 与 `ControlEntry` 的职责要清晰
- continuation state、response handle、background task handle 等控制面信息必须能被持久化
- resume 后会影响下一轮 request 的 durable control state 必须通过 `Session` 查询方法恢复，不要让调用侧手写扫描逻辑
- 不要把会影响恢复正确性的状态只存在 provider 私有字段中

### 4.2 Deterministic / Cache-safe

- 任何会影响 provider 缓存命中的请求材料，都要尽量稳定
- 避免无必要的动态 header、随机排序、临时字段抖动
- 构造 JSON/schema 时尽量保持确定性

### 4.3 序列化与兼容性

- 写入 session log、control state、配置文件、provider wire payload 的结构体，要显式设计 `serde` 行为
- 涉及外部命名约定时，优先显式使用 `#[serde(rename_all = "...")]` 或 `#[serde(rename = "...")]`，不要把 Rust 字段名当成隐式协议
- 可选或后续可能新增的字段，优先补 `#[serde(default)]` 或默认函数，保证旧数据可继续反序列化
- 只在确实需要省略输出时使用 `skip_serializing_if`；不要让“省略字段”破坏反序列化兼容
- append-only 日志、持久化控制态和用户配置默认按“可追加演进”设计，避免轻易引入会卡死旧数据恢复的严格反序列化约束

### 4.4 路径建模

- 涉及文件系统的跨函数、跨 crate 接口，优先使用 `&Path`、`PathBuf`、`AsRef<Path>`，不要用裸 `String` 传递路径
- 路径拼接、比较、归一化使用 `Path` / `PathBuf` / `Component` API，不要手写字符串拼接
- 做 workspace confinement、前缀校验和逃逸判断时，优先基于路径组件和规范化结果判断，不要只做字符串前缀比较

### 4.5 Preview / Diff

- 涉及写操作的审批预览，应尽量提供按文件组织的 diff
- 大 diff 需要考虑截断、导航、当前 hunk 标记等可读性

## 5. 测试规则

### 5.0 新增业务代码必须带单测

- 新增业务代码时，必须同时新增或补齐对应单元测试
- 这里的“业务代码”包括但不限于：
  - 新的 provider 映射与请求构造逻辑
  - 新的 tool 行为
  - 新的 session / approval / event 状态转换
  - 新的 TUI 交互状态与分支逻辑
- 如果某段新增逻辑确实不适合写单测，需要在变更说明里明确写出原因，并至少提供同层级的可执行验证
- 不能以“后面再补”为默认做法把新增业务逻辑无测试落入主分支

### 5.1 什么时候必须加测试

以下情况默认要补测试：

- 公共契约变更
- 新增业务代码
- provider 请求/响应映射变化
- tool 行为变化
- session 恢复、approval、TUI 状态机变化

### 5.2 测试优先级

- 先补最能防回归的单元测试
- 跨层链路再补集成测试
- 对 TUI，优先测状态转换，而不是只测渲染文本

### 5.3 测试目录规范

单元测试必须和业务代码物理分离。业务文件中只保留测试模块声明，不再回填 inline `mod tests { ... }` 测试实现。

默认布局：

- `src/foo.rs` 的测试放在 `src/tests/foo_tests.rs`，父文件声明 `#[path = "tests/foo_tests.rs"] mod tests;`
- `src/ui/foo.rs` 的测试放在 `src/ui/tests/foo_tests.rs`，父文件声明 `#[path = "tests/foo_tests.rs"] mod tests;`
- `src/lib.rs` 的测试放在 `src/tests/lib_tests.rs`，父文件声明 `#[path = "tests/lib_tests.rs"] mod tests;`
- `src/main.rs` 的测试放在 `src/tests/main_tests.rs`，父文件声明 `#[path = "tests/main_tests.rs"] mod tests;`

已按状态域拆分的 façade 模块继续使用专属目录：

- `crates/termquill-tui/src/app/tests/*_tests.rs`
- `crates/termquill-tui/src/runner/tests/*_tests.rs`

测试共享代码放置规则：

- 同一个状态域共享 fixture 使用 `tests/common.rs`
- 单个模块专用 test helper 使用 `tests/<module>_test_support.rs`
- 业务文件不要保留 test-only helper 实现；只能保留 `#[cfg(test)]` 下的测试模块声明

禁止新增：

- inline `mod tests { ... }`
- `module/tests.rs`
- `module/test_support.rs`
- crate root 下的裸 `src/tests.rs`

## 6. 配置与兼容性

- 新增配置项时，必须考虑默认值、旧配置兼容性和 README 更新
- provider 配置项要尽量放在 provider 自己的配置块里
- 不要把仅供调试的开关包装成默认用户能力

## 7. 提交前最低要求

代码变更完成后，默认至少保证：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

如果只改文档，可以不跑全量 gate，但至少确认：

- 链接有效
- 路径真实存在
- 命令示例和当前工程一致
