# RFC-0063 实现完整性审计与代码评审

日期：2026-08-05  
范围：当前 worktree 中 RFC-0063 的全部未提交实现（kernel/runtime、HTTP/SSE、TUI、Desktop、配置、rollout、eval、文档与契约）。  
方式：静态链路审查 + 定向回归验证；不修改产品实现、不 stage、不提交。

## 总结

发现 2 项 P1、1 项 P2；未发现 P0。

第四轮修复的 active-run 注册回滚本身是正确的：重复 run-id 和 foreground bind 失败两条回归均通过，且新 helper 不再误释放其他 run 的 foreground slot。但 RFC-0063 还不能进入 release validation：Desktop 的 `Revise` 成功响应会被 native client 的严格 DTO 拒绝；而 shared public projection 不能表达无草稿的 `Planning` 或终态，违反跨 surface/reload 恢复契约。另有 spawn 拒绝后的 durable decision 孤儿状态，当前只证明 session slot 可用，未证明原 plan 可恢复。

## 详细问题

### P1：Desktop 的 `Revise` 会在 native client 处拒绝真实成功的 HTTP 响应

证据：HTTP receipt 在 `crates/sigil-http/src/dto.rs:3697-3730` 为 `Revise` 返回 `revision_run_id`，并且 RFC 要求客户端以该 run identity 订阅和跟踪。`crates/sigil-desktop/src/dto.rs:2615-2628` 的 `DesktopPlanDecisionCommandReceipt` 使用 `deny_unknown_fields`，却没有该字段；`apps/desktop/src-tauri/src/commands.rs:1334-1348` 和 `apps/desktop/src/types.ts:787-796` 也没有投影它。于是 `DesktopClient::plan_decision` 在 `crates/sigil-desktop/src/client.rs:772-790` 反序列化服务器的成功 JSON 时会因未知 `revision_run_id` 失败。服务器已在返回 receipt 前启动 revision（`crates/sigil-http/src/production_driver.rs:2987-2998`），Desktop 却把它显示为失败。

影响：Desktop 的 Revise 操作不可用且会产生“失败但后台实际运行”的错误体验；用户也无法得到 child run identity 以跟踪、取消或解释该 revision。这直接违背 RFC §9.2 所要求的 typed client、React interaction 与 public run lifecycle 同步。

建议：将 `revision_run_id: Option<String>` 端到端加入 `sigil-desktop` DTO、Tauri IPC、React `PlanDecisionSummary`，并由 `ConversationPanel` 消费它以追踪 revision 生命周期。补一个真实 HTTP JSON -> `DesktopClient::plan_decision` 的 contract test，以及一个 Desktop Revise interaction test；两者都必须包含非空 `revision_run_id`。

### P1：Shared public projection 丢失 `Planning` 与无草稿终态，reload/reconnect 不能恢复 PlanReview lifecycle

证据：`PlanReviewDisplayProjection::into_public` 在 `crates/sigil-runtime/src/conversation_display.rs:1850-1852` 无条件要求 latest attempt 已有 `PlanDraftCreated`；`Started`、`Failed`、`Interrupted`、`Cancelled`、`CompletedWithoutDraft` 都没有 draft，因此直接返回 `None`。但 public DTO 明确定义这些状态（`crates/sigil-kernel/src/public_task_event.rs:133-148`；`crates/sigil-desktop/src/dto.rs:2650-2657`），native client 测试也构造并接受无操作的 `started` 状态（`crates/sigil-desktop/src/tests/client_tests.rs:631-655`）。RFC §8.2 要求 shared projection 表示 `Planning`，§9.1/§9.2 要求 TUI 和 Desktop 显示 Planning，Desktop 在 reconnect/reload 从 durable projection 恢复 pending Plan。

影响：plan review 已 durable `Started`、尚未提交草稿时，重新打开会话只能看到没有 plan review；执行失败或取消且没有草稿时，用户也看不到可解释的 terminal lifecycle。这不是单纯 live UI 缺少 loading，而是 durable/public projection 缺失，导致 Desktop/TUI 恢复语义不一致。

