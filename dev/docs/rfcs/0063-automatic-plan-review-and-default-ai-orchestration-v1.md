# RFC-0063 Automatic Plan Review and Default AI Orchestration V1

状态：implementation-in-progress（第三轮审计见 §13.3；第四轮审计见 §13.4；第五轮审计 2 项 P1、1 项 P2 见 §13.5；2026-08-14 的真实 PlanReview session 复盘与收敛修复见 §13.8；Plan Review workbench、revision guidance/recovery 与 isolated finalizer 增补见 §13.9；real-model campaign 与 current-source Desktop E2E 未通过前不得标记 implemented，见 §12 门槛）

创建日期：2026-08-03

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- [RFC-0012 Protocol and App Server Boundary](0012-protocol-app-server-boundary.md)
- [RFC-0013 Deterministic Evaluation Harness](0013-eval-harness.md)
- [RFC-0018 Plan-to-Task Handoff](0018-plan-to-task-handoff.md)
- [RFC-0028 Real-model Acceptance and Provider Conformance V1](0028-real-model-acceptance-and-provider-conformance-v1.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0057 Cache-stable Compaction and Conversation Continuity V3](0057-cache-stable-compaction-and-conversation-continuity-v3.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0064 Durable User Input Requests V1](0064-durable-user-input-requests-v1.md)

## 1. Summary

Sigil 当前已经具备两条分别成立、但尚未统一成完整产品行为的链路：

1. 用户显式输入 `/plan` 后，TUI 可以运行只读 Plan mode，生成 durable `PlanDraftCreated`，
   展示 Plan ready，并在用户接受后通过 RFC-0018 创建 durable Task；
2. `[task].routing_policy = "auto"` 时，普通 Chat 会先经过 routing-only microturn，模型可以调用
   `request_task_planning`，直接进入 RFC-0053 的 Planner / Executor / Subagent Task 流程。

当前缺口是：模型不能从普通输入中主动请求一个**用户可见、只读、可审阅、等待决定的 Plan
review**。现有自动路由只有 `Chat` 或 `Task` 两种结果；TUI 的 `ComposerMode::Plan` 又只是本地 UI
状态，Desktop 的 `/plan` 只是手动绑定 plan profile，二者都不能成为模型可写的控制面。

本 RFC 将普通输入的语义路由扩展为三个结果：

```text
Chat | PlanReview | Task
```

并冻结以下产品决定：

- AI 可以通过 typed route decision 主动请求 Plan review；
- host/runtime，而不是模型或 renderer，负责进入 Plan review lifecycle；
- Desktop 与 TUI 从同一 durable/public projection 展示 Planning 和 Plan ready；
- Plan review 保持只读，只有用户接受后才进入 RFC-0018 的 Task；
- 明确且适合直接执行的复杂目标仍可自动进入 RFC-0053 Task；
- `routing_policy` 的 schema 默认值改为 `auto`；`manual` 成为显式 opt-out；
- 默认自动路由不授予文件、shell、network、MCP、external-directory、merge 或发布权限；
- exact-route qualification 不再把自动能力整体降级到 Manual，而是控制是否允许
  `DirectTask`；未准入 route 默认保留 `ReviewFirst`，自动进入 Plan review 后等待用户决定；
- `task.default_mode` 从当前 schema 移除。模型路由、显式 `/plan` 与 UI 展示不再依赖这个未接线字段。

RFC-0018 与 RFC-0053 的职责不合并：Plan 仍是待审阅 artifact，Task 仍是唯一 durable execution
engine。本 RFC 只新增从普通 conversation 到 Plan review 的 first-class typed handoff，并修改自动路由
的默认和 rollout 层级。

### 1.1 Relationship to RFC-0018

RFC-0018 继续拥有 Plan artifact、用户 decision、scoped permission grant 和 Plan-to-Task promotion。
本 RFC 只扩展 Plan 的 source：除了显式 `/plan`，还可以来自一个 host-bound automatic
`ConversationRouteDecision(PlanReview)`。两条来源必须汇合到同一个 `PlanReviewCoordinator` 和同一组
RFC-0018 durable records。

### 1.2 Amendment to RFC-0053

RFC-0053 继续拥有 DirectTask、Planner/Executor/Subagent orchestration、parallel participant、recovery、
integration 和唯一 parent final。本文对 RFC-0053 做两项后续修订：

1. ordinary conversation route 从 `Chat | Task` 扩展为 `Chat | PlanReview | Task`；
2. rollout 从 `Manual` fail-closed 默认改为 `Auto + ReviewFirst` 默认，只有 DirectTask 和 proactive
   agent 继续要求 exact-route qualification。

在 RFC-0063 标记 implemented 前，RFC-0053 的当前 `manual + explicit_request_only` 源码默认和
exact-sidecar rollout 仍是实现事实；本文目标不得提前写入用户文档或产品 capability 声明。

## 2. Confirmed current baseline

以下是当前源码事实，而不是目标能力：

### 2.1 已有自动 Task 路由

`sigil-runtime::ConversationCoordinator` 已能在 task enabled 且 effective
`TaskRoutingPolicy::Auto` 时，为普通 user turn 绑定 `TaskPlanningHandoffBinding`。routing-only
microturn 只暴露：

```text
request_task_planning
continue_without_task_planning
```

模型选择前者后，agent loop 返回 `AgentRunDisposition::StartDurableTask`，TUI worker 和 production
application driver 都会继续运行同一 durable Task。该链路已覆盖 Desktop-owned `sigil serve`。

### 2.2 已有显式 Plan-to-Task handoff

TUI `/plan` 已能生成当前 `sigil-plan-v2` structured draft，并通过以下 append-only records
完成审阅和 handoff：

```text
PlanDraftCreated
  -> TaskRun / accepted TaskPlan or compatibility Planner
  -> optional PlanPermissionGranted
  -> TaskCreatedFromPlan
  -> PlanDecisionRecorded(Accepted)
```

Plan acceptance 与 tool permission 是两个决定；接受计划不会自动批准 shell、network、MCP 或外部目录。

### 2.3 当前产品缺口

- automatic router 没有 `PlanReview` 结果；
- `ComposerMode::Plan` 只在 TUI `/plan` 或本地输入状态中被设置，模型不能也不应直接修改它；
- `TaskConfig.default_mode` 只有 config/schema/test 引用，没有初始化 Desktop/TUI composer 的 runtime
  消费者；
- Desktop `/plan` 绑定 manual-only plan profile，但 public HTTP/SSE、Desktop renderer 与 plan
  decision command 尚未形成 RFC-0018 等价的完整 Plan ready 产品链；
- 当前 schema/default 和源码配置保持 `manual + chat`；只有 exact-build qualified sidecar 可能在
  Quick Setup 中写入 `auto + proactive`；
- route-local hard invariant 当前把 `Auto` 整体降级为 `Manual`，没有只降级 direct execution、仍保留
  review-first planning 的中间层。

## 3. Goals

V1 必须达成：

1. 普通输入可以由 AI 自动路由为用户可见的 Plan review，无需用户先输入 `/plan`。
2. Plan review 在 Desktop 与 TUI 中使用同一 durable lifecycle、plan artifact、decision 与
   Plan-to-Task handoff。
3. AI 只提交 typed semantic decision；不能操作 `ComposerMode`、React state、TUI focus 或其他 UI
   implementation state。
4. `routing_policy = "auto"` 成为 schema 默认；显式 `manual` 仍完全关闭普通输入的自动 handoff。
5. 自动路由可以区分：直接回答、先审阅计划、直接进入 durable Task。
6. 高影响、目标存在重要取舍或用户明确要求先审阅的请求，优先进入 Plan review，而不是直接执行。
7. 清晰、已授权目标在 exact route 通过资格检查时仍可直接进入 Task，不为所有复杂请求强制增加一次
   人工确认。
8. route 不具备 DirectTask 准入时，自动能力降级到 ReviewFirst，而不是退回 Manual。
9. 显式 `/plan`、自动 Plan review 和 Plan revision 复用一个 runtime service；不得保留 TUI-only 与
   application-only 两套 plan engine。
10. Plan draft、decision、Task handoff、cancel、restart、queue、compaction 和 session switch 保持
    append-only、可恢复、可审计。
11. 自动路由与 permission、multi-agent、workspace trust 保持正交。

## 4. Non-goals

V1 不做：

- 不允许模型直接把 composer mode 设为 Plan；
- 不把 Plan review 和 Task 合并成一个状态；
- 不让 Plan acceptance 自动批准所有 workspace write、shell、network、MCP、merge 或 external effect；
- 不根据 prompt 关键词、文件数量或正则在 host 侧决定 route；
- 不对每个复杂请求都强制 Plan review；符合 DirectTask policy 的请求仍可直接进入 Task；
- 不让普通 Chat 中的自由文本、Markdown TODO 或 reasoning 被猜成 plan artifact；
- 不让 renderer-local state 成为 restart authority；
- 不因默认开启 routing 而默认开启普通 Chat 的任意 proactive write Agent；
- 不为此能力新增 CLI-first 普通用户主入口；
- 不在本 RFC 中改变 mutation、verification、sandbox、integration 或 Intent Stack 的既有 authority；
- 不在本 RFC 中定义 Task 内部的 outcome completion predicate、delivery progress watchdog 或
  no-progress terminal policy；本文只负责把目标路由到正确 execution authority，进入 Task 后的收敛、
  blocker 和完成证据继续由 RFC-0053 及其后续修订拥有。

## 5. Product model

### 5.1 Three-way semantic routing

普通 user turn 在 `routing_policy = Auto` 时，先进入独立 routing-only microturn。模型必须且只能选择
一个结果：

| Route | 产品含义 | 后续执行 |
| --- | --- | --- |
| `Chat` | 一个 bounded outcome，可直接回答或完成局部工作 | 普通 agent run |
| `PlanReview` | 先进行只读研究并让用户审阅方案 | Plan review run，等待 plan decision |
| `Task` | 目标明确，可直接进入 durable multi-step orchestration | RFC-0053 Task |

显式入口跳过 semantic router：

- `/plan <prompt>`：直接进入 `PlanReview`；
- `/task <prompt>`：直接进入 `Task`；
- `@profile`、显式 skill、session command：继续按各自 typed command 处理。

### 5.2 Route selection policy

`Chat` 适用于：

- 简单解释、单点查询、一个 symbol/call-flow；
- 一个 bounded read-only conclusion，即使需要读取多个相关文件作为证据；
- 一个小型、局部、无独立 workstream 的改动；
- 不需要 durable DAG、计划审阅或多阶段验证的目标。

