# RFC-0027 Local Session Lifecycle V1

状态：accepted / P27.1 implemented / P27.2-P27.6 planned

创建日期：2026-07-15

基线：

- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0024 TUI Checkpoint / Rewind V1](0024-tui-checkpoint-rewind-v1.md)
- Follows: [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)

## 1. Summary

本 RFC 为单 workspace 的本地 session 提供 fork、export、delete 与 retention。TUI 继续是主入口；runtime 提供可被 TUI、CLI adapter 和未来本地 server 复用的服务，kernel 只承载 provider-neutral 的 durable turn/fork contract。

所有破坏性操作都采用 exact preview 到 apply，活动 session fail closed，并把计划与结果写入独立 append-only lifecycle journal。普通 agent run 不隐式删除 session。

## 2. Goals

1. 任意已 finalized 的完整 user turn 都可以 fork，不再要求该 turn 必须产生文件 mutation。
2. 默认导出安全、可移植的 transcript artifact，不复制内部 control state、provider carrier、approval secret 或 raw continuation blob。
3. 删除 session 前绑定 canonical path、content hash、size 与 preview digest，并验证 writer lease；当前或活动 session 不可删除。
4. retention 提供只读 preview 与显式 apply，按 age/count/bytes 选择候选，同时保护当前 session、活动 session 与用户 pin。
5. export/delete/retention 的计划和终态进入 workspace-scoped append-only lifecycle journal，可在 crash 后区分 completed、not applied 与 uncertain。
6. session browser 提供 TUI-first 的详情与操作面，不把生命周期能力只做成隐藏命令。

## 3. Non-goals

- 不读取或迁移旧 `SessionLogEntry` raw JSONL；V1 只支持当前 V2 durable stream。
- 不同步云端 session，不实现多用户 tenancy、跨 workspace 索引或后台 daemon cleanup。
- 不把 conversation fork 描述为 workspace/worktree fork；文件、shell 和远程副作用仍然共享且不会被撤销。
- 不默认导出完整内部 audit stream；developer raw archive 不是 V1 用户入口。
- 不在普通启动、run、resume 或 serve 路径隐式执行 retention。
- 不在 writer lease、preview digest 或 lifecycle journal 不可证明时 best-effort 删除。

## 4. Durable Conversation Fork Contract

`ConversationForkProjection` 从 V2 stream 投影 finalized user turns。每个 `ConversationForkPoint` 绑定：

- source session id；
- user boundary event id / sequence；
- `RunFinalized` event id / sequence；
- 对以上字段计算的 stable turn digest。

fork 只复制选定 finalization 之前的 safe-persisted `User`、`Assistant`、`ToolResult` 和与这些 message 对应的 external provenance；不复制 task、approval、queue、mutation、verification、continuation 或其他 control state。destination 先写 `SessionIdentity` 与 `ConversationForked`，再写 safe prefix。checkpoint fork 与普通 turn fork 复用同一底层实现；checkpoint id/digest 仅作为可选额外 provenance。

## 5. Local Session Catalog and Export

runtime catalog 只接受 configured session directory 的直接子文件、当前命名约束和 V2 stream。目录 symlink、越界 path、unsupported stream、超出 scan limit 的文件都显式标记或拒绝，不能被静默当作可操作 session。

`SessionExportV1` 是原子写入的 JSON artifact，至少包含：schema version、workspace id、source session id/ref、source content hash、export time、provider/model display identity、safe transcript entries、external source/citation 的安全投影与 artifact digest。默认省略 control entries、tool arguments、approval payload、provider-private state、raw URL secret carrier 和 durable internal envelope。导出 destination 默认位于 workspace state 的 `session-exports/`，显式外部 path 也必须执行 symlink/overwrite 防护。

export artifact 本身是 portable receipt；成功后 lifecycle journal 追加 `export_completed`，只记录 source binding、destination ref/hash 和计数，不重复保存 transcript。

## 6. Delete and Lifecycle Journal

