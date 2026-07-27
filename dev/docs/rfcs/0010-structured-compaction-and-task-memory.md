# RFC-0010 Structured Compaction and Task Memory

状态：accepted / RFC-0025 K25.1-K25.18F implemented / current roadmap core complete / P10.1-P10.6 project memory productization deferred

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Related: [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)
- Related: [RFC-0051 Intent Stack / 意图级版本控制 V1](0051-intent-stack-and-intent-level-version-control-v1.md)
- Related: [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)

## 1. Summary

本 RFC 定义长期结构化 compaction 和 `TaskMemoryV1`。它补足当前确定性文本摘要容易丢失决策原因、失败尝试、关键约束和验证证据的问题。

`TaskMemoryV1` 与 RFC-0006 的 `ContextDigestV0` 分层：`ContextDigestV0` 是短期 packing 摘要，`TaskMemoryV1` 是长期、可追溯、可被 Context Engine 召回的任务记忆。

## 2. Goals

- 压缩长任务时保留 objective、constraints、decisions、files changed、commands、verification、failed attempts、risks 和 unresolved issues。
- 让 pruned tool output 能通过 durable handle 找回原始审计记录。
- 确保模型摘要不能创造 evidence 或 verification passed。
- Revert/fork/worktree/branch switch 后旧 memory 不被静默混用。

## 3. Non-goals

- 不替代 durable event log。
- 不把模型摘要当成事实来源。
- 不在本 RFC 中定义 Context Engine ranking。
- 不做跨设备 memory sync。
- Project Memory 删除不撤销原始 user turn、tool/receipt 审计，也无法召回已发送给外部 provider
  的历史 context；它只删除 Sigil 控制的 memory 副本并阻止未来 retrieval。

## 4. Core Types

```rust
struct TaskMemoryV1 {
    memory_id: TaskMemoryId,
    branch_id: Option<BranchId>,
    valid_for_snapshot: WorkspaceSnapshotId,
    supersedes: Option<TaskMemoryId>,
    source_event_ids: Vec<EventId>,
    objective: String,
    constraints: Vec<SourcedFact>,
    decisions: Vec<SourcedDecision>,
    files_changed: Vec<FileChangeRef>,
    commands_run: Vec<CommandReceiptId>,
    verification_results: Vec<VerificationReceiptId>,
    failed_attempts: Vec<AttemptRef>,
    risks: Vec<SourcedFact>,
    unresolved_issues: Vec<SourcedFact>,
}
```

Every sourced fact records:

- source event/receipt/artifact id
- confidence
- whether it is model-generated
- whether it is verified or inferred

## 5. Rules

- Compaction appends memory; it does not rewrite old memory.
- A new memory may supersede an old one but must preserve source linkage.
- Summary cannot emit `VerificationRecorded`.
- Summary can reference a verification receipt.
- Memory validity is bound to branch/snapshot.
- Restore or merge may invalidate memory by appending `MemoryInvalidated` or `MemorySuperseded`.

## 6. Tool Output Pruning

When pruning old tool output from provider context:

- durable audit remains unchanged
- provider-visible context gets concise structured summary
- original observation is reachable by retrieval handle if policy permits
- secret redaction state is preserved

## 7. Product Surface

TUI should show compact memory as a task/session detail:

- current objective
- decisions
- files changed
- checks run
- unresolved items

It should not replay every old tool output into transcript.

## 8. Implementation Slices

1. Typed durable compaction lifecycle and `TaskMemoryV1` sidecar data model.
2. Deterministic extraction from durable events.
3. Optional model-assisted summary with sourced/unverified markings.
4. Context Engine retrieval integration.
5. TUI memory inspect view.
6. Default compaction flow attaches deterministic `TaskMemoryV1` when durable evidence exists.

## 8.1 Implementation Progress

核心语义已实现：

- V2 compaction lifecycle uses `CompactionStarted`, `TaskMemoryRecordedV1`,
  `CompactionAppliedV2` and terminal failure/skip events. `TaskMemoryV1` is a
  canonical-hashed sidecar, rather than a field in a legacy control entry.
- The pre-release build does not read, upcast or migrate legacy `CompactionRecord`
  payloads. Raw legacy `SessionLogEntry` JSONL and a legacy compaction payload inside
  a V2 envelope are rejected before recovery or append can mutate the stream.
- The same V2-only rule applies to durable nested payloads: removed access variants,
  missing approval/grant facets, and incomplete execution or terminal-output evidence
  are rejected directly. The runtime does not infer a replacement value or reserialize
  an old representation as current state.