建议：把 public plan review 设计为可表达无草稿 attempt 的状态联合（draft-specific hash/summary/counts 仅在 `DraftReady` 存在，或提供 host-derived safe placeholders），并让 display projection 对 latest attempt 先投影 status，再按是否有 draft 投影详情。新增跨 surface/reload 测试：Started 无 draft、Failed 无 draft、Cancelled 无 draft 都能通过 HTTP/desktop typed client 展示正确的无 action 状态；DraftReady 仍保持现有 actions/stale 语义。

### P2：revision spawn 在 durable `RevisionRequested` 之后失败时，旧 Plan 会进入无可恢复 decision 状态

证据：`prepare_plan_review_revision` 先 append `PlanDecision::RevisionRequested`（`crates/sigil-runtime/src/plan_review_coordinator.rs:568-578`），随后 HTTP driver 才尝试 active-run 注册和 foreground bind（`crates/sigil-http/src/production_driver.rs:433-458`）。重复 run-id 的现有测试明确模拟“decision 已持久化后 spawn 被拒绝”（`crates/sigil-http/src/tests/production_driver_tests.rs:3828-3867`），但只断言 session mutation slot 没被占用（:3869-3882）。之后任何 Plan decision 都被 `prepare_plan_review_revision` / `record_plan_decision` 的 existing-decision guard 拒绝（`crates/sigil-runtime/src/plan_review_coordinator.rs:531-537`、:616-625）；display 又只隐藏 Accepted/Rejected，仍可向用户展示原 DraftReady 的四个 action（`crates/sigil-runtime/src/conversation_display.rs:1857-1880`）。

影响：一旦该 pre-spawn failure 发生，原 plan 仍可能显示 Run/Save/Revise/Reject，但所有 action 都会返回“already has decision revision_requested”；既无 revision attempt terminal record，也无重试或退出动作。第四轮修复了“会话永久被 foreground slot 卡住”，但未修复“plan 决策永久不可操作”。

建议：将可失败的 spawn preflight 移到 durable `RevisionRequested` 之前，或为 pre-spawn failure 追加可被 projection/retry 消费的 durable recovery/terminal fact，并明确同一 revision identity 的安全重试规则。扩展 `production_revision_duplicate_registration_never_blocks_the_session`：除检查 slot 外，还应断言用户可以恢复为可操作的旧 draft，或通过定义的 retry/terminal UI 路径完成恢复。

## 已确认的实现与验证

- `rollback_revision_run_registration` 仅回滚本调用插入的 active-run entry，bind 失败不 unbind 别人的 foreground slot（`crates/sigil-http/src/production_driver.rs:604-615`）。
- 定向通过：
  - `cargo test -p sigil-desktop plan_review_surface_validates_bounded_typed_decisions`
  - `cargo test -p sigil-http production_revision_duplicate_registration_never_blocks_the_session`
  - `cargo test -p sigil-http production_revision_bind_failure_rolls_back_only_the_run_registration`
  - `env -u SIGIL_API_KEY cargo test -p sigil-runtime --lib plan_review`（16 passed）
  - `pnpm --dir apps/desktop check`：本次确认 `contract:check` 与 `ui:check` 通过；完整 renderer gate 的全绿结论采用本轮提交方提供的 `pnpm check + vitest 276/276` 记录，未将其作为本审计独立复跑证据。
- `git diff --check` 无输出。

## 测试覆盖缺口

1. 没有 HTTP `Revise` receipt 到 `sigil-desktop` strict DTO 的解码契约测试，因此 server OpenAPI/generation 已含字段而 native boundary 漏字段仍可通过全量 gate。
2. 没有从 durable `PlanReviewAttempt(Started|Failed|Interrupted|Cancelled)` 生成 public display 的测试，更没有 Desktop/TUI reload/reconnect 对照测试。
3. 现有 duplicate-registration 测试只覆盖 slot 不泄漏；未断言既已写入的 `RevisionRequested` 对 plan action/retry/recovery 的完整语义。
4. RFC 本身仍正确标为 `implementation-in-progress`：real-model campaign、current-source Desktop Gherkin E2E、PTY acceptance 与 release 复验尚未完成；这些是 release gate，不与上述代码 finding 混淆。

## 复核结论（2026-08-05）

整体结论：**部分认可**。第五轮已正确闭环原报告的 2 项 P1 与 1 项 P2；未发现这些原 finding 的残留阻塞。但复核发现 1 项新的 P2：成功 Revision 从 durable `RevisionRequested` 到 child attempt `Started` 的短暂窗口仍向 reload/reconnect 投影旧草稿的四个 action，用户可见为可点击但服务端必然拒绝的 action。

