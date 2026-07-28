<!-- public-doc-role: advanced-configuration; authority: advanced-settings-guide; sections: task-planning,verification,memory-skills-and-agents,compaction-and-code-intelligence,terminal-and-model-request-overrides,plugins-and-mcp; cta: open-configuration-reference -->

# 高级配置

[文档首页](README.md) · [配置](configuration.md) · [权限](permissions-and-sandbox.md) · [字段参考](configuration-reference.md) · [English](../en/advanced-configuration.md)

请先完成普通设置并确认 Sigil 可以正常工作，再使用本页选项。一次只修改一个区域；结果不清楚时运行 `sigil doctor`。

## 任务规划

<!-- public-doc-topic: task -->

```toml
[task]
enabled = true
routing_policy = "manual"
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_subagents = 8
max_parallel_read_steps = 4
max_parallel_changeset_steps = 2
max_planning_research_agents = 3
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true
```

以上数值是 schema 与迁移安全的兼容默认值。缺少配置时，只有 installed release 携带
qualified sidecar，且 provider、model、官方 endpoint family、task-config digest 与 binary
build 全部精确匹配，Quick Setup 才可能保存 `auto + proactive`。已有配置永远不会被重写；
sidecar 缺失、无效、过期或不匹配时，会 fail closed 到上面的兼容值。`sigil doctor` 会报告
rollout 状态。

coarse rollback 是设置 `routing_policy = "manual"` 与
`multi_agent_mode = "explicit_request_only"`。它会关闭自动 handoff 和 proactive spawn，
但不会删除 durable Task history。route-local 零容忍不变量会对当前 session 与 build 的
后续输入应用同样的 effective fallback。

`routing_policy` 与输入框的 `default_mode` 是两件事。兼容默认值为 `manual`，因此普通输入仍从对话开始。TUI 设为 `auto` 后，模型可以把复杂普通输入通过 typed handoff 交给 durable planner/executor；简单问题仍直接回答，而且 handoff 不会绕过写文件、shell、网络或 merge 审批。Planner、Executor、Subagent 与最终 Synthesis transcript 均保存在隔离 child session，parent 只保留 bounded result 和一个由 host 提交的正式 final。相互独立且已证明为 shared-read-only 的 Task step 可以并发执行；`max_parallel_read_steps` 与 `max_subagents` 共同限制 fan-out，host 仍按稳定的 plan 顺序向 parent 提交终态结果。相互独立的 `ChangesetOnly` 写子智能体 step 也可以并发，受 `max_parallel_changeset_steps` 与 `max_subagents` 共同限制；同批成员绑定同一份不可变 parent workspace snapshot，只生成 proposal 而不修改 parent workspace，parent 重新校验 snapshot 后才提交 proposal/review 记录。受支持的 Git 仓库还可以整批并发运行相互独立的 physical `Worktree` writer。Sigil 会冻结 exact clean 或安全 dirty/untracked baseline，把每个 child 绑定到独立 owned checkout，提取有界 proposal，并通过 deterministic conflict graph 分配 integration lane；不冲突 lane 可以并发 apply 和 verification，但最终 promotion 仍要求 exact integration review 与 authoritative parent verification。直接或带副作用的 shared parent workspace 写入继续保持串行和独占。TUI 的 Task strip 和 info rail 会同时标出全部 active step，取消 Task 会收口整个 active batch。Planner 在接受计划前可以请求一次由 host 托管的独立只读 Explore 批次；`max_planning_research_agents` 默认是 `3`、硬上限是 `4`，设为 `0` 可关闭这个 planner-only fan-out。Host 会等待所有 probe 进入终态后自动恢复 Planner，不需要模型轮询命令。Production HTTP driver（包括 Desktop 持有的 `sigil serve` child）与 TUI 共享同一套 typed Task pause/continue、guidance、integration review、restart control 与 recovery contract。只读计划使用 `/plan`，需要确定进入多步骤执行时使用 `/task`；字段完整的 `sigil-plan-v2` DAG 会直接 promotion，不再二次规划。当模型判断请求中存在可独立审阅或移除的用户结果时，计划还可以提出 typed Intent 定义，并用 provider-local alias 绑定步骤。这些内容在用户接受“计划就绪”卡片前始终只是未授权 proposal；接受后由 host 生成全部运行时身份，并把已接受 IntentPlan 与绑定后的 TaskPlan 原子持久化。在保守的子智能体模式下，只有你或工作区指令明确要求委派时，Sigil 才会启动子智能体。不同角色使用的模型与工具限制见[配置字段参考](configuration-reference.md#任务)。

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

`[memory].enabled = true` 允许 Sigil 加载 `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` 等工作区指令文件。请保持内容简短、及时更新，并确保这些说明适用于仓库中的每个会话。

<!-- public-doc-topic: skills-agents -->

Sigil 原生的可复用工作区技能、命令、子智能体和插件分别位于 `.sigil/skills`、`.sigil/commands`、`.sigil/agents` 和 `.sigil/plugins`。默认还会发现标准 `.agents/skills`、Codex `.codex/agents`、OpenCode `.opencode/{skills,commands,agents}` 和 Claude Code `.claude/{skills,commands,agents}` 资源；同名时 Sigil 原生资源优先。兼容资源继承工作区信任，不再要求逐项审查或启用；兼容 command 通过 `/名称` 调用，agent 通过 `@名称` 手动调用并保持只读。设置 `[skills].compatibility_auto_discover = false` 可关闭默认集合，也可以通过 `compatibility_sources` 添加或精确选择来源。

## 上下文精简与代码智能

<!-- public-doc-topic: compaction -->

```toml
[compaction]
enabled = true
strategy = "cache_aware_v3"
native_carrier_enabled = false
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
tail_messages = 6
```

默认策略是 `cache_aware_v3`：保持 provider/tool 稳定前缀，通过带来源的 portable checkpoint 延续当前意图，按完整回合保留 recent tail，只在上下文即将放不下或可信成本证据证明值得时切换 cache epoch。手动 `/compact` 先进入纯本地 prepare 阶段，不发送 provider 请求、不创建语义压缩 lifecycle，也不改变可见 projection。预览明确提供三种选择：保持当前上下文、只应用可恢复的大型工具输出 projection epoch，或生成完整语义候选。只有第三种选择才会在当前同一 provider/model route 上额外调用一次 LLM：旧 epoch request 原样作为可缓存前缀，只在末尾追加严格 JSON 摘要指令；这不是子 agent，也不会执行工具。模型摘要只补充不可信语义脉络，目标、约束、授权与验证仍以持久化会话记录为准。最终预览和 session 账单会显示摘要调用的 cache read、未缓存输入、输出与成本，且仍需显式确认才激活。自动 V3 只对具备精确 portable proof 且 route capability 受信的 provider 开启；未知或兼容 route 自动回退 `legacy_v2`。手动摘要失败不会静默降级，只有 fit-required/overflow 紧急路径可带明确审计使用确定性 fallback。ratio 与 `tail_messages` 继续可读，供迁移/回滚使用；V3 会把 tail 值翻译成完整回合下限，不再按裸消息数切断工具回合。无法确定模型上下文窗口大小时，可以设置 `fallback_context_window_tokens`；精简失败不会改变当前对话。

`native_carrier_enabled` 是默认关闭的迁移预留开关。当前即使将它设为 `true`，Sigil 也会保持 provider-native materialization fail-closed：只有把 carrier 接回相同精确 route 的下一次请求，额外计费请求才可能产生收益。在该 resume contract 落地前，portable continuity 是唯一启用的精简路径。

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