- Deterministic extraction builds `TaskMemoryV1` from durable/control events
  without inventing verification evidence from model text.
- 自动 compaction 不会把 `TaskMemoryV1.active_plan` 变成第二套可执行 task graph：
  checkpoint 只保留供模型续接的 objective、step title 与 latest status；Task 的 plan version、
  dependency、role、mode、isolation 和生命周期仍以 append-only Task control events 为唯一调度
  authority。fold plan 必须把这些 control events 标为 `ControlState` 且永不折叠；自动 apply
  后 host 必须从完整 durable stream reload，并据此重建 continue 选择与 TUI task list。
- Model-assisted task memory import preserves `model_generated=true`,
  `verified=false`, confidence and source event metadata instead of creating
  evidence.
- TaskMemoryV1 can be converted into RFC-0006 ContextItems with TaskDigest
  source, trust/sensitivity labels, token cost and durable event provenance.

以下是保留用于审计的历史 rollout checkpoint，**不是当前 K25 core 的未完成前置**：

- 在 K25.2 checkpoint，`/compact` 只提供 read-only V2 fold/keep/protection preview，尚未创建
  checkpoint 或呈现 legacy memory data。
- K25.3 checkpoint 加入 inactive V2 initiated lifecycle（`CompactionStarted` plus
  exactly one `CompactionAppliedV2` or `CompactionFailed`) with strict durable
  lineage and explicit idempotent recovery；该阶段尚不让 V2 record
  alter task memory, continuation state, chat context or the TUI flow.
- K25.4 checkpoint 开始持久化 strict、canonical-hashed `TaskMemoryRecordedV1` sidecars
  and checkpoint bindings；它们在该阶段要等同一 Start lineage
  writes `CompactionAppliedV2`; resolver replay validates memory/checkpoint id,
  branch, snapshot, cursor and supersedes lineage, while explicit invalidation
  removes the sidecar。
- K25.5-K25.18F 后续切片已完成 accepted core；页首状态是当前实施结论。上述 checkpoint 只说明
  演进顺序，P10 implementer 不得把“inactive/preview-only”历史描述当作当前 prerequisite。

Productization remains:

- Typed memory remains evidence-referencing, not a fact source: compaction and
  model summaries cannot create verification evidence or change completion
  verdicts.
- Cross-session retention, append-only correction and branch/worktree invalidation UX are not in the
  implemented K25 core; their executable productization plan is frozen in §8.2.
- The current K25 surface intentionally offers no in-place memory editing; P10 correction appends a
  superseding version instead of rewriting history.
- Project Memory P10 继续以 K25.1-K25.18F accepted core 为基线，不重复实施历史 checkpoint。

## 8.2 P10 Project Memory productization

RFC-0051/0053 即使全部完成，也只会让单个 Intent/Task 的计划、执行和审查更可靠；它们不会把
多次任务中沉淀的项目约束、架构决策和失败经验自动变成可检查、可失效、可删除的长期项目记忆。
本节把原“cross-session retention / memory editing”开放项收敛为直接可实施的 Project Memory
计划。

### 8.2.1 Scope and truth model

Project Memory 与既有 `TaskMemoryV1` 分层：

- `TaskMemoryV1` 仍是 task/session-scoped compaction sidecar，绑定 exact branch/snapshot；
- `ProjectMemoryEntryV1` 是 workspace-scoped、跨 session 可检索的候选知识，但不是独立事实源；
- 两者都引用 RFC-0001 event、RFC-0002/0003 receipt/artifact、RFC-0051 Intent 或 RFC-0053 Task
  identity；正文不能替代这些来源；
- workspace identity 使用 canonical repo/workspace id，不因 relative path 相同而跨项目混用；
- V1 不做跨设备同步、不从任意 Git history/commit message 自动猜测项目事实。

每个 project memory entry 至少记录：

- runtime 分配的 stable `logical_memory_id`、per-version `memory_id`、version、`supersedes` 与
  workspace scope；
- kind：constraint、decision、convention、known_failure、validated_workflow、unresolved_risk；
- bounded statement 与 structured payload；
- exact source refs、source snapshot/branch/intent/task、created/last_validated time；
- trust state：`evidence_backed`、`user_asserted`、`model_suggested`；
- validity：snapshot/branch predicate、optional expiry、`active/stale/invalidated/deleted`；
- sensitivity/egress label、confidence、model-generated 标记和 content digest。