`PlanReview` 适用于：

- 用户希望先看方案、设计、RFC、影响分析或执行边界，再决定是否实施；
- 目标包含重要架构取舍，多个可行方向会显著改变最终产物；
- 范围、高风险 effect、迁移策略或验收口径需要在执行前被用户确认；
- 任务影响面较大，而直接执行的收益不足以抵消误解目标的代价；
- effective route capability 是 `ReviewFirst`，且该目标本应进入 durable Task；
- workspace instruction 要求该类工作先 plan/review。

`Task` 适用于：

- 目标和约束已经明确；
- 存在跨组件一致修改、依赖阶段、多个独立 workstream 或长验证；
- 用户明确要求把一组已经存在的工作区变更按可审阅批次完成交付，且需要先盘点范围、划分批次、执行
  分层验证并产生对应交付结果；这类请求即使没有出现 `task`、`plan`、`commit` 或“提交”等词，仍按
  完整语义判断为 durable multi-stage execution，而不是留在普通 Chat 反复检查；
- 计划本身不需要用户先选择方向；
- effective route capability 允许 `DirectTask`；
- 后续 effect 仍能被现有 permission、sandbox、approval 和 verification 独立约束。

文件数量不是判断条件。host 不扫描“计划”“先分析”“直接做”等词；模型根据完整语义和 workspace
instructions 选择 typed route，准确性由 route contract 和 eval 约束。

### 5.3 Default configuration

目标 current schema：

```toml
[task]
enabled = true
routing_policy = "auto"
multi_agent_mode = "explicit_request_only"
```

规则：

- `TaskRoutingPolicy::default()` 改为 `Auto`；
- 显式 `routing_policy = "manual"` 保持 chat-first，只允许 `/plan`、`/task` 等显式入口；
- 删除 `TaskMode` 和 `[task].default_mode`；当前 schema 不保留 alias 或静默迁移；
- 缺少 routing 字段的 current config 使用新的 `Auto` 默认；已经显式写 `manual` 的配置不被改写；
- `multi_agent_mode` 默认仍是 `ExplicitRequestOnly`。accepted TaskPlan step、显式用户 delegation 与
  system-owned planner discovery 继续按既有 typed authority 工作；默认自动 routing 本身不铸造任意
  Chat proactive spawn authority；
- Quick Setup、配置参考、README、Doctor 与 Desktop/TUI 设置摘要必须使用同一默认事实。

### 5.4 Automatic route capability tiers

`routing_policy` 表达用户意图；runtime 还必须从 exact provider/model/build evidence 派生不可由模型修改的
能力层级：

```rust
pub enum AutomaticRouteCapability {
    Unsupported,
    ReviewFirst,
    DirectTask,
}
```

语义：

- `Unsupported`：route 不能可靠完成 current typed tool decision；普通输入按 Chat 处理，并由 Doctor
  明确报告 automatic routing unavailable。不得用自由文本猜 route；
- `ReviewFirst`：自动 router 默认开启，但不向模型暴露 direct Task decision。需要多阶段编排的目标进入
  Plan review，用户接受后再创建 Task；
- `DirectTask`：exact route/build 通过 RFC-0028/RFC-0053 三路评测，可同时选择 Chat、PlanReview 或
  Task。

现有 release-qualified sidecar 从“是否开启 Auto 的总开关”收窄为 capability evidence：

- 无 sidecar、sidecar stale/mismatch 或 route-local invariant failure：`DirectTask -> ReviewFirst`；
- exact qualified route：`DirectTask`；
- tool/schema capability 根本不满足：`Unsupported`；
- `multi_agent_mode = Proactive` 仍需要自己的 exact-route evidence，不因 `ReviewFirst` 自动获得；
- hard invariant kill switch 优先关闭 direct execution 和 proactive spawn，但保留安全的 review-first
  handoff；只有 plan lifecycle 自身也出现 invariant violation 时才降级到 `Unsupported/Manual`。

这样，“自动能力默认开启”不会被未准入 route 整体关闭，同时也不会把未经证明的模型直接放到 durable
execution path。

## 6. Typed routing contract

### 6.1 Model-visible decisions

在现有两个 internal tools 基础上新增：

```text
request_plan_review {
  reason_codes: [
    "explicit_review_intent" |
    "architectural_tradeoff" |
    "scope_uncertain" |
    "high_impact" |
    "permission_boundary" |
    "route_review_required"
  ]
}
```

`DirectTask` microturn 暴露：

```text
request_plan_review
request_task_planning
continue_without_task_planning
```

`ReviewFirst` microturn 只暴露：

```text
request_plan_review
continue_without_task_planning
```

模型必须调用恰好一个 decision tool 并停止。规则继续沿用 RFC-0053：

- routing turn 不回答用户、不读取 workspace、不执行普通 tool；
- objective、source turn、plan/task identity、policy snapshot、permission 和 timestamp 全由 host 绑定；
- reason code 是 bounded enum，不接受自由文本 reasoning；
- free text、多个 decision、未知 tool 或 invalid args 只允许一次 typed retry；
- 第二次仍不满足时写 `routing_unsatisfied` blocked terminal；
- 一旦一个 decision 被接受，同一 response 中其他 tool calls 全部忽略并记录原因；
- routing prompt、tool schema、provider/model/build 与 effective capability 进入 route fingerprint 和 eval
  identity。

### 6.2 Durable route decision

新增 provider-neutral root record：

```rust
pub enum ConversationRoute {
    Chat,
    PlanReview,
    Task,
}

pub struct ConversationRouteDecisionRecordedEntry {
    pub decision_id: ConversationRouteDecisionId,
    pub source_turn: ConversationTurnRef,
    pub route: ConversationRoute,
    pub reason_codes: Vec<ConversationRouteReason>,
    pub configured_policy: TaskRoutingPolicy,
    pub effective_capability: AutomaticRouteCapability,
    pub policy_snapshot_hash: String,
    pub route_contract_fingerprint: String,
    pub decided_at_ms: u64,
}
```

约束：

- `decision_id` 由 exact source turn + route-contract domain 确定性派生；
- 同一 source turn 只能有一个不冲突的 decision；
- model 不生成或覆盖任何 identity；
- `Task` decision 后继续追加现有 `TaskHandoffRequested/Resolved`；
- `PlanReview` decision 后继续追加本 RFC 的 Plan review lifecycle；
- `Chat` decision 只授权下一 provider turn 恢复普通 conversation tool surface，不授予 effect 权限；
- 旧 session 中已经存在、但没有该 root record 的 RFC-0053 handoff 继续按原 projection 读取；新输入不
  反推或补造历史 route decision。

## 7. Plan review lifecycle

### 7.1 Runtime-owned state transition

AI 选择 `PlanReview` 后，agent loop 返回 typed disposition：

```rust
pub enum AgentRunDisposition {
    FinalAnswer,
    StartPlanReview(StartPlanReviewAction),
    StartDurableTask(StartDurableTaskAction),
    TaskPlanAccepted,
    Interrupted,
    Blocked,
}
```

`StartPlanReviewAction` 只携带 host-bound identity/reference，不携带 UI state：

```rust
pub struct StartPlanReviewAction {
    pub decision_id: ConversationRouteDecisionId,
    pub plan_review_id: PlanReviewId,
    pub plan_id: PlanId,
    pub source_turn: ConversationTurnRef,
}
```

TUI worker 与 application driver 收到该 disposition 后，都调用共享 runtime
`PlanReviewCoordinator`。不得：

- 设置 `ComposerMode::Plan` 作为 authority；
- 向 Desktop renderer 发送“请自行启动 plan agent”的文本指令；
- 把原 prompt 再作为第二条 parent User message；
- 复制 TUI 的 plan runner 到 HTTP driver。

### 7.2 Run purpose

新增明确的 run purpose：

```rust
pub enum AgentRunPurpose {
    Conversation(ConversationPurposeContext),
    PlanReview(PlanReviewPurposeContext),
    TaskPlanner(TaskPlannerContext),
    TaskParticipant(TaskParticipantContext),
    TaskSynthesis(TaskSynthesisContext),
}
```

`PlanReview` 与 `TaskPlanner` 不同：

- `PlanReview` 只产生待用户决定的 `PlanDraftCreated`；
- `TaskPlanner` 产生已进入 Task authority 的 accepted `TaskPlanEntry`；
- PlanReview 不创建 TaskRun，不执行 write step，不取得 parent final execution authority；
- PlanReview 使用 planner-scoped read-only registry，可按既有限制调用一次 host-owned read-only
  discovery；
- source objective 通过 transient context 引用原 user turn，parent 不重复追加 User message；
- planner transcript 放在 retry-stable child session；parent 只保存 bounded lifecycle、draft 与 result ref。

### 7.3 Typed plan submission

为了让自动 Plan review 不依赖模型自由文本触发，新增 internal model-visible tool：

```text
submit_plan_draft {
  schema_version: 2,
  summary: string,
  steps: [...],
  intents?: [...],
  target_paths: [string],
  suggested_checks: [...],
  risk?: string,
  notes: [string]
}
```

该 schema 与当前 `sigil-plan-v2` 的 role、dependency、mode、isolation、intent alias、path 和 check
语义完全一致。host：

1. strict-validate schema、DAG、stable ids、paths、checks 和 intent proposal；
2. 生成 canonical structured plan text；
3. 计算 plan hash 和 workspace snapshot binding；
4. append `PlanDraftCreated`；
5. emit bounded public Plan ready projection。

`submit_plan_draft` 由 agent loop 拦截、审计，不进入普通 tool registry，也不获得 workspace effect。

显式 `/plan` 和自动 `PlanReview` 都迁移到该 typed tool。为纯解释、总结或无法形成 executable steps 的
显式 `/plan`，模型可以返回普通只读 final text，不创建 Plan ready。自动 `PlanReview` 则必须提交一个
有效 draft；首轮未提交时 host 注入一次 bounded retry contract，第二次仍无 draft时以
`CompletedWithoutDraft` 收口，展示安全说明，但不创建 Task、不猜 Markdown steps。

现有 fenced `sigil-plan-v2` 只保留为 user-visible canonical rendering 和导出格式，不再作为新 Plan
review lifecycle 的 model-to-host control channel。

### 7.4 Durable lifecycle records

新增：

