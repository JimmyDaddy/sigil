# RFC-0069：Recoverability Boundaries, Plan Direct Execution and Workspace Concurrency V1

状态：实施完成（2026-08-22；同日修订 Plan Run 为 first-class direct execution；验证证据见 R69 execution ledger）

> RFC-0071 implementation note (2026-08-25): R71 current-schema cutover extends the stable admission key and blocker boundary with managed resource domain/scope/detail, physical frontier bridge and authority-owned recovery evidence. Plan/task direct execution remains downstream of that typed admission; no second blocker schema or path-derived recovery proof is introduced.

创建日期：2026-08-22

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- [RFC-0018 Plan-to-Task Handoff](0018-plan-to-task-handoff.md)
- [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0062 Harness-owned Tool Output Spooling and Result Conformance V1](0062-harness-owned-tool-output-spooling-and-result-conformance-v1.md)
- [RFC-0066 Durable Task Execution Contracts V2](0066-durable-task-execution-contracts-v2.md)
- [RFC-0067 Single Execution Spine and Monotonic Plan-to-Task Adoption V1](0067-single-execution-spine-and-monotonic-plan-to-task-adoption-v1.md)
- [RFC-0068 Durable Recovery Spine and Effect-Scoped Retry V1](0068-durable-recovery-spine-and-effect-scoped-retry-v1.md)
- [2026-08-22 全项目可恢复性审查](../../../.repo-local-dev/review/sigil-recoverability-failure-containment-project-review-2026-08-22.md)

## 1. 摘要

Sigil 已经建立严格的 permission、approval、workspace、durability、provider attempt、tool receipt、Plan 和 Task
边界。这些边界保护真实副作用，必须保留。当前缺陷不是“校验太多”，而是：

> 下层校验的失败作用域和可恢复性没有成为领域事实；调用栈通过 generic error、字符串、少量 downcast 或
> `RunFailed` 直接决定上层终局。

这会同时产生两种相反错误：

1. **过度终止**：Plan 预编译、无关 workspace drift、旁路 MCP 操作、projection/journal 故障等局部问题击穿
   整个 Task/Run；
2. **错误完成**：child 明确 blocked，但非空 final prose 被当成恢复证据，步骤显示
   `completed with warnings` 并错误放行下游。

本 RFC 冻结三条统一边界：

```text
Failure Containment Spine
  lower owner classifies scope/recoverability/effect settlement
  -> durable blocker or terminal receipt
  -> upper layer may preserve or narrow scope, never guess or widen it

Plan Direct Execution Spine
  PlanReviewable
  -> user approval + stable Task identity + host linear execution unit
  -> Ready | Paused(CreatePaused)
  -> runner

Workspace Concurrency Spine
  immutable invocation authority
  + mutable WorkspaceObservation
  + effect-local ReadSet/WriteSet
  + MutationReceipt / ReconciliationReceipt
```

最终不变量：

> `Failed` 只表示领域目标已经被 durable evidence 证明不可恢复地终止。安全上的 fail-closed 约束下一次危险
> effect 是否允许执行，不自动等于整个产品任务失败。

## 2. 事故与全项目审查结论

### 2.1 Plan 提交事故

用户提交可读 Plan 时收到：

```text
Intent-enabled write plan step s2-gates must bind exactly one intent alias
```

错误来自 Task intent binding/compiler，却发生在用户审批 Plan 之前。即使 finalizer 随后生成合法 draft，原失败
仍以红色工具卡暴露。系统把“Plan 是否可理解、可批准”错误地等同于“Task IR 是否已经完整可执行”。

### 2.2 Workspace frontier 事故

另一个 worker 修改任意 tracked file 后，当前 child 收到：

```text
agent invocation workspace changed outside its audited mutation frontier
```

随后连 read-only `git status`、artifact read 和 workspace listing 都失败，child admission 继续失败，step/Task
级联终止。全局 workspace snapshot 同时充当审计证据、grant identity、并发锁和成败依据，作用域过大。

### 2.3 全项目同根因 findings

2026-08-22 review 在当前工作树确认：

- TUI 旁路 operation仍能发送全局 `RunFailed` 并清除 foreground run；
- PlanReview、Task planner、Chat/Application recovery仍依赖 generic error/string/downcast 白名单；
- scheduler仍可用 final prose关闭未证明解决的错误；
- projector、SSE journal 和 adapter delivery仍可伪造/改写领域 terminal；
- effect reconciliation只有 required append，缺 probe/settlement/resume闭环；
- workspace rebase已有部分实现，但 read/write set、path CAS 和 writer attribution缺失；
- legacy Plan adoption authority与新 approval/materialization链同时存在；
- 公共状态虽已扩展，但各 surface 仍手写映射并可能漂移。

## 3. 与 RFC-0067、RFC-0068 的关系

### 3.1 Supersede RFC-0067 的 Plan ready 语义

本 RFC 取代 RFC-0067 中以下规范性结论：

- `DraftReady` 必须绑定完整 `ExecutablePlanCandidateV1`；
- Plan compiler 必须在 `DraftReady` 前成功；
- `PlanExecutionAdoptedV1` 必须在一个事件内同时承载 approval、完整 TaskPlan、step contract、intent activation；
- 用户 Run 后必须再通过 model/compiler materialize TaskPlan、DAG、intent或 step contract；
- accepted Task 必须先经过 candidate/workspace admission才可启动 runner。

新的含义是：

```text
PlanReviewable / DraftReady
  = exact readable Plan artifact 已 durable，可审阅、修订、批准
  != executable Task 已编译
```

RFC-0067 的以下原则继续有效：

- user approval 与 stable Task identity 必须 crash-safe、单调、幂等；
- product surfaces 不得各自实现 handoff；
- stale user command不能作用于另一 Plan/hash；
- Task identity建立后，真实 provider、permission、tool、verification或effect问题以
  Blocked/Paused/recovery表达；
- legacy durable log必须可读，不猜测缺失 authority。

### 3.2 扩展 RFC-0068 的恢复脊柱

RFC-0068 主要规范 provider/effect-scoped retry。本 RFC 将相同原则扩展到：

- PlanReview、legacy Task materialization与直接执行；
- workspace observation、path conflict 与 external writer；
- local operation、adapter delivery 和 projector；
- step completion proof、partial output 和 cross-surface status。

RFC-0068 §10.2 的“不改变 workspace invocation grant”按以下兼容解释更新：

- role、permission、tool contract、cancellation、expiry 等 authority不变；
- initial workspace observation作为 audit evidence保留；
- current workspace observation可以按证据 rebase；
- workspace content不是 invocation authority，也不进入“是否仍有权读取”的判决。

## 4. 目标与非目标

### 4.1 目标

1. 建立所有 crate/surface共用的 failure scope、recoverability、effect settlement 和 blocker lifecycle。
2. 保证可恢复下层问题不会自动升级为 Task/Run Failed。
3. 保证 active blocker存在时不会错误 Completed，也不会启动依赖步骤。
4. 移除 model-generated Task materialization作为 Plan Run或普通 Task启动的硬前置。
5. 让 shared worktree中任意并发修改都不会仅因全局 snapshot drift终止任务。
6. 对同文件并发使用局部 CAS、重读、重生成、可证明安全的 merge或 effect-local pause。
7. 完成 unknown effect的 durable reconciliation闭环，禁止盲重放。
8. 保证 domain terminal不被 adapter、journal、projector或 UI状态覆盖。
9. 统一 TUI、Desktop、HTTP、CLI 的状态、恢复动作和错误呈现。
10. 建立完整 fault-injection matrix、跨重启和 cross-surface qualification。

### 4.2 非目标

- 不取消 permission、approval、sandbox、egress 或 workspace confinement；
- 不允许在 unknown effect outcome下自动重放；
- 不做任意语义三方合并；自动合并必须有可证明的目标和验证契约；
- 不要求所有并行 worker使用 worktree；worktree只是一种可选隔离优化；
- 不把 provider私有 error code暴露到 kernel public API；
- 不允许 UI根据英文错误文本推断状态；
- 不引入新的 crate；先在 kernel/runtime现有职责边界内落地。

## 5. 统一领域不变量

### 5.1 Failure scope不可隐式扩大

```text
LocalOperation -> LocalOperation
ToolEffect     -> ToolEffect or owning Step blocker
Step           -> Step blocker
Task           -> Task blocker
Adapter        -> Delivery/Projection blocker
Projection     -> ProjectionDegraded
```

