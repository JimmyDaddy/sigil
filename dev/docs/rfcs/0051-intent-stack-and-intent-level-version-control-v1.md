# RFC-0051 Intent Stack / 意图级版本控制 V1

状态：proposed / execution plan hardened / implementation deferred

创建日期：2026-07-22

依赖：

- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- [RFC-0014 Write Isolation and Worktree Merge](0014-write-isolation-and-worktree-merge.md)
- [RFC-0018 Plan-to-Task Handoff](0018-plan-to-task-handoff.md)
- [RFC-0024 TUI Checkpoint / Rewind V1](0024-tui-checkpoint-rewind-v1.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)

## 1. Summary

现有版本控制回答“文件在时间线上发生了什么”，checkpoint 回答“回到某个 user turn 之前”，task graph 回答“哪些执行步骤存在依赖”。它们都不能直接回答用户真正关心的问题：

> 如果一次改动同时实现了重试、遥测和文档更新，能否只删除“遥测”这个意图，保留重试以及与重试相关的测试和文档？

Intent Stack 把用户认可的变更意图提升为一等、可持久化、可审计的版本对象。每个意图绑定需求陈述、验收条件、依赖、受控 ChangeSet、测试/文档证据和 verification receipt。用户可以按意图审查、比较、丢弃或替换变更，而不必先理解 commit、turn、agent step 或文件边界。

产品承诺可以概括为：

> 撤销一个需求，而不是回到一个时间点。

本 RFC 只冻结语义、所有权和安全边界，**不授权当前变更进入实现**。V1 后续实施也只覆盖归属明确、跨 intent 文件互斥、由 Sigil 受控文件 mutation 产生的普通文件改动；任意人工 drift、同文件跨 intent、共享 hunk、未知副作用或依赖歧义必须 fail closed。

## 2. Problem statement

一次用户可感知的产品变更通常横跨多个文件、测试和文档；一个文件也可能同时承载多个变更意图。当前对象的边界与用户心智并不一致：

| 现有对象 | 可以证明 | 不能证明 |
| --- | --- | --- |
| Git commit / diff | 一组文本变化 | 每段变化服务于哪个用户意图 |
| user turn / checkpoint | 某段时间内的受控写入 | 同一 turn 内多个意图如何拆分 |
| task / task step | 执行计划与依赖 | 最终 artifact 是否仍与原意图一致 |
| `ChangeSet` | 文件级摘要、风险与 validation | hunk/test/doc 的意图归属与 lineage |
| mutation evidence | before/after hash、snapshot 与 workspace revision | 删除一个语义单元后应保留什么 |
| verification receipt | 某 snapshot 上某策略通过 | 证明结果属于哪个意图及其依赖闭包 |

此外，当前 changeset-only child 的完整 patch 仍可能只是 process-local artifact；durable stream 中的摘要引用不足以在 restart 后重建 exact reverse patch。Intent Stack 在任何可写里程碑前，必须先补齐 content-addressed layer artifact 与 retention lifecycle，不能把进程内 patch 当成恢复事实。

如果只在 UI 上给 diff 加标签，Intent Stack 会退化成不可验证的 requirement traceability。如果直接让模型生成一份“反向补丁”，则无法证明它没有删除其他意图、覆盖人工编辑或伪造验证结论。

因此 Intent Stack 必须建立一条完整证据链：

```text
用户认可的意图
  -> versioned intent plan
  -> isolated / controlled ChangeSet
  -> artifact ownership and lineage
  -> workspace snapshot
  -> verification evidence
  -> exact intent operation preview
  -> append-only operation result
```

## 3. Terminology

### 3.1 Intent

`Intent` 是用户可辨认、可独立接受或拒绝的变更目的，例如“失败请求最多重试两次”或“记录重试次数指标”。它不是：

- 一段 prompt 或模型 thought；
- 一个 user turn；
- 一个 agent/task step；
- 一个 commit、branch 或 worktree；
- 一个文件、symbol 或 diff hunk；
- 一条自动推断且未经用户认可的需求。

系统可以建议拆分或合并意图，但进入 accepted stack 前必须呈现稳定的标题、陈述和验收条件，由用户确认或由明确的上游规格导入。

### 3.2 Intent Stack

`Intent Stack` 是面向用户的有序视图，表达“当前变更由哪些意图组成”。其领域真相不是单纯线性栈，而是：

- 一个稳定的 stack identity；
- 一个 append-only、带版本的 intent plan；
- 一个显式 dependency DAG；
- 每个 intent 与 artifacts、ChangeSet、snapshot 和 evidence 的绑定；
- replace、drop、supersede 和 conflict 的 lineage。

展示顺序用于阅读和交互，dependency DAG 决定操作是否合法。UI 不得把“列表中相邻”误解为“可以任意出栈”。

### 3.3 Intent operation

Intent operation 是针对一个或一组 intent 的高层动作：

- `review`：按意图查看实现、测试、文档和验证状态；
- `drop`：移除目标意图的独占贡献；
- `replace`：以新版本实现 supersede 旧实现；
- `revise`：修改需求或验收条件并重算受影响闭包；
- `reorder`：只改变无依赖意图的展示/执行建议顺序。

所有写操作都必须先产生 exact preview，并在执行时重新投影、校验 digest 和 workspace state。

## 4. Goals

