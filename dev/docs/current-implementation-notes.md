# Sigil 当前实现快照

[English](current-implementation-notes.en.md)

本文记录当前仓库实现事实，服务开发者同步。面向普通用户的默认英文入口文档是根目录 `README.md`，中文入口是 `README.zh-CN.md`。用户文档按语言放在 `docs/en/*` 和 `docs/zh-CN/*`。

## 仓库结构

```text
sigil/
  assets/logo/                 # README、release 和 package listing 使用的 logo / wordmark PNG 资产
  crates/
    sigil-kernel/              # 通用 agent 内核与领域契约
    sigil-provider-deepseek/   # DeepSeek provider 实现
    sigil-provider-openai-compat/ # OpenAI-compatible provider 实现
    sigil-tools-builtin/       # 内置工具
    sigil-code-intel/          # LSP client、Tree-sitter fallback 与 code intelligence tools
    sigil-mcp/                 # stdio MCP client 与工具适配
    sigil-runtime/             # 入口共享的 provider / tool / run options 装配
    sigil-http/                # HTTP/SSE adapter 配置 DTO 与后续 server 边界
    sigil/                     # `sigil` binary：默认启动 TUI，子命令用于自动化与调试
    sigil-tui/                 # 第一用户入口的 TUI 状态、渲染和 runner
  docs/                        # 用户文档
  site/                        # GitHub Pages 静态站点源码
  dev/governance/              # 开发约束、代码规范、工程规范
  dev/docs/                    # 架构、路线图与实现快照
  sigil.toml                   # 本地配置文件，默认被 .gitignore 忽略
```

## 当前能力基线

- `sigil-kernel` 统一承载 provider、tool、session、approval、permission、event、memory、compaction 和 task orchestration 契约。
- `sigil-runtime` 统一装配 provider、内置工具、MCP 工具、run options 和 role-scoped task agents。
- `sigil-provider-deepseek` 支持 DeepSeek 流式对话、工具调用、reasoning replay、usage、pricing、Beta endpoint、prefix 和 FIM 专项入口。
- `sigil-provider-openai-compat` 支持 OpenAI-compatible Chat Completions 流式对话、工具调用、usage、base URL、organization/project header 和模型配置。
- `sigil-tools-builtin` 提供文件读写、编辑、删除、多文件 change set apply、搜索、目录枚举和 shell 执行。
- `sigil-code-intel` 提供可选 LSP / Tree-sitter 代码智能，包括符号、定义、引用、诊断、code action 查询，以及需要审批 diff 的 code action / rename edit 工具。
- `sigil-mcp` 支持 stdio MCP server、`initialize`、`tools/list`、`tools/call`、read-only `resources/list` / `resources/read`、read-only `prompts/list` / `prompts/get`、`roots/list`、elicitation handler、progress/listChanged runtime events、lazy activation 和 trust enforcement。
- `sigil-http` 当前承载 HTTP/SSE adapter server config DTO 与边界测试；后续负责 HTTP routing、auth、SSE serialization 和 runtime session/run registry，不依赖 `sigil-tui`，不复制 agent loop。
- `sigil` 提供 `sigil` binary：无子命令时直接启动 TUI；`run` 自动化入口和 `doctor` 本地诊断入口保留为显式子命令；`prefix` / `fim` 保留为隐藏调试或 provider 专项入口，不作为普通用户主心智。
- `sigil --version` 输出 package version、git commit、target 和 profile，用于安装后 smoke、release archive 验证和问题定位。
- `sigil-tui` 承载第一用户入口的 TUI 实现，包括 chat/composer、slash selector、Quick Setup、`/config`、`/doctor`、`/new`、`/resume`、`/plan`、审批 modal、tool activity、diff preview、session 恢复、task 状态展示、context compaction、markdown code block 高亮和 code intelligence 状态展示。

## TUI 模块边界