只有 durable evidence证明上层目标不可继续，scope才可显式升级。升级必须追加 typed event，不能靠 `?`、
`bail!`、message string或 catch-all match隐式完成。

### 5.2 `Irrecoverable` 是 Failed 的必要条件

以下默认不是 Failed：

- compile/schema/model-output不满足；
- provider/resource budget暂时耗尽；
- permission denied/awaiting user；
- workspace drift/stale write/conflict；
- effect outcome uncertain；
- adapter/journal/subscriber不可用；
- projection损坏但领域 log可重建；
- cancellation/interruption。

以下可以是 Irrecoverable，但必须有 evidence：

- durable authority真正冲突且无法安全选择；
- required session facts损坏且无法从 event stream重建；
- proven unauthorized effect已经发生；
- invariant/data corruption使继续执行可能扩大损害；
- 用户明确终止并要求不可恢复结算；
- 显式 policy定义的恢复预算耗尽，且 policy选择 permanent failure而非 pause。

### 5.3 Fail closed约束 effect admission

```text
approval 未 durable      -> 不执行 effect，Task Blocked
prepared mutation stale  -> 不写目标，局部 Rebase/Replan
effect outcome unknown   -> 不重放，进入 Reconciliation
journal/projector failure -> 不发布新 authority，领域 Task 保持原状态
```

fail closed不允许被翻译成“删除 Task identity”或“伪造 RunFailed”。

### 5.4 上层成功单调

- PlanApproved与 direct Task authority原子提交，不存在中间 materialization回滚窗口；
- stable Task identity不因 planner/admission失败消失；
- settled tool/effect不因下一次 provider failure重放；
- durable domain terminal不因 delivery失败改写；
- resolved blocker不因旧 event重放重新激活。

## 6. 统一 Failure/Recovery Schema

### 6.1 核心类型

```rust
pub enum FailureScopeV1 {
    LocalOperation { operation_id: String },
    ToolEffect { task_id: Option<TaskId>, step_id: Option<TaskStepId>, effect_id: String },
    Step { task_id: TaskId, step_id: TaskStepId },
    Task { task_id: TaskId },
    Run { logical_run_id: String },
    AdapterDelivery { run_id: String, adapter: AdapterKindV1 },
    Projection { projection: ProjectionKindV1 },
}

pub enum RecoverabilityV1 {
    Continue,
    RetrySameBoundary,
    RebaseWorkspace,
    Replan,
    AwaitUser,
    AwaitResource,
    ReconcileEffect,
    Cancelled,
    Irrecoverable,
}

pub enum EffectSettlementV1 {
    NotStarted,
    ConfirmedNoEffect,
    Applied,
    PartiallyApplied,
    OutcomeUncertain,
}

pub struct RecoveryBlockerV1 {
    pub schema_version: u16,
    pub blocker_id: String,
    pub domain: RecoveryDomainV1,
    pub scope: FailureScopeV1,
    pub recoverability: RecoverabilityV1,
    pub settlement: EffectSettlementV1,
    pub reason_code: String,
    pub safe_summary: String,
    pub evidence_digest: String,
    pub effect_id: Option<String>,
    pub available_actions: Vec<RecoveryActionV1>,
    pub created_at_ms: u64,
}
```

`reason_code`是 bounded provider-neutral code；原始 provider/OS文本只进入 redacted audit detail，不参与 host
状态选择。

### 6.2 Blocker lifecycle

```rust
RecoveryBlockerRaisedV1 { blocker }
RecoveryBlockerResolutionStartedV1 { blocker_id, action, attempt_id }
RecoveryBlockerResolvedV1 { blocker_id, resolution_receipt_digest }
RecoveryBlockerSupersededV1 { blocker_id, successor_blocker_id }
```

规则：

- 同一个 exact evidence digest只能激活一个 blocker；
- terminal只能出现一次；
- restart从 durable projection恢复 active blockers；
- final text、Notice和 UI action本身不能关闭 blocker；
- resolution receipt必须绑定 exact blocker/effect/workspace/authority frontier。

### 6.3 Error owner contract

最低层 owner必须返回：

```rust
pub enum BoundaryOutcomeV1<T> {
    Completed(T),
    Blocked(RecoveryBlockerV1),
    Interrupted(InterruptionReceiptV1),
    Failed(IrrecoverableFailureV1),
}
```

不得新增“先返回 anyhow，等上层通过 downcast猜 recoverability”的生产路径。`anyhow`只用于尚未分类的编程
错误边界；进入 durable Task/Run terminal前必须转换为 typed outcome。

## 7. Step/Task completion proof

### 7.1 Completion不是 final prose

Step Completed必须同时满足：

```text
no active blocker for step/effects
AND all started effects have terminal settlement
AND required acceptance receipts exist
AND required verification is satisfied or explicitly waived by authority
AND bounded completion report exists
```

completion report是必要条件但不是充分条件。

### 7.2 Tool error history与 active blocker分离

`AgentRunOutcome.tool_errors`继续保留历史审计，但不能同时承担 active state。新增 durable blocker refs：

```rust
TaskParticipantResultV2 {
    ...,
    active_blocker_ids,
    resolved_blocker_ids,
    effect_settlement_receipt_ids,
    acceptance_receipt_ids,
}
```

“一次 command被拒绝、随后安全替代方案成功”需要替代方案的 typed receipt关闭原 blocker；不能仅因模型输出文字
就认为恢复。

### 7.3 下游调度

- upstream Completed且completion proof完整：依赖步骤可 admission；
- upstream Blocked/Paused/Reconciling：下游保持 Pending或Blocked-by-dependency；
- upstream Failed：只有不可恢复依赖才 Failed-by-dependency；
- upstream Cancelled：下游 Cancelled或保持 Pending，取决于 root cancellation scope；
- 禁止把 recoverable upstream blocker投影为 downstream Cancelled。

## 8. Plan review与直接执行

### 8.1 状态机

```text
PlanReviewing
  -> PlanReviewable
       -> RevisionRequested -> PlanReviewing
       -> Rejected
       -> Approved + Task + DirectExecutionPlan(Ready)
            -> Running
            -> Paused(CreatePaused)
```

### 8.2 PlanReviewable contract

`submit_plan_draft`是优选的展示增强通道，只验证 Plan artifact本身：

- bounded Markdown/summary/risk；
- stable plan id/hash；
- 可选步骤大纲和依赖提示；
- 安全持久化与 source reference；
- 不允许 model文本推断 host intent。

模型未调用该工具、结构化输出能力较弱，或在一次有界纠错后仍无法满足 schema 时，只要 final answer
包含非空且未超过持久化上限的完整 Plan文本，host就必须为它分配稳定 id/hash并保存为可审阅 Plan。
该降级记录的 steps/intents/paths/checks为空；host不得从 prose解析 DAG、role、intent、权限或调度语义。
因此 typed draft影响展示质量，但不是 Plan能否审阅、批准或运行的正确性前置。

它不得要求：

- TaskPlan/DAG完全合法；
- 每个写 step有 exact intent alias；
- role/mode/isolation/capability全部可 admission；
- permission scope已经具体；
- execution segment或 verification contract已经生成。

批准前可以运行 precompile，但输出只能是 content-addressed advisory cache：成功或失败都不得决定 Run是否可用，
不得成为 runner输入 authority。当前新写路径不再主动 precompile；仅保留旧会话投影和兼容诊断。

### 8.3 Approval + stable direct Task

用户批准必须在一个 crash-safe append bundle内写入：

```rust
PlanApprovedV2 {
    command_id,
    plan_id,
    plan_hash,
    decided_by: User,
    approved_at_ms,
}

TaskCreatedFromPlanV2 {
    task_id,
    source_plan_id,
    source_plan_hash,
    parent_session_ref,
    task_plan_version: 0, // 没有 TaskPlan；该 legacy link 字段不授予执行权
    phase: Ready,
}

TaskDirectExecutionAdmittedV1 {
    task_id,
    admission_id,
    objective_hash,
    source: ApprovedPlan { plan_id, plan_hash },
}

TaskChecklistUpdatedV1 { // 可选；只有两个以上真实展示项才写入
    task_id,
    revision: 1,
    items: DisplayOnlyPlanLabels,
}
```

`TaskDirectExecutionAdmittedV1`是一等执行权威，不是 `TaskPlan`、不是隐藏 step，也不是兼容占位。完整 approved
Plan保存在 Task objective中并原样进入 executor context。`TaskPlan`只用于真正存在两个以上调度节点、依赖或隔离
边界的高级执行。以下字段不得由批准后的模型调用决定：