```rust
pub enum PlanReviewSource {
    ExplicitPlanCommand,
    AutomaticConversationRoute,
}

pub enum PlanReviewAttemptStatus {
    Started,
    DraftReady,
    CompletedWithoutDraft,
    Failed,
    Interrupted,
    Cancelled,
}

pub struct PlanReviewAttemptEntry {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: PlanReviewSource,
    pub source_turn: ConversationTurnRef,
    pub route_decision_id: Option<ConversationRouteDecisionId>,
    pub child_session_ref: SessionRef,
    pub status: PlanReviewAttemptStatus,
    pub terminal_reason: Option<PlanReviewTerminalReason>,
    pub recorded_at_ms: u64,
}
```

`PlanSourceRef` 扩展为可绑定 `source_turn`、`route_decision_id` 与 `plan_review_id`。Plan artifact、decision、
permission grant 与 Task link 继续复用 RFC-0018 当前类型，不建立第二套 Plan model。

状态流：

```text
ConversationRouteDecision(PlanReview)
  -> PlanReviewAttempt(Started)
  -> optional read-only discovery / planner tools
  -> submit_plan_draft
  -> PlanDraftCreated
  -> PlanReviewAttempt(DraftReady)
  -> user Accept | Reject | Revise | Save
  -> RFC-0018 TaskCreatedFromPlan or terminal decision
```

### 7.5 Plan decision

Plan ready 默认动作：

- `Run plan`：以 `CreateAndRun` 创建 Task，保留 normal permission behavior；
- `Save`：记录 `SavedOnly`，不执行；
- `Revise`：记录 `RevisionRequested` 并启动新 PlanReview attempt；
- `Reject`：记录 `Rejected`，不执行。

可选次级动作可以请求当前 RFC-0018 的 `WorkspaceEdits` scoped grant，但默认 `Run plan` 使用
`PlanApprovalPermission::Ask`。UI 不能把接受计划和“允许所有修改”合并成一个无说明按钮。

Plan decision 必须绑定 exact plan id/hash、workspace snapshot、stream frontier 和 decision actor。自动
PlanReview 不改变 RFC-0018 的 stale、retry、direct promotion、compatibility planner 和 verification
规则。

## 8. Shared application architecture

### 8.1 Ownership

`sigil-kernel` owns：

- `ConversationRoute`、reason、decision 与 projection；
- `PlanReviewId`、attempt lifecycle、run purpose 和 typed disposition；
- Plan artifact/source binding 扩展；
- append-only validation、idempotency、recovery 和 public event taxonomy；
- route/plan/task 间唯一 authority 和 terminal invariants。

`sigil-runtime` owns：

- `ConversationCoordinator` three-way routing；
- `AutomaticRouteCapability` resolution 和 route contract materialization；
- `PlanReviewCoordinator`；
- plan role provider/profile/tool registry 装配；
- `submit_plan_draft` internal tool handling；
- explicit `/plan`、automatic PlanReview、revision 的共享 application service；
- TUI/application driver 可共同调用的 prepare/run/reconcile API。

`sigil-tui` owns：

- 把 typed action 交给 worker；
- 渲染 Planning、Plan ready、decision 与 pending state；
- 键盘、mouse、focus 和 follow-up UX；
- 不解析模型文本决定 route/plan，不把 composer state 当 durable truth。

`sigil-http` / `sigil-desktop` / `apps/desktop` own：

- authenticated typed command、SSE replay/live 和 OpenAPI projection；
- native IPC allowlist 与 React plan card；
- renderer-local loading/focus/layout；
- 不复制 coordinator、plan parser、permission 或 task executor。

### 8.2 Shared projection

共享 public projection 至少包含：

```rust
pub enum PublicConversationPhase {
    Routing,
    Chat,
    Planning,
    AwaitingPlanDecision,
    Task,
    Terminal,
}

pub struct PublicPlanReview {
    pub plan_id: String,
    pub status: PublicPlanReviewStatus,
    pub summary: String,
    pub step_count: usize,
    pub target_path_count: usize,
    pub suggested_check_count: usize,
    pub risk: Option<String>,
    pub allowed_actions: Vec<PublicPlanAction>,
    pub source: PublicPlanReviewSource,
    pub stale: bool,
}
```

public DTO 不暴露：

- child session path、workspace absolute path、private ref、bearer；
- raw prompt、routing reasoning、policy hash、plan permission authority；
- renderer 可伪造的 approval receipt。

Desktop 与 TUI 可采用不同布局，但 `status`、actions、stale、counts、decision 和 Task link 必须一致。

## 9. Product surfaces

### 9.1 TUI

自动进入 Plan review 时：

1. routing phase 使用短暂 `routing` 状态，不把 router free text写入 timeline；
2. typed PlanReview decision 后显示 `Planning` phase marker；
3. live panel 显示 read-only research/progress；
4. draft durable 后显示现有 Plan ready card；
5. `Enter` 运行、`Esc` 拒绝，detail/revise/save 继续使用 panel-local action；
6. composer 可显示由 runtime phase 派生的 `Plan` label，但 `ComposerMode` 不是 authority，且不能让下一条
   普通输入意外继承一次自动 Plan state；
7. pending Plan decision 与 plain Plan-mode Esc 保持不同状态转换；
8. run 期间输入继续进入 follow-up queue；Plan ready 后的新输入按 guidance/revision/ordinary prompt 的
   typed action处理，不通过字符串猜测。

显式空 `/plan` 若继续保留 one-shot composer affordance，只影响用户下一次提交；它必须调用同一
`PlanReviewCoordinator`，不能保留独立 plan implementation。

### 9.2 Desktop

Desktop 必须补齐一等 Plan review surface：

- `/plan` 和自动 PlanReview 都产生相同 Plan card；
- timeline 显示 Planning phase、read-only progress 和 draft summary；
- Plan card 支持 Run、Save、Revise、Reject；
- Run 通过 authenticated typed application command 创建 Task，不把 plan text重新提交为普通 prompt；
- reconnect/reload 从 durable projection 恢复 pending Plan；
- active conversation、draft、IME、scroll、focus 不因进入/退出 Plan review remount；
- 320px、200% zoom、forced-colors、reduced-motion、keyboard/focus gate 继续满足 Desktop governance；
- OpenAPI snapshot、generated TypeScript、native typed client、Tauri allowlist、React interaction test 和
  current-source Desktop E2E 同步更新。

### 9.3 CLI/headless

普通 `sigil run` 不提供 interactive Plan decision presenter。若 automatic route 选择 PlanReview：

- `--non-interactive` 默认生成并输出 bounded plan artifact，terminal status 为
  `awaiting_plan_decision`；
- 不自动接受、不继续执行；
- automation caller 可通过现有/新增 typed plan decision command显式接受；
- 没有 durable store 或 command receipt 时 fail closed，不把 Plan review 偷换成 Task。

## 10. Permission, trust and autonomy

### 10.1 Route decision is not permission

以下事实必须始终成立：

- `ConversationRouteDecision(Task)` 只选择 orchestration path；
- `ConversationRouteDecision(PlanReview)` 只授权 read-only planning lifecycle；
- `PlanDecision(Accepted)` 只接受 exact plan 并创建 Task；
- `PlanPermissionGranted(WorkspaceEdits)` 只覆盖 RFC-0018 定义的 diff-backed scoped file edits；
- tool、shell、network、MCP、external-directory、merge/promotion 和 publish 继续各自求 permission。

### 10.2 Plan review tool scope

PlanReview 默认 tool surface：

- trusted workspace read tools；
- read-only code-intelligence；
- 一次 bounded host-owned read-only Explore discovery；
- `submit_plan_draft` internal tool。

当前 PlanReview 不继承普通 conversation 的 `websearch`/provider-hosted search、`webfetch` 或
remote MCP preparer。scoped registry 必须把 request-level preparer 与其依赖的模型可见 capability
一起裁剪，不能在工具已被 read-only scope 移除后继续向 provider request 注入 hosted capability、
产生 disclosure 或消耗 web budget。未来若要让 plan review 使用 remote read，必须单独定义有界
egress/tool surface 与对应 disclosure、预算和恢复契约，不能依赖普通 conversation 的隐式继承。

PlanReview 禁止：

- workspace write、shell execute、terminal input；
- write Agent、changeset proposal、worktree materialization；
- network mutate、MCP mutate、external-directory write；
- TaskPlan acceptance、mutation receipt、verification completion authority。

第三方自报 read-only 但无法证明 effect 的工具继续 `Ask/Deny`；headless fail closed。

### 10.3 Direct Task

DirectTask 只在目标清晰和 route qualified 时可见。即使 AI 选择 Task：

- executor 不自动获得 tool allow；
- `permission.mode = manual` 的 write/execute 继续 Ask；
- `auto-edit` 或其他已有 policy 的放行来自用户配置，不来自 routing；
- accepted TaskPlan 可按 RFC-0053 启动 participant，但 role/profile/ancestor/permission meet 不变；
- 高风险、重要取舍或 review-first instruction 应由 route contract导向 PlanReview。

## 11. Recovery and idempotency

### 11.1 Source and identities

- 每个 automatic route 绑定 exact persisted user turn 和 root logical run；
- route decision id、plan review id 和 plan id 使用不同 domain separator 确定性派生；
- source turn 不存在、内容冲突或 identity 复用时 fail closed；
- queued input 只在 dispatch 时创建 route binding，pending queue item 不提前进入 provider-visible history；
- model switch 后下一 user turn重新解析 capability；已 durable 的 pending Plan 不被新 route改写。

### 11.2 Crash windows

恢复规则：

| Durable prefix | Recovery |
| --- | --- |
| 无 route decision | 不猜测、不重放 provider request |
| `RouteDecision(PlanReview)`，无 attempt Started | 可创建同 identity attempt，但只有能证明 planner dispatch 未发生时才自动启动 |
| attempt Started，无 terminal/draft | 追加 Interrupted；不自动重放 uncertain provider request |
| `PlanDraftCreated`，缺 live event | 从 projection 重建 Plan ready |
| DraftReady，缺 user decision | 保持 pending，不自动接受 |
| RFC-0018 handoff partial prefix | 按 RFC-0018 deterministic reconcile |
| Task 已开始 | 按 RFC-0053 task recovery，不退回 PlanReview |

PlanReview 即使只读，也可能产生 provider费用和 external read；因此 crash 后不能仅因“没有 workspace write”
就自动重放不确定 attempt。

### 11.3 Cancel and session transitions

