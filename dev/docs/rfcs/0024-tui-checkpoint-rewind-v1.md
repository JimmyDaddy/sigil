# RFC-0024 TUI Checkpoint / Rewind V1

状态：implemented

创建日期：2026-07-13

基线：

- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Related: [RFC-0017 Architecture and TUI Productization](../sigil-capability-roadmap.md)

## 1. Summary

Sigil 已经为受控文件写入记录 `MutationPrepared`、before snapshot artifact、`MutationCommitted`、after hash 和 workspace revision，也已经具备单文件 restore helper、`CheckpointRestored` evidence、artifact retention 与只读 Session Review。当前缺少的是把这些事实组织成用户可以预览、确认和恢复的 TUI workflow。

Checkpoint / Rewind V1 只承诺两类可证明操作：恢复当前 session 中受控文件工具产生的普通文件 mutation；从选定的完整 user turn 创建 append-only conversation fork。它不声称撤销 shell、MCP、网络、数据库、远程服务或其他未知副作用。

## 2. Goals

1. 从 mixed durable stream 投影按 user turn 聚合的 checkpoint，且只把已 committed 的普通文件 mutation 视为可恢复候选。
2. 在任何写入前验证 checkpoint digest、workspace、所有目标当前 hash 与 snapshot artifact 可用性。
3. 对一个 checkpoint 的受控文件提供可审计 batch restore；restore 本身继续产生 mutation records、`CheckpointRestored` 和 verification stale evidence。
4. 在 TUI Session Review 中提供恢复预览、明确确认、冲突说明与 conversation fork。
5. 保持父 session append-only；conversation fork 创建新的 session identity 和 fork provenance，不改写父日志。

## 3. Non-goals

- 不撤销 shell、MCP、plugin、terminal、network、database 或 remote side effects。
- 不使用自动 Git commit、reset 或 worktree 充当 checkpoint。
- 不承诺多文件 restore 是文件系统级原子事务；V1 使用全量 preflight、durable batch lifecycle 和 crash reconciliation。
- 不恢复 directory、rename、hardlink、复杂 symlink、secret-like snapshot、unsupported 或 unavailable artifact。
- 不实现通用 session export/delete/retention；后续 session lifecycle 必须复用本 RFC 的 conversation fork primitive。
- 不新增 `/rewind` 作为普通用户主入口。

## 4. Durable Projection

V1 不新增重复的 `FileSnapshotCaptured` 或第二套 artifact store。`ControlledCheckpointProjection` 从现有 mixed stream 派生：

- `SessionLogEntry::User` 建立 turn boundary；
- `MutationPrepared` 提供 path、before hash、snapshot coverage、workspace id 和 operation identity；
- `MutationCommitted` 证明 mutation 实际完成并提供 after hash；
- `MutationArtifactLifecycleRecorded` 使已清理或不可用 artifact fail closed；
- `WorkspaceMutationDetected` 只形成 unsupported-side-effect warning，不成为可恢复文件；
- `CheckpointRestored` 进入后续 turn evidence，不能改写旧 checkpoint。

同一 turn 内同一路径多次受控写入折叠为一个 file binding：使用最早 committed operation 的 before snapshot，以及最后 committed operation 的 observed-after hash。这样 restore 回到 turn 前状态，同时仍要求当前文件等于该 turn 最后一次已知受控写入。

Checkpoint id 与 digest 由 source session id、turn boundary、ordered file bindings 和 unknown-mutation warning 内容绑定。TUI request 只携带 checkpoint id/digest；worker 必须从当前 session 的 durable records 重新投影并精确匹配，不能信任 UI 提交的 path、artifact id 或 hash。

## 5. Restore Semantics

Restore 分为三个阶段：

1. Preview：重新投影 checkpoint，验证 digest、workspace 和 coverage，列出将恢复的文件以及明确不覆盖的 unknown side effects。
2. Preflight：持有 workspace mutation lease，验证全部目标当前 hash；`Captured` artifact 必须存在且内容 hash 匹配。任何目标失败时，在首个写入前追加 `CheckpointRestoreConflict` 并终止。
3. Apply：追加 `MutationBatchStarted`，顺序执行普通文件 create/update/delete restore。每个成功文件产生新的 prepare/commit/write/`CheckpointRestored` evidence，最后追加 `MutationBatchFinished`。进程中断或中途 I/O failure 由既有 batch/reconciliation 解释，不宣传为原子回滚。