- step数量与依赖；
- role、intent binding或 capability admission；
- execution segment划分；
- “是否允许开始执行”。

`TaskChecklistUpdatedV1`只负责展示，不能授予 scheduling、permission、completion或 retry authority。零项或一项
不得显示成 task list；模型未更新、更新格式错误或完全不支持 tool calling时，executor仍继续执行。host可以从已批准
Plan的 typed step title播种至少两项的 checklist，并在 Task权威终态后收口展示状态。

direct Task没有`TaskPlan`不是缺少 authority。自动 conversation router必须把带有效
`TaskDirectExecutionAdmittedV1`的 current resumable Task暴露为`continue_existing_task`候选，host binding中的
plan version/status保持`None`；current focus已经被一次Chat清除时，host退回选择latest unfinished Task作为候选，
但仍须等待模型typed selection和dispatch CAS才能恢复authority。用户在执行中询问该 Task的状态、进度、中断原因或下一步属于 exact-Task follow-up：
模型以 typed action选择 resume或把当前请求作为 guidance交给同一 direct executor，host不得把它降级到没有
`update_task_checklist` context的普通 Chat。Chat/PlanReview仍可清除 current execution focus；TUI只读地保留最新未完成
Task及其 checklist展示，但该展示不能授予 mutation、resume或 completion authority。

每次 direct executor运行由 `TaskDirectExecutionAttemptV1`记录物理 attempt。Completed attempt必须同时绑定
durable final Assistant message id和安全正文 hash；应用层只从这份绑定恢复最终答案，不能从最近一条文本或
checklist状态猜测完成。Paused/Interrupted/Cancelled attempt不得携带 final-answer binding。

如果用户选择 scoped edits，同一原子 bundle额外写 exact `PlanPermissionGranted`；它只收窄 permission，不引入
materialization。

### 8.4 Planner/model-output failure

普通 `/task` 可以把 model planner作为可选优化。planner没有调用正确 typed tool、schema不合法、缺 accepted
TaskPlan或在 bounded retry后仍失败时，host追加 `TaskDirectExecutionAdmittedV1`并继续 executor，绝不伪造
`TaskPlan`或 `TaskStep`。已经发生 provider
effect且需要 exact-route recovery时仍 Paused，取消仍 Cancelled；durable facts冲突或不可恢复 corruption才 Failed。

这条降级保证弱 tool-calling模型只损失并行/分工优化，不损失基础任务可执行性。

### 8.5 Legacy迁移

- 旧 `PlanExecutionAdoptedV1`、`TaskMaterialization*`和多 step DAG继续 replay；
- 旧 candidate/ready marker继续可读，但不再决定新 PlanReviewable；
- `PlanExecutionService::materialize_approved_plan`与 legacy adoption只保留 replay/test兼容，生产 surface不得调用；
- 新写路径禁止 raw `PlanDecision::Accepted`；
- Doctor报告 partial legacy prefix，不自动猜测缺失 authority。

## 9. Workspace authority与 observation分离

### 9.1 Invocation authority

`AgentInvocationGrantV2`只绑定：

- source task/step/participant identity；
- role、permission upper bound、network upper bound；
- tool contract fingerprint；
- cancellation scope；
- expiry；
- root run identity。

initial workspace snapshot可以作为 audit reference进入 durable record，但不进入“grant是否仍有效”的 capability
fingerprint。workspace内容变化不能撤销读取权限、角色或 tool contract。

### 9.2 WorkspaceObservationV1

```rust
pub struct WorkspaceObservationV1 {
    pub observation_id: String,
    pub workspace_id: String,
    pub generation: u64,
    pub manifest_digest: String,
    pub coverage: SnapshotCoverageV1,
    pub observed_at_ms: u64,
}

WorkspaceObservationRebasedV1 {
    from_observation_id,
    to_observation_id,
    changed_paths,
    changed_paths_truncated,
}
```

read-only工具始终在当前 observation执行；发现 drift只记录/rebase，不失败 invocation。

### 9.3 Effect-local read/write set

```rust
pub struct ReadSetVersionV1 {
    pub path: PathBuf,
    pub expected_kind: WorkspaceEntryKindV1,
    pub expected_hash: Option<String>,
}

pub struct WriteSetTargetV1 {
    pub path: PathBuf,
    pub expected_before_hash: Option<String>,
    pub proposed_after_hash: Option<String>,
    pub merge_contract: MergeContractV1,
}

pub struct WorkspaceEffectBindingV1 {
    pub effect_id: String,
    pub base_observation_id: String,
    pub read_set: Vec<ReadSetVersionV1>,
    pub write_set: Vec<WriteSetTargetV1>,
}
```

read/write set来源只能是 typed tool subjects、prepared artifact、Task step contract或 runtime-owned bounded analyzer；
禁止通过自然语言关键词猜测。

### 9.4 Mutation算法

执行前：

1. capture current observation；
2. 比较 effect-local read/write set；
3. 无关路径变化：append rebase，继续；
4. target/read dependency变化且 effect未开始：返回 `StalePreparedMutation` blocker；
5. 重读并重新生成 prepared artifact；
6. 只有 exact policy允许且 merge proof成立时执行自动 merge；
7. 任何 target写入前再次做 path CAS；
8. commit MutationReceipt并推进 observation。

### 9.5 同文件并发

同文件并发不得直接失败整个 Task：

```text
stale detected before effect
  -> preserve current bytes
  -> reread
  -> regenerate patch or bounded three-way merge
  -> revalidate permission/preview/verification
  -> retry same effect with new attempt id

cannot prove safe merge
  -> current effect Blocked/Replan
  -> Task remains resumable
```

禁止覆盖另一个 worker内容。自动 merge只适用于：

- exact base/local/current三方输入可得；
- merge无 conflict marker；
- target仍在批准 scope；
- result重新 preview/hash/verify；
- 新 MutationReceipt引用双方 observation。

### 9.6 Unknown shell/MCP writer attribution

全仓 before/after manifest diff只能证明“期间观察到变化”，不能证明变化由当前 tool产生。新增：

```rust
WorkspaceMutationObservationV2 {
    effect_id,
    before_observation_id,
    after_observation_id,
    changed_paths,
    attribution: ConfirmedTool | ConfirmedExternal | MixedOrUnknown,
}
```

只有受控 child/process receipt能证明 writer时才标记 ConfirmedTool。`MixedOrUnknown`进入 effect reconciliation，
不能把所有 diff认领为 tool mutation，也不能因为不确定直接 Task Failed。

### 9.7 Worktree定位

- Desktop可以继续默认创建隔离 worktree；
- TUI/CLI/shared checkout必须支持并发 observation/CAS；
- 高冲突写任务可以建议或自动选择 worktree，但必须是产品策略，不是 kernel正确性的前提；
- worktree创建失败是 isolation operation blocker，不自动失败原 Task。

## 10. Effect reconciliation闭环

### 10.1 状态机

```text
EffectStarted
  -> receipt proven -> Settled
  -> outcome uncertain -> ReconciliationRequired
       -> ProbeStarted
       -> ObservedApplied     -> accept effect, continue
       -> ObservedNotApplied  -> exact retry allowed by replay contract
       -> StillUncertain      -> remain Blocked/AwaitUser
```

### 10.2 Required authority

`EffectReconciliationRequiredEntryV1`必须额外可关联：

- logical run/task/step/participant/tool attempt；
- replay contract class；
- base/current workspace observation；
- known output/side-effect receipt ids；
- allowed probe kind和 budget；
- safe product actions。

旧 V1 event通过 optional sidecar兼容，不改写历史 payload。

### 10.3 Probe registry

```rust
pub trait EffectReconciliationProbe: Send + Sync {
    fn kind(&self) -> ReconciliationProbeKindV1;
    fn validate(&self, request: &ReconciliationProbeRequestV1) -> Result<()>;
    async fn observe(&self, request: ReconciliationProbeRequestV1)
        -> Result<ReconciliationProbeReceiptV1>;
}
```

probe必须 read-only、bounded、permission-constrained、durably audited。generic shell不得为了“确认”再次执行原命令。

### 10.4 Terminal settlement

- ObservedApplied：生成 effect settlement receipt，不重放；
- ObservedNotApplied：仅当 replay contract允许，创建新的 exact attempt；
- StillUncertain：保持 blocker active，允许用户检查/取消/提供外部 receipt；
- probe失败：是 reconciliation operation failure，不改写原 effect状态。

### 10.5 Restart

session load必须重建 active reconciliation。恢复后：

