# RFC-0002 Crash-consistent Mutation Protocol

状态：RFC core semantics implemented / productization remains

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
    file_metadata: Option<FileMetadataEvidence>,
    symlink_target: Option<PathBuf>,
    state: SnapshotEntryState,
}

struct FileMetadataEvidence {
    platform: FileMetadataPlatform,
    readonly: bool,
    unix_mode: Option<u32>,
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
- The manifest hash covers `workspace_id`, `scope_hash`, all entries, paths, entry states, content hashes, legacy Unix `mode`, comparable `file_metadata` and symlink targets.
- `mode` is a legacy Unix-only compatibility field and must not be filled on non-Unix platforms.
- `file_metadata.readonly` is the cross-platform comparable permission bit. Unix/macOS entries also record `unix_mode`; Windows and other non-Unix platforms record `readonly` without pretending to have Unix mode.
- Snapshot ids are stable for the same platform metadata coverage. Cross-platform snapshot ids may differ when one platform can observe metadata another platform cannot; verification validity remains bound to the actual `WorkspaceSnapshotId` produced in that workspace.

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

## 16. Implementation Progress

当前进度：

- 已新增 mutation domain 类型：`MutationPrepared`、`MutationCommitted`、`MutationReconciled`、`MutationBatchStarted`、`MutationBatchFinished`、`WriteCommitted`、`SnapshotCoverage`、`MutationSubject` 和 workspace revision/snapshot 绑定。
- 已实现 `MutationCoordinator` / recorder 基础路径，受控文件 mutation 先 append prepare，再执行写入，最后 append commit/write receipt。
- 已实现单文件 controlled write 的 workspace confinement、before hash、intended after hash、CAS 检查、temp file + atomic replace、file/parent sync 和 observed hash。
- 已实现受控单文件 mutation 的最小 artifact capture：已有非敏感文件会在 prepare 阶段将旧内容写入 recorder artifact root，并以 `SnapshotCoverage::Captured` 引用；secret-like 文件默认记录 `SkippedSensitive`，不保存旧内容；legacy workspace `.sigil/sessions` 日志默认不会把 artifact 写回仓库 `.sigil`。
- 已加固 mutation artifact 写入：blob 使用 temp-write + fsync + rename，复用既有 blob 前会校验内容 hash，截断或损坏时会重写。
- 已新增 mutation artifact lifecycle 审计原语：artifact 删除、过期或内容不可用会追加 `MutationArtifactLifecycleRecorded`，历史 mutation event 不会被改写。
- 已实现通用 mutation artifact retention / quota 扫描器：按 artifact metadata 扫描 artifact root，支持 age、count、byte quota，过期和不可用内容都通过 `MutationArtifactLifecycleRecorded` 追加审计事件，不静默删除。
- 已新增用户可见 mutation artifact retention 默认配置：`StorageConfig.mutation_artifact_retention` 默认保留最多 10,000 个 artifact、512 MiB、30 天，并可转换为 scanner policy；普通 agent run 不会隐式清理。
- 已在 `/config` Storage 页展示 artifact retention policy 和显式 cleanup 语义，用户能看到当前默认/覆盖值以及 cleanup 会记录 lifecycle event。
- 已补充 `/config` Storage footer 的显式 artifact cleanup action：用户手动触发后会在 worker 空闲时调用 retention scanner，按当前 policy 清理 mutation artifact，并通过 scanner 追加 `MutationArtifactLifecycleRecorded` durable events。
- 已补充 read-only artifact retention preview：`preview_artifact_retention(_at)` 复用 retention selection 但不删除内容、不追加 lifecycle event；`/config` Storage 会展示当前 artifact 数量/大小、cleanup preview 和预计 cleanup bytes。
- 已补充 read-only artifact inventory：`list_mutation_artifacts` 暴露 artifact id、size、created_at、availability、operation ids 和 source paths；`/config` Storage 会展示前几条 artifact 摘要，用户能看到 cleanup 管理的 artifact 来源。
- 已补充 mutation artifact 单项删除的 worker/action 后端能力：`delete_mutation_artifact` 会删除 blob/metadata 并追加 `MutationArtifactLifecycleRecorded(status=Deleted)` durable event；该能力保留给 future advanced/debug surface，不作为 `/config` Storage 普通 footer 主流程。
- 已补充 `/config` Storage selected artifact inspect view：artifact list 下方会展示当前选中 artifact 的来源、大小、可用性和 restore 影响；完整 artifact id、operation ids 和内部引用保留在 durable audit/backend，不进入普通用户主流程。该视图只读取 metadata，不读取 artifact blob 内容，也不会追加 durable event。
- 已补充 artifact maintenance 的低噪声产品化路径：Storage 只在 preview 发现 expired / unavailable / quota-selected artifact 时显示一个 recommended cleanup 提示；没有可清理项时不在 startup 或 `/config` 打开时打扰用户。
- 已补充 cleanup intent durable audit：显式 `clean` 会先追加 `MutationArtifactCleanupRequested`，再逐 artifact 追加 `MutationArtifactLifecycleRecorded`；preview / inventory 仍然只读，不修改 artifact store。
- 已接入受控 `write_file`、`edit_file`、`delete_file` 与 `apply_changeset` 路径；legacy no-recorder 路径保留兼容。
- 已实现多文件 changeset batch id、per-file prepare/commit、batch started/finished 和 apply-stage failure 的 failed batch evidence。
- 已实现最小 checkpoint restore helper：`SnapshotCoverage::Captured` 会读取并校验 mutation artifact，将 restore 作为新的 prepare/commit/write mutation 记录，并追加 `CheckpointRestored`；`SkippedSensitive` / `Unsupported` / `Unavailable` 会 fail closed，不会静默恢复。
- 已实现 load/reconcile helper：prepared without terminal event 可按当前文件 hash 归类为 not applied、committed、conflict 或 unknown dirty。
- 已实现受控 directory mutation evidence：目录创建/删除使用同一 prepare/commit/reconcile 协议；受控写入创建缺失父目录前会先记录 directory mutation，避免 crash 后出现“目录已创建但无 evidence”的状态。
- 已将 committed/reconciled mutation evidence 接入 RFC-0003 readiness，受控写入会使旧 verification stale 或 missing。
- 已实现 unknown mutation detection MVP：`ToolRegistry` 会对 shell/MCP/custom 类未知副作用工具做执行前后 verification-scope workspace snapshot，比对变化后追加 typed `WorkspaceMutationDetected`；`target/**` 等默认构建产物不会污染验证范围。
- 已扩展 `WorkspaceMutationDetected` reason，支持 verification check 这类声明写入但快照未变化的 `DeclaredWriteEffect`，使 replay/audit 不会把写型检查当作 clean no-op。
- 已实现 post-scan unavailable 的 fail-closed evidence：未知副作用工具执行后如果无法重建 workspace snapshot，会追加 `WorkspaceMutationDetected(ScanUnavailable, unknown_dirty=true)`，而不是把已经执行的工具结果静默当作 clean。
- 已将 write-capable `ToolExecutionStarted` 的 `ExecutionMutationProfile` 持久化，并在 session load / interrupted reconciliation 中扫描未终止执行，产生 precise mutation 或 unknown dirty evidence。
- 已将 persistent terminal cancel 路径接入 `terminal_start` 的 `ExecutionMutationProfile`：取消长进程时会对启动前 snapshot 做最终 reconciliation，并按当前 snapshot 去重而不是按 tool call 粗略跳过，覆盖用户主动 cancel 前后的 terminal 写入。
- 已将 persistent terminal 自然退出路径接入终态刷新与 mutation reconciliation：TUI worker 空闲时会读取 active terminal 的 latest status，发现自然退出后追加 `TerminalTask` 终态，并基于 `terminal_start` 的 `ExecutionMutationProfile` 追加 precise `WorkspaceMutationDetected` 或 unknown-dirty evidence。
- 已将 agent-loop `terminal_cancel` 路径接入同一 reconciliation：模型调用 terminal cancel 并返回终态时，kernel 会追加 `TerminalTask` 终态并基于原始 `terminal_start` profile 记录 workspace mutation，避免 cancel 前已发生的写入被 terminal_cancel 前后 scan 漏掉。
- 已将运行中的 persistent terminal 接入 readiness replay：`terminal_start` 返回后只要 `TerminalTask` 仍处于 active 状态，系统会基于启动时的 `ExecutionMutationProfile` 产生 `running_terminal_task` unknown-dirty evidence；旧 verification 不会等到 terminal exit/cancel 才失效。
- 已覆盖 MCP server lifecycle 的最小 unknown-dirty evidence：TUI lazy activation、TUI MCP refresh 和模型可见 `mcp_activate_server` 可通过当前 session recorder 追加 `WorkspaceMutationDetected(tool_call_id=None, unknown_dirty=true)`，避免外部 MCP 进程启动后被误判为 clean。
- 已覆盖 TUI eager MCP startup 的最小 unknown-dirty evidence：worker 启动期间的 eager MCP refresh 会使用当前 session recorder，server 进程成功启动后追加 `WorkspaceMutationDetected(tool_call_id=None, unknown_dirty=true)`。
- 已将 MCP server 启动失败/初始化崩溃路径改为 fail-closed evidence：只要 activation/refresh 尝试启动匹配的 MCP server 且带 mutation recorder，就会在 spawn/initialize/tools-list 结果之前追加 `WorkspaceMutationDetected(tool_call_id=None, unknown_dirty=true)`。
- 已收紧低层 `ToolRegistry::execute` API contract：带 mutation recorder 执行非只读 Shell/MCP/Custom 未知副作用工具时，必须通过 `execute_after_started_audit` 标记调用方已持久化 `ToolExecutionStarted` / `ExecutionMutationProfile`；裸 `execute` 对这类调用 fail closed，只读 Shell 读取路径保持可执行。
- 已确认当前 plugin integration 仅从静态 manifest 产生 agent/skill/hook/MCP registration review data；尚无 plugin hook command execution runtime，plugin-declared MCP server 也未自动并入 active MCP startup/refresh path。未来启用这些 plugin-owned external process 时必须复用 MCP/external-process unknown-dirty recorder。
- 已补充 MCP ready/restart 的最小 TUI 产品化路径：MCP config footer 对 lazy deferred server 仍执行首次 activation，对 eager/ready/failed/stale server 可手动触发 refresh/restart recovery，并复用已有 refresh mutation recorder path。
- 已补充 MCP refresh intent 的最小持久队列语义：手动 refresh 使用 worker 级 pending set；agent registry 暂时 shared 时不会丢失 refresh intent，并通过短 retry interval 避免 tight-loop failure spam。
- 已补充 MCP list_changed health 可见性：server capability 变化会在 TUI 投影为 stale，并给出 refresh queued notice，避免 ready 后健康变化只静默进入后台 refresh。
- 已加强受控 mutation subject 绑定：`MutationCoordinator` 会校验 relative subject 与 absolute target 一致，防止调用方写入 A 文件却记录 B 文件 evidence。
- 已实现 workspace snapshot 大文件 fail-closed 策略：单文件超过 `VerificationScope.max_file_bytes` 时记录为 `Unsupported`，不读取内容、不生成 clean snapshot id，并使 workspace knowledge 进入 `UnknownDirty`；默认值仍为 `MAX_WORKSPACE_SNAPSHOT_FILE_BYTES`。
- 已实现 workspace snapshot 权限拒绝 fail-closed 策略：无法 stat 或无法读取的条目会记录为 `PermissionDenied`，不产生 clean snapshot id，并使 workspace knowledge 进入 `UnknownDirty`。
- 已在 workspace snapshot entry 中记录可比较 file metadata：所有平台记录 `readonly`，Unix/macOS 继续记录真实 `unix_mode` 与 legacy `mode`；Windows/其他非 Unix 平台不会伪造 Unix mode。`WorkspaceSnapshotId` 会覆盖该 metadata evidence，因此权限 metadata 变化会使 verification snapshot 变化。
- 已明确非空目录递归删除不属于当前受控 mutation 协议：`delete_directory_with_mutation` 会在 `MutationPrepared` 前 fail closed，避免 unsupported recursive delete 留下 prepared-without-commit evidence。

Productization remains：

- 启用 plugin hook command runtime 或自动合并 plugin-declared MCP servers 时，必须在 launch/activation 前接入同一 external-process unknown-dirty recorder；当前代码尚不存在这些 plugin-owned process execution 面。
- MCP ready 后进程 health / 自动恢复体验的主路径已落地：`/config` MCP lifecycle 显示 deferred/activating/ready/failed/stale/refreshing，footer 对 lazy server 执行 activation，对 eager/ready/failed/stale server 执行 refresh/restart recovery；`list_changed` 会标记 stale 并 queue refresh，worker pending refresh set 保留 intent 并避免 tight-loop failure spam，activation/refresh 继续记录 MCP lifecycle unknown-dirty evidence。
- artifact lifecycle 主路径产品化已落地：`/config` Storage 展示 recommended cleanup preview、retention policy、低噪声 recommended cleanup 提示、artifact list 摘要和 selected artifact inspect view；footer 只保留一个 `clean` action，逐 artifact delete、cleanup target 切换和 multi-select 只作为 advanced/debug 后端能力，不作为普通用户主流程。显式 cleanup intent 与逐 artifact lifecycle result 都进入 durable audit。剩余工作仅限后续真正引入后台 periodic maintenance 时的禁用开关；当前普通 agent run 不会隐式清理。
- 大文件 fail-closed unsupported 已可按 `VerificationScope.max_file_bytes` 调整；非 Unix file metadata evidence 已补齐到 cross-platform `readonly`，平台专属 metadata 覆盖差异已在 snapshot contract 中说明。
- 递归目录删除仍不属于当前实现；如果未来需要，应单独设计 recursive directory mutation protocol，而不是复用空目录 delete 语义。

## 17. Open Questions

None for core semantics. Productization questions:

None.
