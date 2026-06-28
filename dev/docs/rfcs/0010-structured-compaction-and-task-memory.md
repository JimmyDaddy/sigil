# RFC-0010 Structured Compaction and Task Memory

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Related: [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)

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

1. Typed compaction record and `TaskMemoryV1` data model.
2. Deterministic extraction from durable events.
3. Optional model-assisted summary with sourced/unverified markings.
4. Context Engine retrieval integration.
5. TUI memory inspect view.

## 9. Acceptance Criteria

- Compaction preserves task objective, constraints, decisions, files, commands and verification references.
- Model-generated facts are visibly unverified unless backed by durable receipt.
- Task memory binds to branch/snapshot and can be invalidated.
- Old sessions without typed memory still load using legacy text summary.

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel compaction
cargo test -p sigil-tui session
```

## 11. Open Questions

- Whether model-assisted memory should be optional by provider/config.
- Whether memory should be workspace-wide, task-scoped, or both.
- What retention policy should apply to cross-session memory.