- routing cancel 在 accepted decision 前结束 root run，不创建 Plan/Task；
- PlanReview cancel 使用同一 root cancellation tree，child、discovery、network read 全部 quiescent 后才写
  Cancelled；cleanup不确定写 Interrupted；
- pending Plan decision 不占用 active provider run，但阻止同 plan 的重复 acceptance；
- session switch/new/fork 继续拒绝 active run；pending plan可随 session durable projection恢复；
- fork 重新绑定 session-local artifact refs，但保留 plan hash/source provenance；
- compaction 的 SessionAnchor/Continuity 必须保留 pending plan、decision、source turn、stale 和 linked Task
  facts；narrative summary 不能替代这些 authority。

## 12. Rollout and evaluation

### 12.1 Rollout change

本 RFC 完成后，默认行为从：

```text
manual unless exact route is qualified
```

改为：

```text
auto by default
  + ReviewFirst baseline
  + DirectTask only for exact qualified routes
```

`routing_policy = "manual"` 是明确、稳定、可诊断的 coarse rollback。

route-local kill switch 的降级顺序：

```text
DirectTask -> ReviewFirst -> Unsupported/Manual
```

不得因 DirectTask 的 duplicate task/spawn/merge invariant 失败，顺带关闭仍然安全且可审阅的 PlanReview；
也不得在 Plan lifecycle 自身出现 duplicate decision、stale acceptance 或 cross-surface mismatch 时继续宣称
ReviewFirst 可用。

### 12.2 Three-way eval corpus

RFC-0028/RFC-0053 corpus 扩展为三类：

- `Chat negative`：简单问答、单 symbol、线性 trace、单点小改；
- `PlanReview positive`：设计先行、重要取舍、范围不确定、高影响迁移、明确先审阅；
- `DirectTask positive`：目标明确的跨层实现、独立 workstreams、长验证、可直接执行的 durable goal。

必须包含一组 delivery-intent 成对回归，防止再次把“收尾”误留在普通 Chat，或因看见某个词就过度执行：

- “分批次收尾当前工作区所有变更”及不含 `提交/commit/task/plan` 的语义等价表达，在
  `DirectTask` capability 下必须选择 `Task`，在 `ReviewFirst` capability 下必须选择
  `PlanReview`，不得选择 `Chat`；
- “只分析当前工作区应该如何分批，不要修改或提交”及语义等价表达必须选择 `PlanReview`，不得选择
  `Task`；
- route decision 只决定 execution path，不授予 commit、merge、push 或其他 effect 权限；这些动作仍按
  既有 typed permission 和 approval contract 独立处理。

每个 exact route 至少包含：

- 20 个 Chat；
- 15 个 PlanReview；
- 15 个 DirectTask；
- 中英文与不含“plan/task”关键词的语义等价 case；
- adversarial case：多文件但单一 bounded outcome、单文件但高影响迁移、用户明确拒绝执行、用户明确要求
  直接执行、workspace instruction 强制 review-first。

DirectTask qualification 门槛：

- Chat -> Task false positive `<= 5%`；
- PlanReview -> Task premature execution `= 0`；
- DirectTask miss `<= 10%`；
- duplicate route/plan/task/final `= 0`；
- permission monotonicity violation `= 0`；
- Desktop/TUI public projection mismatch `= 0`。

ReviewFirst baseline 门槛：

- Chat -> PlanReview over-route `<= 10%`；
- required PlanReview miss `<= 10%`；
- PlanReview 自动创建 Task 或发生 workspace mutation `= 0`；
- invalid/free-text route 在 retry 后被错误当作用户回答 `= 0`；
- pending plan restart/reconnect 丢失 `= 0`。

评测还必须记录 routing microturn 的额外 latency、input/output tokens、cache impact 和 cost。默认开启不能
隐藏每个普通 prompt 增加一次 provider decision 的真实代价。

### 12.3 Doctor

Doctor 使用用户可理解的三项事实：

```text
automatic routing: enabled | disabled | unavailable
automatic plan review: available | blocked
direct task execution: qualified | review-first fallback | unavailable
```

detail 可显示 route/build fingerprint、manifest status、kill-switch reason 和 eval identity；默认设置页不暴露
完整策略矩阵。

## 13. Implementation slices

### R63.0 — Contract and current-only schema

范围：

- 冻结本 RFC；
- 为 RFC-0018/RFC-0053 增加本 RFC supersession note；
- 冻结三路 reason enum、capability tier、default config 和 current-only cutover；
- 删除 `TaskMode/default_mode` 的目标 schema 与全部文档示例。

验收：docs links、术语、配置示例和现状/目标边界一致。

### R63.1 — Kernel route and PlanReview lifecycle

主要范围：

- `sigil-kernel` route decision/domain/projection；
- `PlanReviewId`、attempt、run purpose、disposition；
- `PlanSourceRef` binding；
- event taxonomy、session entry/store、active projection 和 recovery validation；
- current session parser/conformance tests。

验收：duplicate/conflicting decision、source drift、invalid transition、crash prefix、compaction projection 和
roundtrip 全部 fail closed。

### R63.2 — Three-way router and capability resolution

主要范围：

- `request_plan_review` ToolSpec 和 validation；
- `ConversationCoordinator` three-way binding；
- agent routing-only microturn、retry、ignored tool 和 typed disposition；
- `Unsupported/ReviewFirst/DirectTask` resolution；
- queued/direct/application route parity；
- route contract digest 与 deterministic tests。

验收：Chat、PlanReview、Task 三类 fake-provider case；ReviewFirst 请求 Task 为零；invalid/free-text 不泄漏为
final answer。

### R63.3 — Shared PlanReviewCoordinator and typed draft

主要范围：

- 把 TUI-only plan run/handoff preparation下沉到 `sigil-runtime`；
- `submit_plan_draft` internal tool；
- explicit `/plan`、automatic route、revision 共用 runner；
- read-only tool scope、discovery、child session 和 cancellation；
- typed V2 -> canonical plan artifact；
- no-draft retry/terminal。

验收：无 write tool、无 duplicate User、无 Markdown guessing；valid draft 进入 RFC-0018，invalid/stale draft
不 promotion。

### R63.4 — TUI product surface

主要范围：

- worker protocol/classifier/handler；
- app runtime phase、pending plan、follow-up、Esc/Enter/revise/save；
- live panel、timeline、info rail、mouse/keyboard metadata；
- plan mode 与 pending-plan rejection 的独立状态转换；
- state/runner/view-model/renderer/PTY tests。

验收：普通 prompt 无 `/plan` 也能出现 Plan ready；composer mode 不作为 durable authority；restart 后 pending
card 恢复。

### R63.5 — HTTP, Desktop and public contract

主要范围：

- public phase/PlanReview DTO；
- SSE durable replay/live events；
- authenticated plan decision/revision command；
- OpenAPI snapshot/generated TypeScript/native client/Tauri allowlist；
- Desktop Plan card、timeline、actions、a11y/responsive；
- real `sigil serve` contract 和 current-source Desktop Gherkin E2E。

验收：Desktop/TUI 同一 session 的 plan status/action/stale/task link 一致；renderer 不持有 authority 或 private
path。

R63.5 完成状态：

- HTTP `POST /sessions/{id}/plan-decision`（Run/Save/Revise/Reject）已落地，typed command receipt 幂等
  （replay 返回同一 command id + `replayed`），real `sigil serve` contract 测试覆盖 display plan_review
  投影、decision 路由、replay 与未授权 401；
- public `PlanReview` 投影暴露 bounded plan id/hash、status、summary、counts、risk、allowed actions、
  source、stale；plan hash 是 content digest 而非 authority，decision 必须绑定 exact id/hash；
- `sigil-desktop` typed client（`plan_decision` + display 校验）与 Tauri allowlist
  `desktop_plan_decision`、generated ACL manifest、React `PlanCard`（Run/Save/Revise/Reject、stale/failure
  状态）已落地；OpenAPI snapshot 与 generated TypeScript 无 drift；
- 桌面 E2E fixture/feature/steps 已迁移到 ReviewFirst 流（`request_plan_review` → `submit_plan_draft` →
  plan card → Run/Save）；本机 wdio 运行在 webview DOM 层失败（driver attach 后 ~80ms DOM 消失，早于任何
  RFC-0063 代码路径），同一二进制手动启动时 backend + SSE + approval 全链路验证通过，判定为 harness 层
  问题而非实现回归；release pipeline 的 current-source Desktop E2E 需在 CI 复验。

### R63.6 — Default, rollout, Doctor and configuration

主要范围：

- `TaskRoutingPolicy::default() = Auto`；
- 删除 `default_mode`；
- Quick Setup、root/example config、configuration reference、README；
- rollout manifest从 binary on/off 扩展为 capability tier；
- kill-switch `DirectTask -> ReviewFirst`；
- Doctor facts和设置摘要。

验收：fresh/missing-field current config 默认为 Auto；explicit Manual 不变；unqualified route 使用 ReviewFirst；
qualified route允许 DirectTask。

R63.6 完成状态：

- `TaskRoutingPolicy::default()` 已切换为 `Auto`；fresh/missing-field config 默认
  `auto + explicit_request_only`（review-first 基线），explicit `manual` 语义不变；
- route-local hard-invariant kill switch 按 RFC 降级顺序落地：`DirectTask -> ReviewFirst`，
  保留安全的自动 plan review handoff，proactive spawn 同时降回 explicit authority；
  `direct_task_blocked` 是 capability tier 的唯一 gate，未引入第二套 authority；
- Doctor 输出 RFC 12.3 的三项事实（automatic routing / automatic plan review /
  direct task execution），detail 含 route digest 与 remediation；TUI `/doctor` 与 Desktop
  Doctor 复用同一 report；
- Desktop Quick Setup catalog 新增 `orchestration_rollout` summary，与 TUI setup summary、
  README、configuration reference、advanced-configuration、user-guide（en/zh-CN）共用同一
  默认事实；`default_mode` 无残留 schema 引用；
- gates：kernel 1460 / runtime 1007 / http 203 / desktop 63 / tui 1593 全绿；
  clippy -D warnings、fmt、desktop contract drift、renderer 276 tests 全绿。

### R63.7 — Evaluation and release closure

主要范围：

- three-way deterministic corpus；
- delivery-intent 的执行/只分析成对回归，以及不含 `提交/commit/task/plan` 的语义等价 case；
- real-model repetition、latency/cost/cache metrics；
- TUI PTY 与 Desktop E2E；
- cancel/restart/queue/compaction/route-switch chaos；
- 中英文用户文档、changelog、core technical solution 与 release rule。