`crates/sigil-tui/src/app.rs` 保持 `AppState` façade：字段定义、bootstrap、顶层 key routing 和跨状态编排留在这里。具体状态流维护在 `src/app/*`：

- `input_flow.rs`
- `slash_flow.rs`
- `modal_flow.rs`
- `config_flow.rs`
- `setup_flow.rs`
- `session_flow.rs`
- `timeline_flow.rs`
- `tool_card_interaction.rs`
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

`apply_changeset` 支持一次审批后的多文件 create / update / delete。执行前会统一校验 workspace path、hash、mtime、snippet、symlink 和 binary 文本边界；validation 失败时不写任何文件。执行成功或 partial failure 时会写 `.sigil/changesets/<id>/preview.diff` 与 `reverse.diff` artifact，并在结构化结果中返回 artifact path、hash、stats 和 apply status；model-visible content 只返回摘要，不直接返回完整 diff。

审批卡片固定为 `Summary / Files / Diff / Actions` 四区。`write_file`、`edit_file`、`delete_file` 和 `apply_changeset` 的 diff 预览支持按文件切换、按 hunk 跳转和 diff mode 切换。`apply_changeset` 审批会额外显示 change set id、整体 risk、每文件 action/risk，以及基于文件类型的格式化建议。

## Session 与 Control State

默认 session log 位于：

```text
.sigil/sessions/
```

当前实现采用 append-only JSONL：

- session identity 跟随 durable log 恢复，不盲目回退到当前配置里的 provider/model。
- response handle、provider continuation state、prefix snapshot、compaction record 和 usage snapshot 都写入 append-only control log。
- tool approval、execution lifecycle 和 reasoning delta 会追加到 control log。
- task run、plan、step、child-session 和 subagent approval-route 摘要会追加到 control log，并通过 `Session::task_state_projection` 投影。
- skill index snapshot 和 skill loaded 摘要已有 `SkillIndexCaptured` / `SkillLoaded` control entry，并通过 `Session::skill_state_projection` 投影；runtime discovery 已支持 `.sigil/skills`、`.sigil/agents`、`.claude/skills`、`.claude/agents` 和可选 user skills，包含 frontmatter 解析、shadowing warning、hash 与 invalid path/name 跳过；internal read-only `load_skill` 会按 enabled/trusted/model-invocable 与 permission policy 校验后只读取 skill entrypoint，将 skill body 作为当前 run 的 transient context 注入并追加 `SkillLoaded` control entry；TUI `/config` Skills section 会展示 discovered skill 列表、model/user invocable、run mode、trust、source/hash、路径和 tool scope，并可通过 footer 发起 load 或带参数 invoke；plugin manifest discovery 已支持 `.sigil/plugins/<id>/plugin.toml`，TUI `/config` Plugins section 会展示 manifest path、id/name/version、skills/hooks/MCP commands、hash 和执行影响，并通过 footer approve/deny 追加 `PluginManifestCaptured` 与 `PluginTrustDecision` control entries；user direct invocation 的非用户消息注入和 child-session 调度仍是后续 P1 阶段。
- terminal task handle/status/output preview 摘要有独立 control entry 和 `Session::terminal_task_projection`；terminal tool metadata 会被同步成 append-only `TerminalTask` control entry，TUI 会把它们恢复成 activity card，在 info rail 展示 running terminal count，并支持对 focused running terminal card 通过 `Alt-X` 二次确认走 worker `terminal_cancel` 路径取消，同时保留 execution audit entry。
- `sigil-tools-builtin` 已有 terminal process manager：默认 non-PTY 输出写入 `.sigil/tasks/<task-id>/{meta.json,output.log,stdout.log,stderr.log}`，支持 bounded read、status 和 cooperative cancel；显式 `terminal_start` `pty=true` 会走 `portable-pty` backend，专用 blocking read thread 写 combined artifact log，并支持有界队列 `terminal_input`、`terminal_resize` 和 cancel。单次 terminal input 上限为 8 KiB，permission/audit 只记录 task id 与 input bytes，不记录 stdin 原文；non-PTY task 的 input/resize 仍返回结构化 unsupported。
- 已开始但没有终态的工具执行在恢复时标记为 `interrupted`。
- 悬空 tool call 会投影为结构化 `interrupted` tool result。
- 文件变更工具的历史结果卡片会随 session restore 恢复。
- compaction 只追加 `CompactionApplied` control 记录，不改写旧历史。
- hard threshold 自动 compaction 只在 run 回到 idle 后触发，不抢占当前流式执行。

