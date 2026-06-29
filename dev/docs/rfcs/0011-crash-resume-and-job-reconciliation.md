# RFC-0011 Crash Resume and Job Reconciliation

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 crash resume 的产品语义：Sigil 不假装进程内后台任务能从 instruction pointer 透明恢复，而是通过 job intent、step lease、heartbeat、idempotency key 和 tool receipt 做 restart reconciliation。

## 2. Goals

- 重启后清楚区分 resumable、interrupted needs user 和 abandoned。
- 不自动重放写工具或 shell 命令，除非 receipt 证明幂等且策略允许。
- 持久化 mailbox 和 job intent，避免进程内状态丢失后不可解释。
- TUI 启动时展示可恢复任务和风险。

## 3. Non-goals

- 不实现分布式 workflow engine。
- 不承诺恢复本地 OS 进程、PTY 或外部服务连接。
- 不自动重放未知副作用命令。
- 不替代 RFC-0002 mutation reconciliation。

## 4. Core Types

```rust
struct JobIntent {
    job_id: JobId,
    session_id: SessionId,
    task_id: Option<TaskId>,
    agent_profile: Option<AgentProfileId>,
    user_goal_event_id: EventId,
    tool_policy_hash: String,
    expected_effect: ToolEffect,
}

struct StepLease {
    lease_id: LeaseId,
    job_id: JobId,
    owner_process_id: String,
    deadline_ms: u64,
    heartbeat_event_id: Option<EventId>,
}

struct ToolReceipt {
    tool_call_id: ToolCallId,
    idempotency_key: String,
    status: ToolReceiptStatus,
    mutation_operation_ids: Vec<OperationId>,
}
```

## 5. Resume Classes

```rust
enum ResumeDisposition {
    Resumable,
    InterruptedNeedsUser,
    Abandoned,
}
```

Rules:

- Missing heartbeat with non-idempotent write -> `InterruptedNeedsUser`.
- Completed receipt and terminal durable event -> terminal, no replay.
- Started shell/process without terminal event -> reconcile mutation evidence and mark interrupted.
- Pending mailbox message -> show resume action.

## 6. Product Surface

TUI startup should show concise recovery:

- task/session
- last known step
- risk summary
- recommended action: resume, inspect, mark abandoned

Do not present internal lease/idempotency details in the main path.

## 7. Implementation Slices

1. Job intent and step lease durable events.
2. Heartbeat and stale lease reconciliation.
3. Mailbox persistence.
4. Tool receipt/idempotency metadata.
5. TUI recovery panel.

## 7.1 Implementation Progress

核心语义已实现：

- Job intent and step lease are now first-class append-only control entries with
  durable event types.
- Resume job state can be reduced from session entries or rebuilt from the
  mixed-format durable stream.
- Expired acquired leases are projected as `InterruptedNeedsUser`, so restarted
  sessions do not need to keep showing dead work as running.
- Step lease heartbeat events extend matching acquired leases and leave
  mismatched, interrupted or expired work in an explicit recovery state.
- Tool result metadata can now carry receipt idempotency metadata, mutation
  operation ids and a conservative replay decision helper; non-idempotent
  receipts remain denied by default.
- Agent mailbox messages are now durable control entries with queued,
  delivered, consumed, rejected and interrupted states. Restore appends an
  interrupted mailbox event for delivered messages that were not durably
  consumed before process loss.
- TUI audit view renders compact job intent, step lease and mailbox control
  summaries.
- TUI session view now shows a concise recovery panel when stale jobs,
  interrupted mailbox messages or interrupted attempts are present.

Productization remains:

- Receipt metadata is not yet wired into automatic tool replay, and this RFC
  still forbids silent replay of non-idempotent tools.
- This does not resume OS processes, PTYs or external services from their
  instruction pointer.

## 8. Acceptance Criteria

- Restart does not report dead background tasks as running.
- Missing/expired lease has clear reason.
- Mailbox messages are not lost.
- Interrupted write-capable tools produce RFC-0002 reconciliation evidence.
- Resume never silently replays non-idempotent tool calls.

## 9. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel resume
cargo test -p sigil-runtime agent_supervisor
cargo test -p sigil-tui session
```

## 10. Open Questions

- Which tool calls can safely declare idempotency.
- Whether heartbeat should be per step, per agent attempt, or both.
- How much recovery UI should appear before entering the main TUI.
