<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# 高级配置

[文档首页](README.md) · [配置](configuration.md) · [权限](permissions-and-sandbox.md) · [字段参考](configuration-reference.md) · [English](../en/advanced-configuration.md)

请先完成普通设置并确认 Sigil 可以正常工作，再使用本页选项。一次只修改一个区域；结果不清楚时运行 `sigil doctor`。

## 任务规划

<!-- public-doc-topic: task -->

```toml
[task]
enabled = true
routing_policy = "auto"
max_plan_steps = 12
max_replans = 2
max_subagents = 8
max_parallel_read_steps = 4
max_parallel_changeset_steps = 2
max_planning_research_agents = 3
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true
```

以上数值是当前 schema 默认值。`auto` 是 release 默认值，缺少配置的 Quick Setup
因此保存 `auto + explicit_request_only`（review-first 基线）。只有 installed release 携带
qualified sidecar，且 provider、model、官方 endpoint family、task-config digest 与 binary
build 全部精确匹配，Quick Setup 才保存 `auto + proactive`。不兼容的配置会被拒绝；
sidecar 缺失、无效、过期或不匹配时保持在 review-first 基线。`sigil doctor` 会报告
rollout 状态与 direct-task tier。

coarse rollback 是设置 `routing_policy = "manual"` 与
`multi_agent_mode = "explicit_request_only"`。它会关闭自动 handoff 和 proactive spawn，
但不会删除 durable Task history。route-local 零容忍不变量会对当前 session 与 build 的
后续输入应用同样的 effective fallback。