- 不自动重放原 effect；
- 可以自动调度明确 read-only且 policy允许的 probe；
- 同 reconciliation id只允许一个 active probe claim；
- terminal append使用 CAS，迟到结果不能覆盖已结算 outcome。

## 11. Execution segments、partial output与 fallback

### 11.1 ExecutionSegmentV1

legacy或显式高级 Task DAG如果启用多 step编排，runtime必须生成可恢复 execution segment，而不只是把每个短
step都变成独立 child。Plan Run的 direct execution不创建 segment、TaskPlan或TaskStep；它以 direct attempt作为
可恢复物理边界：

```rust
pub struct ExecutionSegmentV1 {
    pub segment_id: String,
    pub task_id: TaskId,
    pub ordered_step_ids: Vec<TaskStepId>,
    pub role: AgentRole,
    pub authority_fingerprint: String,
    pub isolation: TaskIsolationMode,
    pub continuation_contract: ContinuationContractV1,
    pub checkpoint_policy: SegmentCheckpointPolicyV1,
}
```

相同 role/authority/isolation且依赖连续的步骤可共享 participant/provider continuity。segment不能扩大 permission或把
并行依赖伪装成顺序。

### 11.2 Safe frontier

每个 segment维护：

- last durable provider request frontier；
- settled tool/effect ids；
- active blocker ids；
- partial output refs；
- checkpoint/verification refs；
- remaining step contracts。

retry/resume从该 exact frontier继续，不重跑 settled tools。

### 11.3 Partial output

partial output分为：

```text
AdvisoryPartialOutput
  可展示/持久化，但不能证明 step完成

AcceptedPartialArtifact
  有 typed acceptance receipt，可作为后续输入

TerminalOutput
  仅在 completion proof成立后关闭 step/segment
```

provider stream中断后已有 text delta不自动 Failed，也不自动 Completed：

- verification materialization或role-runtime/provider构建在participant dispatch前失败时，记为typed
  zero-dispatch preflight blocker并保持Task `Paused`；adapter必须把typed error保留到Task root finalizer，不能先
  字符串化再降级成普通`Failed`；
- 无 effect且 continuation安全：retry same logical turn；
- 同进程仍持有且能和 predecessor envelope/fingerprint精确核对的 frozen request，可作为下一 physical attempt的
  process-local authority；该证明不能跨进程，restart后仍必须从 durable frontier精确重建；
- provider-hosted read（当前为 WebSearch）只产生远端读取和独立预算消耗，不因“hosted已启用”自动升级为
  `OutcomeUncertain` mutating effect；未来可写 hosted kind必须显式声明 mutation语义并在不确定 dispatch后 reconcile；
- partial artifact已 accepted：保留并从 frontier继续；
- output/effect关系不确定：reconcile；
- 用户可选择接受 partial result时，必须追加 explicit acceptance receipt。

这里的 retry不是 provider adapter在同一 HTTP调用内透明重发。每次 retry都先关闭旧 physical attempt，再由 kernel
持久化 schedule/start并创建新的 physical attempt id。TUI/Desktop在等待和执行阶段显示 reconnecting/retrying；若
有界预算耗尽，Task保持 `Paused`，由现有 Task Continue入口重新进入，而不是把整个 Task标记为 Failed。

### 11.4 Provider/transport fallback

fallback只能发生在：

- current physical attempt已 terminal；
- exact request material可重建；
- settlement是 ConfirmedNoEffect，或 continuation contract证明不会重复 settled effect；
- route/model capability满足同一 provider-neutral contract；
- 新 attempt具有新的 physical attempt id并引用 predecessor。

fallback失败产生 Retry/AwaitResource/Blocked，不直接 Task Failed。不同 provider/model可能改变语义时必须显式
replan或用户确认，禁止静默切换。

### 11.5 Fallback与 workspace

fallback前重新观察 workspace：

- 无关 drift：rebase；
- read dependency变化：重建 request/context；
- prepared write stale：不执行旧 effect；
- effect outcome uncertain：先 reconciliation，禁止 fallback导致重复副作用。

## 12. 状态传播与公共协议

### 12.1 Domain状态

```text
Run:  Running | Retrying | AwaitingUser | Blocked | Paused |
      Reconciling | Succeeded | Interrupted | Cancelled | Failed

Task: Preparing(legacy) | Ready | Running | Rebasing |
      Reconciling | Blocked | Paused | Completed | Interrupted | Cancelled | Failed

Step: Pending | Running | Retrying | Rebasing | Reconciling |
      Blocked | Completed | Interrupted | Cancelled | Failed
```

V1可在内部用 phase + blocker组合投影，不要求一次性扩展所有旧 enum，但 public/durable语义必须可区分。

### 12.2 唯一 terminal映射

| 领域结果 | Public event | HTTP/Desktop/TUI |
| --- | --- | --- |
| Succeeded | `RunFinished` | finished |
| AwaitingUser | `RunAwaitingUserInput` | paused / input required |
| Retry scheduled | `RunPaused` + recovery | retrying / scheduled |
| Active blocker | `RunBlocked` | blocked |
| Effect uncertain | `RunBlocked` + reconciliation | reconciling |
| Interrupted | `RunInterrupted` | interrupted |
| Cancelled | `RunCancelled` | cancelled |
| Irrecoverable | `RunFailed` | failed |

禁止 default/fallback branch把未知 recoverable状态映射为 Failed。新增状态未映射必须编译失败或 conformance test失败。

### 12.3 Local operation

TUI/desktop设置、MCP refresh、diagnostics、artifact cleanup、verification等使用：

```rust
OperationOutcomeV1 {
    operation_id,
    kind,
    status: Succeeded | Rejected | Deferred | Retrying | Failed,
    retryable,
    safe_summary,
}
```

它不拥有 foreground run terminal authority，不能发送 `RunFailed`。

### 12.4 Recovered internal errors

内部 validation/provider attempt如果随后由 typed receipt证明恢复：

- durable audit保留；
- public transcript显示 bounded “自动修正/已重试” notice；
- 不显示主流程红色 terminal tool card；
- inspect/audit surface仍可查看详细历史。

## 13. Adapter delivery与 projector

### 13.1 Domain terminal是唯一真相

adapter不得通过 event handler错误反向让 kernel/runtime改变已经提交的 domain terminal。

### 13.2 Durable outbox

```rust
PublicEventOutboxEntryV1 {
    public_event_id,
    domain_event_id,
    run_id,
    sequence,
    payload_digest,
}

PublicEventDeliveryReceiptV1 {
    public_event_id,
    adapter,
    delivered_at_ms,
}
```

- domain commit与outbox entry在同一 durability boundary；
- HTTP/Desktop可按 public_event_id幂等重放；
- subscriber断开不影响 Run；
- approval authority未可靠交付时暂停对应 effect admission，但 Task保持 Blocked；
- terminal delivery失败重试同一个 terminal，禁止补造 `RunFailed`。

实现约束（2026-08-31，E06）：

- conversation lifecycle recorder 是 root terminal 的唯一提交入口；`RunFinalized`
  与完整 public outbox payload 使用既有 session append-bundle intent 一起提交。
- bundle 在首次物理追加前固定两条 StoredEvent 的 ID、public sequence 和 payload digest；
  恢复只补齐该 intent 的原始字节。delivery receipt 独立追加，不改变 domain terminal。
- 普通 `append_outbox` 拒绝 root terminal。没有完整 bundle intent 的旧 split terminal
  不能反推、补造 outbox；这不是旧 session 的迁移或 upcast 入口。
- unfinished current-schema start 的恢复产生 `Interrupted + outbox` 同一 bundle；若已存在
  可恢复的 terminal bundle，先恢复原结局，不能另写 Interrupted。
- `Failed / Blocked / Interrupted / Cancelled / Paused / AwaitingUserInput / Succeeded`
  在 runtime 与公共事件间保留区别。HTTP 的等待输入 snapshot 可归于 Paused，必须保留
  原始 `RunAwaitingUserInput` 事件；CLI machine V2 保留全部 root terminal 状态。
- Desktop continuity 必须识别 Paused/Blocked，不能把 Paused 映成 Cancelled；TUI 的
  application projection 在 AwaitingUserInput 后不再保持 active，待答问题仍保留。

以上实施记录仅覆盖 root ConversationRun terminal bundle。

PlanReview revision 的独立终态协议（2026-08-31）：

- parent session 的既有 `PlanReviewAttempt` 是唯一领域 owner；不伪造
  `ConversationRunStarted`，也不增设镜像 Started/Finalized 状态机。