`CheckpointRestoreConflict` 是 recovery-critical durable event，包含 checkpoint id、可选 path、reason 和安全的 expected/actual hash。它不保存文件内容。

Restore 完成后，现有 RFC-0003 reducer 必须把 restore 视为新的 workspace mutation；restore 前的 passed receipt 不再 current，TUI 应引导重新 verification。

## 6. Conversation Fork

Conversation fork 从选定完整 user turn 的结束边界创建新 session：

- 复制该边界之前的 safe-persisted `User`、`Assistant` 和 `ToolResult` history；不复制 source session 的 active task、approval、queue、continuation handle 或 mutation control state。
- 新 session 先写自己的 `SessionIdentity`，再写 `ConversationForked` provenance 和 safe history prefix。
- provenance 记录 parent session ref、source session id、source turn index、source boundary event/sequence；不保存 raw prompt 或 secret carrier。
- 父 session 不变；fork 后继续输入属于新 session。

File restore 与 conversation fork 是两个独立事实。组合操作必须明确：conversation 在新 session 分支，文件 restore 仍作用于当前共享 workspace，不能暗示 workspace 隔离。

## 7. TUI Surface

Session Review 是主入口。V1 提供粗粒度动作：

```text
Review · turn 3/5
Files       2 controlled · restorable
Other       shell changes not included
Enter       preview file restore
F           fork conversation here
I           inspect evidence
```

Restore preview 必须显示文件列表、create/update/delete 方向、unknown-side-effect warning 和不可恢复原因。用户再次显式确认后才发送 worker command。Conflict、sensitive、unsupported、unavailable 和 stale digest 都不能降级为静默部分恢复。

键位 metadata、focus routing、mouse hit testing、narrow layout、session switch、新 session message 和 EN/ZH 文档必须同步更新。

## 8. Acceptance Criteria

- 非 Git workspace 中可以恢复受控普通文件 create/update/delete，并留下新的 append-only mutation evidence。
- 只有 matching prepared+committed operation 进入 checkpoint；prepared-only、failed、directory 与 unknown mutation 不会被误称为可恢复文件。
- 同一 turn 多次修改同一文件恢复到 turn 前内容，并以最后 committed hash 做 CAS。
- 任一目标 drift、artifact 缺失、敏感或 unsupported 时，首个 restore write 前 fail closed，并留下 conflict evidence。
- Restore 后旧 verification passed 不再 current。
- Conversation fork 只复制 safe conversation prefix；父 session、active control state 和 mutation history不被改写或冒充继承。
- TUI 明确说明 shell/remote side effects 不包含在 file restore 中。
- Kernel recovery、TUI render/input 和真实 worker-loop E2E 均覆盖成功与冲突路径。

## 9. Implementation Order

1. `C24.1`：durable checkpoint projection and digest。
2. `C24.2`：restore preview, full preflight and conflict record；`C24.4` conversation fork primitive 可并行。
3. `C24.3`：worker-bound exact batch restore and verification invalidation。
4. `C24.5`：TUI Review actions, confirmation, mouse/help/docs。
5. `C24.6`：recovery, worker E2E and completion audit。

## 10. Implementation Progress

- `C24.1` complete：mixed-stream checkpoint projection、same-path folding、stable id/digest 和
  artifact lifecycle availability 已落地。
- `C24.2` complete：exact restore preview、workspace lease 下的 full preflight、batch restore、
  `CheckpointRestoreConflict` 与 restore verification-stale evidence 已落地。
- `C24.4` complete：complete-turn conversation fork、`ConversationForked` provenance、safe
  message prefix 与 external provenance session-scope rebinding 已落地。
- `C24.3` complete：worker-bound preview/execute/fork command、session reload/switch 与
  scope-aware verification stale projection 已落地。
- `C24.5` complete：`Alt-R` Review focus、双 `Enter` preview/confirm、`F` conversation fork、
  `I` evidence inspect、mouse info-rail focus、narrow timeline fallback 和 EN/ZH 帮助已落地。
- `C24.6` complete：kernel 全量测试、TUI 全量测试/all-target Clippy、worker-loop
  success/conflict/fork E2E、docs gate 与本地 full-audit 均通过；Pages 非 viewport 检查通过，
  viewport gate 因本机 Chrome `--dump-dom` 对最小空白页同样超时而保留为环境限制。