1. 让用户按产品意图而不是按 turn、commit 或文件审查一组变更。
2. 为 intent、ChangeSet、artifact、verification 建立可恢复、append-only 的 provenance。
3. 对安全子集支持“只丢弃一个意图，并保留其余意图”的精确操作。
4. 显式呈现依赖闭包、共享贡献、人工 drift、未知副作用和 verification stale 状态。
5. 复用既有 mutation、checkpoint、task DAG、write isolation 和 verification contract，不创建第二套事实源。
6. 保持 `sigil-kernel` provider-neutral；意图语义不得包含 DeepSeek 或某个 UI 的私有字段。
7. 让同一 durable projection 可被 TUI、desktop、CLI automation 或未来非 coding adapter 消费。

## 5. Non-goals

- 不把自然语言自动分段包装成“已证明的意图”。
- 不承诺理解任意历史 Git 仓库、第三方 patch 或用户手写代码的真实动机。
- 不撤销 shell、MCP、network、database、remote service、package publish 或其他外部副作用。
- 不以模型生成的逆向 patch 替代受控 artifact lineage。
- 不保证 V1 对共享 hunk、同一 symbol 内交织修改或任意后续人工编辑做自动语义合并。
- 不在 V1 对同一文件的多个 active intent 做 retained-layer rematerialization；即使 byte range
  不重叠，也统一标为 shared/read-only。
- 不把每个 agent step、tool call 或 validation command 都暴露为用户要管理的 stack item。
- 不新增一组 command-only 的 `/intent-*` 主入口，也不要求用户维护复杂配置矩阵。
- 不替代 Git；Intent Stack 是高于文件版本控制的产品语义层，最终仍可导出普通 diff/commit。
- 不在本 RFC 变更中实现 domain type、event、projection、runtime command 或 UI。

## 6. Product contract

### 6.1 Capture and acceptance

在执行前，Sigil 将用户目标投影为少量、可独立取舍的 intent cards。每个 card 至少包含：

- 稳定标题与一句话 intent statement；
- 可检查的 acceptance criteria；
- 显式依赖与可能受影响的范围；
- 来源：用户直接输入、用户确认的系统建议或可信规格引用；
- 当前状态与不可执行原因。

用户可以先接受整个 plan，再允许执行。简单任务只生成一个 intent，不为了展示功能而强制拆分。

为避免和 RFC-0053 自动 Task 路由产生额外仪式，V1 冻结以下 acceptance 规则：

1. 原始 user turn 由 host 建立一个 `UserDeclaredRoot` intent，statement 绑定 source turn ref；
   它表示用户请求的完整 outcome，不表示系统已证明内部拆分。该单 intent 可以随 Task admission
   一次接受，不需要再弹一个“是否接受你刚才说的话”的确认。
2. Planner/模型建议的多个可独立 drop intent 属于 `SuggestedDecomposition`。runtime 必须展示
   title、statement、acceptance criteria、dependency 和来源；用户一次确认后，才能把 proposal
   变成 accepted IntentPlan。模型、renderer 和 imported untrusted text 不能自行接受拆分。
3. trusted spec import 仍只提供 source/provenance；除非已有显式、内容绑定的规格接受 decision，
   否则不能跳过 acceptance。
4. Task 路径中的 accepted IntentPlan 与引用它的 TaskPlan version 必须在同一个 durable writer
   batch 中提交；只成功一半时恢复不能启动 write participant。Chat 路径没有 TaskPlan，host
   必须在首个 mutating tool admission 前把 accepted root intent 与 exact root logical run/source
   turn 绑定。Task replan 若只改变执行步骤，不增加 intent version；statement、criteria 或
   dependency 变化才创建新的 IntentPlan version。

### 6.2 Intent-level review

完成实现后，review 默认按 intent 分组，而不是只展示文件树。每个 intent card 聚合：

- 独占和共享的 files/hunks；
- 关联测试、文档与 migration；
- 风险、依赖和 verification receipt；
- 未归属、已 drift 或 unsupported artifact；
- 与上一个 intent version 的差异。

用户仍可下钻到普通 unified diff；intent grouping 不能隐藏文件级事实。

### 6.3 Drop intent

`drop` 的用户语义是：“移除该意图的已证明贡献，保留无依赖的其他意图与人工工作。”

V1 只有同时满足以下条件时才可执行：

1. 目标贡献来自当前 session 的受控普通文件 mutation；
2. 每个目标 artifact 都有可用 before/after evidence 与 matching current hash；
3. hunk ownership 为 exclusive，不与其他 active intent 或 unowned edit 重叠；
4. 目标是没有 active downstream dependent 的 leaf intent；V1 不提供自动级联 drop；
5. protected path、approval、workspace lease 与 mutation policy 全部允许；
6. exact preview digest、stack version 和 workspace revision 在 apply 时仍匹配。

任一条件不满足时，系统只解释冲突，不生成“尽力而为”的 patch。

### 6.4 Replace / revise

长期目标允许用户修改一个 intent 的 statement 或 acceptance criteria，然后只重新规划、实现和验证受影响的依赖闭包。旧 intent version、旧 artifact 和旧 receipt 保留为 superseded lineage，不能被原地改写。

V1 可以先提供 read-only impact preview；自动 replace/rebase 必须等 isolated regeneration、artifact ownership 和 conflict semantics 经独立 RFC/implementation slice 证明后再开放。

### 6.5 Verification

Intent Stack 不自行宣称“此意图已验证”。通过状态只来自 RFC-0003 verification receipt，并绑定：