- 一个 revision 的 run identity 由既有 plan-review/attempt identity 确定。真正的
  `Started` 绑定 revision request、ordinal、base plan/hash、source turn 和 child lineage。
  guidance 已接受但同 base/hash 从未出现 attempt 时，adapter 登记失败不伪造终态；用户
  仍可显式 Save 原草稿。首次 Started 与输入后 resume 都必须重验原 RevisionRequested，
  已 Save 的基础计划不能被滞后的 prepared request 再启动；已有 attempt 不适用该例外。
- 成功提交按 `PlanDraftCreated + RevisionSucceeded + PlanReviewAttempt(DraftReady) +
  PublicEventOutbox` 一个既有 crash-safe append bundle 落盘；无 draft 的终态按
  `RevisionFailed + PlanReviewAttempt(实际状态) + PublicEventOutbox` 同样落盘。
  `RevisionFailed` 仅表示此次修订未替换基础计划，不把 Blocked/Paused/Interrupted
  翻译为 run Failed。基础计划保持可审阅。
- outbox 的 domain ID 指向该 attempt 终态 envelope，两个 envelope 紧邻，IDs、sequence、
  完整 public payload 在第一次物理写入前固定。writer 锁下检查实际 Started、完整 lineage、
  基础计划的 RevisionRequested 和 exact hash，禁止旧 split 状态补写或推导成功。
- append acknowledgement、child settlement 或 adapter 失败后先恢复 writer intent、读取原 pair
  并同步 live Session projection，不能用陈旧内存重新写失败。恢复已有完整 intent 只补原字节；
  真正 unfinished revision 没有原 bundle 时只结算 Interrupted，不能从 child draft 推造成功。
- HTTP、TUI 只交付同一原终态，不能再按 `Result::Err` 造 RunFailed；delivery receipt 与
  adapter registry 分别恢复，ACK 不等于 registry 已收敛。非终态 live sequence 的重启连续性
  与完整 public outbox 按下文 E08/A2 协议实施；A1 本身不构成该能力的完成证据。
  TUI 后置审计仍须尝试，失败单独 Notice，不覆盖实际 Finished/Cancelled/Interrupted。
  HTTP receipt 或发布失败也必须尝试从原 terminal 收敛 registry/stream；独立返回 delivery
  错误并保留原 pending outbox，由既有 attach replay 补交，不能让 Paused 残留成为新真相。
- worker 活动期间 public sequence 由该 run 的单一 runtime live bridge 预留，session writer
  在原子追加边界验证同 run 的 durable 下一序号并固定该值；它不是 session stream sequence。
  等待输入时没有活动 worker，HTTP 取消从 session outbox frontier 取得下一值，在原
  publication lock 下提交原子终态后发布 exact event；HTTP journal 不分配领域 public
  sequence。不另建 reservation registry，也不重编号已提交事件。无 live bridge
  的离线恢复从已有 durable public frontier 取下一值，不要求终态固定为 1。
- writer-read 补齐 intent 后须在同 writer 临界区更新共享投影，锁外发布变化；便宜的
  snapshot 仍不做文件 I/O。live Session 只采纳已验证的同一恢复 prefix，重验 frontier，
  保留外部内容隔离、runtime attachments 和既有 live-only promotion，不让恢复绕过安全过滤。
  frontier 校验与 live adoption 同持 writer 锁，过滤 external sidecar 所需审计在锁外沿
  原审计路径落盘。generic control API 对 candidate、base settlement 和 terminal 拆写
  均拒绝，包括同一批次中 candidate 先于 revision Started 的逆序写法。
- `WaitingForInput` 是可继续的 attempt suspension，同一 attempt 可在输入后重新 Started，
  不是本协议的 revision finalization；不得用等待通知占用唯一最终 terminal pair。HTTP 复用
  原 run registry 保留 Paused checkpoint，仅 exact request/attempt/base/child binding 可恢复；
  历史 Waiting receipt 可幂等查询，但不能使已完成 attempt 再次恢复或阻塞新 run。取消超时
  的 late worker 在实际退出前仍持有 session attachment，adapter 只报告 execution uncertain。
- 显式 `/plan` 没有 provider-visible durable User turn，因此原 persistence-safe 目标必须作为
  `PlanReviewAttempt.explicit_objective` 随首次 Started 落盘；automatic route 则只引用真实原 User
  turn，不复制该字段。原目标是同一 review 的不可变 source binding，暂停、重试、修订和终态均
  逐项保留并校验。修订指导仍由原 accepted user-input receipt 提供，不混入或覆盖原目标。
  恢复按 source 读取该原事实，不伪造 User entry，不以通用提示词代替丢失目标；旧 explicit attempt
  缺该事实时 fail closed，不补写、迁移或自动 upcast。
- child-owned research input 的接受和取消必须通过同一 composed managed provisioner 对该
  attempt 的既有 `research/0` SessionLog 做 existing-only mutation admission，再校验 child
  session scope 并显式结算该 SessionLog admission；它不分配 artifact bundle，也不得在缺失
  records 时创建目录、records、marker 或 lock。不得从 parent-relative `child_session_ref`
  回退打开另一份 store，也不得为绕过该绑定放宽 scope 校验。中断后 continuation admission 只能
  引用经验证的原 schema-2 marker 与唯一物理 namespace，并持有自己新的 admission/finalization
  record；不得改写旧 marker、重签旧 user scope 或把空前缀提升为 child。TUI 与 HTTP 使用同一入口。
  这里的“经验证”不是 runtime 解析 marker 后自行信任：runtime 只把 marker 的
  `handle_id + namespace_hash + durable admission` 作为 evidence 交给 kernel port；RA 必须在
  同一 authenticated resource journal instance/authority epoch 中逐项确认原 admission、唯一
  schema-2 physical marker、历史 terminal/settlement evidence，以及 current grant 的
  owner/family/purpose/owner-scope/journal-scope compatibility，才可消费新的 one-shot continuation
  claim。新 journal admission durable 引用原 admission，restart 仍按该引用解析原 namespace；V1、
  缺 marker、未结算、重复候选、跨 root/epoch 或任何 compatibility drift 均拒绝，不能通过重新建
  marker、补签或普通 resolver fallback 恢复。同一 original marker 同时只能有一个未结算
  continuation；read 与 mutation lifetime 重叠时后者按 typed duplicate/admission rejection 收敛，
  不能并发取得两个 writer claim。
  `StorageNamespaceAdmitted` 新增的 `continuation_from` 是 journal payload 的一部分；本仓库的
  resource journal payload hash 覆盖完整 current-schema debug encoding，即使字段为 `None` 也不与
  旧 record hash 等价。因此这条协议只适用于 fresh managed storage：旧 journal 不双读、不重算 hash、
  不迁移或补写 marker，而是在打开时按 corruption/current-schema mismatch fail closed。该格式的验证
  必须在同时隔离 storage roots 和 bootstrap user root 的自建临时目录创建新 journal，不得读取或改写用户历史 state。
- 若 child 已接受输入而 parent 的结算或后续 resume 注册失败，session-attention 恢复必须仅从
  无冲突投影中每个 review 的最新 `WaitingForInput` attempt 以 existing-only read admission 重开
  同一已存在的 `research/0` SessionLog，依 exact parent identity/hash、attempt lineage 和 child
  scope 读取原 receipt；该读取不得创建目录、records、marker、lock 或 artifact lease。缺失或损坏的
  managed records、遗漏的 Submitted answer values、多个可恢复 foreground receipt 一律 fail-closed。
  已 Resolved 的 child Cancel/Decline 仅在 parent 仍为该 exact Waiting 时可用于重做 parent
  settlement。私有答案不得写回 parent log、public DTO、public projection、timeline 或持久 UI
  cache；TUI worker 到 App 的既有非序列化 private recovery command 可暂存以复用 Resume 原决定，
  但不另建复制状态机。TUI 的 exact Resume 仅在 form 的 `PlanReviewResearch` source、原 command id、
  完整 request identity 与 hash 全部匹配时，经私有 worker recovery command 绕过已缓存的 application
  `Uncertain` receipt；该 command 不携带答案，worker 必须重读同一 managed child receipt 后才复用既有
  input dispatcher。恢复查询仍由 worker/runtime 持有 authority，HTTP 复用绕过 adapter receipt cache 的既有
  session-attention resume 入口。

#### E08/A2：public event 交付与重放（2026-08-31）