恢复后下一轮 request 会恢复最新匹配 provider 的 response handle。当前会话身份不会因为 `/config` 保存默认 provider/model 而被静默改写。

计划任务在恢复时不会自动重放。当前 session 存在未完成 task 时，composer 普通输入会作为 continuation guidance 触发 `ContinueTask`；如果只剩 completed/cancelled task，普通输入会回到 chat-first 新对话。`/plan continue` 仍可作为无额外 guidance 的显式继续入口。worker 会从 durable task projection 继续最近一个未完成 task，并跳过已完成步骤；如果显式继续时只有 completed/cancelled task，会返回对应 terminal 状态说明。

## Task Planning 当前实现

计划任务通过 TUI `/plan <任务>` 进入；当前 session 存在未完成 task 时，普通 composer 输入会转成 `ContinueTask` 尝试，并作为 continuation guidance 注入本次 executor/subagent step prompt。没有未完成 task context 时，普通输入仍走 chat-first。普通 chat 没有可直接调用的 `task` / `subagent` launcher；如果模型误调这些工具，agent loop 会返回指导性 tool result，要求通过 `/plan` 和 `task_plan_update` step role 表达 delegated work，而不会伪造子任务执行。worker protocol 使用 `SubmitTask` / `ContinueTask` 命令和 `TaskRunStarted` / `TaskRunFinished` 消息；task run / step / child-session control entry 也会通过实时 `RunEvent::Control` 同步到 TUI。Info rail 从 durable task control entries 渲染 task 状态、最新 plan 版本、完成进度、当前或最近失败步骤，以及当前 plan 的步骤摘要。步骤摘要使用状态化 marker 和行文本颜色：running 高亮，completed 绿色，failed/blocked 红色，cancelled/interrupted 金色，pending 弱化。

`sigil-kernel::SequentialTaskOrchestrator` 先运行 planner role，通过 internal `task_plan_update` tool 接收 plan update，再顺序执行 steps。Executor step 在 parent session 中运行，但 step context 作为 transient request context 注入，不会变成普通 user history。Subagent read/write step 在 child session 中运行，parent session 记录 child-session lifecycle link，并为 child tool approval 与 MCP elicitation 写 route 摘要。

Step 执行中出现过普通 tool error 时，如果 agent 已经读取错误并给出最终回答，orchestrator 会把 step 视为已恢复并继续后续步骤，同时在 `TaskStepEntry.reason` 保留 `recovered tool error` 摘要。Max turns、interrupted tool call、审批拒绝和权限类 tool error 仍会阻断 task。

Role-specific provider 和 run options 由 `sigil-runtime` 装配。Planner 与 subagent-read 默认使用只读 scoped tool registry；executor 默认使用完整 registry；subagent-write 只有在 `[task].allow_write_subagents = true` 时使用完整 registry。`ScopedToolRegistry` 会同时限制 specs、preview、execute、permission hooks 和 egress hooks。

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
- `[task]`
- `[providers.*]`
- `[[mcp_servers]]`

DeepSeek provider 配置位于 `[providers.deepseek]`。OpenAI-compatible provider 配置位于 `[providers.openai_compat]`，`agent.provider` 使用 `openai_compat`，并兼容 `openai-compatible` / `openai_compatible` 输入别名。运行时环境变量 override 在 provider config 层解析；DeepSeek 使用 `SIGIL_API_KEY` / `DEEPSEEK_API_KEY`，OpenAI-compatible 使用 `SIGIL_OPENAI_COMPATIBLE_API_KEY` / `OPENAI_API_KEY`。