- intent plan version；
- intent/closure identity；
- ChangeSet digest；
- workspace snapshot；
- verification policy 与证据。

任何 drop/replace/revise 都会产生新的 workspace mutation。V1 保守地让相关既有 receipt stale，并重新运行要求的 verification；只有未来具备可证明的 scope mapping 后，才能缩小 invalidation 范围。

## 7. Domain model

以下结构表示语义边界，不冻结 Rust 字段命名或序列化格式：

```rust
struct IntentStackProjection {
    stack_id: IntentStackId,
    version: IntentStackVersion,
    workspace_id: WorkspaceId,
    source_session_id: SessionId,
    intents: Vec<IntentChangeUnit>,
    plan_digest: Digest,
}

struct IntentChangeUnit {
    intent_id: IntentId,
    version: u64,
    title: String,
    statement: String,
    acceptance_criteria: Vec<AcceptanceCriterion>,
    depends_on: Vec<IntentId>,
    definition_state: IntentDefinitionState,
    application_state: IntentApplicationState,
    source: IntentSource,
    base_snapshot_id: Option<SnapshotId>,
    execution_bindings: Vec<IntentExecutionBinding>,
    changeset_ids: Vec<ChangeSetId>,
    layer_manifest_ref: Option<ArtifactId>,
    artifact_bindings: Vec<IntentArtifactBinding>,
    verification_receipts: Vec<EvidenceReceiptId>,
    supersedes: Option<IntentVersionRef>,
}

struct IntentExecutionBinding {
    intent_ref: IntentVersionRef,
    origin: IntentExecutionOrigin,
    binding_kind: IntentExecutionBindingKind,
    source_event_id: EventId,
}

enum IntentExecutionOrigin {
    Task {
        task_id: TaskId,
        task_plan_version: u32,
        step_id: TaskStepId,
        attempt_id: Option<TaskStepAttemptId>,
    },
    Chat {
        root_logical_run_id: LogicalRunId,
        source_turn_id: TurnId,
        attempt_id: Option<AgentRunAttemptId>,
    },
}

struct IntentArtifactBinding {
    artifact_id: IntentArtifactId,
    kind: IntentArtifactKind,
    subject: BoundedArtifactSubject,
    ownership: IntentArtifactOwnership,
    before_digest: Option<Digest>,
    after_digest: Digest,
    provenance: ArtifactProvenance,
}

struct IntentLayerManifest {
    intent_id: IntentId,
    intent_version: u64,
    execution_id: IntentExecutionId,
    execution_origin: IntentExecutionOrigin,
    base_snapshot_id: SnapshotId,
    result_snapshot_id: SnapshotId,
    files: Vec<IntentLayerFile>,
    changeset_ids: Vec<ChangeSetId>,
    operation_ids: Vec<OperationId>,
    forward_patch_ref: ArtifactId,
    reverse_patch_ref: ArtifactId,
    manifest_digest: Digest,
}
```

### 7.1 Identity and versioning

- `IntentId` 在同一逻辑需求的 revise/replace lineage 中稳定；每次语义变化增加 version。
- `IntentStackVersion` 每次 accepted plan 变化单调递增。
- `definition_state` 与 `application_state` 必须分离；plan 被 supersede 不等于旧代码已从 workspace 移除，反之亦然。
- intent、stack 和 artifact id 必须由 runtime 分配，不能信任 renderer 或 provider 生成的 id。
- digest 绑定有序内容、依赖、artifact bindings 与必要的 workspace identity；显示文案变化不能偷偷改变操作目标。
- 规范 provenance chain 根据 execution origin 分为：
  - Task：`IntentVersionRef -> TaskPlanVersion -> TaskStep -> TaskStepAttempt -> ChangeSet ->
    parent Mutation/Verification`；
  - Chat：`IntentVersionRef -> root logical run -> source turn -> agent-run attempt ->
    controlled Mutation/ChangeSet -> parent Verification`。
  任一适用的中间 identity 缺失时仍可保留普通执行审计，但不得生成可执行 intent layer。
- 同一个 intent version 可以有多个 changeset/attempt（例如 conflict repair 或 replace），因此
  `changeset_ids` 是有序集合而不是 singular shortcut。只有最终 promotion 到 parent workspace
  且留下 RFC-0002 evidence 的 layer 才进入 `application_state = applied`。
- `IntentLayerManifest.execution_origin` 必须包含 concrete terminal attempt；projection 中
  `attempt_id = None` 的预绑定只能用于 admission/recovery，不能成为 layer authority。

### 7.2 Artifact kinds

V1 至少区分：

- `FileHunk`：受控普通文件 diff 中的稳定内容范围；
- `TestEvidence`：测试文件贡献或 validation result 引用；
- `Documentation`：文档贡献；
- `ChangeSet`：既有 ChangeSet 引用；
- `VerificationReceipt`：RFC-0003 receipt 引用；
- `UnsupportedSideEffect`：只记录 bounded warning，不成为可 drop artifact。

Symbol、AST node 或模型解释可以作为 inspect hint，但不能单独构成 mutation authority。V1 的可执行 identity 必须回落到受控 bytes、diff digest、path binding 和 workspace snapshot。

V1 `FileHunk` identity 使用 layer-local canonical byte diff：

- 绑定 normalized relative path、完整 before/after file digest、old/new UTF-8 byte range、
  old/new range content digest 和 owning layer manifest digest；