本节冻结 A2 的实施契约，不代表整个 E08 或平台资格化已完成。当前切片覆盖已物化
public event 的持久化、交付与补发，以及 revision Waiting 原子 bundle；普通 Control
已按下述纯 Control transition 收敛，混合 ToolResult/assistant/provider 写入后的公开
DTO 物化仍存在先后窗口，TUI 常规 `RunEvent -> WorkerMessage`
生产链也尚未整体迁入 outbox。approval/tool/route 的 adapter registry 与 broker 恢复也
尚未完成同源收敛：补发通知不代表审批入口、输入路由或工具状态已经恢复，不得仅因重放
而重新授予权限。这三项仍属后续整改，不由 consumer replay 冒充完成。

纯 Control transition 的同源提交协议：

- 原本显式 `append_control(s) -> RunEvent::Control` 的生产路径通过
  `EventHandler::commit_controls` 协调提交；Session/writer 仍是领域写入 owner。
  public application bridge 使用同一 live sequence 锁，Session 将每个 source Control
  与它对应的一个或多个 DTO 放入同一个现有 append intent；领域 ID 指向真实 source
  envelope，多个 DTO 保持投影顺序和连续序号。纯 Control 旧 split producer 退出。
- Session 公开投影是可重建、增量推进的内存缓存：首次从已有 entries 恢复，再只消费
  新 entries；它不持有另一个日志/序号 authority。投影候选在写前完成，成功提交后再
  更新缓存；恢复接管若替换 canonical prefix，则丢弃旧游标并从该已验证前缀重建。
  无效 user-input lifecycle 返回错误，不能制造 `RunFailed`。
- 此入口只用于原本显式通知的 controls，不把所有 durable/private Control 自动公开。
  UserInputChanged 使用无答案的 public view；task/integration 使用既有 typed DTO；
  ToolExecution 保留现有 HTTP/Desktop 生命周期消费；agent 通知仅保留触发 activity
  reload 的 kind，不转发 child refs、mailbox 或权限原文。其他未映射 controls 只写领域
  记录，不再通过通用 serde fallback 公开；整批无 DTO 时不消耗 public sequence。
  私有 child handler 保持自己的过滤，不能借新接口绑定 parent 的公开 run。
- 普通 plan review 的 prepare 阶段只校验并构造请求，不提前写 `Started`；实际 executor
  建立 handler 后提交开始、等待、完成等 parent attempt 状态。draft 与 attempt 的既有
  Control batch 保持完整，不再先写状态再另行 emit。research/finalizer 的内部 Control
  只归各自 child session；非 Control 进度沿用既有转发，不能跨 store 声称原子提交。
  revision Waiting/terminal 仍由既有专用 bundle owner 提交，不套普通 attempt 关闭接口。
  没有 active run 的普通 research-input 取消仍归 input command owner，返回原 command
  receipt/entries；它原本不经 `RunEvent::Control` 发布，不能虚构 run handler 或序号来
  套用此入口。该 command 与运行中显式 Control producer 的职责不得混同。
- outbox 与 source 共享 correlation chain，故障只恢复原 intent 的完整字节。
  replay 校验 source 的真实类型、相邻 envelope、同源 DTO 数量/顺序/内容；既有无
  outbox 的记录不反推或补造通知。正常追加不新增全日志扫描，uncertain append 才
  校验完整固定-ID batch；stale sequence 不得重复推进 live Session。ID 在首次物理追加
  前固定；跨进程由 append intent 恢复原字节并重放原 outbox，不重新执行领域 command。
- ToolResult/assistant/provider 的混合 bundle 不拆散来套这个接口，仍在普通 Control
  总体缺口清单内。task stop 的 Control 状态同源，不等于所有其他 TaskRunFinished
  等通知都已与领域写入原子化；approval/tool/route broker、常规 TUI 接管仍未关闭。

- 同一个 session/run 的 public outbox 是公开事件序列的唯一 durable 来源。runtime 的单一
  live producer 只提出下一序号，session writer 在锁内校验；重启从该 durable frontier
  继续，不再接受 HTTP journal、UI 缓存或调用方传入的起始序号。
- 后台 terminal task 的 lifecycle 回调复用该 run 的同一 runtime 序列与 outbox，不由 HTTP
  另行分配序号。只有这一已存在 owner 的 lifecycle 可在 foreground terminal 后继续追加；
  普通 run 事件仍关闭，原 foreground 结局不能改写。尚未启动 runtime 的 preparation
  失败/取消仍是独立 adapter outcome，不冒充已经提交的领域 run terminal。
  HTTP 补交该类 lifecycle 时仍须同步既有 registry 的 generation/status 投影；最后一个
  lifecycle 的 durable publication 与 stream close 原子完成，重放不重启进程或创建新 owner。
- 所有已物化的 public event 在发送之前固定 identity、sequence、完整有界 payload 与
  digest，并经现有 crash-safe append intent 持久化。普通进度以 outbox 本身作为 publication
  fact（domain ID 自引用），不制造不存在的领域记录；root/revision 终态继续引用原领域
  envelope，不能以普通 append 绕过其原子提交。已发布事件不能重新编号或改写 payload。
- 持久化失败时不发送、不推进 live frontier；恢复原 intent 后交付原字节。handler 失败、
  subscriber 断线、receipt 丢失是交付问题，不改写任务结果，也不重跑 provider/tool/effect。
  同一 adapter 按 durable 顺序补交，不能越过未接收事件发布后续事件。
- ACK 按既有事件类别解释：HTTP 的 Durable 类（状态、等待、终态等）必须完成其
  durable replay publication，TUI/application 的状态 ACK 必须完成其 reducer commit，
  不能仅凭成功入内存 channel 就确认。
  ACK 丢失允许至少一次重放；相同 identity/digest 幂等，identity 冲突明确失败。
  ACK 不等于人类已读、画面已呈现、command 成功或领域终态，也不删除状态历史。
- HTTP 的 `TextDelta / ReasoningDelta / ToolCallArgsDelta / ToolProgress` 保持原协议的
  Transient 分类与可丢弃 provisional UI 语义，不新增 token-history replay。它们同样先
  outbox、再按序尝试 live fanout；其 receipt 只表示该次 adapter 受理，不证明进入 HTTP
  durable journal、订阅者收到或重连可恢复。缺 receipt 时只允许原事件至少一次尝试，
  不因 receipt 有无改变领域状态；状态恢复仍依赖 Durable 事件与 application snapshot。
- revision `WaitingForInput` 与对应 `RunAwaitingUserInput` 在同一个既有 writer bundle
  提交，精确绑定 attempt、child、pending request identity/generation/hash。它是可恢复
  suspension，不占用最终 terminal pair；输入后沿原 run/attempt 继续递增。root run 的
  `RunAwaitingUserInput` 仍保持其原领域终态语义，不能仅按 event kind 混淆两者。
  接受回答不提前将 parent 标成 Started；worker 先核对既有 managed child 的 exact
  Submitted/Declined receipt，再接管原 Started 路径。接管前崩溃仍由 Waiting 与原 child
  receipt 恢复；取消沿既有 parent terminal settlement，不凭 adapter 标记授予继续权限。
- startup/attach/reconnect 的 outbox 消费者接入完整 ordered replay；只重放 terminal、
  revision live-only 通知、optional outbox 与 adapter-owned sequence 旧路径同批删除。
  source index 可由日志增量派生，但不拥有第二份事实，也不在每个 delta 上全量重读日志。
- HTTP 有界 replay window 被其他流淘汰时，按已验证的 session outbox 重建目标 run 的
  adapter projection，不能凭旧 watermark 猜测 receipt。重建在既有 journal 内先校验完整
  candidate，按原全局容量裁剪并一次提交；成功后才按原 pending 顺序（含 Transient）交付与
  ACK。旧 cursor 仍遵守 expired/reset 语义，不新增第二份事实或无界 retention registry。
  source 读取前捕获 journal 的进程内 cache revision，提交时在同一 journal 锁内检查；期间
  有并发写入则拒绝旧快照，即使该 run 的旧 watermark 已被容量裁剪移除也不能覆盖新事实。
  该 revision 不持久化、不替代 public sequence，也不构成新的权限或领域状态。
- 验收覆盖 persistence 后发送前崩溃、partial append、发送失败、ACK 丢失、相同序号冲突、
  重开与断线重连、等待输入后恢复、最终 terminal 原样保留，以及真实 HTTP/TUI 消费者。
  自动化全部使用隔离测试入口；不迁移、重签或清理用户旧数据，不执行实际存储 cutover。

### 13.3 ProjectionDegraded

projector apply失败返回：

