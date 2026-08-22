# RFC-0067：Single Execution Spine and Monotonic Plan-to-Task Adoption V1

状态：部分实施（qualification pending；§18 的 real-model / fault-campaign / cross-surface E2E gate 尚未完成）

> RFC-0069 取代本 RFC 对 `DraftReady`、前置完整 candidate compile、单条
> `PlanExecutionAdoptedV1` 与 model-generated Task DAG/materialization 的规范性要求。自 RFC-0069 起，
> `DraftReady` 仅表示可审阅的 durable Plan artifact；用户批准原子创建稳定 Task identity和 first-class
> `TaskDirectExecutionAdmittedV1`，runner立即启动，不创建单步 `TaskPlan` 或 `TaskStep`。model planner/DAG仅可作为
> 普通 Task的可选优化，失败必须降级到同一 direct admission。本 RFC
> 其余关于单一 product handoff、单调 approval/task identity、stale command 和 legacy replay
> 的约束仍有效；相冲突段落仅作为演进历史阅读。

创建日期：2026-08-20

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- [RFC-0018 Plan-to-Task Handoff](0018-plan-to-task-handoff.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0063 Automatic Plan Review and Default AI Orchestration V1](0063-automatic-plan-review-and-default-ai-orchestration-v1.md)
- [RFC-0066 Durable Task Execution Contracts V2](0066-durable-task-execution-contracts-v2.md)

## 1. 摘要

Sigil 当前把 Plan review、Task、权限、验证、恢复和多 agent 编排都建成了一等领域能力；这些能力本身
不应被删除。真正的问题是：**从 `DraftReady` Plan 到 durable Task 之间仍存在一段可失败的二次语义
转换和多段提交**。

当前 `Run` 路径会在用户已经看到“Plan ready”之后，才重新执行以下工作：

- 读取和比较 workspace snapshot；
- 派生 task id、objective 和展示标题；
- 把 Plan step 再转换为 Task plan、step contract 和 intent admission；
- 重新验证 step 数量、DAG、role、mode、isolation、capability 和 intent alias；
- 生成可选 permission grant；
- 逐段写入 TaskRun、TaskPlan、contract sidecar、permission、TaskCreatedFromPlan 和 Accepted decision；
- 最后再启动 Task runtime。

因此，当前 `DraftReady` 实际只意味着“Plan 文档可展示”，不意味着“Plan 已可执行”。用户按下 Run 后，
仍可能因为展示字段、schema、workspace 漂移、资源、registry、持久化或中间 crash 而得到：

- Plan 已消失但 Task 未创建；
- Task prefix 已写一半但没有 final commit；
- Task 创建失败，只剩瞬态提示；
- 环境问题发生在 Task identity 之前，无法用 Task 的 paused/blocked/retry 契约恢复；
- Desktop、TUI、HTTP 在不同入口重复组合同一 handoff 语义。

本 RFC 冻结一个新的核心不变量：

> `DraftReady` 必须表示一个完整、规范化、内容寻址、可被无外部副作用采纳的
> `ExecutablePlanCandidateV1` 已经 durable commit。用户触发 Run 后只允许执行一次 typed CAS 和一次
> crash-safe adoption commit；workspace、provider、tool、credential、permission 和资源检查都在 Task
> identity 已经存在后进入统一 admission，并以 `Ready`、`Blocked` 或 `Paused` 收口。

这条链称为 **Single Execution Spine（单一执行脊柱）**：

```text
semantic route
  -> Plan review
  -> executable candidate compile
  -> PlanReady commit
  -> user Run command
  -> atomic Task adoption
  -> runtime admission
  -> task/step/tool execution
  -> checkpoint
  -> terminal settlement
```

任何产品表面都只能驱动这条链，不能另建 TUI-only、Desktop-only、HTTP-only 或兼容 planner fallback。

## 2. 问题陈述

### 2.1 当前 `DraftReady` 的承诺太弱

`submit_plan_draft` 当前能 strict-validate model-visible Plan schema，并将 Plan draft 与
`PlanReviewAttemptStatus::DraftReady` 一起写入 parent session。这个边界保证 Plan 文档合法，但不保证：

- `task_plan_from_plan_draft` 一定成功；
- task title、step display name、intent aliases 和 contract set 一定可 materialize；
- 当前 `task.max_plan_steps`、role registry 和 capability baseline 与 draft 相容；
- handoff 需要的所有 durable records 能作为一个提交单元写入；
- Run 之后一定先得到 Task identity，再做资源和环境检查。

因此 `DraftReady` 仍允许“先让用户相信可执行，后在 Run 时发现不可执行”的时间差。

### 2.2 当前 `Run` 承担了四种不相容职责

一个用户动作同时承担：

1. **语义编译**：Plan -> Task plan / step contracts / intents；
2. **权威采纳**：用户接受 exact plan hash；
3. **环境 admission**：workspace、资源、provider、tool 和 permission 是否可用；
4. **执行启动**：创建 Task run 并调度第一个 participant。

这四类工作失败语义不同，却共享一个同步调用。结果是任何局部失败都可能被误报成“Run 失败”，而不是
一个已存在 Task 的可恢复 blocker。

### 2.3 多段 append 只能幂等对账，不能提供单一事实

当前 promotion 通过 deterministic task id 和 prefix reconciliation 减少重复创建，但仍会逐段 append：

```text
TaskRun
  -> TaskPlan + TaskStepContractV2 + contract marker
  -> optional PlanPermissionGranted
  -> TaskCreatedFromPlan
  -> PlanDecisionRecorded(Accepted)
```

这是一条可修复的多记录协议，不是一个原子 adoption 事实。每个 crash window 都要求专门的 reconcile
分支；新增 Intent、capability、verification 或 permission sidecar 时又会扩展窗口。

### 2.4 环境失败发生得太早

workspace drift、磁盘不足、provider credential 缺失、tool registry 不满足、外部 writer 占用等都是
执行环境事实。它们可能在用户接受前后变化，不适合被编译进 Plan 的“永远可用”承诺，也不应阻止 Task
identity 的建立。

正确语义应是：