验收：第 12 节门槛全部满足后，才将 RFC 状态改为 implemented。

R63.7 完成状态：

- `OrchestrationEvalCaseClass` 三路化为 `Chat | PlanReview | DirectTask`，report schema v2；
  observation 记录 durable three-way route decision 计数，gate 推导 Chat→Task FP（<=5%）、
  PlanReview→Task premature（=0）、DirectTask miss（<=10%）、ReviewFirst baseline 的
  Chat→PlanReview over-route 与 required-PlanReview miss（<=10%），以及 majority-misroute /
  duplicate-repetition / hard-invariant 检查；
- `dev/evals/model-fixtures/orchestration-v1` 冻结为 20 Chat / 15 PlanReview / 15 DirectTask
  （rfc-0063-orchestration-v1），包含 delivery-intent 成对回归（收尾批次的中英文与无
  `提交/commit/task/plan` 语义等价表达、只分析不修改）、adversarial case（多文件单 outcome、
  单文件高影响迁移、明确拒绝执行、明确直接执行、workspace instruction review-first）；
  route-contract derivation 校验完整 corpus 计数与单一 corpus version；
- route-local kill switch 混沌路径有测试覆盖：qualified route 先 DirectTask，hard invariant
  落地后降级 ReviewFirst 且保留 plan review binding（`direct_task_blocked` 单一 gate）；
- routing microturn 的 latency/input/output tokens/cache/cost 由 model-eval usage 记录
  （microturn 是 run 的第一个 provider request，随 `ModelEvalReportRecordV3` 逐请求统计）；
  TUI PTY / 真实 `sigil serve` contract / CLI process tests 迁移到新默认（Auto + typed routing
  decision）并通过；Desktop Gherkin E2E 已迁移到 ReviewFirst 流，本机 wdio 在 webview DOM
  层失败（harness 层，早于任何 RFC-0063 代码路径），release pipeline 需复验；
- changelog（en/zh-CN）、README、configuration/advanced-configuration/user-guide、
  core technical solution 与 Doctor 文档已同步。

## 13.1 Post-implementation audit fixes

Release-validation-pending 状态下完成的全量审计修复（2026-08-04）：

- P1：PlanReview 只读边界由 host 强制执行——`build_plan_review_tool_registry` 用硬编码只读
  scope 与可配置 planner allowlist 求交并附加 mutation deny list；`run_plan_review` 强制
  `PermissionMode::ReadOnly`；TUI/application 两个调用点全部切换。测试覆盖“planner 配置了
  write_file/bash 仍零暴露”。
- P1：草稿绑定 workspace snapshot——`PlanReviewRunRequest.workspace_snapshot_id` 由
  prepare/revision 路径用 `plan_handoff_workspace_snapshot_id` 生成并写入 draft；
  `create_task_from_plan` 在 snapshot 未变时 direct-promote（task_plan_version=1），变化时
  降级 compatibility planner 并持久化 stale_reason；display `plan_review.stale` 从
  draft binding 与当前 snapshot 真实推导（`conversation_display_page(_from_records)` 新增
  current snapshot 参数，HTTP driver 传入解析后的 workspace root）。
- P1：TUI 使用真实 provider capability——两处 `provider_supports_routing_tools: true` 替换为
  `agent.provider_capabilities().supports_tool_stream` / `provider_capabilities.supports_tool_stream`；
  不支持 tool stream 的 provider（如 Gemini）不再进入自动 routing。
- P1：RFC 状态降级为 `implementation-complete / release-validation-pending`；real-model
  campaign 与 current-source Desktop E2E 通过前不得标记 implemented。
- P2：Cancelled/Failed 终止路径写入 durable attempt——新增
  `PlanReviewCoordinator::close_plan_review_run`，TUI 与 application 两个终止分支都调用，
  不再残留 Started 后被 recovery 猜测成 Interrupted；`UserCancelled`/`RunFailed` 原因持久化。
- P2：`plan_review` 定向测试封闭——`application_plan_review_continuation...` 用 EnvScope 固定
  占位 `SIGIL_API_KEY`，`env -u SIGIL_API_KEY cargo test -p sigil-runtime plan_review` 全绿。

第二轮审计修复（2026-08-04，live recheck 后）：

- P1：Desktop/HTTP `Revise` 现在真实执行新 PlanReview——runtime 新增
  `execute_plan_review_revision`，HTTP driver 在 `plan_decision(Revise)` 后 spawn
  `HttpPlanReviewRevisionEventHandler` 运行 read-only revision 并提交新 draft。修复过程中发现并
  修正 kernel 投影两处 latent bug：同 attempt 的合法 Started→terminal 迁移不再误报 conflict
  （`legal_same_attempt_transition`），`latest_active_attempt` 跳过已有 terminal 记录的 attempt。
- P1（revision 跨进程恢复语义）：`Started` 记录的所有权从 prepare 移到 run executor——
  `prepare_plan_review_revision` 只持久化 `RevisionRequested` decision，
  `PlanReviewCoordinator::ensure_attempt_started` 由 `execute_plan_review_revision` 与 TUI
  `run_prepared_plan_review` 在 run 前写入。原实现把 `Started` 持久化在 plan_decision 调用里，
  executor 重新加载 session 时 recovery 会把尚未运行的 attempt 误判为 crashed run 并关闭成
  `Interrupted`，导致 revision 的 DraftReady 提交 conflict（
  `execute_plan_review_revision_runs_the_new_attempt_and_commits_the_draft` 端到端测试覆盖）。
- P1：TUI revision 路径切换 fail-closed registry——`build_plan_review_tool_registry` 同时用于
  automatic 与 revision 两个调用点。
- P1：HTTP display 与 plan decision 统一 workspace root——driver 的 plan 相关请求全部经
  `resolve_workspace_root` 解析 `config.workspace.root`。
- P1：TUI pending plan 增加 stale 投影——`PendingPlanApproval` 携带 base
  `workspace_snapshot_id`/`stale`/`stale_reason`，由 `plan_handoff_stale_reason` 真实推导；
  stale 时 Run/Save 被阻断并显示原因（live panel 警告行 + footer 缩略提示），Revise/Reject 保留
  作为恢复/退出路径（与 Desktop 的差异有意为之并在 UI 提示中呈现；runtime fail-closed 校验仍
  是最终护栏）。
- P2：终止 closure 覆盖全部返回路径——`run_plan_review(...).await?` 的 Err 路径在三个 executor
  （runtime continuation、`execute_plan_review_revision`、TUI `run_prepared_plan_review`）都先
  `close_plan_review_run_if_open`（attempt 已 terminal 或从未 Started 时为 no-op）再返回原错误，
  closure 自身失败时两个错误都透出；TUI 不再静默丢弃 close 错误；新增
  `PlanReviewRunOutcome::Interrupted`，`AgentRunDisposition::Interrupted` 关闭为
  `Interrupted/RunInterrupted` 而不是 Failed。
- P2：`SIGIL_API_KEY` EnvScope 加全局 `test_env::lock()`，环境变更与其他读凭据测试串行化。

## 13.2 Validation ledger（两轮审计修复后）

2026-08-04/2026-08-05 全量验证记录（worktree `worktree-rfc-0063-implementation`，无 stage/commit）：

| Gate | 命令 | 结果 |
| --- | --- | --- |
| format | `cargo fmt --all --check` | pass |
| workspace check | `cargo check --workspace` | pass |
| workspace tests | `cargo test --workspace` | pass，0 failed（kernel 1460、runtime 1011、TUI 1594、HTTP 237、desktop/tauri、sigil 全绿） |
| clippy | `cargo clippy --all-targets -- -D warnings` | pass |
| canonical full tier | `./scripts/check-touched.sh --tier full` | pass（exit 0：fmt + check + test + clippy） |
| Desktop | `pnpm --dir apps/desktop check` | pass（仅 rolldown chunk-size warning） |
| docs | `./scripts/check-docs.sh` | pass（修复两处 changelog 内部术语 "durable projection"） |
| hermetic targeted | `env -u SIGIL_API_KEY cargo test -p sigil-runtime --lib plan_review` | 14 passed，0 failed |
| revision e2e | `execute_plan_review_revision_runs_the_new_attempt_and_commits_the_draft` | pass（本地 SSE fixture 端到端） |
| TUI stale | `stale_pending_plan_blocks_run_and_save_but_keeps_revise_and_reject` 等 | pass |
| kernel projection | `cargo test -p sigil-kernel --lib` | 1460 passed，0 failed |

第三轮审计修复后复验（2026-08-05）：

| Gate | 命令 | 结果 |
| --- | --- | --- |
| workspace tests | `cargo test --workspace` | 5434 passed，0 failed |
| clippy | `cargo clippy --all-targets -- -D warnings` | pass |
| canonical full tier | `./scripts/check-touched.sh --tier full` | pass（exit 0） |
| Desktop | `pnpm --dir apps/desktop check` + `pnpm vitest run` | pass（276/276） |
| docs | `./scripts/check-docs.sh` | pass |
| TUI revision lifecycle | `plan_revision_runs_supervised_review_returns_session_and_surfaces_new_draft` | pass |
| runtime finalizer | provider-construction 失败 / draft-commit conflict 回归 | 均 pass，attempt 以 `Failed/RunFailed` 终止 |
| HTTP production E2E | `production_plan_review_revision_runs_supervised_and_publishes_terminal_event` | pass（生产 driver + SSE：terminal event + stream close + foreground slot 释放） |

另修复：`child_logical_run_id` 从 `plan-review:{uuid}:{uuid}` 改为 `plan-review-{uuid}-{uuid}`，
因为 HTTP SSE cursor 以 `:` 分隔组件，冒号会导致事件发布失败（生产 driver E2E 暴露）。

仍属 `implementation-in-progress`：real-model campaign、Desktop Gherkin E2E（本机 wdio
webview DOM harness 失败，早于 RFC-0063 代码路径）、PTY 验收脚本与 three-way eval 门槛的
release 复验未在本机完成，按 §15 acceptance criteria 与 §12.2 门槛执行。

## 13.3 Third audit fixes（2026-08-05）

第三轮审计（3 项 P1、1 项 P2）及修复：