`routing_policy` 是三路语义准入策略。当前 schema 默认值为 `auto`，普通输入会先
进入一次独立的 routing-only 决策回合；模型只能给出一个 typed decision：
`Chat`、`PlanReview` 或 `Task`（具体可选集合取决于 effective route capability）。
`PlanReview` 决定会进入只读 plan review 并等待你的决定，只有被接受的计划才能创建
durable Task；`Task` 决定会直接进入 durable planner/executor 流程；简单问题仍直接
回答，而且路由不会绕过写文件、shell、网络或 merge 审批。设置 `routing_policy = "manual"`
会关闭普通输入的自动路由，`/plan` 与 `/task` 仍是显式入口。Planner、Executor、Subagent 与最终 Synthesis transcript 均保存在隔离 child session，parent 只保留 bounded result 和一个由 host 提交的正式 final。相互独立且已证明为 shared-read-only 的 Task step 可以并发执行；`max_parallel_read_steps` 与 `max_subagents` 共同限制 fan-out，host 仍按稳定的 plan 顺序向 parent 提交终态结果。相互独立的 `ChangesetOnly` 写子智能体 step 也可以并发，受 `max_parallel_changeset_steps` 与 `max_subagents` 共同限制；同批成员绑定同一份不可变 parent workspace snapshot，只生成 proposal 而不修改 parent workspace，parent 重新校验 snapshot 后才提交 proposal/review 记录。受支持的 Git 仓库还可以整批并发运行相互独立的 physical `Worktree` writer。Sigil 会冻结 exact clean 或安全 dirty/untracked baseline，把每个 child 绑定到独立 owned checkout，提取有界 proposal，并通过 deterministic conflict graph 分配 integration lane；不冲突 lane 可以并发 apply 和 verification，但最终 promotion 仍要求 exact integration review 与 authoritative parent verification。直接或带副作用的 shared parent workspace 写入继续保持串行和独占。TUI 的 Task strip 和 info rail 会同时标出全部 active step，取消 Task 会收口整个 active batch。Planner 在接受计划前可以请求一次由 host 托管的独立只读 Explore 批次；`max_planning_research_agents` 默认是 `3`、硬上限是 `4`，设为 `0` 可关闭这个 planner-only fan-out。Host 会等待所有 probe 进入终态后自动恢复 Planner，不需要模型轮询命令。Production HTTP driver（包括 Desktop 持有的 `sigil serve` child）与 TUI 共享同一套 typed Task pause/continue、guidance、integration review、restart control 与 recovery contract。只读计划使用 `/plan`，需要确定进入多步骤执行时使用 `/task`；字段完整的 `sigil-plan-v2` DAG 会直接 promotion，不再二次规划。当模型判断请求中存在可独立审阅或移除的用户结果时，计划还可以提出 typed Intent 定义，并用 provider-local alias 绑定步骤。这些内容在用户接受“计划就绪”卡片前始终只是未授权 proposal；接受后由 host 生成全部运行时身份，并把已接受 IntentPlan 与绑定后的 TaskPlan 原子持久化。在保守的子智能体模式下，只有你或工作区指令明确要求委派时，Sigil 才会启动子智能体。不同角色使用的模型与工具限制见[配置字段参考](configuration-reference.md#任务)。

## 验证

```toml
[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

只添加你理解的检查。仓库提示可以被建议，但不会仅因存在而运行。会修改相关文件的检查必须再由不写入的检查跟进，结果才是当前的。

## 记忆、技能与子智能体

<!-- public-doc-topic: memory -->

```toml
[memory]
enabled = true
writable = true
```

`enabled` 允许 Sigil 加载 `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` 等工作区指令文件。请保持内容简短、及时更新，并确保这些说明适用于仓库中的每个会话。

`writable` 默认启用；如需退出，可显式设为 `false`。Sigil 会提供需要审批的 `remember_user_preference`、`remember_project_fact`，以及 `inspect_memory`、`forget_memory` 工具。是否需要长期记忆由模型根据用户语义自行判断，不通过关键词机械匹配 prompt。写入成功会返回包含 `scope`、`memory_id` 和 `version` 的 durable receipt；拿到该回执前，Sigil 不得声称信息已跨会话记住。用户偏好在本机工作区之间共享，项目事实按当前 canonical workspace 隔离；疑似凭据或 secret 的内容会被拒绝。

Forget 会停止后续召回，并物理删除 Sigil 控制的记忆 sidecar；它不能撤回已经发送给 provider 的上下文，也不会删除独立的 session 和审计证据。

<!-- public-doc-topic: skills-agents -->

Sigil 原生的可复用工作区技能、命令、子智能体和插件分别位于 `.sigil/skills`、`.sigil/commands`、`.sigil/agents` 和 `.sigil/plugins`。默认还会发现标准 `.agents/skills`、Codex `.codex/agents`、OpenCode `.opencode/{skills,commands,agents}` 和 Claude Code `.claude/{skills,commands,agents}` 资源；同名时 Sigil 原生资源优先。兼容资源继承工作区信任，不再要求逐项审查或启用；兼容 command 通过 `/名称` 调用，agent 通过 `@名称` 手动调用并保持只读。设置 `[skills].compatibility_auto_discover = false` 可关闭默认集合，也可以通过 `compatibility_sources` 添加或精确选择来源。

## 上下文精简与代码智能

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
strategy = "cache_aware_v3"
native_carrier_enabled = false
```

`cache_aware_v3` 是唯一策略：在模型服务支持时保持可复用的历史输入稳定，延续当前意图并按完整回合保留最近内容；只有上下文即将放不下，或可信成本证据证明值得时，才开始新的缓存周期。执行 `/compact` 本身就是生成、校验并原子激活一个可恢复语义 checkpoint 的明确请求，不再打开确认弹窗。route 准入后，它会在当前模型服务和模型上额外调用一次 LLM：保留上一份请求作为可缓存前缀，只在末尾追加严格 JSON 摘要指令。这不是子智能体，也不会执行工具。模型摘要只补充不可信语义脉络；目标、约束、授权、完成状态和验证仍以保存的会话历史为准。Sigil 会先展示进度，随后给出已激活回执或可操作的拒绝原因。摘要生成、精确 token proof、经济性准入任一失败，或摘要期间会话发生变化时，当前上下文保持不变。不支持的 route 会直接不可用，不再选择旧算法。手动摘要失败不会静默降级，只有上下文必须缩小或即将溢出的紧急路径才可带明确审计使用确定性 fallback。大型工具输出 aging 继续作为独立的确定性维护路径，不再隐藏在 `/compact` 中。上下文大小按“连接/模型精确配置 → provider 内置元数据 → `fallback_context_window_tokens`”解析；TUI 普通设置提供“自动、64K、128K、256K、1M”预设，“自动”表示不写精确覆盖。已有的自定义值仍然有效，也可以继续在 `sigil.toml` 中维护。

`native_carrier_enabled` 是默认关闭的 provider-native 加速开关。当前将它设为 `true` 不会产生效果，因为 Sigil 尚不会在相同模型服务路径的下一次请求中复用模型服务专属的精简状态。portable continuity 仍是唯一启用的精简路径。

<!-- public-doc-topic: code-intelligence -->

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
auto_discover = true
```

启用后，Sigil 可以使用已经安装的语言服务器提供代码导航、诊断和经过检查的编辑。按 `Alt-D` 检查已修改源码。缺少语言服务器不会阻止普通对话或文件工具继续工作。

## 终端与模型请求环境变量覆盖

<!-- public-doc-topic: terminal -->

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[terminal.notifications]
enabled = false
method = "auto"
minimum_run_duration_ms = 10000
```

终端、远程环境或终端复用器不支持某项能力时，请将其关闭。通知默认关闭，并使用不含提示词、路径、工具详情、模型服务、具体模型或会话 ID 的固定文本。可以按照[终端兼容性](terminal-compatibility.md)检查实际效果。

<!-- public-doc-topic: model-request-env -->

`SIGIL_MODEL_REQUEST_TIMEOUT_SECS`、`SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS` 和 `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS` 可以临时覆盖共享的模型请求超时。模型服务凭据和端点设置仍放在各服务的专用页面。

## 插件与 MCP

<!-- public-doc-topic: plugins -->

Sigil 会从 `.sigil/plugins/<id>/plugin.toml` 发现插件，并在 `/config` 中等待你检查。插件发生变化后，需要重新检查才能允许运行。插件入口不能请求继承凭据环境变量。

<!-- public-doc-topic: mcp -->

使用 `[[mcp_servers]]` 配置 MCP。本机服务端会从清空的环境启动；只有通过用户级配置中的 `inherit_env`，才能授予确实需要的环境变量。远端认证、信任与兼容性见 [MCP 指南](mcp.md)，精确字段见[配置字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