```text
Task adopted
  -> admission observes current environment
  -> Ready | Blocked(actionable reason) | Paused
```

而不是：

```text
environment check fails
  -> no Task exists
  -> user只能重新描述目标或重做 Plan
```

### 2.5 “继续执行”不能依赖 host 侧短语匹配

自然语言是否授权执行 pending Plan，继续由模型在受限 typed routing microturn 中选择
`run_pending_plan | keep_pending_plan`，host 只校验 exact plan id/hash/status。显式 UI 的 Run/Enter、
HTTP command 或 Desktop action 本身是 typed command，不需要再理解 prompt。

本 RFC 禁止为了绕过 handoff 问题新增“继续”“run”“执行方案”等 host-side phrase table。

## 3. 设计目标

V1 必须达成：

1. `DraftReady` 与“可生成 durable Task”成为同一个可测试承诺。
2. Run 不调用模型、不读取 workspace、不解析 Plan、不访问 tool registry、不启动 child。
3. Run 成功只产生一个 atomic `PlanExecutionAdoptedV1` durable authority。
4. Run 被接受后，Task identity、semantic title、accepted plan、step contracts 和 intent lineage 同时存在。
5. 环境、权限、资源和 route 问题进入 Task admission，并产生可恢复的 `Blocked` / `Paused`。
6. Task、step、participant、provider attempt 和 tool call 的 started 状态最终都必须有 terminal settlement。
7. TUI、Desktop、HTTP、CLI automation 复用同一 application service 和同一 command receipt。
8. compact、resume、session switch、process crash 后从 durable control state 恢复，不重新解释用户 prompt。
9. Plan revision 在新 candidate commit 前不覆盖当前可审阅 Plan；candidate commit 后才原子替换。
10. 现有 approval、sandbox、workspace trust、Intent、verification 和 isolation 安全边界不被削弱。
11. 真实模型 acceptance 必须验证最终 world state，而不是只检查 assistant 宣称成功。

## 4. 非目标

V1 不做：

- 不合并 Plan review 和 Task；Plan 仍是待审阅 artifact，Task 仍是唯一执行 engine；
- 不把 DeepSeek、Gemini、Claude 或 Codex 私有字段加入 `sigil-kernel`；
- 不让 Plan acceptance 自动批准 shell、network、MCP、external directory、merge 或发布；
- 不在 Plan compile 时冻结短生命周期 invocation grant；
- 不假设 DraftReady 之后 workspace、provider、credential、tool 或磁盘永不变化；
- 不把所有失败都重试；可能产生副作用且结果未知的 attempt 仍 fail closed；
- 不从 Markdown、assistant prose 或用户短语猜 task step、permission 或 command；
- 不通过 renderer-local optimistic state 冒充 durable Task；
- 不为了减少代码量删除 RFC-0066 的 step contracts、capability admission 或 no-progress 约束；
- 不承诺竞品实现不存在缺陷；竞品仅提供可验证的设计参照。

## 5. 竞品与外部调研

本节基于 2026-08-20 的本地 clone 快照和官方一手资料。源码快照：

| 项目 | 本地 commit | 重点文件 |
| --- | --- | --- |
| DeepSeek Harness | `47f943859bef` | `agent.ts`、`tool-calls.ts`、`repair.ts`、real coding E2E |
| OpenAI Codex | `4808c162eeb7` | `core/src/tasks/mod.rs`、`thread-store`、app-server lifecycle |
| Gemini CLI | `ae0a3aa7b928` | `exit-plan-mode.ts`、A2A `executor.ts`、checkpointing |
| OpenCode | `884c25603395` (`dev`) | V2 `session/runner/llm.ts`、`publish-llm-event.ts` |

### 5.1 DeepSeek Harness：简单但完整的 execution loop

DeepSeek Harness 最值得学习的不是 plugin 数量，而是 loop contract：

- 每个 provider request 都从 session log 派生；
- `turn/start`、`step/start`、`step/end`、`turn/end` 由同一个 driver 持有；
- step 和 turn 的 terminal boundary 位于 `finally`，错误和 abort 也能结构化收口；
- tool call 可并发执行，但 call/result 按模型声明顺序 commit；
- abort 会 drain 已启动调用，并为未启动调用补 provider-valid error result；
- crash repair 会为 dangling tool call、step 和 turn 追加 deterministic closers；
- real-model E2E 在 agent 外重新运行测试，并检查测试文件未被篡改。

官方源码：