workspace lifecycle journal 位于 workspace state，独立于 configured session directory，采用 versioned JSONL、单 writer lease、严格 sequence 与 previous-record hash。每次破坏性操作先追加 planned record，文件 mutation 完成并 sync parent 后追加 completed record。

delete preview 绑定 source canonical path、session id、bytes、mtime、content hash 和 preview digest。apply 必须：

1. 拒绝 current/protected path；
2. 重新 canonicalize 并确认仍是 catalog direct child；
3. 对 `.writer-lock` 和 data file 获取非等待独占 lease；
4. 重新验证 source binding 与 preview digest；
5. append `delete_planned` 并 fsync journal；
6. remove data file 与 stale writer-lock，sync session directory；
7. append `delete_completed`。

planned 已落盘但 completed 缺失时，recovery 根据 source 是否仍存在投影 `not_applied` 或 `uncertain`，不得自动重试删除。

## 7. Retention

`SessionRetentionPolicy` 使用可选 `max_sessions`、`max_bytes`、`expire_older_than_ms`。默认建议为 500 个 session、2 GiB、180 天，但只影响显式 preview/apply。候选按最旧优先稳定排序；age 条件先选，再从剩余 session 中选择满足 count/bytes 上限所需的最小旧集合。

以下对象永不进入候选：current session、调用方提供的 active/protected paths、用户 pin、catalog invalid/unsupported entry，以及无法取得 lease 或完整 digest 的 session。apply 接受完整 preview digest；任何候选发生 drift 时整批在首个删除前失败。成功 apply 为每个文件写 planned/completed，并追加 batch terminal summary。

## 8. TUI Surface

`/resume` session browser 继续负责筛选和 resume。对选中 session 使用 `Ctrl-O` 打开独占焦点的 Session Actions modal，展示 title、时间、大小、turn 数、fork availability 与 protected 原因：

```text
Enter   resume selected session
F       fork latest finalized turn and switch to destination
E       export safe transcript
D       preview delete; Enter confirms exact preview
Esc     close without mutation
```

Retention 放在 `/config` Storage maintenance 中，先显示 policy、protected/candidate 数、预计释放 bytes，再由独占确认 modal apply。异步操作携带 request id；modal 关闭、选择变化或刷新后，迟到回包不能作用到新目标。所有 action 保留 composer draft，错误留在 modal，不让按键穿透 composer。

## 9. Implementation Slices

1. P27.1：finalized-turn fork projection、stable digest、通用 fork primitive，checkpoint fork 复用同一实现。
2. P27.2：bounded local session catalog、V2 validation、safe export artifact 与 atomic writer。
3. P27.3：append-only lifecycle journal、delete preview/apply、writer lease 与 crash recovery projection。
4. P27.4：retention config/policy、pin/protected set、batch preview/apply。
5. P27.5：TUI Session Actions、Storage maintenance、worker request-id flow、mouse/help metadata。
6. P27.6：process/worker E2E、EN/ZH docs、full audit。

每个 slice 独立提交。P27.3 不得与 P27.4 合并，避免单 session 删除安全性和批量策略选择在同一 commit 中失去可审查边界。

## 10. Acceptance Criteria

- 无文件 mutation 的 finalized turn 可以 fork，父 stream byte-for-byte 不变，destination 不继承 active control state。
- export 中不存在 control envelope、credential、provider-private continuation 或未经 SafePersist 的 external carrier。
- current/active、symlink、越界、legacy/invalid 和 drifted session 均不能删除。
- delete 在 durable planned record 前不动源文件；completed record 只在 remove 与 parent sync 成功后出现。
- retention preview 确定性、只读；apply 对完整 candidate set 做 preflight，普通 run 不触发 cleanup。
- TUI 键盘和鼠标都能完成 fork/export/delete preview；composer 不抢焦点。
- targeted tests、workspace fmt/check/test/Clippy、docs/link/mirror 与 diff gate 通过。

## 11. Progress

- P27.1 complete：新增 finalized-turn projection/digest 与通用 turn fork；不再依赖 controlled mutation checkpoint。原 checkpoint workflow 保留并复用同一 safe-prefix creator，fork provenance 明确区分 turn binding 与可选 checkpoint binding。
