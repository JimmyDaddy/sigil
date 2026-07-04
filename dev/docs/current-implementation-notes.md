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
    sigil-provider-anthropic/  # Anthropic provider 实现
    sigil-provider-gemini/     # Gemini provider 实现
    sigil-tools-builtin/       # 内置工具
    sigil-code-intel/          # LSP client、Tree-sitter fallback 与 code intelligence tools
    sigil-mcp/                 # stdio MCP client 与工具适配
    sigil-runtime/             # 入口共享的 provider / tool / run options 装配
    sigil-http/                # HTTP/SSE adapter DTO、auth/SSE helper 与后续 server 边界
    sigil/                     # `sigil` binary：默认启动 TUI，子命令用于自动化与调试
    sigil-tui/                 # 第一用户入口的 TUI 状态、渲染和 runner
  docs/                        # 用户文档
  site/                        # GitHub Pages 静态站点源码
  dev/governance/              # 开发约束、代码规范、工程规范
  dev/docs/                    # 架构、路线图、RFC 与实现快照
  dev/docs/archive/            # 已过期的一次性验证报告和历史资料
  sigil.toml                   # 本地配置文件，默认被 .gitignore 忽略
```

## 当前能力基线

- `sigil-kernel` 统一承载 provider、tool、session、approval、permission、event、memory、compaction 和 task orchestration 契约。
- `sigil-runtime` 统一装配 provider、内置工具、MCP 工具、run options、role-scoped task agents 和 Context V0 source provider contract / hard-cap enforcement，并提供 provider-neutral 的配置草稿、状态请求/刷新任务、context-window helper、agent-message route helper、session-control append helper，以及隐藏 DeepSeek prefix / FIM developer debug adapter 给入口层复用；runtime repo/source context 候选会携带 explicit path、exact symbol、source path、weak lexical retrieval 的 score breakdown，safe context assembly 还可消费 code-intel 提供的 warm LSP snapshot，并在 snapshot 缺失或超时时只写 excluded provenance，不阻塞请求。trusted plugin hook output 和 caller-supplied MCP resource text 只能通过 Context V0 source provider adapter 进入 dynamic suffix；untrusted plugin output 与缺少 egress decision 的 external MCP resource 只保留 excluded provenance，不渲染 snippet。
- `sigil-provider-deepseek` 支持 DeepSeek 流式对话、工具调用、reasoning replay、usage、pricing、Beta endpoint、prefix 和 FIM 专项入口。
- `sigil-provider-openai-compat` 支持 OpenAI-compatible Chat Completions 流式对话、工具调用、usage、base URL 和 organization/project header；聊天模型选择来自 `[agent].model`。
- `sigil-provider-anthropic` 支持 Anthropic Messages API 流式对话、工具调用、usage、base URL、版本 header 和输出 token 上限；聊天模型选择来自 `[agent].model`。
- `sigil-provider-gemini` 支持 Gemini 流式对话、工具调用、usage 和 base URL；聊天模型选择来自 `[agent].model`。
- `sigil-tools-builtin` 提供文件读写、编辑、删除、多文件 change set apply、搜索、目录枚举和 shell 执行。
- `sigil-code-intel` 提供可选 LSP / Tree-sitter 代码智能，包括 request-local RepoMapLite source map、符号、定义、引用、诊断、code action 查询，以及需要审批 diff 的 code action / rename edit 工具；RepoMapLite 仍是 request-local / in-memory source map，不是 persistent repo graph。code-intel service 会把真实 LSP symbol / diagnostic / reference 查询结果写入短期 warm cache，并提供只读 snapshot 给 Context V0 调度层，prompt assembly 不会临时启动或查询 LSP。
- `sigil-mcp` 支持 stdio MCP server、`initialize`、`tools/list`、`tools/call`、read-only `resources/list` / `resources/read`、read-only `prompts/list` / `prompts/get`、`roots/list`、elicitation handler、progress/listChanged runtime events、lazy activation 和 trust enforcement。
- `sigil-http` 当前通过 `lib.rs` façade 暴露 HTTP/SSE adapter API，内部按 protocol、config/auth、listener、SSE、DTO、driver、registry 和 OpenAPI schema 拆分；listener 只负责 HTTP framing/auth/registry routing，不依赖 `sigil-tui`，不复制 agent loop。
- `sigil` 提供 `sigil` binary：无子命令时直接启动 TUI；`run` 自动化入口、`doctor` 本地诊断入口和 `serve` HTTP/SSE adapter preflight 入口保留为显式子命令；`serve` 当前只验证 localhost/token defaults 并输出 routing pending 状态，不启动 HTTP listener；`prefix` / `fim` 保留为隐藏调试或 provider 专项入口，通过 `sigil-runtime` debug adapter 调用，不作为普通用户主心智，也不让 binary 直接依赖 provider crate。
- `sigil --version` 输出 package version、git commit、target 和 profile，用于安装后 smoke、release archive 验证和问题定位。
- `sigil-tui` 承载第一用户入口的 TUI 实现，包括 chat/composer、slash selector、Quick Setup、`/config`、`/doctor`、`/new`、`/resume`、`/plan`、审批 modal、tool activity、diff preview、session 恢复、task 状态展示、context compaction、markdown code block 高亮和 code intelligence 状态展示；provider 配置、状态请求、状态刷新任务生命周期和 context-window 解析通过 `sigil-runtime` 的 provider-neutral API 进入，agent message route 和通用 control append 也复用 runtime helper，不直接依赖 provider crate。

## TUI 模块边界

`crates/sigil-tui/src/app.rs` 保持 `AppState` façade：bootstrap、顶层 key routing 和跨状态编排留在这里。运行状态、composer、approval 和 session browser 字段归入 `src/app/state.rs` 的领域 bundle。具体状态流维护在 `src/app/*`：

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

`runner.rs` 是 worker façade。worker protocol、spawn 装配、event/approval bridge、session/compaction flow 维护在 `runner/*`；worker loop 的 scheduler、active run、queue、MCP/provider refresh、agent/task runtime 和 terminal refresh 维护在 `runner/worker_loop/*`，测试维护在 `runner/tests/*`。

`ui.rs` 是 renderer 模块入口。shell layout、theme、geometry、text、timeline、tool card、markdown、approval、setup/config、modal 等渲染块维护在 `ui/*`。

TUI 主题处理集中在 `crates/sigil-tui/src/ui/theme/`。`sigil-kernel` 只保存 `[appearance]`、`ThemeId` 和原始颜色 override 字符串，保持对 `ratatui` 零依赖；`sigil-tui` 将配置解析为 `ThemePalette`，renderer 通过 `AppState` 的 config snapshot 或 `TimelineRenderOptions` 消费语义 token。主题切换只影响 TUI 外观，不写入 session/control log、approval 记录或 provider-visible context。

## 用户交互状态

TUI 当前保持 chat-first：

- inline viewport 占满当前终端可视区。
- 左侧主区域展示 live transcript 和底部 composer。
- 右侧 `Info rail` 展示 `Session / Permissions / Agents / LSP / Usage / Controls`。
- 窄终端自动收起 info rail，优先保证 chat/composer 可用。
- 启动恢复旧会话时，会把完整 scrollback 分批 seed 到 terminal scrollback，避免长会话集中在单帧重放。
- prompt 提交后 composer 清空并保持可见。
- composer 支持常见 readline 风格编辑键，包括当前行首/尾、字符/词移动、词删除、`Ctrl-K/Y` kill/yank、`Ctrl-J` 换行、terminal keyboard enhancement 已启用且可上报 modifier 时的 `Shift-Enter` / `Alt-Enter` 换行，以及 `Ctrl-Z` 恢复最近一次被 `Esc` 清空的非空 draft；paste 通过 bracketed paste 作为文本插入，不会把多行内容解释成提交，大段 paste 只折叠展示但提交完整原文；draft restore 和 paste 折叠都是运行态编辑状态，不写入 durable session/control log。
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

`apply_changeset` 支持一次审批后的多文件 create / update / delete。执行前会统一校验 workspace path、hash、mtime、snippet、symlink 和 binary 文本边界；validation 失败时不写任何文件。执行成功或 partial failure 时会写入 Sigil 用户态 state artifact root 下的 `changesets/<id>/preview.diff` 与 `reverse.diff` artifact，并在结构化结果中返回 artifact label、hash、stats 和 apply status；model-visible content 只返回摘要，不直接返回完整 diff 或 home 绝对路径。

审批卡片固定为 `Summary / Files / Diff / Actions` 四区。`write_file`、`edit_file`、`delete_file` 和 `apply_changeset` 的 diff 预览支持按文件切换、按 hunk 跳转和 diff mode 切换。`apply_changeset` 审批会额外显示 change set id、整体 risk、每文件 action/risk，以及基于文件类型的格式化建议。

permission path 分类使用 primary trust zone + risk overlay 两层信号。`docs`、`dev/docs`、`.sigil` runtime state、project assets、user state/cache 和 external path 先归入对应 `PathTrustZone`；类似 `sigil.toml`、`.env*`、`credentials*`、`secret*`、`secrets*` 的敏感命名会额外产生 `SensitiveName` overlay。这样 `docs/credentials.md` 仍能显示为 workspace docs，但写入风险会按 overlay 升级为 protected，不再依赖单个 `if` 分支顺序在“文档区”和“敏感路径”之间二选一。

shell 审批在进入 policy 前会先做命令意图分析。`cargo check`、`cargo fmt -- --check`、`cargo test`、`scripts/check-touched.sh`、只读 `git`、搜索和列表命令会归入稳定 command family；`cargo check 2>&1`、带 workspace `cd` 的等价命令以及只追加 `tail/head/wc/cat` 的输出过滤命令共享同一 workspace check session grant subject。`/dev/null`、`/dev/stdout` 和 `/dev/stderr` 不作为 external directory subject；`grep --include` 模式不作为路径 subject；workspace-relative missing path 仍按 workspace subject 展示。审批 footer 只把真实 decision 渲染为 action badge，快捷键作为灰色 hint 展示，避免把 `Enter` 或 `Y/N` 误表现为额外按钮。审批、composer agent panel、composer queue panel 和 activity agent list 的核心键位通过集中 key router 解析，footer/keymap 测试覆盖高风险 context。

## Session 与 Control State

默认 session log 位于 Sigil 用户态 state 目录：

```text
<state-root>/workspaces/<workspace-id>/sessions/
```

当前实现采用 append-only JSONL：

- 新写入的 session log 行使用 RFC-0001 `StoredEvent` envelope，包含 `schema_version`、`event_type`、`event_version`、`event_class`、`event_id`、`session_id`、`stream_sequence` 和 `record_checksum`；旧的裸 `SessionLogEntry` 行不重写，读取时稳定 upcast 为 legacy record。读取 v2 stream 会校验同一 stream 内 `session_id` 一致和 `stream_sequence` 严格连续；新 v2 行出现后再混入 legacy 行会 fail closed。
- `Session::load_from_store` 使用 writer-mode loader，持有进程内锁和 OS 文件锁完成 tail validation / recovery、读取和 load-time reconciliation append。尾部损坏会先写入 `.sigil-recovery` quarantine copy 和 recovery intent，再截断并追加 `LogTailRecovered` audit event；read-only loader 只报告损坏，不执行恢复副作用。
- 暂时无法精确归类为 RFC-0001 专用 domain event 的既有 control entry 会以 `SessionEntryRecorded` 兼容事件包装，避免把 changeset、usage 等现有记录冒充为 RFC-0002 mutation 或 RFC-0003 verification 事实。插件与 Agent profile 信任决策会映射为 `ExtensionTrustDecision`；更细的 mutation / verification / workspace trust payload 由后续 RFC 落地。
- `decode_typed_stored_event` 已提供 reducer-facing typed decode seam：mutation、verification、task、agent thread、terminal 和 changeset family 会收敛成强类型 `TypedDomainEvent`，`SessionStreamRecord::typed_domain_event_record` 同时保留 projection cursor；尚未收敛的已知 family 继续作为 `DomainEvent`，unknown critical event 仍 fail closed。
- session identity 跟随 durable log 恢复，不盲目回退到当前配置里的 provider/model。
- response handle、provider continuation state、prefix snapshot、compaction record 和 usage snapshot 都写入 append-only control log。
- 入口层追加普通 control entries 时复用 `sigil-runtime` 的 session-control append helper；helper 负责统一处理内存 session 与直接 JSONL store 写入，TUI runner 不再各自复制该逻辑。
- tool approval 和 execution lifecycle 会追加到 durable control log；流式 reasoning/text delta 属于运行态 live event，不作为事实日志长期保存。
- task run、plan、step、child-session 和 subagent approval-route 摘要会追加到 control log，并通过 `Session::task_state_projection` 投影。
- skill index snapshot 和 skill loaded 摘要已有 `SkillIndexCaptured` / `SkillLoaded` control entry，并通过 `Session::skill_state_projection` 投影；runtime discovery 已支持 `.sigil/skills`、`.sigil/agents`、显式 compatibility source 开启后的 `.claude/skills`、`.claude/agents`、`.reasonix/agents` 和可选 user skills，包含 frontmatter 解析、shadowing warning、hash 与 invalid path/name 跳过；internal read-only `load_skill` 会按 enabled/trusted/model-invocable 与 permission policy 校验后只读取 skill entrypoint，将 skill body 作为当前 run 的 transient context 注入并追加 `SkillLoaded` control entry；TUI `/config` 的 `Agents` section 已改用 workspace-aware `AgentProfileRegistry` 展示 built-in、native、compatibility 与 plugin-contributed profiles，显示 source/kind/trust/effective enabled/user/model、provider/model、tool scope 与 nickname candidates，主 footer 只暴露 trust/disable；底层 enabled/user/model policy decision 继续通过 append-only control entry 表达，但不作为普通用户主流程按钮；`Skills` section 只展示 inline/reusable skill，显示 enabled/trust/source/hash/run mode/tool scope/path patterns，主 footer 只暴露 use；use 会打开可选 instructions 输入框并生成受 runtime `load_skill` policy 约束的请求；TUI slash fallback 也只列出 trusted inline skills，`run_as=child_session` 兼容资源不再作为普通 skill slash row 展示或通过 `/skill-id` 解析启动；composer 起始 `@` 会打开 agent mention selector，只列出 enabled、trusted、user-invocable 的 agent profiles；提交 `@profile <prompt>` 会通过 TUI worker `InvokeAgentProfile` 调用 runtime `AgentToolRuntime::invoke_agent_profile`，以 `AgentInvocationSource::Mention` 启动 foreground child thread，手动入口按 enabled/trusted/user-invocable 校验且不依赖普通 chat delegation hard-gate；native 与 plugin agent profile 都支持 `aliases` / `alias` 和 `slash_names` / `slash_name` metadata，registry 会对 alias/slash 冲突做 deterministic warning 并禁用冲突别名；plugin manifest discovery 已支持 `.sigil/plugins/<id>/plugin.toml`，manifest 可贡献 agents、skills、hooks 与 MCP servers。TUI `/config` Plugins section 会展示 manifest path、id/name/version、agents/skills/hooks/MCP commands、hash 和执行影响，并通过 footer approve/deny 追加 `PluginManifestCaptured` 与 `PluginTrustDecision` control entries；只有 session 中 trust decision 匹配当前 manifest hash 的 plugin 才会把 agent registration 输入 `AgentProfileRegistry`，并以 namespaced profile id 进入 Agents section。
- terminal task handle/status/output preview 摘要有独立 control entry 和 `Session::terminal_task_projection`；terminal tool metadata 会被同步成 append-only `TerminalTask` control entry，TUI 会把它们恢复成 activity card，在 info rail 展示 running terminal count，并支持对 focused running terminal card 通过 `Alt-X` 二次确认走 worker `terminal_cancel` 路径取消，同时保留 execution audit entry。
- `sigil-tools-builtin` 已有 terminal process manager：运行时注入的默认 non-PTY 输出写入 Sigil 用户态 state artifact root 下的 `tasks/<task-id>/{meta.json,output.log,stdout.log,stderr.log}`，model-visible 路径使用 `state/artifacts/tasks/...` label；`terminal_start` 支持 `mode=foreground|background|interactive`，其中非 PTY foreground 会在 agent loop 内等待进程终态，期间只通过 transient `ToolProgress` live event 更新 TUI，同一个 terminal task card 按 task id 原地替换，完成后只向模型返回一次包含 `exit_code`、`verdict`、`duration_ms`、`output_log_ref`、`rerun_not_needed` 的结构化 final tool result；foreground 等待使用独立长任务契约，默认总时限 1800 秒、无输出/无状态变化时限 300 秒，不沿用普通 tool call 的 `AgentConfig.tool_timeout_secs`，模型可通过 `foreground_timeout_secs` 与 `foreground_inactivity_timeout_secs` 显式调整，超时 final facts 会标记 `timeout_kind=total|inactivity`；缺省时 workspace check family（如 `cargo check/test/fmt --check` 与 `check-touched`）走 foreground，其他非 PTY 命令保持 background，`pty=true` 缺省为 interactive。`ToolResult::to_model_content()` 会把 UI-only `output_preview` 和超长 details 字符串替换成 omitted metadata，完整 preview 仍保留在 control/TUI metadata 与 artifact log 中；`terminal_read` 缺省只返回 offset、bytes、status、log 等摘要，不把读到的日志片段塞进模型上下文，只有显式 `include_content=true` 时才返回一页有界 raw output 用于诊断。显式 `terminal_start` `pty=true` 会走 `portable-pty` backend，专用 blocking read thread 写 combined artifact log，并支持有界队列 `terminal_input`、`terminal_resize` 和 cancel。单次 terminal input 上限为 8 KiB，permission/audit 只记录 task id 与 input bytes，不记录 stdin 原文；non-PTY task 的 input/resize 仍返回结构化 unsupported。`bash` 和 `terminal_start` 会注入 `$SIGIL_SCRATCH_DIR`，对应用户态 cache root 下的 workspace scratch 目录，对模型显示为 `cache/tmp`。
- 已开始但没有终态的工具执行在恢复时标记为 `interrupted`。
- 悬空 tool call 会投影为结构化 `interrupted` tool result。
- 文件变更工具的历史结果卡片会随 session restore 恢复。
- compaction 只追加 `CompactionApplied` control 记录，不改写旧历史。
- hard threshold 自动 compaction 只在 run 回到 idle 后触发，不抢占当前流式执行。

恢复后下一轮 request 会恢复最新匹配 provider 的 response handle。当前会话身份不会因为 `/config` 保存默认 provider/model 而被静默改写。

计划任务在恢复时不会自动重放。composer 普通输入始终保持 chat-first，不会因为当前 session 存在未完成 task 自动触发 `ContinueTask`。显式 durable task 继续入口是 `/task continue`，`/plan continue` 不再作为兼容 alias。worker 会从 durable task projection 继续最近一个未完成 task，并跳过已完成步骤；如果显式继续时只有 completed/cancelled task，会返回对应 terminal 状态说明。

## Task Planning 当前实现

计划任务通过 TUI `/task <任务>` 进入；`/plan` 只进入一次性 Plan mode 或直接运行一个 read-only planning prompt，只有 planner 返回包含至少一个可执行 step 的 fenced `sigil-plan-v1` 结构化 draft 时，才会创建 durable task handoff。结构化 draft 包含 summary、steps、target paths、suggested checks、risk 和 notes；未结构化 final text 只作为普通 assistant 输出展示，不创建 Plan ready surface，也不再从 prose 中猜测执行 scope 或 path token 数量。Kernel 仍保留独立于 durable task 的 `PlanApproved` control entry 与 `PlanApprovalProjection`，记录 plan version/hash、批准时间、`ask` 或 `workspace_edits` 权限、scope、过期策略和是否清理 planning context；`workspace_edits` 只覆盖带 required preview 的 workspace file write tool，不放宽 shell/execute、network、MCP 或 Agent spawn。较底层的 `ApprovePlan` permission 路径仍会把 workspace path 保守写入 `PlanApprovalScope.workspace_paths`，但 TUI Plan ready handoff 展示结构化 draft 中显式的 `target_paths` 与 checks，而不是从文本猜测。Agent loop 已在执行阶段接入 approved scope enforcement：当前有效 `PlanApproved(workspace_edits)` 只会把 scope 内 workspace file write 的 `Ask` 降级为 `Allow`，显式 `Deny`、外部目录、无 subject、scope 外路径和非文件写工具仍走原 permission policy；模型语义偏离 approved plan 时要求重新批准的检测仍是后续项。结构化 plan prompt 完成后，TUI live band 会展示包含 steps、target paths 和 checks 的 Plan ready surface；按 `Enter` 会从 draft 创建并运行 durable task，按 `Esc` 丢弃。Plan prompt 使用普通 agent loop，但用户 prompt 和 plan-mode 指令都只作为本轮 transient context 注入，不追加为 parent User entry；工具面使用 planner scoped registry，同时保留 agent-thread tools 以支持显式只读 delegation。普通 composer 输入始终 chat-first；继续 durable task 必须使用 `/task continue` 或 task UI action，不会被未完成 task 自动劫持。普通 chat 明确要求 subagent / 子 agent delegation 时，TUI 会把该意图传为 `AgentDelegationRequirement`；如果本轮没有任何非错误的 Agent 类工具结果创建或引用 child thread，agent loop 会先用 transient retry prompt 要求模型调用 `spawn_agent` 等 agent-thread tool，重试仍未满足时不写入该 final answer；无效输入或 tool execution error 不会解除 delegation guard，running join-before-final handle 只表示委派要求已满足，最终回答仍会被 blocker 阻止，直到 `wait_agent` / `read_agent_result` 完成。model-visible `spawn_agent(join_before_final)` 会立即返回 running handle/status 和 result ref，不等待 child 完成；`wait_agent` 只返回轻量状态和 result ref，不返回 child final answer 正文；如果 session 里仍是 running、但当前 runtime 已没有可等待的 live handle，`wait_agent` 会追加 `AgentThreadStatusChanged(Unavailable)`，返回 `polling_recommended=false` 与 `rerun_not_needed=true`，要求模型报告不可用状态而不是继续轮询。完整 child final answer 保留在 child session，需要额外细节时必须显式调用 `read_agent_result` 分页读取；分页正文只作为当前 request 的 transient context 传给模型，持久 parent tool result 和 TUI 工具卡只保留 offset、长度、截断状态与 result ref；已从 offset 0 完整交付过的结果会返回 `already_delivered`，避免为了长报告调大 summary、反复 wait 重复 summary、恢复后重复回放分页正文，或把完整 child transcript 灌回主上下文。Agent loop 会在 durable assistant message 上写入 `assistant_kind`：带 tool call 的中间 assistant message 标记为 `tool_preamble`，最终回复标记为 `final_answer`；TUI live 期间可以展示 pre-tool stream 作为进度，但不会再把 `tool_preamble` content 追加成正式 assistant bubble。恢复主 transcript 时优先使用该显式 kind，旧 session 才回退到 tool_calls/content 启发式；如果后面已有 final answer，tool preamble 和 final 前 reasoning trace 不再作为正式 assistant 回答回放。worker protocol 使用 `SubmitPlanPrompt` 处理 plan-mode prompt，使用 `SubmitTask` / `ContinueTask` 命令和 `TaskRunStarted` / `TaskRunFinished` 消息处理 durable task；task run / step / child-session control entry 也会通过实时 `RunEvent::Control` 同步到 TUI。Info rail 从 durable task control entries 渲染 task 状态、最新 plan 版本、完成进度、当前或最近失败步骤，以及当前 plan 的步骤摘要；`Agents` 区会列出 `main` 和具体 child agent，存在 child agent 时 composer 会渲染紧凑 agent 面板，输入光标位于 composer 最后一行时 `Down` 聚焦该面板，`Up/Down` 选择 agent 行，`Enter` 切换可见 transcript，`Alt-A` / `Shift-Alt-A` 也可在可见 agent transcript 间循环切换，`/agent <main|child-id>` 可通过 slash selector 精确选择，`/agent rename <child-id|current> <name>` 会追加 `TaskChildSessionDisplayName` control entry 作为 presentation-only 展示名覆盖；`/agent close <child-id|current>` 由 TUI 解析目标后交给 worker `CloseAgent`，再通过 runtime `close_agent_thread` 复用 model-visible `close_agent` 校验并追加 `AgentThreadClosed`，running thread 仍需后续 cancel path；runtime delegate 对有效目标的 `message_agent` 会向 active background child mailbox 投递 follow-up，入口层通过 `AgentToolRuntime::route_agent_message` 追加 requested -> resolved/rejected 的 `AgentThreadMessageRouted` 审计；tool result 明确返回 `delivered_to_mailbox`、`will_apply_after_current_turn` 和 `interrupts_in_flight_provider_stream=false`，语义是 next safe point，不承诺 mid-stream interrupt；展示名优先级是 persisted rename、plan step `display_name`、最后退回 role+ordinal（如 `read 1` / `write 1`）。选择 child agent 后主聊天区切到对应 child session transcript，并保留 sticky breadcrumb。步骤摘要使用状态化 marker 和行文本颜色：running 高亮，completed 绿色，failed/blocked 红色，cancelled/interrupted 金色，pending 弱化。

当 kernel 因 delegation blocker、pending child agent 或 final-answer facts 注入而拒绝当前候选 final 并继续下一轮时，TUI 会丢弃尚未落盘的 streaming assistant 候选，只展示后续 accepted final answer，避免用户看到两段重复或互相覆盖的总结。

Foreground `join_before_final` child agent 会基于 durable `AgentThreadStarted` control state 在主时间线渲染 agent activity 卡片。卡片和 footer 都会提示 `Ctrl-B background`，用于请求把当前前台 child agent detach 到后台继续执行。

runtime delegate 在 final answer 前会检查 agent thread projection：非 terminal 的 `join_before_final` child thread 会阻止最终回答并要求 `wait_agent`；terminal 但结果尚未通过 `read_agent_result` delivered 的 join-before-final child 会阻止最终回答并要求读取结果。delegate 还会从 durable session entries 汇总 approvals（区分 policy allow、用户 allow once/session、session grant 创建和复用）、tool commands/gates、subagents、changed files，以及使用 kernel readiness reducer 生成的 pre-final readiness preview 作为 `session_facts`；模型需要基于记录的 facts 汇报验证和变更。`spawn_agent` 会对父级最近 user request 与 child objective/prompt 中的路径/模块 token 做保守重叠检测，发现明显同 scope 时发出 warning，提醒 parent 只继续非重叠工作或在 final 前读取 child 结果。

`sigil-kernel::SequentialTaskOrchestrator` 先运行 planner role，通过 internal `task_plan_update` tool 接收 plan update，再顺序执行 steps。Executor step 在 parent session 中运行，但 step context 作为 transient request context 注入，不会变成普通 user history。Subagent read/write step 在 child session 中运行，parent session 记录 child-session lifecycle link，并为 child tool approval 与 MCP elicitation 写 route 摘要。

Step 执行中出现过普通 tool error 时，如果 agent 已经读取错误并给出最终回答，orchestrator 会把 step 视为已恢复并继续后续步骤，同时在 `TaskStepEntry.reason` 保留 `recovered tool error` 摘要。Max turns、interrupted tool call、审批拒绝和权限类 tool error 仍会阻断 task。

Role-specific provider 和 run options 由 `sigil-runtime` 装配。Planner 与 subagent-read 默认使用只读 scoped tool registry；executor 默认使用完整 registry；subagent-write 只有在 `[task].allow_write_subagents = true` 时使用完整 registry。`ScopedToolRegistry` 会同时限制 specs、preview、execute、permission hooks 和 egress hooks。runtime worker 现在使用 workspace-aware `AgentProfileRegistry`，可从 `[skills].workspace_agents_dir`（默认 `.sigil/agents`）发现 Sigil-native workspace agent profiles：`.sigil/agents/<id>/agent.toml` 或 `.sigil/agents/<id>/AGENT.md`。Native profile 默认 enabled、manual-only、needs-review、read-only，显式 trusted 且 model_allowed 后才进入 model-visible agent index；`AgentProfileTrustDecision` append-only control entry 会通过 `AgentProfileTrustProjection` 覆盖非 system profile 的 trust 状态，TUI worker 的 agent tools 注册面和 runtime supervisor 都使用 session-aware registry，因此 source/profile hash 变化后旧 trust decision 会失效并回到 `needs_review`，默认退出 model-visible agent index；duplicate id/alias/slash 与 symlink escape 会 warning 并跳过或禁用冲突别名。同一 registry 也会把 skill discovery 中 `run_as=child_session` 的 trusted compatibility entries 投影为 subagent profiles，包括 `.sigil/agents/*.md`，以及显式配置 `[skills].compatibility_sources = ["claude", "reasonix"]` 后的 `.claude/agents/*.md` 和 `.reasonix/agents/*.md`；`disable-model-invocation` / `disableModelInvocation` 会映射为 manual-only，`allowed-tools` / `allowedTools` 只收窄 profile tool scope，包含 `disallowed-tools` / `disallowedTools` 的 subtractive scope 因无法安全表达为 `AgentProfile` 会 warning 并跳过。受信任 plugin manifest 可通过 `[[agents]] path = "agents/<id>/agent.toml"` 或 Markdown profile 贡献 agent；runtime 会校验 manifest-relative 路径，未 trust plugin 只展示 capability，不注册 runtime profile，已 trust 且 hash 匹配的 plugin agent 会以 `AgentProfileSource::Plugin` 和 namespaced id 进入 registry。`spawn_agent` 创建 child registry 时会把 role tool scope 与 profile tool scope 取交集，profile 不能扩大角色工具面；child run 会把 profile description/instructions 作为 transient child system prompt 注入，不写入 parent history。

`AgentProfilePolicyDecision` append-only control entry 会通过 `AgentProfilePolicyProjection` 覆盖非 system profile 的 effective `enabled` / `user_invocable` / `model_invocable` 策略。该 overlay 绑定当前 source/profile hash，hash 变化后旧 policy 失效；runtime filtering 和 `spawn_agent` 注册面使用 effective policy，但不会修改源 `AgentProfile`，避免 policy replay 污染 profile snapshot hash。

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

DeepSeek provider 配置位于 `[providers.deepseek]`。OpenAI-compatible provider 配置位于 `[providers.openai_compat]`，`agent.provider` 必须使用 `openai_compat`。Anthropic provider 配置位于 `[providers.anthropic]`，Gemini provider 配置位于 `[providers.gemini]`。`agent.provider` 只接受 `deepseek`、`openai_compat`、`anthropic` 和 `gemini`；其他值会作为 unsupported provider 报错，不再隐式回退到 DeepSeek。运行时凭据环境变量只使用 Sigil 专属入口：DeepSeek 使用 `SIGIL_API_KEY`，OpenAI-compatible 使用 `SIGIL_OPENAI_COMPATIBLE_API_KEY`，Anthropic 使用 `SIGIL_ANTHROPIC_API_KEY`，Gemini 使用 `SIGIL_GEMINI_API_KEY`；通用 provider env 会被忽略，避免 Sigil 认证和其他工具共享状态。

TUI 首次进入一个未信任 workspace 时会先显示 workspace trust gate；用户确认后才进入正式 TUI、加载 repo-local instructions 和发现 repo-local checks。workspace trust 不会自动把所有 repo-local checks 提升为 task required checks；默认只要求用户显式配置的 checks，CI/Cargo/Makefile discovery 保持为 suggested checks，直到经过显式 approval、sandbox decision 或 global policy promotion。TUI `/config` 只暴露 provider 高频项、storage cleanup、permissions、memory、compaction 主开关/阈值/状态、code intelligence 主开关/启动模式、terminal 兼容性状态、Appearance 粗粒度 theme/syntax/currency、Agents browser、Skills browser、Plugins trust review 和 MCP server 状态/激活入口。它可以在 `deepseek`、`openai_compat`、`anthropic` 和 `gemini` 间切换；DeepSeek FIM 显示为 provider 专项高级项，非 DeepSeek provider 下标记为不支持。provider 草稿字段、保存序列化、DeepSeek 余额/模型列表请求和 provider/model context-window metadata 都通过 `sigil-runtime` 的 provider-neutral DTO/helper 执行；runtime 的 `ProviderStatusTaskManager` 负责 provider 状态刷新任务替换、取消和过期结果过滤，TUI 不直接依赖 provider crate 或 HTTP client。低频 provider 专项字段继续保留给配置文件和环境变量；MCP server 的 command、args 与 startup timeout，Code Intelligence auto_discover/report_missing，terminal mouse/OSC52/scroll sensitivity，以及 Appearance color token overrides 都通过 `~/.sigil/sigil.toml` 或显式配置文件维护，而不是作为 `/config` 主流程字段。Storage 页展示 recommended cleanup preview、retention policy 和 artifact inventory 摘要；只有存在 expired / unavailable / quota-selected artifact 时才显示 recommended cleanup 提示；footer 只提供一个 `clean` action，逐 artifact delete、cleanup target 切换和 multi-select 不作为普通用户主流程。Permissions 页展示 Mode、Checks、workspace trust、repo instruction trust、repo check 数量和高级 scope/rule 摘要；默认 `manual` 只给当前任务显示 run/retry action，只有显式切到 `trusted_only` 时才允许 trusted checks 自动启动，repo-local check 的一次性执行/重试入口属于 task status surface 而不是 `/config` footer。TUI eager MCP 后台启动失败只更新 MCP lifecycle status，不再把 startup timeout 作为普通 Notice 打到主流程；MCP lifecycle 会对 verification scope 做启动前后 workspace scan，未改动工作区的启动失败不会生成 `WorkspaceMutationDetected` 或污染 readiness，真实改动或 scan 不可用才进入 RFC-0002/0003 mutation evidence；用户主动 refresh/activate 仍会得到结果提示。`[task]` 使用单一 `max_subagents` 配置控制活跃子 agent 总数，默认值为 8；foreground、background、只读与可写子 agent 共用同一并发槽位，token 用量只记录到 agent result，不再作为 spawn 拒绝预算。Agents browser 使用 `AgentProfileRegistry` 展示 built-in、native 和 compatibility profiles，支持 PgUp/PgDn 切换，显示 source/kind/trust/effective enabled/user/model、provider/model、tool scope 与 nickname candidates，主 footer 只提供 trust/disable/close；更细的 enabled/user/model policy 仍由配置和高级控制面承载，不作为普通用户主流程。Skills browser 只展示 inline/reusable skill，支持 PgUp/PgDn 切换，展示 trust/source/hash/run mode/invocable/tool scope/path patterns，并通过单一 footer use 动作生成受 runtime `load_skill` policy 约束的请求；slash selector 的 skill fallback 同样只展示 trusted inline skills。Plugins review 使用当前 session control projection 解析既有 trust decision，支持 PgUp/PgDn 切换 plugin，并通过 approve/deny 将 manifest snapshot 与 trust decision 追加到当前 session JSONL。

`sigil doctor` 与 TUI `/doctor` 复用 runtime 诊断逻辑，检查配置加载、workspace、session log、provider/auth 来源、MCP command/trust、code intelligence LSP plan、terminal `TERM`、终端 profile/layers，以及 mouse/OSC52/scroll sensitivity 兼容性设置。诊断只展示 secret 来源，不输出 secret 值。

## Packaging 当前实现

当前 distribution 实现支持首发包管理器 artifacts 和本地验证路径：

- npm scoped package 生成：`scripts/prepare-npm-packages.sh`
- Homebrew tap formula 生成：`scripts/render-homebrew-formula.sh` 输出 `sigil-ai.rb`
- Cargo git-tag 安装：`cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1 --locked sigil`
- checkout 安装：`cargo install --path crates/sigil --locked`
- 本地 release archive：`scripts/build-release-archive.sh`

release archive 脚本会用 release mode 构建 `sigil`，注入 git commit、target 和 profile 构建元数据，对 built binary 运行 `sigil --version` 与 `sigil doctor` smoke，然后输出 `dist/sigil-<version>-<target>.tar.gz` 和对应 `.sha256`。
archive payload 包含 `sigil` binary、README、`assets/logo/*` 和安装文档，确保 README 中的仓库相对 logo 路径在解压后仍然有效。

GitHub release workflow 位于 `.github/workflows/release.yml`。推送 `v*` tag 或手动指定既有 tag 时，它会在 Linux、macOS、Windows runner 上构建 release archives，生成 GitHub artifact provenance attestations，汇总 checksum，按 Conventional Commit 生成 release notes，渲染 `sigil-ai.rb` Homebrew formula asset，从 release archives 准备 npm package tarballs，并用 `gh release create` 发布 GitHub release。流程说明维护在 [`release-process.md`](release-process.md)。

独立 Homebrew tap 仓库同步、npm registry 发布、crates.io package name 决策和自更新仍是 core agent runtime 之外的 release-management 工作。

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

`resources/list` / `resources/read` 和 `prompts/list` / `prompts/get` 只在 server initialize capabilities 声明对应 capability 时注册为 provider-visible 只读工具。它们复用 MCP trust policy、permission subjects、egress logging 和 secret egress 阻断，不会自动注入 system prompt。已经通过 resource read 路径取得的 bounded text 可以由 runtime 的 MCP resource context adapter 转成 `McpResource` Context V0 row，但仍必须经过 MIME filter、size cap、egress decision 和 packer 校验。

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

`code_intelligence.auto_discover = true` 时，会按 workspace marker / 文件扩展名自动发现 Rust、TypeScript/JavaScript、Python、Go，并只把 PATH 上可用的内置 allowlist server 纳入启动计划。Rust 项目默认使用 `rust-analyzer`，没有可用 LSP server 时回退到 Tree-sitter Rust outline / syntax diagnostics。

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

覆盖率检查默认生成 workspace 单测覆盖率报告，统一由 `scripts/coverage.sh` 执行。需要发布级阈值时显式指定，例如 `COVERAGE_MIN_LINES=96 ./scripts/coverage.sh`。

本地 pre-commit hook：

```bash
git config core.hooksPath .githooks
```

单独运行 staged coverage 检查：

```bash
./scripts/check-staged-coverage.py
```

staged gate 会先读取 staged source snapshot，再检查 Rust 业务代码新增可执行行是否伴随同 crate 的测试文件改动。可识别的声明、导入和类型形状不会进入可执行新增行判断。

为了缩短本地提交耗时，staged gate 不再为每次提交生成 LCOV；完整 workspace 覆盖率由显式 `./scripts/coverage.sh` 和 CI 报告观察趋势。

staged gate 只作为测试证据检查，不替代 targeted tests、`check-touched` 或发布级 coverage threshold。

staged coverage 脚本的 diff 分类、同 crate 测试证据和覆盖率辅助解析有独立 Python 单测：

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
