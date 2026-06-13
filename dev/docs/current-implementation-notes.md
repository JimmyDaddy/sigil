# Sigil 当前实现快照

[English](current-implementation-notes.en.md)

本文记录当前仓库实现事实，服务开发者同步。面向普通用户的默认英文入口文档是根目录 `README.md`，中文入口是 `README.zh-CN.md`。用户文档按语言放在 `docs/en/*` 和 `docs/zh-CN/*`。

## 仓库结构

```text
sigil/
  crates/
    sigil-kernel/              # 通用 agent 内核与领域契约
    sigil-provider-deepseek/   # DeepSeek provider 实现
    sigil-provider-openai-compat/ # OpenAI-compatible provider 实现
    sigil-tools-builtin/       # 内置工具
    sigil-code-intel/          # LSP client、Tree-sitter fallback 与 code intelligence tools
    sigil-mcp/                 # stdio MCP client 与工具适配
    sigil-runtime/             # 入口共享的 provider / tool / run options 装配
    sigil-cli/                 # 薄 CLI 启动器与调试入口
    sigil-tui/                 # 第一用户入口
  docs/                        # 用户文档
  dev/governance/              # 开发约束、代码规范、工程规范
  dev/docs/                    # 架构、路线图与实现快照
  sigil.toml                   # 本地配置文件，默认被 .gitignore 忽略
```

## 当前能力基线

- `sigil-kernel` 统一承载 provider、tool、session、approval、permission、event、memory 和 compaction 契约。
- `sigil-runtime` 统一装配 provider、内置工具、MCP 工具和 run options。
- `sigil-provider-deepseek` 支持 DeepSeek 流式对话、工具调用、reasoning replay、usage、pricing、Beta endpoint、prefix 和 FIM 专项入口。
- `sigil-provider-openai-compat` 支持 OpenAI-compatible Chat Completions 流式对话、工具调用、usage、base URL、organization/project header 和模型配置。
- `sigil-tools-builtin` 提供文件读写、编辑、删除、搜索、目录枚举和 shell 执行。
- `sigil-code-intel` 提供可选 LSP / Tree-sitter 代码智能，包括符号、定义、引用、诊断、code action 查询，以及需要审批 diff 的 code action / rename edit 工具。
- `sigil-mcp` 支持 stdio MCP server、`initialize`、`tools/list`、`tools/call`、read-only `resources/list` / `resources/read`、read-only `prompts/list` / `prompts/get`、`roots/list`、elicitation handler、progress/listChanged runtime events、lazy activation 和 trust enforcement。
- `sigil-cli` 当前公开 `run` 自动化入口和 `doctor` 本地诊断入口；`prefix` / `fim` 保留为调试或 provider 专项入口，不作为普通用户主心智。
- `sigil-tui` 是第一用户入口，承载 chat/composer、slash selector、Quick Setup、`/config`、`/doctor`、`/resume`、审批 modal、tool activity、diff preview、session 恢复、context compaction、markdown code block 高亮和 code intelligence 状态展示。

## TUI 模块边界

`crates/sigil-tui/src/app.rs` 保持 `AppState` façade：字段定义、bootstrap、顶层 key routing 和跨状态编排留在这里。具体状态流维护在 `src/app/*`：

- `input_flow.rs`
- `slash_flow.rs`
- `modal_flow.rs`
- `config_flow.rs`
- `setup_flow.rs`
- `session_flow.rs`
- `timeline_flow.rs`
- `tool_focus.rs`
- `approval_flow.rs`
- `mouse_flow.rs`
- `worker_bridge.rs`
- `command_dispatch.rs`

状态流测试维护在 `crates/sigil-tui/src/app/tests/*_tests.rs`。新增 TUI 行为时优先落到对应 flow 和同域测试，不要把状态机重新堆回 `app.rs`。

`runner.rs` 是 worker façade。worker protocol、spawn 装配、运行 loop、event/approval bridge、session/compaction flow 维护在 `runner/*`，测试维护在 `runner/tests/*`。