- canonical representation 由 runtime 的单一 diff materializer 生成，不信任 provider 行号；
- identity 只在该 before/after pair 内稳定。formatter、codegen 或人工 edit 改变完整文件 digest
  后，旧 hunk 进入 `drifted`，不得用 fuzzy/context matching 迁移；
- 同一 normalized file 被多个 active intent 修改时一律 `shared`，即使 canonical byte ranges
  当前不重叠。完整文件 digest 会随着任一 layer/drop 改变；V1 不猜测 retained-layer rebase，
  也不引入 composition artifact 来提高可执行率。

Forward/reverse patch 原文不能放进 event JSON，也不能只留在 process memory。可执行 layer 必须引用 content-addressed artifact，event 只保存 artifact id、digest、bounded hunk metadata 和 lifecycle state。artifact 过期、缺失或 hash 不匹配时，该 intent 永久降级为 read-only，直到通过新的受控 execution 建立 lineage。

### 7.3 Ownership

```text
exclusive  -> 只属于一个 active intent，可进入 V1 drop 候选
shared     -> 多个 intent 共同依赖，V1 只读并阻止单独 drop
unowned    -> 无法证明归属，永不自动改写
drifted    -> 当前 workspace 不再匹配记录，必须人工处理或重新建立 lineage
```

系统不得为了提高可执行率而把 shared/unowned artifact 猜成 exclusive。

## 8. Durable events and projection

Intent Stack 复用 RFC-0001 append-only session truth。建议的领域事件如下，最终命名在实现前由 contract review 冻结：

- `IntentStackCreated`
- `IntentPlanRecorded`
- `IntentPlanAccepted`
- `IntentExecutionBound`
- `IntentChangeSetBound`
- `IntentLayerManifestRecorded`
- `IntentArtifactBindingsRecorded`
- `IntentVerificationLinked`
- `IntentOperationRequested`
- `IntentOperationPrepared`
- `IntentOperationResolved`
- `IntentConflictRecorded`
- `IntentVersionSuperseded`

约束：

1. accepted plan 不原地编辑；新版本 supersede 旧版本。
2. provider 输出只能成为 proposal，runtime admission 后才进入 durable plan。
3. artifact binding 必须引用既有 mutation/ChangeSet/snapshot evidence，不能复制一份不一致的影子事实。
4. operation request 记录 operation id、stack version、preview digest、目标 intent/closure 和 bounded reason。
5. operation prepared 只在完整 preflight 后追加，绑定 exact artifact set、current workspace revision、
   permission/approval authority 和即将使用的 RFC-0002 batch identity；它不是已写入证明。
6. resolved event 记录 committed、rejected、cancelled、conflicted、partially_applied 或 interrupted，以及
   关联的新 mutation/evidence id。
7. recovery 根据 durable state 重建 projection；process-local cache 和 UI optimism 不是真相。
8. projection 必须对 unknown event、缺失 artifact、旧 schema 和不完整 operation fail closed。
9. intent execution、drop 与 conflict 是 recovery-critical typed event，不能用自由文本 `Note` 代替。
10. 旧 session 没有 intent facts 时只显示 `Intent history unavailable`，不得从旧 prompt、commit message 或 diff 追溯猜测。

## 9. Execution semantics

### 9.1 Build and bind

推荐执行顺序：

```text
accepted intent plan
  -> dependency-aware task plan
  -> isolated implementation per intent or safe group
  -> ChangeSet projection
  -> artifact ownership analysis
  -> verification
  -> intent review projection
```

Write isolation 是建立清晰 ownership 的优先手段，但不是证明本身。即使一个 worktree 只分配给一个 intent，merge 后仍必须记录准确 artifact binding，并检查与其他 intent/human edit 的重叠。

V1 中，一个产生可操作 layer 的 write execution 必须精确绑定一个 accepted intent。read/review/verify 可以服务多个 intent；未绑定或绑定多个 intent 的写入可以继续按普通 ChangeSet 审查，但标记为 `unassigned`，不得 selective drop。Child workspace proposal 只有在 parent apply 成功并留下受控 mutation evidence 后，才能成为 active intent layer。

#### 9.1.1 TaskPlan and verification bridge

RFC-0053 启用 Intent Stack 后，执行链固定为：

```text
IntentPlanAccepted + TaskPlanAccepted (one durable batch)
  -> TaskStepSpec.intent_refs
  -> TaskStepAttempt profile/snapshot/input binding
  -> isolated ChangeSet / integration lane
  -> parent RFC-0002 mutation
  -> parent RFC-0003 verification
  -> IntentLayerManifest + intent review
```

规则：

1. Planner 只能 proposal intent alias；runtime 从当前 accepted IntentPlan 解析 stable ref，并把
   resolved mapping 纳入 TaskPlan digest。unknown/stale/duplicate ref 在任何 write attempt 前拒绝。
2. V1 write step 必须精确绑定一个 `IntentVersionRef` 才可形成 exclusive layer；read/review/verify
   可以绑定多个 intent closure。共享实现应拆为显式 shared intent，或保持 `shared/unassigned`
   read-only，不允许为了支持 drop 而复制相同 hunk。
3. Task replan 只改变 step/attempt 时保留 intent version，并追加新的
   `IntentExecutionBound`；statement、criteria 或 dependency 变化先 supersede IntentPlan，再允许新
   TaskPlan write step。