- [agent loop](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/agent-loop/src/agent.ts)
- [tool scheduler](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/agent-loop/src/tool-calls.ts)
- [crash repair](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/session/src/repair.ts)
- [real coding E2E](https://github.com/deepseek-ai/deepseek-harness/blob/master/examples/headless-agent/tests/coding-task.e2e.ts)

Harness 的 Plan mode 比 Sigil 简单：批准后切换 mode，并由同一 agent 在下一 step 继续；它没有 Sigil
当前完整的 durable Task DAG、Intent、step contract 和 integration authority。因此 Sigil 不应复制它的
领域模型，但应复制它的**单 owner lifecycle、ordered commit、crash closure 和外部 world-state 验证**。

### 5.2 OpenAI Codex：turn/item 生命周期是产品协议

Codex app-server 将 thread、turn 和 item 作为公开 typed lifecycle：turn 以 `turn/started` 开始，以
`turn/completed` 结束，最终状态明确为 completed/interrupted/failed；item 也遵循
`item/started -> deltas -> item/completed`。恢复使用同一个 thread id 继续 append。

本地源码进一步显示：task body 结束后统一生成 `TurnComplete` / `TurnAborted`，terminal event append 后
显式 flush rollout；abort 会先让 task 观察 cancellation，再落 interrupted marker 和 terminal event。

官方资料：

- [Codex app-server lifecycle](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [Codex task terminal settlement](https://github.com/openai/codex/blob/main/codex-rs/core/src/tasks/mod.rs)

需要学习的是 terminal lifecycle 成为协议，而不是 renderer 猜测状态。但 terminal record 本身只证明
流程已收口，不证明任务 outcome 正确；因此 Sigil 的 terminal closure 必须再叠加
outcome/readiness 断言和 real E2E。

### 5.3 Gemini CLI：Plan approval 与执行切换短而明确

Gemini CLI Plan mode 是只读环境；`exit_plan_mode` 完成 Plan 校验和用户批准后切换 approval mode，记录
approved plan path，并让同一 loop 从下一 step 执行。其 A2A executor 则先创建并发布 task，再进入主
执行循环；异常转成 failed，`finally` 保存最终状态并清理 terminal task。

Gemini 的 checkpointing 会在文件修改工具执行前保存 Git snapshot、conversation 和 tool call。这个设计
证明 mutation safety 应贴近 effect boundary，而不应塞进 Plan-to-Task adoption。

官方资料：

- [Gemini CLI Plan Mode](https://geminicli.com/docs/cli/plan-mode/)
- [Gemini CLI Checkpointing](https://geminicli.com/docs/cli/checkpointing/)
- [Gemini CLI planning tools](https://geminicli.com/docs/reference/tools/)

Sigil 应学习“批准后不再二次理解 Plan”和“Task 先存在再执行”，但不采用 plan file path 作为唯一
authority；Sigil 仍使用内容寻址的 durable candidate。

### 5.4 Claude Code：session continuity、Plan mode 与 checkpoint 分层

Claude Code 官方文档把三个概念分开：

- session JSONL 持久化 message、tool use 和 result，resume 在同一 session id 继续 append；
- Plan mode 只允许只读探索，用户批准后才进入执行；
- checkpoint 在每次 edit 前捕获代码状态，可跨 resumed session 使用。

官方资料：

- [How Claude Code works](https://code.claude.com/docs/en/how-claude-code-works)
- [Claude Code checkpointing](https://code.claude.com/docs/en/checkpointing)
- [Claude Code permission modes](https://code.claude.com/docs/en/permission-modes)

其 checkpoint 不覆盖 Bash 或外部系统副作用，这一限制反而说明 Sigil 仍需要 RFC-0002/RFC-0003 的
mutation evidence、unknown-outcome 和 verification 语义，不能把“有 checkpoint”当成任意重试许可。

### 5.5 OpenCode V2：有价值的局部契约，也暴露未完成项

OpenCode V2 runner 已做到：

- 单 session 本地 active drain；
- tool call 在 effect 前记录；
- success/failure/provider-executed outcome typed persistence；
- provider 或 cancellation 失败时结算 unsettled tools；
- local tools 结束后从 durable history 开下一 provider turn。

但源码 TODO 仍明确包含 durable busy/retrying/idle/terminal-failure、interruption recovery、bounded retry
等未完成项。这个对照说明：**局部 tool pairing 正确，不等于完整任务可恢复**。Sigil 的 spine 必须覆盖
Plan adoption、Task admission、step/participant、tool 和 final synthesis 全链，而不是只修 agent loop。

### 5.6 Durable execution 的通用参照

Temporal 将 crash-proof execution 的核心描述为：运行状态由 durable history 恢复，进程或基础设施失败后
从中断处继续。Sigil 不引入 Temporal 依赖，但采用同一原则：process-local future 不是 authority，event
history 和版本化 reducer 才是 authority。

参考：[Temporal Platform Documentation](https://docs.temporal.io/)。

### 5.7 调研结论

竞品并不证明“功能越少越好”，而是共同证明：

1. 每个开始状态都必须有同 owner 的 terminal closure；
2. 计划批准后不应再次由模型或 host 从 prose 推导执行语义；
3. Task/session identity 要在执行和环境检查前存在；
4. 工具并发可以复杂，但 durable commit 顺序必须确定；
5. crash repair 要补齐协议结构，不能只恢复 UI；
6. checkpoint、permission、admission、execution 是不同平面；
7. “模型说完成”必须由外部 world state 验证。

## 6. 核心设计原则

### 6.1 Ready means adoptable

公开状态 `DraftReady` 只允许在以下事实同时 durable 后出现：

```text
validated PlanDraft
+ ExecutablePlanCandidateV1
+ candidate hash
+ source attempt terminal
+ PlanReadyCommittedV1 marker
```

如果只有 Plan 文档而 candidate compile 失败，状态是 `InvalidDraft` 或 `CompileFailed`，不是
`DraftReady`。产品面应直接展示具体、可行动的原因。

### 6.2 Run is commit-only

Run 允许：

- 校验 command/session/plan/candidate identity；
- 校验 expected frontier 和 Plan 当前仍可 Run；
- 选择 `StartImmediately | CreatePaused`；
- 绑定用户在同一 action 中显式选择的 permission option；
- 生成 command receipt、timestamp 和单次 adoption event；
- 进行一次 crash-safe append + sync。

Run 禁止：

- provider/model 调用；
- filesystem/workspace snapshot 读取；
- tool/MCP/agent registry 枚举；
- 从 Plan prose 重新生成 task step；
- DAG、role、mode、isolation、display name 或 capability 二次规范化；
- 启动 child、process、terminal 或 network；
- 在多个产品 adapter 中分别构造 promotion prefix。

除 session writer 不可用、CAS stale、candidate 损坏或 authority conflict 外，Run 不应失败。

### 6.3 Adopt first, admit second

用户批准建立的是“执行这个 exact candidate”的 durable authority，不是“当前环境已经满足”。Task adoption
成功后，统一 admission service 才观察：

- workspace current snapshot 和 audited mutation frontier；
- provider/model route 和 credential；
- exact agent/tool/MCP registry capability；
- permission、sandbox、workspace trust 和 external-directory policy；
- active writer、worktree/isolation 和 process capability；
- 磁盘、artifact/session reserve、内存和并发预算；
- verification/check runner 可用性。

admission 结果必须是 durable typed state，不能只抛 `anyhow::Error`。

### 6.4 Monotonic progress

“单调前进”不表示 Task 永远不能重试，而是：

- Plan identity/hash/candidate 一旦 adopted，不回退成“Task 不存在”；
- Task plan version 只增不减；
- participant attempt ordinal 只增不减；
- blocker 被解决后追加 `Resolved` / 新 admission，不重写旧 blocker；
- 已完成且 contract 相同的 step 不因 resume/compact 被重新标成 pending；
- crash 只能追加 repair/interrupt/blocked 事实，不能删除已经承认的 effect；
- terminal Task 不再被普通 conversation 输入隐式复活。

### 6.5 One application service

所有入口统一调用：

```rust
trait PlanExecutionService {
    fn adopt(
        &self,
        session: &mut Session,
        command: PlanRunCommandV1,
    ) -> Result<PlanRunReceiptV1>;
}
```

TUI Enter、鼠标 Run、Desktop IPC、HTTP command 和 model-selected `run_pending_plan` 只能构造 typed
`PlanRunCommandV1`；它们不能触碰 promotion 细节。

## 7. `ExecutablePlanCandidateV1`

### 7.1 数据模型

```rust
pub struct ExecutablePlanCandidateV1 {
    pub schema_version: u16,
    pub compiler_version: u16,
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub candidate_hash: String,
    pub task_id: TaskId,
    pub semantic_title: String,
    pub safe_objective: String,
    pub task_plan: TaskPlanEntry,
    pub step_contracts: Vec<TaskStepContractBoundEntryV2>,
    pub contract_set_digest: String,
    pub step_mapping: Vec<PlanToTaskStepMapping>,
    pub prepared_intent_admission: Option<PreparedIntentAdmissionV1>,
    pub permission_scope_candidate: Option<PlanPermissionScopeCandidateV1>,
    pub required_capabilities: Vec<TaskCapabilityV2>,
    pub compile_binding: PlanCompileBindingV1,
}
```

`candidate_hash` 对 canonical provider-neutral payload 计算，排除：

- timestamp；
- command/request UUID；
- process-local grant；
- current provider credential；
- current registry instance identity；
- volatile workspace availability；
- UI selection/focus。

### 7.2 Compile binding

```rust
pub struct PlanCompileBindingV1 {
    pub source_attempt_id: String,
    pub source_turn_id: String,
    pub task_config_contract_hash: String,
    pub planner_schema_hash: String,
    pub task_contract_schema_hash: String,
    pub intent_schema_hash: Option<String>,
    pub base_workspace_snapshot_id: Option<String>,
}
```

这些字段证明 candidate 由哪个 current contract 编译；它们不是 runtime permission。

### 7.3 Compiler 职责

Plan compiler 必须在 `DraftReady` 前完成：

1. `SafePersist` 和大小上限；
2. semantic task title 与 bounded presentation fields；
3. stable task/step id；
4. DAG、dependency、step count；
5. role、mode、isolation 组合；
6. host baseline 与 model-required capabilities 的并集；
7. `TaskStepContractV2`、check refs、deliverables、acceptance criteria；
8. verify participant 禁止规则；
9. intent proposal/alias 的内容绑定预编译；
10. target path 的语法和 workspace-relative 规范；
11. permission scope candidate；
12. canonical serialization 和 digest；
13. 从 candidate 重新 materialize 后的 self-check。

Compiler 是纯、确定性组件；需要读取 workspace 的 evidence 必须由 Plan review research 阶段先捕获为
输入。Compiler 不读取“当前环境是否仍可执行”。

### 7.4 Intent authority

用户批准前不能创建生效的 Intent acceptance authority。Candidate 只保存已经验证、内容寻址的
`PreparedIntentAdmissionV1`。`PlanExecutionAdoptedV1` 同时绑定 user command receipt 和 prepared digest，
由 reducer 激活 Intent lineage。

激活过程必须是预先证明不会失败的纯 materialization；任何可能失败的 intent validation 都必须在
candidate compile 阶段完成。

### 7.5 Permission scope

Candidate 可证明哪些 path/effect 有资格获得 plan-scoped grant，但不自动生成 grant。Run command 可以
选择：

```rust
pub enum PlanRunPermissionChoiceV1 {
    KeepCurrentPolicy,
    GrantScopedEditsOnce,
}
```

Adoption event 只在 candidate 已证明 scope 非空且合法时接受第二个选项。该 grant 仍不能覆盖 sandbox、
protected path、network、MCP、external directory、secret egress、merge 或发布策略。

## 8. Plan ready 的原子提交

### 8.1 新 durable records

```rust
ControlEntry::ExecutablePlanCandidatePreparedV1(candidate)

ControlEntry::PlanReadyCommittedV1 {
    plan_id,
    plan_hash,
    candidate_hash,
    attempt_id,
    committed_at_ms,
}
```

推荐 ordered batch：

```text
PlanDraftCreated
  -> ExecutablePlanCandidatePreparedV1
  -> PlanReadyCommittedV1   // final marker
```

`PlanReadyCommittedV1` 同时是 PlanReview attempt 的 terminal authority；projection 不再需要另一个可能漂移
的 `PlanReviewAttempt(DraftReady)` 作为决定性事实。

### 8.2 Crash 语义

- crash 在 candidate 前：Plan review attempt 仍 open，按现有规则收口 Interrupted；
- crash 在 candidate 后、marker 前：candidate 不可见为 Ready，recovery 可按 exact hash 重试同一 batch；
- marker durable 后：`DraftReady` 必须可投影，Run 不再编译；
- draft/candidate/marker 任一 identity 不匹配：session corruption，fail closed 并由 Doctor 报告；
- 不允许把缺 marker 的 prefix 猜成 Ready。

### 8.3 Revision

Revision 使用新的 plan id/hash/candidate。旧 Plan 保持可操作，直到新 `PlanReadyCommittedV1` durable；
新 marker append 后才用 `RevisionSucceeded` 原子切换 active Plan。revision failure 不能破坏 base Plan。

## 9. Run 与 Task adoption

### 9.1 Typed command

```rust
pub struct PlanRunCommandV1 {
    pub command_id: String,
    pub session_id: String,
    pub plan_id: PlanId,
    pub expected_plan_hash: String,
    pub expected_candidate_hash: String,
    pub expected_durable_frontier: u64,
    pub start_mode: PlanTaskStartMode,
    pub permission: PlanRunPermissionChoiceV1,
    pub source: PlanRunCommandSource,
}
```

`source` 只用于审计，可为 TUI keyboard、TUI mouse、Desktop、HTTP、CLI 或 model typed route；它不改变
domain behavior。

### 9.2 单一 adoption event

```rust
ControlEntry::PlanExecutionAdoptedV1 {
    command_id,
    plan_id,
    plan_hash,
    candidate_hash,
    task_id,
    task_title,
    start_mode,
    permission_grant,
    adopted_candidate,
    initial_phase: TaskExecutionPhaseV1::Preparing,
    adopted_at_ms,
}
```

该 event 是 Task identity、accepted plan、step contracts、intent activation、Plan decision 和 handoff link
的唯一 commit authority。Projector 可从它派生现有 public projections，但不能再要求随后多条 event 才
承认 Task 存在。

### 9.3 幂等与冲突

- 相同 `command_id + candidate_hash` 重试返回同一 receipt；
- 相同 candidate 已被另一个 command adopted，返回同一 task id 和 `already_adopted`；
- Plan 已 revised/rejected、candidate hash 不同或 frontier CAS stale，返回 typed rejection，Plan 保持当前
  可操作状态；
- append/sync 失败不能返回 accepted；
- persistence 成功但 response 丢失时，客户端用 command id 重试并读回 receipt；
- Task id 由 candidate 稳定派生，不按重试次数变化。

### 9.4 唯一允许“Run 后无 Task”的情况

只有以下情况可以不创建 Task：

- command 对 stale/rejected/revised Plan；
- candidate digest 或 session invariant 损坏；
- session writer 无法 durable append/sync；
- 同一 command identity 与另一 payload 冲突。

磁盘预算、workspace drift、provider、credential、tool、permission、network 或 verification 不可用，都必须
先创建 Task，再进入 Blocked/Paused。

## 10. Task lifecycle 与 admission

### 10.1 生命周期

```text
Adopted/Preparing
  -> Ready
  -> Running
  -> Completed

Preparing | Ready | Running
  -> Blocked
  -> Preparing (new admission attempt)

Preparing | Ready | Running | Blocked
  -> Paused
  -> Preparing (explicit resume)

Preparing | Ready | Running | Blocked | Paused
  -> Cancelled | Failed
```

`Blocked` 是可恢复状态；`Failed` 只表示 contract 损坏、不可恢复 side-effect ambiguity、hard safety 或
明确耗尽的 repair policy。普通资源和配置缺口不得直接 Failed。

### 10.2 Admission attempt

```rust
pub struct TaskAdmissionAttemptV1 {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub ordinal: u32,
    pub candidate_hash: String,
    pub observed_environment: TaskAdmissionObservationV1,
    pub outcome: TaskAdmissionOutcomeV1,
}

pub enum TaskAdmissionOutcomeV1 {
    Ready(TaskRuntimeLeaseBindingV1),
    Blocked(TaskBlockerV1),
    Paused(TaskPauseReasonV1),
}
```

每次 resume 或 relevant environment change 追加更高 ordinal；不覆盖历史 attempt。

### 10.3 Structured blockers

最低集合：

```text
workspace_changed
workspace_snapshot_unavailable
missing_required_capability
provider_unavailable
credential_unavailable
permission_required
workspace_trust_required
external_writer_active
isolation_unavailable
disk_space_exhausted
artifact_storage_unavailable
session_storage_degraded
verification_runner_unavailable
route_rebind_required
```

每个 blocker 必须包含：

- stable reason code；
- safe user-facing summary；
- affected step/capability；
- retryability；
- available typed actions；
- evidence digest；
- created/resolved timestamps。

### 10.4 Permission 和 danger-full-access

Admission 消费 effective permission profile，但不把 `network: deny` 之类 capability facet 当成已发生的
PermissionDenied。只有 exact tool invocation 的最终 policy decision 为 deny 才产生权限拒绝。

`danger-full-access` 仍不等于关闭所有 hard safety，但该 mode 已明确允许的 workspace/memory write 不得
重复进入 Ask。mode、tool plan 和 hard safety 的最终组合应由统一 permission planner 给 admission/runtime，
不能由 Task 再实现一套规则。

## 11. 统一执行脊柱

### 11.1 Ownership

每层只有一个 terminal owner：

| 层级 | Start authority | Terminal owner | 必需终态 |
| --- | --- | --- | --- |
| Plan review | `PlanReviewAttemptStarted` | PlanReviewCoordinator | Ready / WithoutDraft / Failed / Interrupted / Cancelled |
| Plan compile | child draft result | PlanCompiler | Prepared / CompileFailed |
| Task adoption | `PlanRunCommandV1` | PlanExecutionService | Adopted receipt / typed rejection |
| Admission | adopted Task | TaskAdmissionService | Ready / Blocked / Paused |
| Task run attempt | Ready + run command | TaskRuntime | Completed / Blocked / Paused / Failed / Interrupted / Cancelled |
| Step attempt | scheduler lease | Task orchestrator | Completed / Blocked / Failed / Interrupted / Cancelled |
| Participant | invocation grant | Agent supervisor | result / failed / interrupted / cancelled |
| Provider attempt | request Started | provider loop | completed / rejected / failed / interrupted / uncertain |
| Tool call | exact authorized call | tool scheduler | success / error / denied / interrupted / unknown outcome |

下层 terminal 必须在上层 terminal 前 durable 或被上层 recovery marker 明确覆盖。

### 11.2 Execution rules

1. Parent Task writer 是 plan/step/result 的唯一 durable commit owner。
2. Child future 不持有或写 parent Session。
3. 并行 body 可以乱序完成，terminal envelope 按 stable plan order commit。
4. cancellation 停止启动新 effect；已启动 effect drain/reconcile。
5. 未启动 tool call 获得 typed interrupted result。
6. side-effect outcome uncertain 时先观察外部状态，不盲目重放。
7. tool error 默认是模型可见输入；只有 unresolved blocker 才阻断 Task。
8. final synthesis 必须基于 accepted plan、step terminal、verification 和 readiness evidence。
9. 没有 final synthesis 时不能把“所有 tool future 已返回”当 Task Completed。

### 11.3 No hidden fallback

删除以下 fallback：

- DraftReady Plan 在 Run 时回退到 isolated planner；
- promotion 失败后把输入当普通 Chat 继续；
- Task missing 时从 plan prose 重建；
- resume 时从本地化 guidance 猜 `ResumeTask`；
- UI 收到 optimistic Run 后自行合成 task card；
- provider protocol failure 后在同一 logical attempt 透明重放。

若 candidate 不完整，它就不能进入 DraftReady；若 runtime 当前不可用，它就是 adopted Task 的 Blocked。

## 12. Recovery、compaction 与 session continuity

### 12.1 Startup recovery

recovery reducer 按顺序处理：

1. 修复/拒绝 torn JSONL tail；
2. 检查 Plan draft/candidate/ready marker 完整性；
3. 投影 `PlanExecutionAdoptedV1`；
4. adopted 但没有 admission terminal 的 Task 进入新的 admission attempt；
5. Running 但缺 terminal 的 step/participant/tool 按 effect evidence结算 Interrupted 或 UnknownOutcome；
6. Task 追加 Paused/Blocked，而不是回退成 pending Plan；
7. UI 从同一 projection 恢复 Plan、Task、steps、blockers 和 actions。

### 12.2 Compaction

Plan、candidate、Task、step contract、admission、checkpoint、verification 和 blocker 都是 control plane，
不受 provider-visible conversation compaction 删除或改写。Compaction 可以改变下一 request 的 model
context，但不能：

- 改 candidate hash；
- 丢 step status；
- 重置 attempt ordinal；
- 重新打开 terminal Task；
- 把 Blocked 伪装成 Pending；
- 让 UI 依赖 compaction summary 恢复 Task。

### 12.3 Session switch 与 attachment

有 active Task/participant/effect permit 时，session switch 必须先完成 quiescence 或返回 typed busy。
只读历史滚动、viewport resize、打开详情或 resume picker 不能触发 Task cancellation。UI lifecycle 与
Task cancellation owner 必须完全分离。

## 13. 产品语义

### 13.1 Plan Workbench

Plan 状态：

- `Preparing plan`：研究或 compile 未完成；
- `Ready to run`：candidate 和 ready marker 已 durable；
- `Needs changes`：compile 失败，展示具体 step/field/reason；
- `Stale`：Plan 被 revision/reject/supersede；
- `Running as task`：显示 semantic Task title 和 progress。

不显示内部 `plan-task-<hash>`、projection marker 或 “awaiting durable projection” 作为主信息。

### 13.2 Run interaction

用户按 Enter、点击 Run 或通过 typed API 提交后：

1. UI 等待 durable receipt；
2. receipt accepted 后立即展示 Task `Preparing`；
3. admission 通过后显示步骤进度；
4. admission blocked 后显示原因和可执行 action；
5. persistence/CAS rejection 保留 Plan Workbench 和 Run action。

不得出现“Run 被高亮、Enter 已按下但没有动作”的无 receipt 状态。

### 13.3 Task panel

默认展示：

- semantic task title；
- completed/running/blocked/pending 数量；
- 当前 step 和下一 action；
- blocker 的用户语言；
- pause/resume/retry/review actions。

Task terminal 后从 active panel 退出，保留在 session audit/history；后续普通对话不再长期携带完整 active
Task panel。

### 13.4 Surface parity

TUI、Desktop、HTTP 的 typed DTO 必须包含同一：

- plan id/hash/candidate hash；
- command id/receipt；
- task semantic title；
- phase/status；
- step projection；
- current blocker/actions；
- truncation/detail handle。

前端不得通过文本、tool name 或 session JSONL 局部扫描重建这些状态。

## 14. 安全模型

### 14.1 Authority separation

```text
model proposal
  != executable candidate
  != user adoption authority
  != runtime admission lease
  != tool execution permit
  != verification evidence
```

每层只能收窄或验证上层，不能扩大权限。

### 14.2 Workspace drift

Candidate 绑定 Plan research 时的 base snapshot，仅用于 scope/evidence。Adoption 不因当前 workspace read
失败而消失。Admission 获取 current snapshot：

- exact match：正常 Ready；
- 只有 audited self-mutation：按 frontier 规则继续；
- 外部未归因漂移：`Blocked(workspace_changed)`；
- snapshot unavailable：`Blocked(workspace_snapshot_unavailable)`。

用户可选择 typed re-admit、replan 或 cancel；host 不自动把旧 candidate 迁移到新 workspace。

### 14.3 Resource failure

磁盘、artifact store、provider quota/credential 等失败不得绕过安全，也不得抹掉 Task。资源检查失败先
写最小 blocker/checkpoint；session writer 使用既有 emergency reserve 尽力保证该 terminal fact durable。

### 14.4 Model semantic routing

pending Plan 的自然语言执行授权仍由模型调用 exact typed route tool。Host 只验证：

- 当前只有一个 exact runnable Plan；
- route receipt 绑定 source turn、plan id/hash/candidate hash；
- command 未被 supersede；
- Run action 当前仍允许。

禁止添加中英文 phrase/regex fallback。

## 15. Schema 与 cutover

### 15.1 Current schema

本 RFC 采用 current-schema clean cutover，不引入 alias、宽松 deserializer 或隐式迁移。

新的 `DraftReady` 必须有 `PlanReadyCommittedV1`。缺 candidate/marker 的旧 Plan 只能投影为
`LegacyPlanNeedsRecompile`，不能冒充 Ready。

### 15.2 显式 recompile

用户可对仍可读取的旧 structured Plan 选择 `Recompile plan`：

- 创建新的 Plan review/compile attempt；
- 重新 strict-validate current schema；
- 生成新的 candidate/hash/marker；
- 不猜测旧 sidecar；
- 不自动接受；
- 原 Plan 保留为历史证据。

已存在、已 accepted 的 durable Task 不回退到 Plan，也不要求重新 adoption；它继续按 RFC-0066 的 current
Task recovery contract 运行。

### 15.3 删除旧 promotion path

新链路完成 qualification 后删除：

- Run 时的 `task_plan_from_plan_draft`；
- Run 时的 compatibility isolated planner；
- 多段 Task prefix reconcile；
- renderer 或 adapter 自建 handoff；
- `TaskCreationFailed` 作为普通环境问题的 catch-all。

`TaskCreationFailed` 只保留为旧历史记录的 replay 语义，不再由 current adoption 生成。

## 16. 实施分期

### R67.0：观测与 invariant companion

- 为当前链路记录 PlanReady->Run->TaskCreated 每个阶段和 failure reason；
- Doctor 检查 DraftReady 是否可 promotion、Task prefix 是否 incomplete；
- 冻结真实失败 session 的 sanitized replay fixtures；
- 增加 `Run` 中 provider/fs/registry access 的测试 spy。

退出条件：能量化当前 failure windows，且 fixture 可稳定复现。

### R67.1：Plan compiler 与 candidate

- 新增 `ExecutablePlanCandidateV1` 和 canonical hash；
- 把 title、step、DAG、contract、intent 和 capability materialization 移到 compiler；
- 增加 compile self-check 和 property tests；
- compile failure 进入 typed Plan state。

退出条件：candidate round-trip 等价，任何 Ready draft 都能纯内存 materialize Task facts。

### R67.2：PlanReady marker 与 projection

- 新增 candidate/ready durable records；
- DraftReady 只从 final marker 投影；
- recovery/Doctor 处理 incomplete candidate prefix；
- Plan detail/public DTO 暴露 candidate identity 和 compile status。

退出条件：fault injection 在 batch 任意边界都不会投影虚假 Ready。

### R67.3：Atomic Task adoption

- 新增 `PlanRunCommandV1`、`PlanExecutionAdoptedV1`、receipt store；
- `PlanExecutionService` 成为唯一入口；
- reducer 从 adoption event 同时投影 Task、accepted plan、contracts、intent 和 plan decision；
- 删除 Run 时二次 compile 和多段 append。

退出条件：Run path 只有 CAS + 单次 append/sync，provider/fs/registry 调用为 0。

### R67.4：Runtime admission

- 新增 Preparing/Ready/Blocked/Paused projection；
- workspace、capability、provider、credential、permission、resource 检查统一进入 admission；
- blockers/actions typed 化；
- relevant environment change 和显式 retry 产生新 attempt ordinal。

退出条件：所有可恢复环境故障都留下 Task，修复环境后可继续同一 Task。

### R67.5：Execution spine closure

- 将 task/step/participant/provider/tool terminal owner 统一到表 11.1；
- crash repair 覆盖每层 dangling started；
- final synthesis/readiness 成为 Task Completed 的必要条件；
- unresolved blocker 与历史 recovered warning 分离。

退出条件：fault injection 后不存在未解释的 started record 或 silent terminal。

### R67.6：产品表面统一

- TUI keyboard/mouse、Desktop、HTTP 接入同一 command/receipt；
- Task `Preparing`、blocker、retry/replan/cancel UX；
- semantic title 和步骤进度；
- terminal Task 自动离开 active panel；
- resize/history/resume 不影响 Task cancellation owner。

退出条件：三表面 projection contract tests 和真实 PTY/Desktop E2E 通过。

### R67.7：Qualification 与旧路径删除

- deterministic replay/fault campaigns；
- real DeepSeek、OpenAI-compatible、Anthropic/Gemini route acceptance；
- real workspace world-state E2E；
- 删除 compatibility promotion 和 current-schema 旧分支；
- 更新主架构、用户文档、Doctor 和 changelog。

退出条件：§18 全部 gate 通过后才能标记 implemented。

## 17. 代码落点

建议职责：

| 模块 | 变更 |
| --- | --- |
| `sigil-kernel/src/plan.rs` | candidate、compiler input/output、hash、ready marker domain type |
| `sigil-kernel/src/task.rs` | Preparing/Ready/Blocked/Paused phase、admission attempt、blocker |
| `sigil-kernel/src/session/entry.rs` | 新 current-schema control entries |
| `sigil-kernel/src/projection.rs` | PlanReady/adoption/admission/task reducer |
| `sigil-kernel/src/session/writer.rs` | recovery-critical adoption receipt 和 fault-injection seam |
| `sigil-runtime/src/plan_review_coordinator.rs` | draft commit 前 compiler；移除 Run-time promotion |
| `sigil-runtime/src/application_run/task_control.rs` | shared `PlanExecutionService` adapter |
| `sigil-runtime/src/agent_supervisor/task_runner.rs` | admission 后启动，统一 terminal settlement |
| `sigil-runtime/src/doctor/session.rs` | candidate/adoption/attempt closure audits |
| `sigil-tui/src/runner/*` | typed Run command/receipt；不构造 promotion |
| `sigil-tui/src/ui/plan_workbench.rs` | compile/ready/preparing/blocked UX |
| `sigil-http` / `sigil-desktop` | 同一 DTO、幂等 command receipt |
| model eval / PTY / Desktop E2E | route、adoption、resume、world-state qualification |

不要创建一个同时依赖 TUI、provider 和 persistence 的“大 handoff manager”。Domain type 在 kernel，compile
和 application orchestration 在 runtime，renderer 只投影。

## 18. 验收标准

### 18.1 Domain 与 property tests

1. 任意 `DraftReady` projection 必有 exact candidate 和 ready marker。
2. 任意 Ready candidate 都能在无 filesystem/provider/registry 的纯内存测试中 materialize adoption。
3. candidate canonical round-trip hash 稳定；map/order/timestamp/UUID 不造成 drift。
4. display name、Unicode、超长 summary、边界 step count、DAG cycle、unsupported role/mode 被 compile 阶段
   正确处理。
5. model-required capabilities 只能与 host baseline union，不能下调权限要求。
6. intent proposal 与 adoption authority digest 精确绑定。

### 18.2 Atomicity 与 recovery

7. Plan ready batch 每个 byte/event 边界的 crash injection 都不会产生虚假 Ready。
8. adoption append 前失败保留 Plan；append 后 response 丢失可用 command id 幂等读回 Task。
9. adoption event 后任意 crash 都能恢复同一 task id、plan version、contracts 和 intent lineage。
10. adopted 但 admission 未结算的 Task 在 startup 恢复为 Preparing/Blocked，不回退 pending Plan。
11. started step/participant/provider/tool 在 crash 后都有 terminal repair 或 unknown-outcome evidence。

### 18.3 Admission

12. workspace drift、missing tool/capability、credential 缺失、disk full、external writer 和 route rebind 都先
    创建 Task，再产生 typed blocker。
13. blocker 解决后同一 Task 以更高 admission ordinal 继续；completed step 不重复。
14. danger-full-access 下已允许的 memory/workspace write 不产生冗余 Ask。
15. `network: deny` facet 不被显示成 PermissionDenied。

### 18.4 产品与协议

16. TUI keyboard Enter、mouse Run、Desktop 和 HTTP 产生相同 command/receipt/domain events。
17. Run accepted 后必显示 semantic Task title 和 `Preparing`，不存在无动作空窗。
18. Plan/Task 内部 id 默认不作为标题。
19. compact、resume、resize、历史滚动、打开 modal 不丢 plan/steps，也不 cancel Task/child。
20. terminal Task 从 active panel 退出，但 audit/history 仍可读取。

### 18.5 Real E2E

21. key-gated real DeepSeek 测试执行不少于 6 次 provider request，预算上限由 runner 显式配置并记录。
22. real task 覆盖：read -> edit -> test -> failed test repair -> retest -> final synthesis。
23. agent 外部重新运行测试并校验受保护文件 byte-identical；assistant prose 不作为完成证据。
24. fault campaign 覆盖 provider disconnect、tool error、approval deny/alternate path、disk pressure、session
    append failure 和 process restart。
25. 同一 exact failure fixture 连续运行 20 次，不能出现 Task missing、silent failure、duplicate mutation 或
    unpaired tool result。

## 19. 指标与 release gate

最低 telemetry：

```text
plan_compile_success_ratio
plan_ready_without_candidate_count            == 0
plan_run_command_accepted_count
plan_run_adoption_success_ratio               == 100% excluding typed stale/conflict
plan_run_without_task_count                   == 0 after accepted receipt
task_admission_outcome{ready,blocked,paused}
task_blocker_count{reason}
task_started_without_terminal_count           == 0 after repair window
step_started_without_terminal_count           == 0 after repair window
tool_call_without_result_count                == 0 after repair window
task_world_state_acceptance_pass_ratio
plan_run_to_task_adopted_latency_ms
task_adopted_to_ready_or_blocked_latency_ms
```

Release 不能只看 unit test。必须同时满足：

- deterministic replay；
- append/fault injection；
- PTY/Desktop/application contract；
- exact-build real-model campaign；
- external world-state verification；
- Doctor 对真实 session 无 orphan/incomplete authority。

## 20. 被拒绝的方案

### 20.1 继续修每个 promotion error

拒绝。每增加一个 sidecar、权限或 contract，就增加新的 Run-time failure 和 crash window；局部补丁不能让
DraftReady 获得可执行语义。

### 20.2 删除 durable Task，完全复制 Harness 单 loop

拒绝。这样会丢失 Sigil 已建立的 DAG、step contract、capability、Intent、isolation、integration review、
cross-surface projection 和长任务恢复能力。正确方向是让这些能力汇入一个 spine，而不是删除它们。

### 20.3 把所有 validation 留在 Run，但包装成一个函数

拒绝。函数数量变少不改变失败时机；用户仍会在 Ready 后看到不可执行。

### 20.4 Run 前先做所有环境检查

拒绝。环境会变化，且检查本身会失败。它会继续制造“用户已接受但 Task 不存在”。

### 20.5 自动忽略 workspace/permission/resource drift

拒绝。可靠性不能通过放宽安全和覆盖外部变化获得；正确语义是 adopted Task 的 typed Blocked。

### 20.6 用 prompt 关键词保证进入正确入口

拒绝。生产 host 不得通过自然语言短语实现 semantic routing。模型选择 typed surface，host 校验 durable
authority。

### 20.7 只补 UI loading/error

拒绝。UI 可以让失败更可见，但不能修复 Task identity、atomicity、admission 和 recovery 缺口。

## 21. 风险与缓解

### 21.1 Adoption event 过大

Candidate 有 plan/contract 内容，必须受 max steps、per-field bytes、aggregate bytes 和 canonical serializer
上限约束。若超过上限，compile 失败，不在 Run 时才失败。

### 21.2 单 event 与既有 projector 重构成本

短期可让 reducer 从 `PlanExecutionAdoptedV1` 派生现有 internal views；不要同时生成一组旧 authority event，
否则会恢复双事实源。测试先覆盖 public projection parity，再删除旧 path。

### 21.3 Admission 变成新的“大总管”

Admission 只聚合 typed probes 和返回 outcome，不执行 tools、不修改 workspace、不生成 plan。每个 probe
保留原 crate owner，并通过 provider-neutral facts 汇合。

### 21.4 Blocked Task 堆积

产品面将 active blocker 与历史 Task 分开；用户可 retry/replan/cancel。Retention/GC 只清理 terminal 或
显式 abandoned Task，不删除仍有恢复 authority 的 Task。

### 21.5 Candidate 与 runtime contract 漂移

Candidate 绑定 schema/compiler/config contract hash。Runtime 若不再支持该 contract，不隐式迁移，Task
admission 进入 `Blocked(contract_recompile_required)`，由显式 replan/recompile 处理。

## 22. 最终决策

Sigil 保留 Plan review 与 durable Task 的分层，但修改它们之间的边界：

1. Plan 在进入 Ready 前完成 executable compile；
2. Ready 必须携带内容寻址 candidate；
3. Run 只做 typed CAS 和单次 atomic adoption；
4. Task identity 先于所有环境 admission；
5. 环境问题变成 Task blocker，不再让 Task 消失；
6. 所有执行层共享一个 monotonic lifecycle 和 terminal closure；
7. 所有表面共享一个 application service 和 command receipt；
8. crash/resume/compact 从 durable facts 恢复，不重新解释用户意图；
9. completion 由 verification/readiness/world state 证明，不由 assistant 自述决定。

这不是把 Sigil 简化成一个能力更弱的 loop，而是把已有复杂度从“分散的失败路径”重组为“一个可证明、
可恢复、单调前进的执行系统”。