`ui.rs` 是 renderer 模块入口。shell layout、theme、geometry、text、timeline、tool card、markdown、approval、setup/config、modal 等渲染块维护在 `ui/*`。

## 用户交互状态

TUI 当前保持 chat-first：

- inline viewport 占满当前终端可视区。
- 左侧主区域展示 live transcript 和底部 composer。
- 右侧 `Info rail` 展示 `Session / Permissions / Agents / LSP / Usage / Controls`。
- 窄终端自动收起 info rail，优先保证 chat/composer 可用。
- 启动恢复旧会话时，会把完整 scrollback 分批 seed 到 terminal scrollback，避免长会话集中在单帧重放。
- prompt 提交后 composer 清空并保持可见。
- 主屏不要求用户用 `Tab` 在卡片间切焦点；`Shift-Tab` 轮换并持久化默认 `allow / ask / deny` 权限模式。

运行态提示只做渲染层投影，不写回 durable transcript。live phase 只保留在运行态和事件流里。

## Tool Activity 与 Diff

Tool result 默认以独立 activity 展示。当前 renderer 会区分常见内置工具：

- `read_file`
- `ls`
- `glob`
- `grep`
- `bash`
- `write_file`
- `edit_file`
- `delete_file`
- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`

简单只读 `rg / grep / fd / find` bash 命令会识别为 `Searched`。其他结构化 payload 走树形 fallback，不直接 dump 原始 JSON 或 call id。

`write_file`、`edit_file` 和 `delete_file` 的结果 activity 默认展开执行时捕获的 bounded unified diff。diff 行显示旧/新行号，activity 正文跳过重复 hunk header，并在文件头汇总 hunk 数。大 diff 会提示 `diff truncated` 和隐藏行数。

审批卡片固定为 `Summary / Files / Diff / Actions` 四区。`write_file`、`edit_file` 和 `delete_file` 的 diff 预览支持按文件切换、按 hunk 跳转和 diff mode 切换。

## Session 与 Control State

默认 session log 位于：

```text
.sigil/sessions/
```

当前实现采用 append-only JSONL：

- session identity 跟随 durable log 恢复，不盲目回退到当前配置里的 provider/model。
- response handle、provider continuation state、prefix snapshot、compaction record 和 usage snapshot 都写入 append-only control log。
- tool approval、execution lifecycle 和 reasoning delta 会追加到 control log。
- 已开始但没有终态的工具执行在恢复时标记为 `interrupted`。
- 悬空 tool call 会投影为结构化 `interrupted` tool result。
- 文件变更工具的历史结果卡片会随 session restore 恢复。
- compaction 只追加 `CompactionApplied` control 记录，不改写旧历史。
- hard threshold 自动 compaction 只在 run 回到 idle 后触发，不抢占当前流式执行。

恢复后下一轮 request 会恢复最新匹配 provider 的 response handle。当前会话身份不会因为 `/config` 保存默认 provider/model 而被静默改写。

## 配置与 Provider

根配置结构由 `sigil-kernel` 解析：

- `[workspace]`
- `[session]`
- `[agent]`
- `[permission]`
- `[memory]`
- `[compaction]`
- `[code_intelligence]`
- `[terminal]`
- `[providers.*]`
- `[[mcp_servers]]`

DeepSeek provider 配置位于 `[providers.deepseek]`。OpenAI-compatible provider 配置位于 `[providers.openai_compat]`，`agent.provider` 使用 `openai_compat`，并兼容 `openai-compatible` / `openai_compatible` 输入别名。运行时环境变量 override 在 provider config 层解析；DeepSeek 使用 `SIGIL_API_KEY` / `DEEPSEEK_API_KEY`，OpenAI-compatible 使用 `SIGIL_OPENAI_COMPATIBLE_API_KEY` / `OPENAI_API_KEY`。

TUI `/config` 只暴露 provider 高频项、permissions、memory、compaction、code intelligence 控制项、terminal mouse/OSC52/scroll sensitivity 兼容性设置和 MCP server 常用字段。它可以在 `deepseek` 与 `openai_compat` 间切换；DeepSeek FIM 显示为 provider 专项高级项，OpenAI-compatible 下标记为不支持。低频 provider 专项字段继续保留给配置文件和环境变量。

`sigil doctor` 与 TUI `/doctor` 复用 runtime 诊断逻辑，检查配置加载、workspace、session log、provider/auth 来源、MCP command/trust、code intelligence LSP plan、terminal `TERM`、终端 profile/layers，以及 mouse/OSC52/scroll sensitivity 兼容性设置。诊断只展示 secret 来源，不输出 secret 值。

## MCP 当前实现

MCP server 通过 `[[mcp_servers]]` 配置接入。当前支持：

- stdio 启动
- `initialize`
- `tools/list`
- `tools/call`
- `resources/list`
- `resources/read`
- `prompts/list`
- `prompts/get`
- provider-visible 名称清洗、截断和 hash 去重
- `roots/list`
- `elicitation/create`
- `notifications/progress`
- `notifications/*/list_changed`
- lazy activation
- required / optional server 失败策略
- trust class
- per-server approval default
- egress logging
- secret egress 阻断
- pinned identity 校验

`resources/list` / `resources/read` 和 `prompts/list` / `prompts/get` 只在 server initialize capabilities 声明对应 capability 时注册为 provider-visible 只读工具。它们复用 MCP trust policy、permission subjects、egress logging 和 secret egress 阻断，不会自动注入 system prompt。

MCP tool/resource/prompt 输出会先脱敏再做默认输出限额，并在 `ToolResultMeta` 中写入 truncation 与 MCP server/tool/trust/operation metadata。

`roots/list` 只暴露入口已解析的 workspace root。`notifications/progress` 进入 TUI live panel，不写重复 timeline；`notifications/tools|resources|prompts/list_changed` 会标记 server stale，并在 worker 空闲边界刷新该 server 的 provider-visible tools。

TUI elicitation 通过 modal 让用户确认 flat primitive object 字段；非交互默认 runtime 明确返回 unsupported。elicitation 决策写入 append-only control state，但不保存用户输入值。

## Code Intelligence 当前实现

Code intelligence 默认关闭。开启后 runtime 注册只读工具：

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`
- `code_actions`

同时注册需要审批 diff 的写工具：

- `code_action`
- `code_rename`

`code_intelligence.discovery.enabled = true` 时，会按 workspace marker / 文件扩展名自动发现 Rust、TypeScript/JavaScript、Python、Go，并只把 PATH 上可用的内置 allowlist server 纳入启动计划。Rust 项目默认使用 `rust-analyzer`，没有可用 LSP server 时回退到 Tree-sitter Rust outline / syntax diagnostics。

工具结果同时受 `max_results` 和 `max_payload_bytes` 约束，截断信息写入 metadata。

## 开发验证

代码变更默认按工程规范跑相关 gate：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

覆盖率门禁默认要求 workspace 单测行覆盖率 `>= 96%`，统一由 `scripts/coverage.sh` 执行。

本地 pre-commit hook：

```bash
git config core.hooksPath .githooks
```

单独运行 staged coverage 检查：

```bash
./scripts/check-staged-coverage.py
```

staged coverage 脚本的 diff 分类、LCov 解析和新增行覆盖率计算有独立 Python 单测：

```bash
python3 -m unittest scripts/test_check_staged_coverage.py
```

## 文档分工

- `README.md`：英文用户第一入口。
- `README.zh-CN.md`：中文用户第一入口。
- `docs/en/*`：英文用户文档。
- `docs/zh-CN/*`：中文用户文档。
- `dev/governance/*`：直接约束当前工程协作的规范文档。
- `dev/docs/current-implementation-notes.en.md`：英文实现快照。
- `dev/docs/*`：架构、路线图、设计演进和实现快照。
- `AGENTS.md`：仓库内 agent 协作说明。