4. Worktree/changeset child receipt 只证明 proposal；只有 RFC-0053 promotion 在 parent workspace
   留下 mutation evidence 后，runtime 才能记录 active `IntentLayerManifest`。child/lane Passed
   不能直接链接为 parent intent verified。
   RFC-0053 的 ref-only `GitRefAdvance` 在本 RFC V1 中也只产生 read-only/unassigned provenance；
   它没有 parent workspace RFC-0002 mutation evidence，不能把 intent 标记为 applied/verified。
   含 executable `intent_refs` 的 Task 在 V1 必须选择 `WorkspaceApply`。
5. acceptance criterion 与 test/check 的关联分为 `Advisory` 和 `SystemVerified`。只有 check spec
   显式声明 bounded intent/criterion scope、receipt 绑定当前 parent snapshot 且 policy coverage
   满足 RFC-0003 时，才可投影 `SystemVerified`；模型、文件名相似度或用户界面勾选只能形成
   advisory link。
6. Synthesis 必须同时读取 Task readiness 和 Intent projection：存在 required intent 未应用、
   artifact/conflict 不可解释或 required criterion 未满足时，不得宣称完整 intent verified。

#### 9.1.2 Chat execution and verification bridge

0053 的简单解释、查询和单点局部修改保持 Chat，不为 Intent Stack 强制创建 durable Task。
简单 Chat 写入使用以下链：

```text
UserDeclaredRoot IntentPlanAccepted
  + IntentExecutionBound(Chat root logical run/source turn)
  -> exact agent-run attempt
  -> controlled file mutation / ChangeSet projection
  -> parent workspace snapshot
  -> parent RFC-0003 verification
  -> IntentLayerManifest + intent review
```

规则：

1. host 在首个 mutating tool admission 前，把 exact source turn 建立的单一
   `UserDeclaredRoot` intent 与当前 root logical run 绑定；模型不能提交 intent/root-run id。
2. 一次 Chat run 只有一个 accepted root intent 时，受控普通文件 mutation 可以形成 exclusive
   layer。模型在运行中追加的子目标不自动变成独立 intent；需要多个可 drop intent 时，必须转为
   SuggestedDecomposition acceptance 或 Task。
3. binding 必须贯穿 tool call、RFC-0002 mutation batch、可选 ChangeSet projection、result
   snapshot 和 verification。没有 ChangeSet 的直接 file tool 路径必须由 runtime 从 mutation
   evidence 生成 bounded ChangeSet projection，不能让模型补写 lineage。
4. shell/MCP/network/unknown effect 仍只记录 `UnsupportedSideEffect`，不能因为和 Chat root
   intent 同 run 就获得 drop authority。
5. continue/retry 使用新的 agent-run attempt 并追加 execution binding；source turn、root run、
   workspace snapshot 或 intent version 漂移时，旧 binding 只能 read-only。
6. Chat 与 Task 最终都通过相同 `IntentLayerManifest`、artifact ownership、drop、retention 和
   verification contract；TUI 不暴露两套不同的 Intent Stack 操作语义。

### 9.2 Exact preview

任何写操作前，worker 必须重新投影并返回：

- stack/version 和目标 intent closure；
- 目标是否仍是 leaf intent；
- 将 create/update/delete 的普通文件；
- 反向或替换 diff；
- retained intents 及其受影响状态；
- shared/unowned/drifted/unsupported 冲突；
- 将 stale 和需要重跑的 verification；
- preview digest、workspace revision 与有效期条件。

UI 只提交 stable ids、version 和 digest，不提交可信 path、patch、hash 或 dependency closure。

### 9.3 Apply

V1 `drop` 复用 RFC-0002 mutation protocol：

1. 获取 workspace mutation lease；
2. 从 durable state 重新投影 operation；
3. 验证 stack version、preview digest、artifact availability 和全部 current hashes；
4. 在首个写入前完成全量 conflict preflight；
5. 从已验证的 reverse patch artifact 对当前完整文件计算唯一目标 bytes；不允许 fuzzy patch，也不调用模型重写冲突；
6. 为 drop 产生新的 mutation batch 和逐文件 prepare/commit evidence；
7. 追加 operation result 与 verification stale evidence；
8. 失败或 crash 按既有 batch reconciliation 解释。

与 checkpoint 一样，V1 不宣传多文件文件系统级原子性。未知状态必须进入 reconciliation，不能在 UI 中假装完整成功；partial apply 必须投影为 `partially_applied`/`conflict`，绝不能显示 `dropped`。

#### 9.3.1 Crash-consistent operation state machine

Intent operation 不新建第二套事务系统。它把高层 operation identity 绑定到 RFC-0002 的
mutation batch，并按下列状态机恢复：

```text
Requested
  -> Rejected | Cancelled | Prepared
Prepared
  -> Cancelled | Applying
Applying
  -> Committed | PartiallyApplied | Conflicted | Interrupted
```

- `Requested` 只证明用户/上游请求存在；可以安全重做 preview，不能开始写入。
- `Rejected` 表示 permission/policy/user review 明确拒绝；`Cancelled` 表示用户撤回或 Task/
  session cancellation 在任何 per-file prepare evidence 前成功收口。两者都是 durable terminal，
  不产生 mutation/verification transition。
- `Prepared` 必须绑定 exact stack version、preview digest、artifact manifest、permission/approval
  authority、workspace revision 与尚未执行的 mutation batch id。
