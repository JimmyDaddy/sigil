# RFC-0002 Crash-consistent Mutation Protocol

状态：Draft

创建日期：2026-06-25

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Required by: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 Sigil 受控 workspace mutation 的崩溃一致协议。它把文件写入、checkpoint restore、changeset apply、shell/MCP/plugin 可能写入和外部 workspace 变化统一成可审计、可恢复、可被 verification reducer 消费的 mutation evidence。

核心决策：

1. 受控写入先持久化 `MutationPrepared`，再执行文件 mutation，最后持久化 `MutationCommitted`。
2. 崩溃后发现 prepared-but-not-committed 时追加 `MutationReconciled`。
3. 多文件 changeset 使用 batch id，每个文件有独立 prepare/commit/reconcile。
4. `WorkspaceSnapshotId` 是 verification 的内容绑定依据；`WorkspaceRevision` 只在一个 workspace/worktree stream 内有序。
5. 无法精确覆盖的写入产生 `WorkspaceMutationDetected` 或 `UnknownDirty`，不能被当作 clean passed evidence。

## 2. Goals

- 避免“文件已写但日志无 evidence”或“日志写了但文件未写”的不可解释状态。
- 为 RFC-0003 提供稳定 mutation events、snapshot id、workspace revision 和 stale reason。
- 支持受控 file tools、checkpoint restore、changeset apply 和非受控外部 mutation detection。
- 明确多文件部分成功后的 reconciliation。
- 让 crash recovery 不自动重放写工具或 shell 命令。

## 3. Non-goals

- 不实现通用 database/network/external service rollback。
- 不保证 LocalBackend 下 shell 副作用可完整 rewind。
- 不定义 ExecutionBackend sandbox 细节。
- 不定义 Artifact Store 完整 retention/encryption 机制。
- 不实现 distributed multi-node transaction。

## 4. Core Types

```rust
struct MutationPrepared {
    operation_id: OperationId,
    batch_id: Option<MutationBatchId>,
    tool_call_id: Option<ToolCallId>,
    causation_event_id: EventId,
    subject: MutationSubject,
    before_hash: Option<String>,
    intended_after_hash: Option<String>,
    snapshot_coverage: SnapshotCoverage,
    workspace_id: WorkspaceId,
    base_workspace_revision: WorkspaceRevision,
    sync_class: MutationSyncClass,
}

enum SnapshotCoverage {
    Captured(ArtifactId),
    NoPriorContent,
    SkippedSensitive,
    Unsupported,
    Unavailable,
}

struct MutationCommitted {
    operation_id: OperationId,
    batch_id: Option<MutationBatchId>,
    observed_after_hash: Option<String>,
    workspace_revision: WorkspaceRevision,
    workspace_snapshot_id: WorkspaceSnapshotId,
    committed_subject: MutationSubject,
}

struct MutationReconciled {
    operation_id: OperationId,
    batch_id: Option<MutationBatchId>,
    observed_state: MutationObservedState,
    resolution: MutationResolution,
    workspace_revision: Option<WorkspaceRevision>,
    workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}
```

## 5. Mutation Subject

```rust
enum MutationSubject {
    File {
        path: PathBuf,
        file_type: FileType,
    },
    Directory {
        path: PathBuf,
    },
    Workspace {
        scope_hash: String,
    },
    External {
        description: String,
    },
    Unknown,
}
```

Rules:

- File paths are normalized workspace-relative paths.
- Symlink targets are represented in the snapshot manifest.
- External or unrepresentable paths cannot produce precise clean evidence.
- Unknown mutation subjects make verification stale or inconclusive.

## 6. Controlled Single-file Write Flow

```text
1. Resolve and validate workspace-confined path.
2. Capture before state and optional artifact.
3. Compute intended after hash.
4. Append and sync MutationPrepared.
5. Compare-and-swap: current hash must still equal before_hash.
6. Write via temp file and atomic replace.
7. Sync file.
8. Sync parent directory.
9. Compute observed after hash.
10. Append and sync MutationCommitted.
```

If step 5 fails, the tool records a conflict result and must not write.