- P1：TUI Revise 改为受监督的 ActiveRun——revision 不再丢弃 Session 与 JoinHandle：走
  `RunTaskResult` 通道归还 session、注册 `state.run.active`（cancel/shutdown 拥有它）、
  emit `PlanRunStarted`，成功经 `PlanRunFinished` 由 worker_bridge 从 durable projection 重建
  新草稿卡片；`WorkerMessage::PlanRevised` 变体删除。新增 worker 生命周期 e2e：
  `plan_revision_runs_supervised_review_returns_session_and_surfaces_new_draft`。
- P1：HTTP Revise 改为受监督的 owned run——revision 持有 session attachment 全程（并发 mutation
  串行化）、注册 `active_runs`（wait_for_idle/cancel/shutdown 拥有它）、cancel_sender 接
  cancellation 命令、完成后用 `publish_next_run_event_and_close_stream` 补发显式 terminal
  public event（RunFinished/RunCancelled/RunFailed）并移出 `active_runs`；
  `HttpPlanDecisionCommandReceipt` 新增 `revision_run_id` 让客户端按 run identity 订阅；
  OpenAPI 与 desktop contract 同步。
- P1：Desktop stale plan 保留恢复入口——按 action 禁用：stale 只禁用 Run/Save，Revise（基于
  当前 workspace 重新规划）与 Reject（退出）保持可用，与 TUI 语义一致；`submitPlanDecision`
  守卫同步；stale notice 文案说明恢复路径；`App.test.tsx` 更新为 per-action 断言。
- P2：Started 之后的全部错误路径统一 finalizer——`execute_plan_review_revision` 把 provider
  构造、tool 注册、run 与 durable outcome commit 包进单一错误 finalizer，任何 `?` 失败都先
  `close_plan_review_run_if_open(Failed)` 再透出错误（closure 自身失败时两个错误都可见）；
  新增 provider-construction 失败与 draft-commit conflict 两个回归测试，均断言 attempt 以
  `Failed/RunFailed` 终止而不是残留 Started。

## 13.4 Fourth audit fix（2026-08-05）

- P2：HTTP Revise 的 spawn 前注册改为可回滚事务——先注册 `active_runs`（加锁失败与重复
  run id 检测在任何注册之前失败，无副作用），最后才绑定 registry foreground slot；bind 失败
  时用 `release_owned_revision_run` 回滚 active-run 注册。原实现先绑 slot 再注册 active_runs，
  若加锁失败或发现重复 run id 直接返回，slot 永久占用：没有 active run 可取消/清理，后续
  mutation 被 `SessionForegroundRunActive` 拒绝，且 `RevisionRequested` 已持久化无法重试。
  回归测试 `production_revision_duplicate_registration_never_blocks_the_session`：预置重复
  run id → Revise 被拒绝 → 断言 session 仍可 `reserve_durable_session_mutation`、预置 entry
  未被误删。
- P3（评审建议，非阻塞）：bind 失败的回滚改为专用 `rollback_revision_run_registration`（只移除
  run-map entry 并唤醒等待者），不再复用 `release_owned_revision_run` 的通用“已拥有 slot”释放
  语义（bind 未成功就不应 unbind）。定向测试
  `production_revision_bind_failure_rolls_back_only_the_run_registration`：预置其他 run 占用
  slot → spawn 拒绝 → 断言 run-map 已回滚、预置 slot owner 未被误释放（mutation 仍被
  OTHER run 阻断）。

## 13.5 Fifth audit fixes（2026-08-05）

- P1：Desktop `Revise` 成功响应端到端携带 `revision_run_id`——`sigil-desktop` 的
  `DesktopPlanDecisionCommandReceipt`（strict `deny_unknown_fields`）、Tauri IPC
  `DesktopPlanDecisionSummary`、React `PlanDecisionSummary` 全部声明可选字段并投影；
  `ConversationPanel` 消费它（Revise 成功后通知 "plan revision started"）。
  契约测试 `plan_decision_revise_accepts_the_supervised_revision_run_identity`：真实 HTTP
  成功 JSON（含 `revision_run_id`）→ `DesktopClient::plan_decision` 解码成功；交互测试断言
  非空 run identity 触发的通知。
- P1：public projection 表达无草稿 attempt——`PublicPlanReview`/`HttpPlanReview`/
  `DesktopPlanReview`/Tauri/React 的 draft-specific 字段（plan_hash/summary/counts）全部
  变为可选；`PlanReviewDisplayProjection::into_public` 先投影 attempt status，draft 详情仅在有
  draft 时输出，`Started`/`Failed`/`Interrupted`/`Cancelled`/`CompletedWithoutDraft` 不再从
  display 消失，reload/reconnect 可恢复 Planning 与无草稿终态；`DraftReady` 保持既有
  actions/stale 语义。测试：runtime display 投影（Started→Failed→Cancelled 无 draft 均可见、
  无 actions）、desktop client decode/validate（含 draft-less JSON）、openapi/contract 同步。
- P2：spawn 被拒后旧 plan 可恢复——新增 kernel `PlanDecision::RevisionFailed`；driver 在
  `RevisionRequested` 已持久化而 spawn 注册失败时调用
  `application_record_revision_failure` 追加 durable `RevisionFailed` fact；
  `prepare_plan_review_revision` 允许在 `RevisionFailed` 后重试同一 retry-stable revision
  identity，`record_plan_decision` 允许在 `RevisionFailed` 后执行 Run/Save/Reject。
  `production_revision_duplicate_registration_never_blocks_the_session` 扩展：断言 durable
  `RevisionFailed` 已持久化、原 plan 的 Save 决策成功（plan 决策恢复路径，而非仅 slot 可用）。

## 13.7 Orchestration PTY acceptance ReviewFirst adaptation（2026-08-07）

`scripts/tui-orchestration-pty-acceptance.py` 适配 RFC-0063 的 ReviewFirst 基线：无发布资格
manifest 时自动路由降级为 ReviewFirst（工具集为 `request_plan_review` / `continue_without_task_planning`，
不含 `request_task_planning`），fixture 相应改为：routing 微轮返回 `request_plan_review`，新增
`plan_review` 请求分支按场景返回 `submit_plan_draft`（draft 即可执行计划，plan accepted 后 task
直接按 draft 执行，不再经过 `task_plan_update` planner 阶段），每个场景在 TUI 的 "Plan ready" 卡片
按 Enter 批准，approval 卡片匹配放宽到 "Approve action?" / "Review file changes"，断言从
`task_handoff_requested/resolved` 改为 `plan_draft_created` 计数与新的请求分布。本地
`python3 scripts/tui-orchestration-pty-acceptance.py --binary target/debug/sigil` 通过；
CI 的 real-PTY acceptance 此前自 2026-08-03 memory 合并后持续失败，本次适配后应恢复。

## 13.6 Fifth-audit review（2026-08-06）

第五轮审计修复复核通过：三项修复（`revision_run_id` 端到端投影、无草稿 attempt 的 public projection、
`RevisionFailed` 恢复路径）均已在 main 合入；按 §14 的 targeted gates 执行
`cargo test -p sigil-kernel conversation_route / plan_review`、`cargo test -p sigil-runtime
conversation_coordinator / plan_review`、`cargo test -p sigil-tui plan / routing`、
`cargo test -p sigil-http plan_review` 全部通过（115 passed），`./scripts/generate-desktop-contract.sh
--check` 与 `pnpm --dir apps/desktop check` 通过。剩余 release 门槛不变：real-model campaign
（§12.2 三路 eval，消耗真实额度）与 current-source Desktop E2E（需 CI 复验）未通过前不得标记
implemented。

## 13.8 Real-session convergence and provider-interruption remediation（2026-08-14）

对 session `72d5c0cc-73c3-4e1d-9530-5da992cb1fba` 的 parent/child JSONL 逐条复盘确认：
PlanReview 在约 335 秒内发起 10 次 provider physical attempt、完成 26 次只读工具调用，却没有调用
`submit_plan_draft`；child request prefix 从约 13 KiB 增长到 238 KiB，最后一次响应已产生输出后因
TLS `unexpected EOF` 被 durable 分类为 `ProtocolRejectedAfterOutput`，原实现直接把整个 review
终止为 Failed。该 session 同时暴露出普通 conversation 的 hosted preparer 被 PlanReview scoped
registry 隐式继承，以及旧配置把历史默认 `4/3` 固化为显式 web cap 的问题。

本轮将 PlanReview 收敛契约改为两个 host-owned phase：research phase 最多 4 个 model turn；随后
最多 1 个 submit-only finalization turn，后者只暴露 internal `submit_plan_draft`，不得继续研究或
继承 ordinary-conversation hosted preparer。若 research 正常完成但未提交 draft，或 durable
physical-attempt 明确为 `ProtocolRejectedAfterOutput`，只允许基于 child session 已记录证据进入这
一次 finalization；`TransportOutcomeUncertain`、零输出连接失败或其他未证明边界不自动重放。
两个内部 phase 使用 child cancellation，外层 coordinator 只在最终 outcome 确定后竞争一次 root
natural terminal，避免子 run 提前 finalize 使后续收尾失去监督。

hosted request-level preparer 现在绑定其真实模型可见 capability；`websearch` 被 PlanReview allow/deny
scope 移除后，不再注入 hosted tool、展示 disclosure 或占用 hosted budget。ordinary hosted request
先取得 provisional reservation，只有 provider stream 已建立、provider 已响应或 transport outcome
不确定时才 commit request count；pre-wire validation、本地 pre-dispatch cancellation 和已证明的
`ConnectFailedBeforeDispatch` 会写唯一 terminal outcome 并释放 reservation。预算耗尽判断先于
disclosure，未发送的请求不再产生虚假披露。

新增回归覆盖：4 research + 1 submit-only 的精确请求分布、TLS 输出后中断的 submit-only recovery、
uncertain transport 不重放、PlanReview scope 的零 hosted injection/disclosure/charge、pre-wire 与
本地 cancellation refund、connect retry 复用一次 provisional reservation、全量 pre-dispatch connect
failure refund，以及 stream established/NotUsed 和 uncertain transport 均只收费一次。RFC 状态仍为
implementation-in-progress；本轮不替代 §12 的 real-model campaign 与 Desktop E2E release 门槛。

本轮实际 gate：`cargo fmt --all --check`、`cargo check --workspace`、`cargo test --workspace`
（0 failed）、`cargo clippy --all-targets -- -D warnings`、`pnpm --dir apps/desktop check`
（Vitest 276/276 + production build）、desktop contract drift、`./scripts/check-docs.sh`、
`python3 scripts/test-tui-stateful-pty-acceptance.py`（45/45）与 `git diff --check` 全部通过。
需要 checksum-pinned DeepSeek V4 tokenizer 的 real-binary stateful PTY campaign 本机未执行，不能由
上述 deterministic terminal replay 冒充。