```rust
ProjectionOutcomeV1<T> {
    Ready(T),
    Degraded(ProjectionDegradedV1),
}
```

它不能构造 domain `RunFailed`。runtime尝试从 durable session重建 projector；若 approval/authority视图无法证明，
只暂停新 effect。

## 14. 产品表面与 UX

### 14.1 Plan

点击 Run后立即关闭 Plan决策态并显示 Task running/paused：

```text
Plan 已批准 · Task 正在执行
[查看 Task] [暂停]
```

不存在“重试准备”这一新写路径动作。只有 runner实际遇到 provider、permission、tool、verification或effect blocker时，
Task才显示相应的 typed recovery action；Plan卡不回退为“提交失败”，Task id始终可见。

### 14.2 Workspace conflict

```text
正在重新同步工作区
另一个编辑修改了 src/foo.rs；Sigil 正在重新读取并更新本次改动。
```

只有无法安全继续时显示 effect-local blocker：

```text
当前修改需要重新规划
[重新生成] [查看双方差异] [改用 worktree] [取消当前步骤]
```

### 14.3 Reconciliation

```text
外部操作结果需要确认
Sigil 不会重复执行该操作，正在进行只读核对。
[查看证据] [重新核对] [标记已完成] [取消任务]
```

手工“标记已完成”需要明确 authority并写 acceptance receipt，不能只改 UI。

### 14.4 TUI状态与内容分离

运行状态放在 tool card header/status band，不作为 stdout内容行。`foreground shell command is running` 一类
host状态不得混入 captured output；running/blocked/retrying/reconciling使用统一 badge和辅助说明。

## 15. 完整故障矩阵

| 边界 | 故障 | Effect settlement | 最小 scope | 状态/动作 | 禁止行为 |
| --- | --- | --- | --- | --- | --- |
| Plan finalizer | draft schema首次无效、随后修正 | NoEffect | Plan attempt | recovered notice | 红色 RunFailed |
| Plan precompile | intent alias缺失 | NoEffect | advisory compile | Plan仍 Reviewable | 拒绝 Plan提交 |
| Plan approval | CAS frontier stale | NoEffect | approval command | refresh/retry | 创建第二 Task |
| Plan Run | model DAG/schema无效 | NoEffect | advisory only | direct execute | 阻止 Task启动 |
| Task planner | 未输出 accepted plan | NoEffect | optional optimization | host linear fallback | Task Failed |
| Read tool before start | 任意 workspace drift | NoEffect | observation | rebase/read latest | revoke invocation |
| Prepared write before body | unrelated path drift | NoEffect | observation | rebase/continue | global failure |
| Prepared write before body | same target drift | NoEffect | tool effect | reread/regenerate | overwrite current bytes |
| Prepared write during commit | CAS mismatch | NoEffect/Partial | tool effect | rollback/reconcile | Task Failed |
| Same-file merge | clean exact 3-way | NoEffect before commit | tool effect | preview/reapprove/commit | unreviewed merge |
| Same-file merge | semantic conflict | NoEffect | step effect | replan/block | conflict-marker write |
| Unknown shell body | external writer concurrent | Uncertain | effect | reconcile attribution | claim all changes |
| Shell exit known nonzero | Applied/NoEffect known | tool effect | model-visible result/replan | generic Internal |
| Shell killed after side effect | Uncertain | tool effect | reconcile | blind replay |
| Mutation applied, receipt append fails | Uncertain | tool effect | reconcile | retry write |
| Provider connect before bytes | ConfirmedNoEffect | provider turn | retry/fallback | step Failed |
| Provider disconnect after output | partial/uncertain | provider turn | continue/reconcile | discard settled tools |
| Provider auth/billing | NoEffect | run/participant | AwaitUser/Resource | permanent Failed by default |
| Provider retry budget exhausted | NoEffect | participant/task | Paused | Task Failed by default |
| Partial output only | NoEffect | segment | continue/accept/block | Completed from text |
| Active blocker + final prose | any | step | Blocked | Completed with warnings |
| MCP refresh registry shared | NoEffect | LocalOperation | Deferred/Retrying | clear foreground run |
| Diagnostics unavailable | NoEffect | LocalOperation | operation failed | RunFailed |
| Projector apply error | NoEffect | Projection | degraded/rebuild | manufacture terminal |
| SSE subscriber disconnect | NoEffect | Adapter | retry/resubscribe | stop domain run |
| Durable public journal unavailable | NoEffect | Delivery | DeliveryBlocked/outbox | change domain terminal |
| Approval event not delivered | NotStarted | effect admission | Blocked | execute effect |
| Terminal event delivery fails | domain already terminal | Adapter | replay same terminal | synthesize RunFailed |
| Restart with active retry | NoEffect | exact attempt | single claim/resume | duplicate dispatch |
| Restart with uncertain effect | Uncertain | exact effect | reconcile | auto replay |
| Durable corruption proven | unknown | owning Task/Run | Irrecoverable | continue unsafely |
| Explicit user cancel | bounded | cancellation scope | Cancelled/Interrupted | Failed |

## 16. 实施切片

切片必须按依赖执行；每个切片完成条件包含 code、tests、docs/status和指定 gate。

### R69.0 规范与现有改动对齐（serial）

目标：冻结本 RFC，消除 RFC-0067/0068/技术方案冲突。

文件：

- 本 RFC
- RFC-0067、RFC-0068
- `dev/docs/sigil-rust-agent-core-technical-solution.md`
- `.repo-local-dev/rfcs/STATUS.md` 和 R69 execution ledger

验收：docs link/path检查；所有旧规范条款有 superseded/extended标记。

### R69.1 Unified recovery schema与blocker projection（serial foundation）

目标：新增 §6 类型、durable events、strict decoder、active/resolved projection和 public safe view。

主要文件：

- `crates/sigil-kernel/src/event.rs`
- `crates/sigil-kernel/src/session/*`
- `crates/sigil-kernel/src/task.rs`
- `crates/sigil-kernel/src/tool.rs`

验收：duplicate/mismatched terminal、restart projection、redaction/size bounds、unknown newer schema测试。

### R69.2 Status propagation与completion proof（depends R69.1）

目标：删除 final-text recovery判决和 generic downcast terminal；Task/Step只由 typed outcome/blocker settle。

主要文件：

- `crates/sigil-kernel/src/task_orchestrator/scheduler.rs`
- `crates/sigil-kernel/src/task_orchestrator/runner.rs`
- `crates/sigil-kernel/src/agent/tool_results.rs`
- `crates/sigil-runtime/src/agent_supervisor/*`
- `crates/sigil-runtime/src/application_run*`

验收：active blocker + text仍 Blocked；resolved receipt后 Completed；recoverable upstream不启动 downstream。

### R69.3 TUI/local operation与cross-surface conformance（depends R69.1）

目标：旁路 operation不再使用 RunFailed；PlanReview/Chat/Task/Application共享 terminal mapping。

主要文件：

- `crates/sigil-tui/src/runner/protocol.rs`
- `crates/sigil-tui/src/runner/worker_loop/*`
- `crates/sigil-tui/src/app/worker_bridge*`
- `crates/sigil-http/src/*`
- `crates/sigil-desktop/src/*`
- `apps/desktop/src/*`

验收：active run + MCP/diagnostics/verification不清 run；OpenAPI/TS contract无 drift；所有状态穷尽映射。

### R69.4 Plan/Task cutover（depends R69.1）

目标：完成 §8 直接执行新写路径，删除 production materialization/adoption入口；Plan Run原子创建 first-class
direct execution admission，普通 Task planner失败自动降级；只有真实高级调度才创建 TaskPlan。

主要文件：

- `crates/sigil-kernel/src/plan.rs`
- `crates/sigil-runtime/src/plan_review_coordinator.rs`
- `crates/sigil-runtime/src/conversation_display.rs`
- TUI/HTTP/Desktop Plan surfaces

验收：缺 intent alias/DAG或弱 structured-output模型仍可执行；点击 Run后无 preparation blocker；单项任务不显示
伪 task list；direct Task可暂停、恢复；同一 command/task幂等；scoped grant原子保存；crash point matrix；legacy
replay。

### R69.5 Workspace authority/observation与path CAS（depends R69.1）

目标：完成 §9 grant拆分、observation event、read/write set、path CAS、same-target stale和changed-path evidence。

主要文件：

- `crates/sigil-kernel/src/agent_thread.rs`
- `crates/sigil-kernel/src/mutation/*`
- `crates/sigil-kernel/src/tool.rs`
- `crates/sigil-tools-builtin/src/file_tools.rs`
- `crates/sigil-tools-builtin/src/changeset_tool.rs`