After a successful `MutationCommitted`, controlled file tools emit `WriteCommitted` as the normalized tool-facing write receipt. `WriteCommitted` must reference the `operation_id`, optional `batch_id` and causing `MutationCommitted` event. Projections must deduplicate by `operation_id` so `MutationCommitted` and `WriteCommitted` do not count as two independent writes.

## 7. Multi-file Changeset Flow

Multi-file operations are not atomic. They use a batch id.

```rust
struct MutationBatchStarted {
    batch_id: MutationBatchId,
    operation_id: OperationId,
    expected_subjects: Vec<MutationSubject>,
}

struct MutationBatchFinished {
    batch_id: MutationBatchId,
    status: MutationBatchStatus,
    committed_operations: Vec<OperationId>,
    failed_operations: Vec<OperationId>,
}
```

Rules:

- Each file still has its own `MutationPrepared` and `MutationCommitted`.
- Partial success is represented explicitly.
- Recovery reconciles each prepared operation independently.
- Batch finished status is derived from per-file outcomes.
- Verification sees the resulting parent workspace snapshot, not the intended batch.

## 8. Restore Flow

Checkpoint restore is a new mutation.

Rules:

- Restore uses the same prepare/commit/reconcile protocol.
- Restore creates a new workspace revision.
- Restore creates a new workspace snapshot id.
- Restore emits `CheckpointRestored` after the committed or reconciled mutation transition.
- `CheckpointRestored` references the restore batch or operation and the new workspace snapshot.
- Restore invalidates prior verification for affected scope.
- Restore conflict requires user confirmation or fails without writing.

## 9. External or Unknown Mutation Flow

Shell, persistent terminal, MCP, plugin, external process and user edits may modify the workspace outside controlled tools.

Minimum events:

```text
ToolExecutionStarted
WorkspaceMutationDetected
ToolExecutionFinished(status = Finished | Failed | Interrupted)
```

Rules:

- If pre/post scan detects precise changes, emit `WorkspaceMutationDetected` with subject list and snapshot transition.
- If scan is unavailable, incomplete or untrusted, emit `WorkspaceMutationDetected` with `MutationSubject::Unknown` and set `WorkspaceKnowledge::UnknownDirty`.
- Unknown dirty invalidates existing verification evidence in the affected scope.
- Shell commands are not automatically replayed during recovery.

Workspace knowledge states:

```rust
enum WorkspaceKnowledge {
    Clean(WorkspaceRevision),
    Dirty(WorkspaceRevision),
    UnknownDirty,
}
```

Write-capable execution recovery:

```rust
struct ExecutionMutationProfile {
    tool_call_id: ToolCallId,
    effect: ToolEffect,
    workspace_id: WorkspaceId,
    scan_scope_hash: String,
    pre_execution_snapshot_id: Option<WorkspaceSnapshotId>,
}

enum ToolEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}
```

Rules:

- `ToolExecutionStarted` for `WorkspaceWrite`, `ExternalWrite` or `Unknown` effects must persist an `ExecutionMutationProfile` before the tool process starts.
- Writer-mode load scans for write-capable `ToolExecutionStarted` events that have no terminal `ToolExecutionFinished`.
- If a pre-execution snapshot exists and the scan can complete, recovery emits `WorkspaceMutationDetected` for the observed transition.
- If the scan cannot complete or the started tool may have written outside the scan scope, recovery emits `WorkspaceMutationDetected` with `MutationSubject::Unknown` and marks the workspace `UnknownDirty`.
- Recovery must not replay shell, MCP, plugin or persistent terminal commands to discover their effects.

## 10. Workspace Snapshot Manifest

`WorkspaceSnapshotId` is content-bound.

Minimum V1 manifest input:

```rust
struct WorkspaceSnapshotManifestV1 {
    workspace_id: WorkspaceId,
    scope_hash: String,
    entries: Vec<WorkspaceSnapshotEntry>,
}

struct WorkspaceSnapshotEntry {
    normalized_path: PathBuf,
    file_type: FileType,
    content_hash: Option<String>,
    mode: Option<u32>,
    symlink_target: Option<PathBuf>,
    state: SnapshotEntryState,
}

enum SnapshotEntryState {
    Present,
    Missing,
    PermissionDenied,
    External,
    Unsupported,
}
```

Rules:

- Entry order is deterministic.
- Paths are normalized and workspace-relative.
- `content_hash` for regular files uses `sha256:<hex>` over raw file bytes.
- Symlink target is included instead of silently following outside workspace.
- Permission denied, external and unsupported entries prevent a clean passed snapshot unless policy explicitly excludes them.
- If the manifest cannot be built completely for the verification scope, the workspace becomes `UnknownDirty`.
- `WorkspaceSnapshotId` uses `sha256:jcs-v1:<hex>` over the canonical JSON form of `WorkspaceSnapshotManifestV1`.
- The manifest hash covers `workspace_id`, `scope_hash`, all entries, paths, entry states, content hashes, modes and symlink targets.

## 11. WorkspaceRevision Scope

`WorkspaceRevision` is a local counter.

Rules:

- It is scoped to one `workspace_id` and one worktree/snapshot stream.
- It is not comparable across sessions, worktrees or branches.
- Verification validity is determined by `WorkspaceSnapshotId`, not by revision number alone.
- A revision increment indicates a known mutation or reconciliation point.

## 12. Recovery

On load, writer-mode reconciliation scans for `MutationPrepared` without terminal mutation event.

It also scans for write-capable `ToolExecutionStarted` without terminal `ToolExecutionFinished`, as defined in the external mutation flow.

Recovery states:

```rust
enum MutationObservedState {
    NotApplied,
    AppliedAsIntended,
    AppliedDifferently,
    Unknown,
}

enum MutationResolution {
    MarkNotApplied,
    MarkCommitted,
    MarkConflict,
    MarkUnknownDirty,
}
```

Rules:

- Reconciliation appends `MutationReconciled`.
- Reconciliation does not replay tool code.
- If current hash equals before hash, mark not applied.
- If current hash equals intended after hash, mark committed/reconciled.
- If current hash is neither, mark conflict or unknown dirty.
- If snapshot coverage was skipped for sensitive content, reconciliation cannot restore prior content and may only classify current state.
- If unfinished write-capable tool execution cannot be scanned completely, reconciliation emits unknown workspace mutation evidence instead of preserving stale verification.

## 13. Verification Interface

RFC-0003 consumes these mutation outputs:

- `operation_id`
- `batch_id`
- `tool_call_id`
- `causation_event_id`
- mutation subject
- before hash
- intended after hash
- observed after hash
- snapshot coverage
- workspace revision
- workspace snapshot id
- sync class
- recovery resolution

Stale reasons:

```rust
enum VerificationStaleReason {
    WorkspaceChanged(EventId),
    CheckSpecChanged(EventId),
    PolicyChanged(EventId),
    EnvironmentChanged(EventId),
    SandboxChanged(EventId),
    TrustChanged(EventId),
    UnknownDirty(EventId),
}
```

## 14. Sync Class

Mutation events are recovery-critical.

Rules:

- `MutationPrepared`, `MutationCommitted` and `MutationReconciled` require `RecoveryCritical`.
- File content and parent directory sync are required before `MutationCommitted`.
- Batch started/finished are recovery-critical when any child operation is recovery-critical.

## 15. Test Matrix

Required deterministic tests:

- single-file prepare then commit success
- `WriteCommitted` references `MutationCommitted` and is deduplicated by `operation_id`
- compare-and-swap conflict prevents write
- crash after prepared before file write reconciles to not applied
- crash after file write before commit reconciles to committed if hash matches intended
- crash with different observed hash reconciles to conflict/unknown dirty
- sensitive file skipped snapshot cannot be restored
- multi-file batch partial success records per-file outcomes
- restore emits `CheckpointRestored` and creates new workspace revision and snapshot id
- shell post-scan detects workspace mutation
- unfinished write-capable shell on load emits precise mutation or unknown dirty
- unavailable scan produces unknown dirty
- symlink escape marks snapshot entry external
- permission denied entry prevents clean snapshot
- workspace revision is not compared across worktrees
- verification stale reason references invalidating event id

## 16. Open Questions

- Exact location and retention policy for mutation artifacts.
- Exact handling of very large files in snapshot manifests.
- Whether file mode should be included on all platforms or only executable bit.
- Whether directory-level mutation subjects are needed for first implementation.
