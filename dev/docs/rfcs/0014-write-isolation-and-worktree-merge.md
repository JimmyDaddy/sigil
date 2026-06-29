# RFC-0014 Write Isolation and Worktree Merge

状态：draft / planning

创建日期：2026-06-29

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Depends on: [RFC-0005 Execution Backend](0005-execution-backend.md)
- Unlocks: [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md) write isolation integration

## 1. Summary

本 RFC 定义 Sigil 的写型 agent 隔离与合并语义。它把当前保守的 shared workspace 串行写入，演进为可审计的 changeset-only / worktree 写隔离，同时不把 prompt 约束当成隔离边界。

核心决策：

1. Shared workspace 写入必须持有独占 write lease。
2. 并行写 agent 只能在 `ChangesetOnly` 或 `Worktree` 隔离模式下运行。
3. Child workspace 的 verification 只绑定 child `WorkspaceSnapshotId`，不能自动继承到 parent。
4. Merge 到 parent workspace 必须通过 RFC-0002 mutation protocol 产生 parent mutation evidence。
5. Merge 后 parent required checks 必须重新运行，才能得到 parent `Passed` verdict。

## 2. Goals

- 为 `/task` 写型步骤提供运行时强制隔离，而不是只靠模型提示避免同文件冲突。
- 让 child agent 的写入输出以 changeset 或 isolated workspace 的形式进入 parent review。
- 将 merge、conflict、reject、partial apply 和 cleanup 都记录为 durable events。
- 让 E07.5 write isolation、E03.4 worktree merge review 和后续并行写 agent 有共同事实层。
- 保持默认 TUI 操作简单：主流程只展示少量状态和一个推荐动作，细节进入 task detail / audit。

## 3. Non-goals

- 不默认开放多个 write agent 共享同一工作区。
- 不在本 RFC 中实现任意深度 recursive agent tree。
- 不自动解决 merge conflict。
- 不保证 shell/database/network/external service 副作用可回滚。
- 不把低频 worktree 管理暴露成主路径菜单或 `/config` 日常操作。

## 4. Isolation Modes

```rust
enum WriteIsolationMode {
    SharedWorkspaceExclusive,
    ChangesetOnly,
    Worktree,
}
```

Rules:

- `SharedWorkspaceExclusive` uses the parent workspace and requires an exclusive write lease.
- `ChangesetOnly` lets the child produce a proposed changeset without mutating the parent workspace.
- `Worktree` gives the child an isolated checkout or overlay-backed workspace.
- Unsupported or unavailable isolation modes fail closed. The runtime must not silently downgrade parallel writes into shared workspace writes.

## 5. Core Records

```rust
struct WriteLeaseAcquired {
    lease_id: WriteLeaseId,
    workspace_id: WorkspaceId,
    owner_agent_id: AgentId,
    isolation_mode: WriteIsolationMode,
    scope: WriteLeaseScope,
}

struct WriteLeaseReleased {
    lease_id: WriteLeaseId,
    status: WriteLeaseReleaseStatus,
}

struct IsolatedWorkspaceCreated {
    isolated_workspace_id: WorkspaceId,
    parent_workspace_id: WorkspaceId,
    owner_agent_id: AgentId,
    isolation_mode: WriteIsolationMode,
    base_snapshot_id: WorkspaceSnapshotId,
    backend: IsolatedWorkspaceBackend,
}

struct IsolatedChangeSetProduced {
    changeset_id: ChangeSetId,
    owner_agent_id: AgentId,
    base_snapshot_id: WorkspaceSnapshotId,
    child_snapshot_id: Option<WorkspaceSnapshotId>,
    source_isolation: WriteIsolationMode,
}

struct MergeReviewRequested {
    review_id: MergeReviewId,
    changeset_id: ChangeSetId,
    parent_workspace_snapshot_id: WorkspaceSnapshotId,
}

struct MergeReviewResolved {
    review_id: MergeReviewId,
    decision: MergeDecision,
    reason: Option<String>,
}
```

Merge apply reuses RFC-0002 mutation events:

- `MutationBatchStarted`
- per-file `MutationPrepared`
- per-file `MutationCommitted`
- `MutationBatchFinished`
- normalized `WriteCommitted`

