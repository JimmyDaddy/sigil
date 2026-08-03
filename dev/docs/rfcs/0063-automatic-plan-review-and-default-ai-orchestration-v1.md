# RFC-0063 Automatic Plan Review and Default AI Orchestration V1

状态：proposed / design complete / implementation deferred

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
- 受既有 egress/disclosure/network policy 约束的 read-only remote capability；
- 一次 bounded host-owned read-only Explore discovery；
- `submit_plan_draft` internal tool。

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

### R63.7 — Evaluation and release closure

主要范围：

- three-way deterministic corpus；
- delivery-intent 的执行/只分析成对回归，以及不含 `提交/commit/task/plan` 的语义等价 case；
- real-model repetition、latency/cost/cache metrics；
- TUI PTY 与 Desktop E2E；
- cancel/restart/queue/compaction/route-switch chaos；
- 中英文用户文档、changelog、core technical solution 与 release rule。

验收：第 12 节门槛全部满足后，才将 RFC 状态改为 implemented。

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
