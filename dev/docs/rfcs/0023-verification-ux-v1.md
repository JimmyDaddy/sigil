# RFC-0023 Verification UX V1

状态：accepted / implementation in progress

创建日期：2026-07-13

基线：

- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Related: [RFC-0018 Plan-to-Task Handoff](0018-plan-to-task-handoff.md)

## 1. Summary

RFC-0003 已经提供可信 check、snapshot-bound receipt、stale invalidation、check-run lifecycle 和 reducer。当前 TUI 能显示 `run check <id>`、状态与简短失败原因，但用户仍缺少一个可执行、可定位且可追溯的 verification workflow。

Verification UX V1 将把现有事实组织为一个 task/session 内的用户流程：推荐下一项检查、精确重跑一项检查、定位失败证据，以及查看 receipt 与 changeset/snapshot 的已证明关系。

## 2. Goals

1. 在当前 task 或 session 上显示一项明确、可执行的推荐检查，并说明它为什么被推荐。
2. 允许用户只重跑一个已受信任的 check，不借此自动执行 repo-discovered candidate。
3. 将失败定位到 check id、退出/超时原因、durable event 和可用输出/产物，而不是只显示泛化失败标签。
4. 将可用的 verification receipt 明确关联到其验证的 workspace snapshot；存在同一 scope 的已应用 changeset 时，建立 append-only、不可猜测的 link。
5. 保持 TUI-first；CLI/protocol 可以以后复用同一 command，但 V1 不以新增命令作为普通用户主入口。

## 3. Non-goals

- 不让模型 final text、用户手写说明或 child summary 替代 verification receipt。
- 不把 candidate check 自动提升、自动执行，或把 workspace trust 当成无条件 shell 权限。
- 不让一次 rerun 覆盖 check spec、scope、trust、sandbox 或 workspace snapshot 的漂移。
- 不把 mutating check 伪装成 passed；它仍必须由后续 non-writing check 证明。
- 不引入新的验证执行器、第二套 session log，或跨 session 的 receipt 复制。

## 4. Product Flow

```text
task/session needs verification
  -> Verification card: "Recommended: cargo-test"
  -> Why: required by current task / stale after workspace change / retry failed check
  -> Enter: run this exact trusted check
  -> queued / running / terminal result
  -> failure: open concise evidence location
  -> success: show receipt, current snapshot, and linked changeset when proven
```

“Recommended” has a deterministic priority:

1. current `RequiredAction::ReRunNonWritingCheck`;
2. current `RequiredAction::RunCheck`;
3. a failed/inconclusive current-scope check eligible for retry;
4. current `RequiredAction::ApproveCheckExecution`, which routes to one-time promotion rather than execution.

Repo-discovered candidates appear only as reviewable candidates. They never become a recommended executable check until existing promotion/trust policy creates a matching `TrustedCheckSpec`.

## 5. Durable Model

V1 reuses `VerificationCheckRun`, `VerificationRecorded`, `ReadinessEvaluated`, `CheckSpecRecorded`, `ChangeSetApplied`, and `WorkspaceSnapshotId`.

It adds two small append-only records only where the existing receipts cannot state the user-facing relationship without inference:

```rust
struct VerificationReceiptLinkRecorded {
    receipt_id: ReceiptId,
    receipt_event_id: EventId,
    scope: EvidenceScope,
    workspace_snapshot_id: WorkspaceSnapshotId,
    changeset_id: Option<ChangesetId>,
    changeset_apply_event_id: Option<EventId>,
}

struct VerificationFailureLocatorRecorded {
    check_run_id: VerificationCheckRunId,
    receipt_id: Option<ReceiptId>,
    command_event_id: Option<EventId>,
    output_artifact_id: Option<ArtifactId>,
    summary: String,
}
```

The link is emitted only after validating that the receipt is applicable to the exact current scope and snapshot. A changeset link is emitted only when a durable applied-changeset record has the same workspace lineage and precedes the receipt. Missing evidence stays `None`; the UI must say “not linked” rather than imply a relation.

## 6. Execution and Safety

Manual rerun resolves the current scope and exact `TrustedCheckSpec` from the verification projection. It uses the existing execution backend, policy hash, trust decision, timeout and check-run lifecycle. It appends queued/running/terminal records before exposing a final card.

The kernel rerun request carries only the task/step identity, check id/hash, policy hash and observed snapshot id. The runtime supplies the authoritative workspace root separately; a UI or protocol payload cannot redirect a task-bound check into another directory. Preflight rejects scope, policy, spec, snapshot, duplicate-running and already-satisfied bindings before appending `Queued`.

If current state has drifted since the card was rendered, the worker rejects the request and refreshes the projection. It never runs a lookalike id, stale hash, untrusted candidate, or a check in another task/session scope.

## 7. TUI Surface

The default task/session surface shows one compact Verification card, not a new command palette:

```text
Verification · needs attention
Recommended  cargo-test
Why          source changed after the last check
Enter        run check
I            inspect failure / receipt
```

Existing keyboard-help metadata, focus routing, mouse hit testing, narrow layouts and session audit must update together. `I` is illustrative until command metadata and conflict review select the final key; V1 may instead reuse the existing detail focus action. Raw receipt ids, snapshot ids and event ids stay in inspect/audit views rather than the compact card.

## 8. Acceptance Criteria

- A required trusted check is visible as a recommendation and runs only after an explicit TUI action.
- Running or queued checks suppress duplicate action; a terminal failed/inconclusive check exposes exact rerun.
- Failure inspect identifies the check, terminal reason and durable evidence location; no fabricated output is shown.
- A passed receipt shows its exact snapshot; a changeset link is shown only when durable evidence proves it.
- Existing stale/mutating/trust/sandbox semantics continue to pass reducer and runner tests.

## 9. Implementation Order

1. Build a projection-backed verification action/view model and deterministic recommendation policy.
2. Extract exact-scope manual rerun through existing task verification runner and durable lifecycle events.
3. Add failure locator and receipt-link records/projections with recovery tests.
4. Render the card, inspect surface, input/mouse routing and help metadata.
5. Add TUI runner E2E and update user documentation.