### 原 P1：Desktop `Revise` strict DTO

**认可。** `revision_run_id` 已进入 `crates/sigil-desktop/src/dto.rs:2618-2631`、Tauri IPC `apps/desktop/src-tauri/src/ipc.rs:1908-1919` 和 React `apps/desktop/src/types.ts:787-796`。`ConversationPanel` 在取得非空 identity 后给出 started notice（`apps/desktop/src/ConversationPanel.tsx:1697-1708`）。真实 loopback HTTP decode 回归 `plan_decision_revise_accepts_the_supervised_revision_run_identity` 已通过。

### 原 P1：无草稿 attempt 的 shared public projection

**认可。** `crates/sigil-runtime/src/conversation_display.rs:1846-1900` 现在先投影 latest attempt，再按 draft 是否存在填充 optional 详情；无草稿状态无 action。runtime durable reload 回归 `plan_review_attempt_without_draft_still_projects_its_terminal_status` 已通过，覆盖 Started、Failed 与 Cancelled。

### 原 P2：spawn 拒绝后 `RevisionRequested` 孤儿

**认可。** `PlanDecision::RevisionFailed` 是 append-only 的 recoverable durable fact（`crates/sigil-kernel/src/plan.rs:149-166`）；`record_plan_decision` 与 revision prepare 对其显式放行（`crates/sigil-runtime/src/plan_review_coordinator.rs:505-580`、:591-695）。production driver 在 spawn error 后追加该事实（`crates/sigil-http/src/production_driver.rs:2979-3015`）。扩展后的 duplicate-registration 回归验证 reload 后 `RevisionFailed` 存在且 Save 成功，已通过。

### P2：成功 Revision 在 attempt 尚未 `Started` 前仍将旧 DraftReady action 投影为可用

**证据。** `prepare_plan_review_revision` 先 durable append `RevisionRequested`（`crates/sigil-runtime/src/plan_review_coordinator.rs:570-580`），随后 HTTP driver 才 `spawn` child worker（`crates/sigil-http/src/production_driver.rs:471-490`）。在 worker 写入新 attempt `Started` 前，`PlanReviewDisplayProjection::into_public` 仍以旧 attempt 为 latest；它只隐藏 `Accepted`/`Rejected`，并且会对旧 `DraftReady` draft 返回 Run/Save/Revise/Reject（`crates/sigil-runtime/src/conversation_display.rs:1850-1887`）。`ConversationPanel` 收到成功 receipt 后立即清卡并触发 display reload（`apps/desktop/src/ConversationPanel.tsx:1697-1708`），因而 race 可在当前 surface 出现；reconnect/reload 也会复现。与此同时 runtime 对 `RevisionRequested` 明确拒绝下一条 action（`crates/sigil-runtime/src/plan_review_coordinator.rs:531-538`、:618-630）。

**影响。** 成功的 revision 尚未开始时，用户可能重新看到旧 Plan card 的四个可点击 action；再次点击只会得到 `already has decision revision_requested`。这不会越权，但违反同一 durable projection 派生 action 的一致性，并在慢调度、重载或重连时造成明显的错误操作入口。

**建议。** 让 public projection 显式表达 `RevisionRequested` 的过渡态（例如 `revision_pending`），或至少在该 durable decision 为 latest 时将旧 draft 的 `allowed_actions` 清空并在 Desktop/TUI 展示“revision starting”。不要重回在 spawn 前写 `Started` 的旧方案。补一条可控调度测试：持久化 `RevisionRequested`、故意阻塞 child executor 后，HTTP/desktop reload 的 projection 必须没有可提交 action；`RevisionFailed` 后 action 恢复，新 attempt `Started` 后显示 Planning。

### 本次复核验证

- `cargo test -p sigil-desktop plan_decision_revise_accepts_the_supervised_revision_run_identity`：通过。
- `cargo test -p sigil-runtime plan_review_attempt_without_draft_still_projects_its_terminal_status`：通过。
- `cargo test -p sigil-http production_revision_duplicate_registration_never_blocks_the_session`：通过。
- `pnpm --dir apps/desktop exec vitest run src/App.test.tsx -t 'disables run and save but keeps revise and reject for a stale draft'`：通过（1 passed）。