Task and verification projections may also emit existing child/merge control entries such as `ChildChangesetMerged` or `AgentMergeApplied` when a child result is accepted into the parent.

## 6. Write Lease Rules

- At most one shared-workspace write lease may be active for a workspace.
- Read-only agents may run while a write lease is active only if their tools are filesystem-read-only and do not execute shell commands that can mutate the workspace.
- A write lease owner may use controlled file tools, changeset apply and approved mutating checks according to policy.
- Lease acquisition and release are durable events. Runtime-only locks are not enough.
- On restore, stale active leases are reconciled through RFC-0011 job/lease recovery instead of being silently ignored.

## 7. Changeset-only Flow

```text
child write step starts
  -> acquire child write isolation record
  -> child reads parent snapshot as base
  -> child produces changeset artifact
  -> parent requests merge review
  -> parent accepts/rejects
  -> accepted changeset applies through RFC-0002 mutation batch
  -> parent verification becomes Missing or Stale until checks pass
```

Rules:

- The child must not mutate the parent workspace in `ChangesetOnly` mode.
- A changeset must declare its base snapshot and touched subjects.
- Applying the changeset uses compare-and-swap against the parent current snapshot/subject hashes.
- Partial apply is represented by RFC-0002 batch status and reconciliation.

## 8. Worktree Flow

```text
child write step starts
  -> create isolated worktree
  -> child runs with workspace_id = child worktree
  -> child verification binds to child snapshot
  -> child emits changeset/diff against parent base
  -> parent merge review decides
  -> parent apply creates parent mutation evidence
  -> parent required checks run again
```

Rules:

- Worktree creation must record base commit/snapshot and cleanup responsibility.
- Worktree cleanup is best-effort but must be auditable.
- Child verification receipts remain child-scoped.
- Parent verification is never satisfied by child worktree `Passed` evidence alone.

## 9. Verification Integration

Merge events affect RFC-0003 readiness:

- `MergeReviewResolved(accepted)` without parent apply evidence does not satisfy verification.
- Parent apply evidence creates a new parent workspace snapshot.
- Parent required checks must run against that parent snapshot.
- Rejecting a child changeset does not mutate parent workspace and should not make parent verification stale.
- Conflict or partial apply yields `Blocked`, `Failed` or `Inconclusive` according to reducer state.

## 10. Product Surface

TUI main task surfaces should stay coarse:

- show isolation mode: `exclusive`, `changeset`, or `worktree`;
- show child state: `running`, `ready for review`, `merged`, `rejected`, `conflict`;
- show one primary action: `review changes`, `apply`, `run parent check`, or `resolve conflict`.

The default product flow should not expose low-frequency worktree inventory, per-file artifact deletion, lease internals or policy matrices. Those belong in task detail, session audit, doctor or advanced config.

## 11. Implementation Slices

1. Isolation contract and durable records.
2. Shared workspace write lease enforcement.
3. Changeset-only child write output.
4. Worktree manager MVP.
5. Merge review and parent mutation handoff.
6. Task DAG write isolation integration.
7. TUI merge/recheck product surface.
8. Cleanup and recovery hardening.

## 12. Acceptance Criteria

- Parallel write steps cannot run in the same shared workspace.
- `ChangesetOnly` child agents cannot mutate parent workspace.
- Worktree child verification does not transfer to parent after merge.
- Accepted merge creates parent RFC-0002 mutation evidence.
- Parent verification is stale/missing until required checks pass on parent snapshot.
- Resume reconstructs active write leases, child isolated workspaces and merge review state from durable events.
- TUI shows the user one clear next action instead of exposing internal policy complexity.

## 13. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel write_isolation
cargo test -p sigil-kernel task_write_isolation
cargo test -p sigil-kernel verification_merge
cargo test -p sigil-tui task_sidebar
```

## 14. Open Questions

- Whether the first implementation should support `ChangesetOnly` before physical git worktrees.
- Whether worktree creation should require a Git repository, or support copy/overlay fallback.
- How long abandoned isolated workspaces should be retained before cleanup.
- Whether merge review should reuse existing changeset UI or introduce a task-detail panel.