验收：unrelated drift、same-target drift、clean merge、semantic conflict、read-only after drift、concurrent workers。

### R69.6 Effect reconciliation runtime（depends R69.1, R69.5）

目标：完成 required→probe→terminal→resume，接入 task blocker和 restart。

主要文件：

- `crates/sigil-kernel/src/session/effect_reconciliation.rs`
- `crates/sigil-kernel/src/mutation/recorder.rs`
- `crates/sigil-runtime/src/*reconciliation*`
- TUI/HTTP/Desktop recovery actions

验收：effect applied/not-applied/still-uncertain，receipt append fault，single claim，no blind replay。

### R69.7 Execution segments、partial output与fallback（depends R69.1, R69.2, R69.4）

目标：legacy/显式高级 Task可投影segments；Plan Run保持单执行单元；participant continuity使用safe frontier；partial
output不伪造completion；provider fallback遵守 effect settlement。

主要文件：

- `crates/sigil-kernel/src/task.rs`
- `crates/sigil-kernel/src/task_orchestrator/*`
- `crates/sigil-kernel/src/agent/provider_stream.rs`
- `crates/sigil-runtime/src/agent_supervisor/*`

验收：跨step same participant continuation、partial output retry/accept/reconcile、fallback不重跑settled tools。

### R69.8 Adapter outbox与projection degradation（depends R69.1）

目标：domain terminal与delivery解耦；public outbox幂等重放；projector不能制造 RunFailed。

主要文件：

- `crates/sigil-kernel/src/public_task_event.rs`
- `crates/sigil-runtime/src/application_run.rs`
- `crates/sigil-http/src/journal.rs`
- `crates/sigil-http/src/sse.rs`
- Desktop stream/reducer

验收：Notice/Approval/ToolResult/terminal四阶段故障注入；domain terminal保持；恢复后同 event id重放。

### R69.9 Fault campaign、docs与qualification（depends R69.2-R69.8）

目标：执行 §15 全矩阵，补用户/开发者文档和事故回归，完成跨重启、cross-surface和真实共享 worktree测试。

验收：§18 全部门禁；无 unresolved P1/P2；review报告追加复核与修复记录。

## 17. Code landing map

| 责任 | Owner |
| --- | --- |
| failure/blocker/effect settlement public domain | `sigil-kernel` |
| durable blocker/reconciliation/outbox projections | `sigil-kernel::session` |
| Plan review/materialization orchestration | `sigil-runtime` |
| workspace observation/path CAS/mutation receipts | `sigil-kernel::mutation` + builtin tools |
| provider attempt/segment continuation | `sigil-kernel::agent` + runtime supervisor |
| Task/step terminal ownership | `sigil-kernel::task_orchestrator` |
| public protocol and HTTP delivery | `sigil-http` |
| Desktop native DTO/stream | `sigil-desktop` + `apps/desktop/src-tauri` |
| Desktop presentation | `apps/desktop` React reducer/components |
| TUI operation/run projection | `sigil-tui` |
| CLI adapter | `sigil` |

Kernel public类型保持 provider-neutral；provider私有 status/error只在 provider crate转换为统一 observation。

## 18. 资格门禁与完成定义

### 18.1 Targeted gates

- kernel blocker/session/event/tool/mutation/plan/task/scheduler tests；
- runtime PlanReview/direct execution/legacy materializer/application/supervisor tests；
- TUI worker bridge、MCP、verification、provider recovery、plan handoff tests；
- HTTP journal/SSE/production driver tests；
- Desktop Rust DTO/event tests与 TypeScript reducer tests；
- provider retry/partial-output/fallback tests。

### 18.2 Contract gates

- `./scripts/generate-desktop-contract.sh --check`
- `pnpm --dir apps/desktop check`
- Kernel → Runtime → HTTP → Desktop/TUI status conformance fixture；
- legacy session replay和newer-schema fail-closed tests。

### 18.3 Workspace/fault campaign

必须自动化 §15 全部行，至少覆盖：

- 两个 worker同 worktree修改无关文件；
- 两个 worker修改同文件的before-body/during-body分支；
- mutation applied与receipt append之间 crash；
- provider output/side effect各 frontier；
- process restart at retry scheduled/started/reconciling/terminal delivery；
- journal/projector fault injection；
- Plan approval + Task + direct admission + optional checklist原子 append frontier，以及 legacy materialization replay。
- direct Task执行中插入 exact-Task follow-up后，continuation binding保持`plan_version=None`、同一 Task/checklist可见且
  executor仍获得 checklist update context；unrelated Chat只保留被动进度展示，不获得 Task mutation authority。

### 18.4 Repository gates

- `cargo fmt --all --check`
- `cargo check`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `./scripts/check-docs.sh`
- `./scripts/check-touched.sh --scope dirty --tier full`

### 18.5 Completion definition

本 RFC 只有在以下条件全部成立时才能标记实施完成：

1. §16 R69.0-R69.9全部 done；
2. §15 故障矩阵每一行有自动化 evidence或明确真实平台 gate；
3. 当前 review的 7 个 P1、3 个 P2全部复核关闭；
4. RFC-0067/0068/技术方案/用户文档与实现一致；
5. TUI、Desktop、HTTP、CLI 对同一 durable状态显示一致；
6. shared worktree并发不会仅因任意 workspace drift终止 Task；
7. unknown effect不会盲重放，且能跨 restart完成 reconciliation；
8. active blocker不能被 final prose或 adapter failure关闭；
9. Plan approval、Task identity和domain terminal满足单调性；
10. full gates通过且没有 unresolved P1/P2 finding。

## 19. 文档迁移

实施时必须同步：

- RFC-0067：标注被本 RFC取代的 DraftReady/candidate/adoption条款；
- RFC-0068：更新 workspace grant解释、统一 taxonomy、execution segment/partial-output/fallback链接；
- technical solution：Plan/Task、workspace、TUI operation、public event/delivery、reconciliation；
- `docs/en` / `docs/zh-CN`：Plan批准后准备受阻、workspace conflict、reconciliation、retry/blocked状态；
- Desktop/TUI help与 status copy；
- `.repo-local-dev/rfcs/STATUS.md` 和 R69 execution ledger；
- review报告的复核结论与修复执行记录。

## 20. 被拒绝的方案

### 20.1 禁止其他 worker修改同 worktree

拒绝。它把产品正确性建立在用户无法可靠维持的外部纪律上，并降低多 agent价值。worktree可选但不能成为共享
checkout的强制正确性前提。

### 20.2 删除 workspace snapshot

拒绝。snapshot对审计、verification、recovery和 prepared mutation仍有价值；需要删除的是其“全局执行锁”
语义。

### 20.3 捕获所有错误并继续

拒绝。unknown effect、真实越权和 corruption必须 fail closed。正确方案是收窄 scope并要求 typed recovery，而不是
吞错。

### 20.4 扩大 downcast白名单

拒绝。每增加一个恢复域就修改多层具体错误类型会继续产生遗漏；必须使用统一领域 outcome。

### 20.5 只修 UI文案

拒绝。红色错误只是错误领域状态的表现；如果 durable Task/Run已经被误判 Failed，换文案无法恢复任务。

### 20.6 Plan批准前强制完整 Task compile

拒绝。它混淆人类决策 artifact与执行 IR，正是首次事故根因。批准后再强制模型生成 Task DAG同样被拒绝，因为只是
把不稳定边界移到 Run按钮之后。precompile只能是 advisory cache；基础执行 authority必须由 host确定性生成。

### 20.7 自动三方合并所有同文件冲突

拒绝。无法证明语义安全时自动 merge可能丢失双方意图。必须局部 Blocked/Replan并保留双方内容。

## 21. 安全与可靠性结论

本 RFC 不降低严格边界，而是明确严格边界应保护什么：

- permission/approval保护 effect admission；
- workspace CAS保护目标内容；
- effect reconciliation保护“不重复副作用”；
- durable blocker保护可恢复任务不被错误完成；
- domain terminal authority保护真实结果不被 adapter改写；
- Plan/Task phase separation保护用户决策不被编译细节推翻。

完成后，Sigil 的安全模型从“任何不满足预期都终止全局流程”转为：

```text
detect precisely
  -> contain locally
  -> preserve settled facts
  -> retry/rebase/reconcile with durable evidence
  -> pause only the owning boundary
  -> fail globally only when irrecoverability is proven
```