- `Prepared` 只有在 RFC-0002 尚无任何 per-file prepare/commit evidence 时可转
  `Cancelled`；一旦进入 `Applying`，取消请求只能先停止后续 admission，再根据实际 evidence
  收口为 committed/partial/conflicted/interrupted，不能伪造无 effect 的 Cancelled。
- `Applying` 不是单独的模型状态；任一 RFC-0002 per-file prepare/commit evidence 已存在时，由
  projection 推导。每个文件 evidence 必须带同一 intent operation id。
- `Committed` 仅在 batch reconciliation 证明全部目标文件 commit、result snapshot 已建立且
  `IntentOperationResolved(committed)` 已追加后成立。
- 已有部分 commit、缺失 artifact、hash drift 或无法证明的 batch 状态分别投影为
  `PartiallyApplied`、`Conflicted` 或 `Interrupted`，不得自动重发 provider/tool/文件写请求。
- restart 时，若 RFC-0002 已完整 commit 但 terminal intent event 缺失，reconciler 可以从
  同一 batch evidence 幂等追加唯一 `IntentOperationResolved`；证据不足时只能进入人工
  reconciliation。
- verification invalidation 与 application-state transition 必须与 terminal resolution 使用同一
  operation/batch lineage；恢复不能出现“文件已改变但旧 receipt 仍显示 verified”的中间终态。

### 9.4 Dependency closure

- drop 一个被 active downstream intent 依赖的 intent 时阻止操作；V1 只允许 leaf intent。
- 级联 drop 必须由未来 RFC 单独定义 closure preview、确认和 partial failure 语义，不能由 UI 临时扩大范围。
- 只改变展示顺序不得改变 dependency DAG 或 execution truth。
- replace/revise 后，所有受影响 downstream intent 进入 `needs_rebuild` 或 `needs_review`，不能沿用旧 receipt。
- 循环依赖使 accepted plan invalid；执行前必须拒绝。

### 9.5 Checkpoint interaction

Intent drop 本身会产生新的受控文件 mutation，但不能被 checkpoint 当作普通原始 intent 写入。否则后续 rewind 可能把已删除代码重新加入，而 Intent Stack 仍投影为 `dropped`。

V1 在统一 redo/reconciliation 语义落地前，必须：

1. 让 checkpoint projection 识别 intent execution/drop/replace operation id，并把这些 mutation
   排除出普通 restore candidate；
2. 在 checkpoint exact preview/apply preflight 中查询 active Intent Stack。若目标文件/range 与
   active intent layer 重叠，或 restore 会跨越 intent application-state transition，则在首个写入
   前返回 typed `intent_state_conflict`；
3. 允许与所有 active intent artifact path/range 可证明 disjoint 的 checkpoint restore继续使用
   RFC-0024；不能因一个 stack 存在而全局禁用 rewind；
4. Session Review 展示 bounded warning 和关联 intent，不提供“忽略 Intent 状态强制恢复”；
5. 未来若允许 rewind 跨 intent operation，必须在同一恢复协议中追加 intent
   application-state transition 和 verification invalidation，不能只改文件。

### 9.6 Artifact retention and deletion

Intent Stack 不新建 artifact store。forward/reverse patch、layer manifest 和 before-content
继续复用 RFC-0002 content-addressed artifact/lifecycle。

- `applied` 且仍向用户提供 drop/replace 的 intent layer artifact 自动进入 retention-protected
  set；普通 age/count/bytes cleanup 不得静默选择它们。
- cleanup preview 必须列出将失去的 intent operation capability。用户可以显式确认物理删除；
  apply 复用既有 `MutationArtifactCleanupRequested` /
  `MutationArtifactLifecycleRecorded`，并追加对应 Intent conflict/read-only projection。
- artifact deleted/expired/corrupted 后，历史 title、source、digest 和 lifecycle 仍可审计，但
  drop/replace 永久返回 `artifact_unavailable`，不能从 Git diff、模型或旧 preview 重建 authority。
- `secret-like`、unsupported 或无法安全持久化的 bytes 从一开始就不能形成可执行 layer；
  不得为了保留 Intent 功能降低 RFC-0002 的敏感内容策略。
- intent dropped/superseded 后，artifact 可退出 protected set，但仍服从普通 retention horizon；
  cleanup 与 Intent projection 重建必须幂等。

### 9.7 Resume, fork, branch and worktree identity

- 同一 session resume 从 durable events 重建同一 stack/version，不重新解释 prompt。
- conversation fork 只继承 read-only provenance ref，不继承 mutation authority 或 active
  application state。用户必须显式 adopt 到新 stack/version，且 adoption 重新绑定当前 workspace
  snapshot；否则只能 inspect。
- Task worktree child 可继承 attempt-scoped intent binding，但 child layer 不进入 parent active
  stack；只有 parent promotion 后的 mutation/verification bridge 能激活。
- branch/workspace switch 后，旧 stack 默认 `out_of_scope`。只有 workspace id、branch lineage
  和 snapshot adoption 全部匹配时才可创建新 version；不得让相同 relative path 暗示 identity
  继承。
- Git merge、checkpoint restore 或人工 drift 改变已绑定 bytes 时，projection 追加 conflict/
  invalidation，不原地改写旧 binding。

## 10. Safety and trust boundaries