`ProjectMemoryEntryV1` 必须是独立、可删除 sidecar，不把 statement、structured payload、source
excerpt、embedding 或 content-derived digest 内联进 append-only event JSON。durable stream 只
记录 runtime 随机分配的 sidecar handle/version、workspace scope、lifecycle transition、source
event ids 和非内容型状态；sidecar 自己保存并校验 content digest。这样 resolver 可以在内容存在
时验证 integrity，又能在 physical delete 后只留下不可用于重建或字典匹配正文的 tombstone。
任何 adapter 不得为了方便 projection 把正文复制回 recovery-critical event。
Project Memory 正文 sidecar 按 entry/version 独立存储，不跨 logical memory 做 content
deduplication；否则一个 lineage 的 physical delete 无法删除其受控副本。

TTL 只表示“需要重验”，不能单独证明事实过期或仍有效。source artifact/receipt 不可用、workspace
identity 改变、用户删除或新的 durable evidence 冲突时，entry 必须 fail closed 为 stale/
invalidated，不能继续作为高信任约束注入。

### 8.2.2 Admission and updates

- deterministic extractor 只能从已提交的 typed event/evidence 形成
  `evidence_backed` candidate；模型只能形成 `model_suggested` candidate；
- `user_asserted` 必须来自 exact user action 或受信规格导入，并保留原 source ref；
- 自动 admission 只允许 bounded、非敏感、可由当前 durable evidence 完整支持的候选；架构偏好、
  安全例外、凭据、个人信息和模型推断必须等待用户确认或保持 task-local；
- project memory 不原地编辑。修正、重验或 scope 变化追加新 version 并 supersede 旧 version；
- Intent/Task 完成不是自动“学会”的充分条件。只有 synthesis 后的 parent snapshot、final
  verification 与 terminal evidence 齐全，才能提升 candidate trust；
- fork/branch/worktree child 只产生 candidate；parent promotion/adoption 前不能更新 active
  project memory。

### 8.2.3 Inspect, forget and physical delete

TUI 项目记忆视图必须支持按 kind/trust/validity 检查来源、最后验证时间、适用范围和使用历史，
并提供 `Keep`、`Mark stale`、`Forget` 三类清晰动作：

- `Mark stale` 可以只针对一个 version，追加 invalidation 但不删除历史来源；`Forget` 默认且仅针对整个 `logical_memory_id`
  supersedes lineage，原子追加所有已知 version 的 tombstone，使 retrieval/projection 立即停止
  使用任何版本。V1 不提供容易误解的“只 Forget 当前版本但旧正文仍 active”动作；
- 用户显式选择 physical delete 时，目标同样是已 tombstone 的完整 lineage。必须删除每个 version
  的 memory statement/payload、embedding/index/cache 副本和 entry-owned sidecar。append-only
  lifecycle 只保留 opaque logical/version ids、workspace scope、deletion reason/time 和不可恢复
  状态；旧 version inspect 统一显示 tombstone，不保留可对短正文做字典匹配的 content-derived
  digest；
- tombstone 保留 source lineage ids 与 version（不保留 source excerpt），阻止 deterministic/
  model candidate 从同一已删除 lineage 自动重新 admission。新的独立 evidence 只能形成新的
  pending candidate，且必须再次由用户确认，不能用“新 id”绕过 Forget；
- source event/receipt 若属于其他审计或 retention 合同，不随 memory 删除；以后 inspect 显示
  `source unavailable/deleted`，不得从 provider cache、Git 或旧摘要重建已删除正文；
- 已经发送给 provider/remote retrieval service 的历史 bytes 无法由本地 tombstone 召回。
  inspect/delete preview 必须明确这个边界；若 connector 支持独立删除 API，必须另获用户授权并
  记录 remote deletion receipt，不能把本地成功伪装成远端已删除；
- secret-like/credential material 不进入 Project Memory。删除、索引清理、cache eviction 与
  Context Engine projection 必须幂等；use history 只保留 id/version 与 decision，不复制正文，
  restart 后 tombstone cascade 仍覆盖后来发现的 orphan/superseded sidecar。独立审计/source
  artifact 的 refcount 与 retention 不由 memory lineage delete 改写。

### 8.2.4 Retrieval and use

- RFC-0006 ranking 必须先按 workspace/branch/snapshot validity、trust、sensitivity/egress 与
  token budget 过滤，再排序相关性；
- `evidence_backed` 可以作为 planning constraint，但不能伪造当前 verification；过期 receipt
  只能提示历史经验；