## 13.9 Plan Review workbench, revision guidance and finalizer isolation amendment（2026-08-15）

session `5aeeb257-83fb-41c5-809b-68edcc0be15a` 暴露出 §13.8 收敛修复仍未覆盖的产品与生命周期缺口：

- DraftReady 只在 12-row live panel 显示 compact summary、3 个 step title 与
  `plan details truncated`；用户在 Run/Revise 前无法审阅完整方案；
- TUI 把 `r` / `s` / `Esc` 作为 pending-plan 全局快捷键，空 composer 中 printable key 被吞，`Esc`
  还直接等价 Reject；
- Revise 不先收集 guidance，而是复用原 objective 和固定 reason；
- research child 与 submit-only finalization 仍共享 conversation/history，使 finalizer 在只允许
  `submit_plan_draft` 时继续调用 research tool，最终表现为 `unknown tool grep`；
- revision Started 后 Failed/Interrupted 会让 latest attempt 覆盖原 DraftReady plan，原 plan actions 消失；
  terminal attempt identity 又被复用，retry 无法形成新 execution attempt。

本节是 RFC-0063 的规范性增补；与旧 §7/§9/§11 冲突时，以本节为准。

### 13.9.1 Dedicated Plan Review workbench

Planning/Researching 仍是 conversation live progress；DraftReady 后，plan 成为 shell 主内容模式，不再是
status band 或 renderer-local modal。TUI/桌面端都保留 compact card 作为入口，但 Run/Save/Revise/Reject
只能从能访问完整 detail 的 review workbench 发起。

TUI 响应式规则：

- plan 内容有效宽度至少 96 columns 时保留 info rail；不足时自动折叠 rail；
- 高度不超过 11 rows 时 workbench takeover，临时隐藏 composer 与 rail；退出 workbench 后恢复；
- 正文独立滚动，action bar 固定；resize 后 clamp scroll/action focus，不以终端 cursor 作为状态源；
- `Enter` 从 compact card 打开 workbench，workbench 内确认当前选中 action；
- `Up/Down/PageUp/PageDown/Home/End` 滚动，`Tab/Shift-Tab` 或 `Left/Right` 选择 action；
- `Esc` 只关闭 workbench，不产生 domain decision；Reject 必须显式选中并确认；
- workbench 关闭且 composer 可编辑时，printable character 必须进入 composer；`Shift-Tab` 可重开当前
  pending plan。

TUI durable projection 与 local navigation state 分离。reload/reconnect 只重建 plan/status/actions，scroll、
focus 与 temporary close 状态重新初始化。`PendingPlanApproval` 不再作为 detail authority；它可以缓存从
durable projection 得到的 immutable public detail 以供同一帧渲染，但 identity/hash/actions 才是 command
binding，cache 不能授予或扩大 authority。

### 13.9.2 Complete plan detail contract

public surface 分两层：

```rust
pub struct PublicPlanReviewSummaryV1 {
    pub active_plan: Option<PublicActivePlanSummaryV1>,
    pub revision: Option<PublicPlanRevisionSummaryV1>,
    pub attempt_status: PlanReviewAttemptStatus,
    pub source: PlanReviewSource,
}

pub struct PlanReviewDetailV1 {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub workspace_snapshot_id: String,
    pub source: PlanReviewSource,
    pub summary: BoundedPlanSummary,
    pub steps: Vec<PlanReviewStepDetailV1>,
    pub target_paths: Vec<String>,
    pub suggested_checks: Vec<String>,
    pub risk: PlanRisk,
    pub notes: Vec<String>,
    pub lineage: PlanLineageV1,
    pub legacy_markdown: Option<String>,
}
```

step detail 至少包含 title、detail、role、mode、isolation、depends_on、target paths 与 suggested checks。
TUI 与 Desktop 必须消费同一个 kernel converter，不能分别从 session records 猜字段。

完整 summary 在 typed draft validation 时最多 2 KiB；超过上限拒绝 draft，不在 durable artifact 中静默
截断。conversation/SSE compact summary 仍可限制为 160 chars，但必须设置 `summary_truncated=true`，并
以 Unicode scalar/grapheme-safe 方式产生显式省略标记。

本地 bearer 认证 HTTP 增加：

```text
GET /sessions/{session_id}/plans/{plan_id}?expected_plan_hash=...
```

响应绑定 `session_id + plan_id + plan_hash`，hash mismatch/stale/cross-session 均 fail closed；immutable
detail 返回 ETag。Desktop strict DTO/Tauri IPC/React type 与 OpenAPI 同步，TUI 直接调用同一 kernel detail
service。legacy plan 只允许 bounded `legacy_markdown` fallback，不伪造 structured step fields。

### 13.9.3 Revision lifecycle

Revise 首先使用 [RFC-0064](0064-durable-user-input-requests-v1.md) 创建 host-owned
`RevisionGuidance` request；guidance 提交前不得 append `RevisionRequested` 或 dispatch provider。
answer acceptance 与 `PlanRevisionRequestedV1` 在同一 application mutation/transaction 中落盘。

identity 分离：

```text
revision_request_id       一次用户修改意图，retry-stable
attempt_id + ordinal      每次真实 execution，retry 必须新建
resulting_plan_id         成功 draft 的 immutable identity
```

canonical projection 同时保留：

```text
active_plan: DraftReady base plan
revision:
  awaiting_guidance | queued | researching | waiting_for_input |
  finalizing | failed | cancelled | succeeded
```

candidate attempt 不得替换 `active_plan`。revision pending/running 时暂停 Run/Save，但允许 Review original、
Answer/Cancel revision；Failed/Interrupted/Cancelled/CompletedWithoutDraft/
SubmitOnlyProtocolViolation 必须 append terminal revision close 并恢复 base plan 的
Run/Save/Revise/Reject。成功 draft 通过 base plan id/hash、revision request id 与 attempt identity 做
lineage 校验后，才原子切换 active plan。retry 保留 guidance/revision request，创建新 attempt ordinal；
不得复用 terminal attempt。

`SavedOnly`、`RevisionRequested` 与所有 draft-less terminal status 的 allowed actions 由同一 reducer 按
active-plan/revision state 推导；public converter 不能仅因 latest attempt 是 DraftReady 就输出四个动作。

### 13.9.4 Fresh submit-only finalizer

research 与 finalization 是两个隔离 child execution context：

- research child 只读、bounded turn，可使用 RFC-0064 提问；
- finalizer 使用 fresh child session/request，不继承 research assistant/tool messages、provider continuation
  handle 或普通 hosted preparer；
- host 传入 bounded evidence bundle：base plan/reference、revision guidance、research result summaries、
  artifact refs/hashes、workspace snapshot、remaining frontier；不得回填无界 tool transcript；
- finalizer 模型可见工具只有 `submit_plan_draft`，且系统契约明确本 turn 必须且只能提交一次；
- 任意其他 tool call 在 dispatch 前转为 typed `SubmitOnlyProtocolViolation`，不得进入 registry 的
  unknown-tool 路径，更不得执行；
- host 最多创建一次 fresh corrective finalizer attempt。再次违反协议或未提交 draft即关闭 attempt/
  revision，恢复 base plan；
- child draft 已 durable、parent commit 未完成时，recovery 从 child artifact 执行 exact parent commit，
  不 replay provider generation。

research result 与 evidence bundle 必须按 RFC-0057/0059/0062 的 bounded projection + durable artifact
原则构造；finalizer 不通过继续增长 research transcript 来“保留证据”。

### 13.9.5 Migration and recovery

current schema 采用 clean-cut current-only records。旧 session 中只有单一 revision attempt 的数据通过
read-only compatibility projection 映射为：

- 最近一个有效 DraftReady plan => `active_plan`；
- 后续 Failed/Interrupted/Cancelled/CompletedWithoutDraft attempt => terminal `revision` summary；
- base plan actions 按 stale/decision facts恢复；
- 无法证明 base lineage 或出现冲突 terminal facts => typed unsupported/corrupt projection，禁止自动猜测。

session `5aeeb257-83fb-41c5-809b-68edcc0be15a` 必须成为固定回归 fixture：旧 base DraftReady 仍可完整
review；finalizer 的 `grep` calls 被映射为 submit-only violation；revision terminal failure 后 base actions
恢复；Retry revision 先请求 guidance，再创建新 attempt identity。

### 13.9.6 Implementation slices and validation ledger

增补实施分为三个可独立验收但不可冒充整体完成的 slice：

- `R63.A`：完整 Plan detail converter/HTTP/DTO + TUI/Desktop workbench 与键位/响应式布局；
- `R63.B`：RFC-0064 revision guidance、active-plan/revision 双投影、全失败恢复与 retry identity；
- `R63.C`：fresh finalizer、submit-only typed violation、child-to-parent crash recovery 与 legacy fixture。

除 §14 既有 gate 外，至少增加：

```bash
cargo test -p sigil-kernel plan_review_detail
cargo test -p sigil-runtime plan_revision
cargo test -p sigil-http plan_detail
cargo test -p sigil-desktop plan_detail
cargo test -p sigil-tui plan_workbench
pnpm --dir apps/desktop check
./scripts/generate-desktop-contract.sh --check
python3 scripts/test-tui-stateful-pty-acceptance.py
./scripts/check-touched.sh --tier full
```

必须覆盖 tiny/wide/resize、Esc 非 Reject、printable key 不被吞、detail hash/ETag、reload/reconnect、所有
revision terminal branch、retry new attempt、non-submit no-dispatch、child durable draft crash gap、真实
`sigil serve` strict DTO 与 Desktop E2E。上述三 slice 与 RFC-0064 release validation 未全部通过前，
RFC-0063 保持 `implementation-in-progress`。

## 13.10 Interactive Task planner suspension and recovery（2026-08-16）

首轮 qualified-route real-model smoke 暴露出一个此前 deterministic fixture 未覆盖的真实行为：Task
planner 会在计划前调用 RFC-0064 `request_user_input`，而 orchestrator 把合法的
`AwaitingUserInput` 当成“isolated planner did not produce an accepted plan”，导致 Task 失败，用户问题也无法
从 root surface 回答。该失败不是 route 误判；它说明 interactive planner 的 suspend/resume contract 尚未
接入 Task participant 生命周期。

本轮已完成以下闭环：