1. 只有受控普通文件 mutation 可以进入 V1 可写范围；shell、MCP、remote 和 unknown effects 永远只提示。
   shell receipt 本身仍是 `UnsupportedSideEffect`；但 shell 在 RFC-0005
   `IsolatedMutation` 或 RFC-0053 owned child workspace 中产生的普通文件 bytes，如果随后经过
   exact delta materialization、单 intent attempt binding、parent `WorkspaceApply` 与 RFC-0002
   evidence，可以由最终文件 layer 获得 ownership。该规则不把 shell 的 network/remote/global
   effect 伪装成已撤销。
2. protected path、secret-like content、symlink、directory、rename 和 unsupported artifact 沿用更严格的既有 policy。
3. renderer 只消费 bounded DTO；不得获得 snapshot 内容、absolute internal path、raw provider payload 或 mutation authority。
4. intent statement、title、model explanation 与 imported spec 都是不可信文本，不能触发工具或绕过 approval。
5. drop/replace 是新的写操作，必须遵守与普通编辑相同或更严格的 approval、lease 和 audit 要求。
6. 人工 drift、并行 agent overlap 或 changed dependency 一律返回结构化 conflict，不自动覆盖。
7. intent labels 不是 security boundary；真正的 authorization 仍来自 runtime policy 和 exact evidence。

## 11. Product surface

Intent Stack 应是 TUI-first 的高层 review surface，同时保持 adapter-neutral projection。建议形态：

```text
Intent Stack · 3 changes

✓ Retry failed requests                 verified
  4 files · 3 tests · no dependency

● Add retry telemetry                   verified
  3 files · depends on Retry failed requests
  [Review] [Drop]

△ Document operations guidance          needs review
  2 files · shared paragraph
```

交互原则：

- 默认只展示 intent、状态、依赖、影响规模和一个主动作；
- 展开后才显示 files/hunks/evidence，不复制完整 Git client；
- destructive action 始终先进入 exact preview 和明确确认；
- conflict card 必须给出“为什么不能自动做”和可行的人工处理入口；
- 简单单意图任务不增加额外仪式；
- TUI 首先承载该能力，desktop 等 surface 后续只能复用同一 projection/command contract。

## 12. Execution slices

本 RFC 保存后保持 implementation deferred。未来只有在单独排期并通过 contract review 后，才按以下顺序实施：

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| R51.0 | 冻结 domain/event/DTO schema、canonical digest、operation error taxonomy 与 fixture corpus | contract review 把 §15 已锁定决策逐项编码为 golden schema/digest fixtures；无代码行为变化 |
| R51.1 | IntentPlan admission、`UserDeclaredRoot`/`SuggestedDecomposition`、与 TaskPlan 同批接受、append-only projection | proposal 不能伪造 acceptance；batch crash/replay、schema/property、旧 session unavailable-state tests |
| R51.2 | `IntentExecutionBinding` 的 Task/Chat origin、attempt/ChangeSet/parent mutation/verification bridge、criterion evidence level | lineage 缺口降级 read-only；Chat direct-file mutation projection；Task replan/WorkspaceApply；GitRef-only negative fixture；stale parent receipt；Advisory/SystemVerified fixtures |
| R51.3 | canonical layer materializer、content-addressed artifact、file-exclusive ownership、retention protection 与 inspect projection | cross-intent same-file 一律 shared；exclusive/shared/unowned/drift/formatter/codegen/secret/cleanup/delete/restart fixtures |
| R51.4 | exact drop preview、RFC-0002 batch apply、operation recovery 与 verification invalidation | CAS、lease、all-file preflight、partial commit、terminal-event repair、no-replay、checkpoint-conflict tests |
| R51.5 | dependency closure、revise/replace 的 read-only impact preview 与 fork/branch/worktree adoption | DAG cycle/leaf/supersede/replan/out-of-scope/adoption tests；仍不开放 replace mutation |
| R51.6 | TUI Intent Stack review/drop/retention/conflict surface | render/input/narrow-layout/mouse/session-switch/restart/stale-request tests；一个清晰主动作 |
| R51.7 | typed HTTP/Desktop/automation adapter 与 canonical dogfood | same-command/projection conformance；real worker-loop E2E；§13 全门槛；无 P1/P2 |

依赖顺序：

```text
R51.0 -> R51.1 -> R51.2 -> R51.3 -> R51.4 -> R51.6 -> R51.7
                           \-> R51.5 -----------/
```

R51.4 是首个“意图级版本控制”可执行里程碑。R51.1-R51.3 只有接受、追踪和
read-only review 价值，不能单独宣传已经支持按意图撤销。Isolated regeneration、
replace/rebase mutation 不再混入 V1 active slices；它们必须在 R51.4 dogfood 后以独立安全 RFC
冻结 merge/overlap/human-drift 语义。

## 13. Acceptance gates

- 同一 turn 内两个无依赖 intent 修改不同文件时，可独立 review，并安全 drop 其中一个。
- 两个 active intent 修改同一文件时，即使 non-overlapping 也稳定投影为 shared/read-only；
  V1 不声称可以单独 drop。
- 用户原始单一明确目标可形成 `UserDeclaredRoot`；模型建议的多 intent 分解在用户接受前不产生
  accepted plan，IntentPlan/TaskPlan 的一次接受不会出现只有一半 durable 的状态。
- 每个可执行 layer 都能沿对应 Task 或 Chat execution chain 追溯到 exact attempt、controlled
  mutation/ChangeSet 与 parent verification；任一适用 identity 缺失只允许 read-only review。