- `user_asserted` 必须在 UI 中可辨识；`model_suggested` 默认低信任，只能作为待验证 hint；
- 每次跨 session 注入记录 bounded retrieval decision 与 memory ids，用户可以检查“这次为什么
  想起它”；
- Intent/Task planner 只能引用 memory id/version，runtime 重新解析 active entry；provider 不能
  通过复制旧文本绕过 stale/deleted 状态。

### 8.2.5 Execution slices

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| P10.1 | 冻结 ProjectMemory schema、workspace identity、trust/validity/sensitivity taxonomy 与 config migration | canonical digest/schema fixtures；旧 TaskMemory 行为不变；unknown version fail closed |
| P10.2 | ref-only durable candidate/admit/supersede/invalidate/tombstone lifecycle、deletable sidecar 与 resolver | event JSON 零正文/embedding/content-derived digest；crash/replay/idempotency/property tests；orphan/partial batch 不进入 active projection |
| P10.3 | deterministic extraction 与 Intent/Task parent-evidence bridge | model suggestion 不自动升信；child/failed/stale receipt negative corpus |
| P10.4 | cross-session store/index、retention、cache eviction 与 whole-lineage physical delete | version/tombstone cascade、no-body-dedupe、restart/orphan/delete/source-retention/index consistency tests；旧 version inspect 降级；event/provider boundary UX；secret fixtures 零持久化 |
| P10.5 | RFC-0006 retrieval、egress/token gate 与 planner stable-ref consumption | stale/deleted/cross-workspace 零召回；injection audit；provider text 不能复活 tombstone |
| P10.6 | TUI inspect/source/validity/use-history/forget surface 与 dogfood | narrow-layout/input/session-switch/stale-request tests；跨三次真实任务保持、纠正并删除一条记忆 |

跨 RFC 前置与依赖顺序固定为：

```text
RFC-0051 R51.0 ---------> P10.1 -> P10.2 -> P10.3 -> P10.4 -> P10.5 -> P10.6
RFC-0051 R51.2 + RFC-0053 O6f ----^
```

P10.1 只有在 R51.0 冻结 stable Intent ref wire shape 后才加入 Intent source variant；P10.3 只有在
R51.2/O6f 完成 execution/promotion bridge 后才提升 Intent/Task parent evidence。实现者不能提前
猜 schema 或把 child proposal 当 parent fact。

默认启用 cross-session retrieval 前，必须满足：跨 workspace、stale、invalidated、deleted 和
secret-like entry 的注入数为 0；所有注入均可追溯 source/trust/version；`Forget` 在 restart、
fork 和 cache rebuild 后仍生效。

## 9. Acceptance Criteria

- Compaction preserves task objective, constraints, decisions, files, commands and verification references.
- Model-generated facts are visibly unverified unless backed by durable receipt.
- Task memory binds to branch/snapshot and can be invalidated.
- Legacy compaction payloads are rejected rather than reconstructed from a text summary.
- Project Memory 开启后，每条跨 session 注入都带 source、trust、validity、version 与 use record。
- 用户可以 inspect、supersede、invalidate 和 whole-lineage forget；physical delete 后该 lineage
  的正文不会被 Sigil-controlled sidecar/index/cache/Git reconstruction 复活，UI 也不会声称能
  撤回历史 provider egress 或独立审计来源。
- stale、deleted、cross-workspace、secret-like 与 orphan candidate 不进入 planner/provider context。

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel compaction
cargo test -p sigil-kernel idle_auto_compaction_preserves_task_list_memory_and_executable_projection
cargo test -p sigil-tui session
```

## 11. Locked decisions and deferred boundaries

- model-assisted memory 由 config 可选；启用时只产生 `model_suggested` candidate，不能自动成为
  evidence-backed 项目事实。
- 同时保留 task/session-scoped `TaskMemoryV1` 与 workspace-scoped
  `ProjectMemoryEntryV1`；两者不共享 mutation authority。
- cross-session retention 对 active project entry 默认受保护；超出普通 quota 时先 stale/cleanup
  preview，不静默删除。用户 `Forget`/physical delete 优先于 retention。
- “编辑记忆”实现为 append-only supersede，不原地修改；所有 surface 必须可查看旧 version 与
  失效原因。
- 跨设备 sync、团队共享 memory、远端知识库写回、自动解决互相冲突的 project facts 继续延后，
  需要独立信任、身份与同步 RFC。