- planner 初始 turn 可返回 typed suspension；Task durable 转为 `Paused`，planner participant 与 child
  session 保持非终态，root session 写入 exact-bound public attention route；
- TUI 与 application/HTTP decision path 都在同一 supervised foreground owner 下回答，重新装配 planner
  provider/read-only discovery surface，并以同一 participant、同一 child transcript、新 physical attempt
  继续；guidance assessment 仍禁止提问，避免已接受计划的 control microturn 被悬挂；
- parent route 镜像 public answer lifecycle，私有答案只从 child session 恢复。child answer durable 后任意
  controller crash 都复用原 command；provider dispatch 后则按 physical-attempt evidence 安全释放或 fail
  closed，不盲目双发；
- kernel/runtime/TUI 分别新增 participant identity、application crash gap、真实 parent+child TUI restore 与
  supervised worker E2E。受影响 crate 全量测试与 `clippy -D warnings` 通过，具体计数见 RFC-0064 §19。

此前 smoke 在用户问题处中止的结果不得计入 §12 campaign。代码提交后必须重新生成 exact route identity，
先执行并逐条审阅新的 `1×50` smoke；只有零 invariant failure 且 route/plan-review/direct-task 分布符合门槛，
才允许执行 `3×50` paid campaign。RFC 状态仍为 `implementation-in-progress`。

## 13.11 Candidate smoke remediation（2026-08-16）

基于 §13.10 candidate 的首次完整 `1×50` 预检真实执行了 50/50 provider-admitted repetition，全部完成且
没有 hard invariant violation，但只得到 41/50 base acceptance，因此未进入 `3×50`。逐条 session 审计确认：

- 8 个 `orch-dt-*` 都已完成目标行为并通过 root verification，但旧 fixture 用
  `.to_ascii_lowercase()` 源码片段绑定一种实现，合法的 `.to_lowercase()` 被误判失败；corpus 改为运行未修改
  的 public acceptance test (`cargo test --quiet`)，不再以实现字符串替代行为 oracle；
- `orch-pr-06` 的 research 正常进入 submit-only finalizer，但模型提交了损坏 JSON。coordinator 原有的 fresh
  corrective attempt 只覆盖 non-submit tool，正确工具的 typed validation error 会错误终止为
  `Interrupted`；现在同样在独立 finalizer session 自动纠正一次，两次均失败才以 typed protocol violation
  关闭；
- 该 1× 样本还观察到一个跨层实施请求与一个中文比较设计评审各漏判为 Chat。routing contract 补强通用的
  跨层协调、比较设计评审和跨语言语义规则，不按 case id 或关键词特判。

第二次预检进一步发现两条 committed fixture 自身不满足其 capability contract：一个 direct-task prompt
要求修改 frozen workspace 中不存在的 `quota.rs`，一个 Chat trace 只授予 `read_file` 却没有给出所谓“三个
source files”的路径；两者都会合理触发 user-input suspension。fixture 已分别改回真实 parser/formatter
change set，并显式列出三个只读路径。该轮后半段还遭遇连续 DeepSeek TLS handshake EOF；所有外部失败均
保留为 provider outage 证据，不计入 candidate qualification。

受影响 kernel/runtime 全量测试与两 crate `clippy -D warnings` 已通过。上述修改改变 route/corpus identity，
必须从新 commit 重新生成 exact route contract 并重跑 `1×50`；旧 campaign 只作为失败证据，不能计入 release
qualification。RFC 状态仍为 `implementation-in-progress`。

## 14. Validation plan

按 slice 运行最小相关 gate，最终至少包括：

```bash
cargo test -p sigil-kernel conversation_route
cargo test -p sigil-kernel plan_review
cargo test -p sigil-runtime conversation_coordinator
cargo test -p sigil-runtime plan_review
cargo test -p sigil-tui plan
cargo test -p sigil-tui routing
cargo test -p sigil-http plan_review
pnpm --dir apps/desktop check
./scripts/test-tui-orchestration-pty-acceptance.py
pnpm --dir apps/desktop e2e:desktop
./scripts/check-docs.sh
./scripts/check-touched.sh --tier standard
```

核心跨 crate 语义完成后运行 `./scripts/check-touched.sh --tier full`。实际脚本名若在实施前发生变化，
R63.7 必须以当时仓库的 canonical gate 更新本文，不能新增仅为文档凑数的命令。

## 15. Acceptance criteria

RFC-0063 只有同时满足以下条件才可标记 implemented：

1. `routing_policy` current schema 默认是 `Auto`，显式 `Manual` 仍可关闭自动 handoff。
2. 普通输入可以由 AI typed decision 进入 PlanReview，不需要 `/plan` 或 host 关键词判断。
3. AI 从未直接修改 TUI/desktop composer state；UI phase 由 shared projection 派生。
4. 自动 PlanReview 使用 read-only tool surface，不能修改 workspace、运行 execute tool 或创建 write Agent。
5. valid typed V2 draft 在 Desktop 与 TUI 都显示可审阅 Plan ready。
6. Reject/Save/Revise/Run 都是 durable、idempotent、可恢复的 typed command。
7. Run 复用 RFC-0018/RFC-0053 Task path，不复制 executor，不把 plan text重新写成普通 User prompt。
8. unqualified route 默认 ReviewFirst；qualified route可以 DirectTask；Unsupported route 不猜自由文本。
9. DirectTask、Plan acceptance、permission grant 和 tool approval 是四个独立 authority。
10. default Auto 不扩大 `multi_agent_mode`、write、execute、network、MCP、external directory 或 merge权限。
11. queue、cancel、restart、session switch/fork、compaction、model switch 不重复 route、plan、task、provider
    attempt 或 final answer。
12. Desktop/TUI 的 pending plan、actions、stale、Task link 与 terminal状态语义一致。
13. `default_mode` 已从 current schema、示例、文档和运行时心智移除。
14. three-way deterministic/real-model eval 达到第 12.2 节门槛。
15. delivery-intent 成对回归证明明确交付请求不会停留在 Chat，分析/禁止执行请求不会进入 Task，且
    `ReviewFirst` fallback 不会被误判为 DirectTask authority。
16. targeted、standard、full、PTY、Desktop contract/E2E 和 docs gates 按阶段通过。

## 16. Rejected alternatives

### 16.1 让模型直接设置 `ComposerMode::Plan`

拒绝。它把 kernel semantic decision 绑定到 TUI 私有实现，Desktop 无法共享，restart 无法恢复，也让模型
获得 UI authority。模型只能请求 `PlanReview`，host负责 lifecycle，产品面只做 projection。

### 16.2 所有复杂任务都先显示 Plan ready

拒绝。它会给明确、低歧义、用户已经要求执行的目标增加不必要确认，削弱 RFC-0053 durable Task 的自动
收敛价值。三路 router 应保留 DirectTask。

### 16.3 所有复杂任务都直接进入 Task

拒绝。架构取舍、迁移边界、高影响操作和用户明确 design-first 的请求需要 first-class review surface；仅靠
后续 tool approval不能替代“做什么”的方案确认。

### 16.4 无 qualification 时回退 Manual

拒绝。PlanReview 是只读且需用户接受的安全中间层；DirectTask证据不足不应关闭整个自动能力。降级应是
`DirectTask -> ReviewFirst`。

### 16.5 用关键词决定 PlanReview

拒绝。它无法处理语义等价表达、workspace instruction 和跨语言请求，也会把“不要 plan”之类文本误判。
host只验证 typed decision，不替模型做 semantic classification。

### 16.6 继续保留未接线的 `default_mode`

拒绝。它把“输入框默认偏好”和“AI semantic route”混成两个近似模式，并且 Desktop/TUI 当前没有统一消费。
显式 `/plan` 已足够表达用户主动选择，`routing_policy` 表达自动选择。

## 17. Risks and mitigations

### 17.1 默认增加一次 routing provider call

风险：普通 prompt latency、token 和费用上升。

缓解：routing-only request 保持最小稳定 prefix、bounded output 和无普通 tool；eval/release report必须展示
p50/p95 latency、tokens、cache和cost，不得只报告准确率。若代价不可接受，用户可显式 Manual；未来优化也
必须保持 typed decision，不能退回关键词 router。

### 17.2 过度进入 PlanReview

风险：简单任务增加摩擦。

缓解：单独的 Chat -> PlanReview over-route 门槛、negative corpus、route-local kill switch 和 Doctor事实。

### 17.3 DirectTask 过早执行

风险：模型误解目标后进入 durable execution。

缓解：DirectTask exact qualification；高影响/取舍导向 PlanReview；permission与verification不放宽；
`PlanReview -> Task premature execution = 0` 是硬门槛。

### 17.4 两套 Plan engine 漂移

风险：TUI `/plan`、Desktop profile和automatic route产生不同 artifact/permission/recovery语义。

缓解：R63.3 先下沉共享 `PlanReviewCoordinator`，产品面不得各自解析 fenced text或创建 PlanDraft。

### 17.5 现有配置行为变化

风险：缺少 `routing_policy` 的 current config在升级后从 Manual 变为 Auto。

缓解：release notes、Doctor和首次启动摘要明确说明；显式 Manual 是单字段 rollback；Auto 的未准入基线是
ReviewFirst，不是无条件 DirectTask。

## 18. Completion boundary

RFC-0063 完成后，Sigil 将具备一个默认开启的、可解释的三路 conversation admission：

```text
直接回答
先给可审阅计划
进入 durable Task 执行
```

它不会让模型拥有 UI authority，也不会把“自动规划”解释成“自动授予副作用权限”。Plan artifact、Task
execution、tool approval 和 multi-agent authority 仍是四个独立控制面；Desktop 与 TUI 只是对同一 durable
事实使用不同的信息架构。

本 RFC 不承诺未经 qualification 的 DirectTask，也不解决 external effect compensation、任意自治 agent
team 或跨 session 项目目标控制。它只关闭当前最直接的产品缺口：AI 能判断“现在应该先让用户看计划”，
并以一条共享、typed、可恢复的产品链真正进入 Plan review，而不要求用户预先知道并输入 `/plan`。

同样，本文只保证类似“分批收尾当前工作区”的请求进入 `Task` 或安全的 `PlanReview` fallback，不把路由
成功解释为交付完成。Task 内部仍必须分别证明计划步骤、验证、审批、Git delivery outcome 和最终收敛；
重复执行等价命令却没有 workspace、HEAD、step 或 receipt 状态变化的 no-progress 处理，不属于本 RFC
的 conversation admission contract。