- 单点局部修改保持 Chat 时仍可形成一个 `UserDeclaredRoot` layer，不为 Intent Stack 强制建
  Task；同一 Chat run 中未经接受的模型子目标不能伪装成独立可 drop intent。
- 带 executable intent binding 的 Task 选择 ref-only promotion 时在首个 effect 前拒绝；它不能
  绕过 parent workspace mutation evidence 获得 applied/verified 状态。
- shared hunk、unowned edit、人工 drift、missing artifact 或 stale digest 在首个写入前 fail closed。
- 被 downstream intent 依赖的目标不可 drop；V1 不自动扩大到依赖闭包。
- drop 产生新的 append-only mutation/operation evidence；旧 stack/version/history 不被改写。
- drop 后相关 verification receipt 变 stale，并按 policy 重跑后才恢复 verified。
- crash/restart 能区分 requested、rejected、cancelled、prepared、partially applied、committed 与
  conflicted operation。
- batch 已完整 commit 但 terminal event 缺失时只做证据驱动的幂等收口；未知状态不会重发写入。
- intent-drop mutation 不会被普通 checkpoint restore 成功改写后仍留下错误的 `dropped` 状态。
- cleanup 不会静默删除仍支持 drop 的 artifact；用户显式删除后，历史仍可审计且 operation
  永久、可解释地降级为 read-only。
- resume 保持 identity；fork、branch/worktree switch 在显式 adopt 前不继承 mutation authority。
- `SystemVerified` criterion 必须绑定当前 parent snapshot 上 policy-qualified receipt；模型关联只能
  是 `Advisory`。
- TUI 不把 unsupported shell/remote side effect 描述为已撤销。
- provider、renderer 或模型文本不能伪造 intent admission、artifact ownership 或 successful operation。
- 现有 checkpoint、task DAG、write isolation 和普通 Git diff workflow 不回退。

## 14. Canonical product demo

首个 dogfood demo 固定为：

1. 用户要求“为 API 请求增加重试，同时添加重试遥测并更新运维文档”。
2. Sigil 提议并记录三个 intent：Retry、Telemetry、Operations docs，以及依赖关系。
3. 实现和验证后，review 按三个 intent 聚合跨文件贡献。
4. 用户选择 drop Telemetry。
5. Sigil 预览将删除的代码和测试，指出受影响的文档段落；若段落与 Retry 共享则阻止自动删除或要求先拆分。
6. 确认后只移除已证明的 Telemetry 独占贡献，保留 Retry，实现新的 mutation evidence，并重新运行相关 verification。

该 demo 只有在第 5 步的 shared/drift 情况能够稳定 fail closed 时才算通过；“大多数时候模型猜对”不构成验收证据。

## 15. Locked decisions and deferred boundaries

为使 R51.0 可以直接执行，本 RFC 冻结原开放问题：

1. Intent acceptance 使用独立 typed event；Task 路径与对应 `TaskPlanAccepted` 由同一个 durable
   writer batch 提交，Chat 路径在首个写入前绑定 root logical run/source turn。任一适用 identity
   缺失都不进入 executable projection。
2. V1 hunk identity 使用 §7.2 的 layer-local canonical byte diff。formatter/codegen 改变完整文件
   digest 后标记 drifted，不跨运行 fuzzy 迁移。
3. 同一 normalized file 服务多个 active intent 时一律 shared；V1 不支持 retained-layer
   rematerialization 或 composition artifact。
4. criterion evidence 按 §9.1.1-§9.1.2 分为 Advisory/SystemVerified；只有 verification policy 和当前
   parent snapshot receipt 可以产生 SystemVerified。
5. resume/fork/worktree/branch 采用 §9.7：resume 重放，其他边界在显式 adopt/rebind 前只读。
6. artifact 被显式或 policy cleanup 删除后，依赖它的 drop/replace 永久降级 read-only；不得重建
   authority。
7. 本 RFC 的 adapter-neutral contract 只泛化 coding artifact/operation；非 coding Outcome/Case
   由后续 RFC 建立上层抽象，不复用文件 mutation authority。

以下边界有意延后，不阻塞 R51.0-R51.7：

- shared hunk 的语义拆分、composition artifact 与任意历史 Git attribution；
- isolated regeneration、replace/rebase mutation 和自动处理 human drift；
- shell、network、MCP、数据库、发布等外部副作用的撤销/补偿；
- 跨设备 intent sync 与非 coding Outcome/Case version control。

## 16. Decision and deferral

本 RFC 选择：

- 建立 Intent Stack 作为用户可感知的语义版本层；
- 以 append-only intent plan、artifact ownership、exact preview 和既有 mutation/verification contract 作为可信基础；
- 首版只支持归属明确、非重叠、受控普通文件改动的 intent drop；
- 对共享贡献、外部副作用、人工 drift 与复杂 replace/rebase 保持 fail closed；
- 先把 review/provenance 做对，再开放 destructive operation。

完成 R51.0-R51.7 后，Intent Stack V1 的 accepted-plan、provenance、read-only review 与安全
selective drop 闭环完成；它仍不等于通用语义 merge、外部副作用补偿或项目级长期记忆。

本 RFC 当前仍为 `proposed / implementation deferred`。本次变更不新增代码、事件 schema、
命令、UI 或用户文档入口；后续实施必须由新的明确任务启动。R51.0 只需重新核对当时的 live
contracts、冻结具体 Rust/DTO 命名与 golden fixtures，不再重新打开 §15 已冻结的产品语义。