TUI `/config` 只暴露 provider 高频项、permissions、memory、compaction、code intelligence 控制项、terminal mouse/OSC52/scroll sensitivity 兼容性设置、Skills browser、Plugins trust review 和 MCP server 常用字段。它可以在 `deepseek` 与 `openai_compat` 间切换；DeepSeek FIM 显示为 provider 专项高级项，OpenAI-compatible 下标记为不支持。低频 provider 专项字段继续保留给配置文件和环境变量。Skills browser 使用当前 workspace/config 执行 discovery，支持 PgUp/PgDn 切换 skill，展示 trust/source/hash/run mode/invocable/tool scope/path patterns，并通过 footer load/invoke 生成受 runtime `load_skill` policy 约束的请求。Plugins review 使用当前 session control projection 解析既有 trust decision，支持 PgUp/PgDn 切换 plugin，并通过 approve/deny 将 manifest snapshot 与 trust decision 追加到当前 session JSONL。

`sigil doctor` 与 TUI `/doctor` 复用 runtime 诊断逻辑，检查配置加载、workspace、session log、provider/auth 来源、MCP command/trust、code intelligence LSP plan、terminal `TERM`、终端 profile/layers，以及 mouse/OSC52/scroll sensitivity 兼容性设置。诊断只展示 secret 来源，不输出 secret 值。

## Packaging 当前实现

当前支持两条本地分发验证路径：

- 源码安装：`cargo install --path crates/sigil --locked`
- 本地 release archive：`scripts/build-release-archive.sh`

release archive 脚本会用 release mode 构建 `sigil`，注入 git commit、target 和 profile 构建元数据，对 built binary 运行 `sigil --version` 与 `sigil doctor` smoke，然后输出 `dist/sigil-<version>-<target>.tar.gz` 和对应 `.sha256`。
archive payload 包含 `sigil` binary、README、`assets/logo/*` 和安装文档，确保 README 中的仓库相对 logo 路径在解压后仍然有效。

GitHub release workflow 位于 `.github/workflows/release.yml`。推送 `v*` tag 或手动指定既有 tag 时，它会在 Linux、macOS、Windows runner 上构建 release archives，生成 GitHub artifact provenance attestations，汇总 checksum，按 Conventional Commit 生成 release notes，渲染 `sigil.rb` Homebrew formula asset，并用 `gh release create` 发布 GitHub release。流程说明维护在 [`release-process.md`](release-process.md)。

独立 Homebrew tap 仓库同步和自更新仍是后续工作。

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
- TUI eager MCP 后台激活；单个 server 失败不阻断普通任务
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

staged gate 会先读取 staged source snapshot，再计算新增行覆盖率。可识别的 `enum`、`struct` 和 `union` 声明行不会进入可执行行分母，即使 LCOV 为这些行生成了 zero-count 记录。

staged coverage 脚本的 diff 分类、LCov 解析和新增行覆盖率计算有独立 Python 单测：

```bash
python3 -m unittest scripts/test_check_staged_coverage.py
```

## 文档分工

- `README.md`：英文用户第一入口。
- `README.zh-CN.md`：中文用户第一入口。
- `docs/en/*`：英文用户文档。
- `docs/zh-CN/*`：中文用户文档。
- `site/*`：GitHub Pages 静态站点源码，由 `.github/workflows/pages.yml` 发布。
- `assets/logo/*`：README、release 页面、package listing 和 social preview 使用的品牌 PNG 资产。
- `dev/governance/*`：直接约束当前工程协作的规范文档。
- `dev/docs/current-implementation-notes.en.md`：英文实现快照。
- `dev/docs/*`：架构、路线图、设计演进和实现快照。
- `AGENTS.md`：仓库内 agent 协作说明。
