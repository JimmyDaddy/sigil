# RFC-0007 Task DAG and Isolated Agent Workflows

状态：draft / E07.1-E07.4 implemented / E07.5 gated

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Depends on: [RFC-0014 Write Isolation and Worktree Merge](0014-write-isolation-and-worktree-merge.md)

## 1. Summary

本 RFC 定义 `/task` 从 sequential orchestrator 演进到 DAG-based orchestrator 的边界。目标是支持只读步骤并发、显式依赖、review / verify 阶段和 bounded replanning，同时不引入共享工作区并行写入风险。

## 2. Goals

- 让 task plan 显式表达 `depends_on`、mode、review 和 verify。
- 允许 read-only steps 并发。
- 写入步骤默认串行；并行写必须通过 worktree、changeset 或 write lease 隔离。
- Reviewer 可以是模型 agent；Verifier 必须以系统 Verification Contract 为最终依据。
- Replan、supersede、merge conflict 和 dependent cancellation 都进入 durable control state。

## 3. Non-goals

- 不开放任意深度 recursive agent tree。
- 不默认允许多个 write agent 共享同一 workspace。
- 不让 child workspace 的 `Passed` 自动继承为 parent workspace `Passed`。
- 不在本 RFC 中实现完整 worktree merge UI；只定义运行时不变量。

## 4. Core Types

```rust
struct TaskGraph {
    task_id: TaskId,
    graph_version: u32,
    steps: Vec<TaskGraphStep>,
}

struct TaskGraphStep {
    step_id: TaskStepId,
    title: String,
    mode: TaskStepMode,
    depends_on: Vec<TaskStepId>,
    status: TaskGraphStepStatus,
    isolation: TaskIsolationMode,
}

enum TaskStepMode {
    Read,
    Write,
    Review,
    Verify,
}

enum TaskIsolationMode {
    SharedReadOnly,
    SequentialWorkspaceWrite,
    ChangesetOnly,
    Worktree,
}
```

## 5. Scheduling Rules

- DAG must be acyclic before execution.
- Ready queue contains steps whose dependencies are terminal successful or explicitly skipped by policy.
- Read-only steps may share workspace and run concurrently within configured concurrency/token budget.
- Write steps require exclusive workspace write lease unless isolation is `ChangesetOnly` or `Worktree`.
- If a dependency fails, dependent steps become blocked or cancelled according to policy.
- Replan creates a new graph version; old steps become `Superseded`, not deleted.

## 6. Canonical Pipeline Templates

Templates are suggestions, not mandatory phases:

- Code change: Explore -> Implement -> Review -> Verify
- Research: Explore -> Review
- Docs change: Explore -> Implement -> Verify
- Simple config change: Implement -> Verify

The model may propose a custom DAG, but the runtime validates dependencies, mode, isolation and policy before execution.

## 7. Verification and Merge Rules

- Child verification binds to child workspace snapshot only.
- Parent merge creates parent workspace mutation evidence.
- Parent required checks must run after merge.
- Verifier agent output is advisory; final verdict comes from RFC-0003 readiness.

## 8. Product Surface

TUI should show:

- DAG progress
- blocked reason
- running read-only agents
- write isolation mode
- review finding summary
- parent re-check requirement after merge

Main task UI should keep one recommended action per state, such as `continue`, `run parent check`, `review conflict` or `cancel blocked step`.

## 9. Implementation Slices

1. Plan schema and durable graph projection without parallel execution.
2. Read-only ready queue concurrency.
3. Review/verify state separation.
4. Bounded replanning and superseded steps.
5. Write isolation integration with changeset/worktree support.

## 9.1 Implementation Progress

- E07.1 implemented plan schema, dependency metadata and durable graph projection.
- E07.2 implemented read-only ready queue batching with concurrency budget, running-write exclusion, sequential write handoff and shared-read-only write denial coverage.
- E07.3 implemented review / verify state separation while keeping system verification authoritative.
- E07.4 implemented bounded plan versions and `Superseded` projection semantics: accepting a newer plan version marks older plan versions superseded, preserves completed step history, marks unfinished old-plan steps as `Superseded`, clears current-step pointers to superseded plans, and surfaces the state in TUI summaries.
- E07.5 write isolation remains gated until RFC-0014 changeset/worktree isolation is ready.

## 10. Acceptance Criteria

- Read-only steps can run concurrently without write permissions.
- Write steps cannot run concurrently in the same workspace by default.
- `depends_on` is present in model-visible schema and runtime validation.
- Replan creates append-only graph version history.
- Resume can reconstruct graph state from durable events.
- Child verification does not transfer to parent after merge.

## 11. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel task_dag
cargo test -p sigil-runtime agent_supervisor
cargo test -p sigil-tui task_sidebar
```

## 12. Open Questions

- Whether RFC-0014 write isolation should start with changeset-only before worktree support.
- Whether read-only fanout should be configured per task or globally.
- How much DAG detail belongs in the main task strip versus a task detail panel.
