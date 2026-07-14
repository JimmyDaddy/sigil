# RFC-0003 Verification Contract and Workspace Snapshot

状态：RFC core semantics implemented / productization remains

创建日期：2026-06-25

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0014 Write Isolation and Worktree Merge](0014-write-isolation-and-worktree-merge.md) for future child/worktree merge productization

## 1. Summary

本 RFC 定义 Sigil 的 verification contract：系统如何判定一次 run、task step 或 child agent result 是否已经验证，如何绑定 workspace snapshot，如何处理写入后验证失效，以及如何避免模型 final answer 被误当成完成证明。

核心决策：

1. 执行状态和验证结论分离。
2. Verification verdict 必须由系统 reducer 根据 evidence 计算，不能由模型自报。
3. `Passed` 必须绑定当前验证范围的 `WorkspaceSnapshotId`。
4. 未信任 workspace 只能发现候选检查，不能自动执行仓库脚本或配置。
5. 任何 verification scope 内的 mutation 都会使旧 verification stale。
6. RFC-0002 的 mutation events 是本 RFC 判定 stale、restore 和 merge 后重新验证的硬依赖。

## 2. Goals

- 修正“模型停止调用工具 = 完成”的语义风险。
- 修正 `/task` step 只要 final text 非空就可能 completed 的语义风险。
- 提供 foreground chat、`/task`、subagent 和未来 protocol surface 共享的 completion semantics。
- 定义 check discovery、policy inheritance、workspace snapshot binding 和 stale invalidation。
- 让 TUI 能独立展示 run status 和 verification verdict。

## 3. Non-goals

- 不定义具体测试命令自动推断的完整启发式。
- 不在本 RFC 内实现 sandbox backend；verification 通过 RFC-0005 `ExecutionBackend` 绑定实际 backend、capability 和 sandbox profile hash。
- 不实现 checkpoint restore；restore 后 verification stale 规则由本 RFC 定义。
- 不解决 distributed workflow verification。
- 不让 verifier agent 的自然语言回答覆盖系统 verdict。

## 4. State Model

Execution lifecycle:

```rust
enum RunStatus {
    Running,
    Completed,
    Paused,
    Blocked,
    Failed,
    Cancelled,
    Interrupted,
}
```

Verification verdict:

```rust
enum VerificationVerdict {
    NotEvaluated,
    NotApplicable,
    Pending,
    Passed,
    Failed,
    Missing,
    Inconclusive,
    Stale,
    Skipped,
}
```

Rules:

- `RunStatus` describes execution lifecycle.
- `VerificationVerdict` describes proof status for the relevant workspace snapshot.
- `Pending` means checks are currently running; terminal run states must not keep `Pending`.
- `NotEvaluated` means the system has not yet decided whether verification is required.
- `Passed` requires an applicable verification receipt.
- `Skipped` requires explicit user or policy evidence.
- `Stale` must reference the mutation event that invalidated prior evidence.
- Terminal `RunStatus` values are `Completed`, `Blocked`, `Failed`, `Cancelled` and `Interrupted`.
- New runs or task steps must reduce `Pending` to `Passed`, `Failed`, `Missing`, `Inconclusive`, `Stale`, `Skipped` or `NotApplicable` before entering a terminal run state.
- `NotEvaluated` is allowed for active initial state and legacy/historical projection. It should not be the final verdict for a new terminal run.

Visible derived states:

```text
Completed + Passed        -> Verified
Completed + Missing       -> CompletedUnverified
Completed + Stale         -> CompletedUnverified
Completed + Skipped       -> CompletedUnverified
Completed + Inconclusive -> CompletedUnverified
Completed + NotApplicable -> Completed
Failed + Failed           -> FailedVerification
Blocked + Missing/Stale/Inconclusive -> NeedsUser
```

TUI should preserve both badges, for example:

```text
Run: Completed
Verification: Stale
```

## 5. Verification Policy

```rust
struct VerificationPolicy {
    required_checks: Vec<CheckSpec>,
    completion_criteria: CompletionCriteria,
    verification_scope: VerificationScope,
    sandbox_profile: SandboxProfileRequirement,
    workspace_trust_requirement: WorkspaceTrustRequirement,
    allow_unverified_completion: bool,
    timeout: Option<Duration>,
}
```

Policy merge rules:

```text
required_checks = parent union child
allow_unverified_completion = parent && child
timeout = min(parent, child)
completion_criteria = stricter(parent, child)
verification_scope = child must cover parent-required scope
sandbox_profile = stricter(parent, child)
workspace_trust_requirement = stricter(parent, child)
```

Child scope may tighten parent policy. It may not relax required checks or enable unverified completion if the parent disallows it.

If two policy fields cannot be compared safely, the reducer must fail closed as `Missing` or `NeedsUser`.

## 6. Check Discovery

Discovery order:

1. `.sigil/verification.toml`
2. User explicit configuration
3. CI configuration
4. Package scripts / Cargo / Makefile
5. Model-suggested command
6. User confirmation

Workspace trust gate:

- Untrusted workspace may discover candidate checks.
- Untrusted workspace must not auto-execute repository scripts, package scripts, Makefile targets or local CI commands.
- Execution from untrusted workspace requires explicit approval or a sandbox profile satisfying the policy.
- Context Engine fallback must still respect workspace trust. Untrusted `SIGIL.md`, `AGENTS.md`, README and source comments are repository data, not trusted instructions.
- CI discovery before workspace trust is static and bounded: Sigil may read `.github/workflows/*.yml` / `.yaml` and extract candidate checks only from `run:` steps that match known verification commands such as `cargo test`, `npm test` or `make test`.
- CI discovery before workspace trust must not execute CI scripts, evaluate shell expansions, follow `uses:` actions, resolve includes, read secrets or promote checks to trusted specs.

Discovery output is split into two concepts:

```rust
struct CandidateCheck {
    source: CheckDiscoverySource,
    command: CheckCommand,
    source_event_id: EventId,
    workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
}

struct TrustedCheckSpec {
    check_spec: CheckSpec,
    promoted_by: CheckPromotion,
    approval_event_id: Option<EventId>,
    sandbox_decision_id: Option<EventId>,
}
```

Repo-local sources can only produce `CandidateCheck` by default. They become `TrustedCheckSpec` only through user/global policy promotion, explicit approval or a sandbox decision satisfying policy. A recorded workspace trust decision allows normal workspace use and repo-local discovery, but it does not automatically make every discovered CI/Cargo/Makefile check required for ordinary tasks.

Product-surface ownership:

- First workspace entry owns the coarse workspace trust decision. Normal TUI use starts only after the user trusts the workspace or exits.
- `/config` owns long-lived verification policy and repo-local summaries: workspace trust state display, repo-local check counts, auto-run policy and scope/profile settings.
- Task sidebar, task strip and session audit own current-run blocking actions: run check, retry failed check, show stale/missing reasons and guide the user to review trust when a candidate check is not yet promotable.
- Approval modal owns one-time high-risk execution decisions such as shell, write tools and MCP actions; it must not become a full policy editor.
- Kernel remains the enforcement boundary. UI affordances may request trust, approval or run-check actions, but policy merge, trust staleness, check-spec hash changes and sandbox/approval applicability are computed by kernel state.
- A task surface may link or focus the relevant `/config` review item, but should not duplicate the complete repo-local trust management UI.

## 7. Verification Scope and Snapshot

Verification checks bind to a content snapshot, not to wall-clock time.

```rust
struct VerificationScope {
    include: Vec<PathPattern>,
    exclude: Vec<PathPattern>,
    tracked_files_only: bool,
    generated_roots: Vec<PathBuf>,
}

struct VerificationBinding {
    workspace_id: WorkspaceId,
    workspace_snapshot_id: WorkspaceSnapshotId,
    verification_scope_hash: String,
    check_spec_hash: String,
    environment_fingerprint: String,
    sandbox_profile_hash: String,
    execution_backend: Option<ExecutionBackendKind>,
    execution_backend_capabilities: Option<ExecutionBackendCapabilities>,
    workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    approval_event_id: Option<EventId>,
    sandbox_decision_id: Option<EventId>,
}
```

Default scope:

- Git tracked files.
- Unignored new source files.
- User explicit include paths.
- Exclude `.git`, Sigil state directories, common build/cache directories, dependency caches and generated roots.
- Current default excludes are `.git/**`, `.sigil/sessions/**`, `.sigil/tasks/**`, `.sigil/terminal/**`, `.sigil/cache/**`, `.sigil/artifacts/**`, `.sigil/tmp/**`, `.sigil/input-history.jsonl`, `.sigil-state/**`, `.sigil-recovery/**`, `target/**`, `node_modules/**`, `dist/**`, `coverage/**`, `.pytest_cache/**`, `.env` and `.env.*`.
- Repo-local `.sigil/skills/**`, `.sigil/commands/**`, `.sigil/agents/**`, `.sigil/plugins/**` and `.sigil/verification.toml` are not excluded by default; they are repository inputs and must affect the content-bound workspace snapshot when present.

Rules:

- `WorkspaceRevision` is scoped to one workspace, worktree or snapshot stream.
- `WorkspaceRevision` is not a global order across sessions or worktrees.
- Verification validity should use content-bound `WorkspaceSnapshotId`.
- A receipt is applicable only if `verification_scope_hash` covers the current policy scope.
- A check that modifies files in verification scope produces mutation evidence, not final passed evidence.
- After a writing check such as formatter, fixer, snapshot update or codegen, Sigil must re-run a non-writing check before `Passed`.

V1 snapshot manifest follows RFC-0002:

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
```

If snapshot construction cannot cover the verification scope, the workspace becomes `UnknownDirty` instead of producing a clean `WorkspaceSnapshotId`.

## 8. Evidence Model

Evidence is derived from durable events. It is not a second mutable truth source.

Relevant events:

```text
MutationPrepared
MutationCommitted
MutationReconciled
WriteCommitted
WorkspaceMutationDetected
CheckpointRestored
CommandFinished
CheckFinished
CheckSpecRecorded
DiagnosticRecorded
TodoChanged
VerificationRecorded
VerificationPolicyChanged
VerificationCheckRun
EnvironmentFingerprintRecorded
ReadinessEvaluated
WorkspaceTrustDecision
SandboxDecisionRecorded
ChildVerificationReceiptLinked
ChildChangesetMerged
AgentMergeApplied
```

Receipt minimum fields:

```rust
struct EvidenceReceipt {
    receipt_id: ReceiptId,
    source_session_id: SessionId,
    source_event_id: EventId,
    source_event_type: String,
    scope: EvidenceScope,
    producer_tool_call: Option<ToolCallId>,
    workspace_revision: Option<WorkspaceRevision>,
    workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    policy_hash: Option<String>,
    changeset_id: Option<ChangesetId>,
    status: ReceiptStatus,
    artifact_refs: Vec<ArtifactId>,
    redaction_state: RedactionState,
    recorded_at_stream_sequence: u64,
}
```

`VerificationReceipt` additionally includes `VerificationBinding`.

## 9. Mutation and Staleness

Tool and external effects:

```rust
enum ToolEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}

enum WorkspaceKnowledge {
    Clean(WorkspaceRevision),
    Dirty(WorkspaceRevision),
    UnknownDirty,
}
```

Rules:

- Controlled file tools create precise write evidence.
- Shell, MCP, plugin or external process may create `WorkspaceMutationDetected`.
- If mutation detection is incomplete or untrusted, workspace knowledge becomes `UnknownDirty`.
- Any mutation in verification scope makes previous relevant verification `Stale`.
- Restore is a new workspace mutation and invalidates prior verification.
- Child worktree verification does not transfer to parent after merge; parent must run required checks again.

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

Invalidating event mapping:

- `WorkspaceChanged` references `MutationCommitted`, `MutationReconciled`, `WriteCommitted`, `WorkspaceMutationDetected`, `CheckpointRestored`, `ChildChangesetMerged` or `AgentMergeApplied`.
- `CheckSpecChanged` references `CheckSpecRecorded` or `VerificationPolicyChanged`.
- `PolicyChanged` references `VerificationPolicyChanged`.
- `EnvironmentChanged` references `EnvironmentFingerprintRecorded`.
- `SandboxChanged` references `SandboxDecisionRecorded`.
- `TrustChanged` references `WorkspaceTrustDecision`.
- `UnknownDirty` references `WorkspaceMutationDetected` or the recovery event that produced unknown dirty.

Restore and merge events:

- Resuming a session is not a workspace mutation.
- `LogTailRecovered` is not a workspace mutation by itself.
- `CheckpointRestored` is a workspace mutation.
- `ChildChangesetMerged` or `AgentMergeApplied` creates a new parent workspace snapshot.
- Every stale reason references the invalidating event id and, when available, from/to workspace snapshot ids.

## 10. Readiness Reducer

Inputs:

- `RunStatus`
- `VerificationPolicy`
- `VerificationScope`
- workspace trust state
- latest workspace snapshot
- write and mutation evidence
- check evidence
- approval denial, cancellation, interruption and max-turn events
- trust, sandbox, environment and policy changes

Output:

```rust
struct ReadinessEvaluation {
    run_status: RunStatus,
    verification_verdict: VerificationVerdict,
    visible_state: VisibleCompletionState,
    reasons: Vec<ReadinessReason>,
    required_actions: Vec<RequiredAction>,
}
```

Rules:

- Final assistant text cannot set `Passed`.
- A verifier agent can propose checks or summarize observations, but cannot override system verdict.
- Missing required check yields `Missing`, not `Passed`.
- Check failure yields `Failed`.
- Unknown dirty workspace yields `Stale` when a prior successful receipt exists for the relevant scope, with `VerificationStaleReason::UnknownDirty` referencing the invalidating event.
- Unknown dirty workspace yields `Inconclusive` when there is no prior successful receipt to invalidate, or when the unknown-dirty evidence has no specific event id.
- Ambiguous check output is represented as an inconclusive check receipt; for `AllRequiredChecks` it yields `Inconclusive` and requires a rerun or user resolution.
- Approval denial may yield `Blocked`, `Failed` or `CompletedUnverified`, depending on policy and task state.
- User cancellation yields `Cancelled`; verification verdict remains independent.

## 11. Task and Subagent Integration

Task step completion:

- Step final text is not enough for verified completion.
- Non-blocking tool errors remain evidence and may affect visible state.
- Step status and verification verdict are recorded separately.
- Parent task aggregates child agent receipts, not just child summary text.

Child agent rules:

- Child policy can only tighten parent policy.
- Child write output must produce changeset or mutation evidence.
- Child `Passed` only applies to the child workspace snapshot.
- Merge into parent creates a new parent workspace snapshot and requires parent checks.

Parent-side durable link:

```rust
struct ChildVerificationReceiptLinked {
    parent_session_id: SessionId,
    child_session_id: SessionId,
    child_receipt_id: ReceiptId,
    child_event_id: EventId,
    child_workspace_id: WorkspaceId,
    child_workspace_snapshot_id: WorkspaceSnapshotId,
    policy_hash: String,
    changeset_id: Option<ChangesetId>,
    merge_event_id: Option<EventId>,
}
```

Parent reducers use receipt links or imported receipts. They must not parse child summary text to infer verification.

## 12. TUI and Protocol Surface

TUI should show:

- run badge
- verification badge
- latest applicable check
- stale reason
- missing required checks
- workspace trust warning
- whether command execution required approval or sandbox
- Narrow TUI surfaces compact verification reasons as: first actionable/stale reason plus `+N more`; full session audit still preserves full reason labels in the durable `ReadinessEvaluated` entry.

TUI ownership rules:

- `/config` is a review and policy-management surface, not the control center for every verification action.
- Task and session surfaces should keep the user on the current workflow for run/retry actions; only long-lived trust or policy changes should route to `/config`.
- Single-use approval prompts stay in the approval modal and must write durable approval/provenance events consumed by the verification reducer.

Protocol should expose both:

```text
RunStatus
VerificationVerdict
```

It should not collapse them into a single status field.

## 13. Migration

Initial rollout:

1. Add domain types and reducer behind tests.
2. Project existing sessions into `NotEvaluated` or `NotApplicable` where evidence is unavailable.
3. Record new verification evidence for new runs only.
4. Show unverified completion for writes without evidence.
5. Wire `/task` to use reducer-derived step completion.

Old sessions:

- Must continue to load.
- Must not be retroactively marked `Passed`.
- May show `NotEvaluated` when historical evidence is insufficient.

## 14. Test Matrix

Required deterministic tests:

- pure question maps to `NotApplicable`
- code write without check maps to `Missing`
- successful check after write maps to `Passed`
- write after successful check maps to `Stale`
- terminal run never persists `Pending`
- new terminal run never persists `NotEvaluated`
- receipt scope must cover current policy scope
- child receipt with only local stream sequence is rejected without source session/event id
- formatter modifies source and cannot produce final `Passed`
- user skips check with policy support maps to `Skipped`
- required check failure maps to `Failed`
- untrusted workspace discovers but does not auto-run Makefile/script checks
- untrusted check execution requires approval event or sandbox decision id
- unknown shell mutation maps to `UnknownDirty` and stale verification
- check spec change makes old receipt stale
- policy change makes old receipt stale
- environment change makes old receipt stale
- sandbox profile change makes old receipt stale
- workspace trust change makes old receipt stale
- restore invalidates previous passed verification
- child worktree passed evidence does not transfer after parent merge
- child receipt link survives parent session restore
- final text cannot force `Passed`
- `/task` step with recovered tool error is not silently verified
- cancellation preserves independent verification verdict
- policy inheritance cannot relax parent required checks
- completion criteria, scope, sandbox and trust requirements cannot be relaxed by child policy

## 15. Implementation Progress

当前进度：

- 已新增 `RunStatus`、`VerificationVerdict`、`VisibleCompletionState`，并保持执行生命周期和验证结论分离。
- 已实现 verification policy、policy merge、scope/trust/sandbox requirement、check spec hash 和 receipt applicability。
- 已实现候选检查 discovery：用户全局配置、`.sigil/verification.toml`、CI、package scripts、Cargo、Makefile；未信任 workspace 只产生 candidate，不自动提升为 trusted check；CI discovery 在 trust 前只做静态 workflow `run:` 白名单扫描，不执行 workflow/action。
- 已实现 `WorkspaceSnapshotManifestV1`、scope include/exclude/generated roots、git tracked/unignored snapshot、symlink/external/unsupported/missing entry 处理和 content-bound `WorkspaceSnapshotId`。
- 已收窄默认 exclude：legacy runtime state（`.sigil/sessions/**`、terminal/cache/artifacts 等）、常见 build/cache 目录和 dotenv secret-like 文件仍排除，但 repo-local `.sigil/skills/**`、commands、agents、plugins 和 verification config 会进入 workspace snapshot。
- 已实现 workspace snapshot 大文件 fail-closed：超过 `MAX_WORKSPACE_SNAPSHOT_FILE_BYTES` 的文件标记为 `Unsupported`，不产生 clean snapshot id，避免把未覆盖的大文件误判为 verified。
- 已实现 `run_verification_check` MVP：执行 trusted check、记录 command/check evidence、绑定 snapshot/policy/trust/sandbox/environment hash，并识别写型或自修改 check 不能产生最终 passed evidence。
- 已将写型或自修改 check 接入 RFC-0002 mutation evidence：runner 会追加 `WorkspaceMutationDetected`，使 replay/audit/stale-cause 能看到 check 本身造成或可能造成的 workspace 污染。
- 已实现 readiness reducer：写入无 receipt -> `Missing`，成功当前 receipt -> `Passed`，后续 mutation -> `Stale`，unknown dirty 有 prior passed -> `Stale`，unknown dirty 无 prior passed -> `Inconclusive`，ambiguous check receipt -> `Inconclusive`，失败/skip/cancel/recovered tool error 均有独立语义。
- 已接入 RFC-0002 typed `WorkspaceMutationDetected` evidence：readiness 会消费 detection event 的 scope、from/to workspace snapshot、tool effect 和 unknown-dirty 标志。
- 已接入 RFC-0002 `CheckpointRestored` evidence：checkpoint restore 会作为 workspace mutation 进入 readiness，旧 verification 不会在 restore 后继续视为 current passed。
- 已将 `/task` readiness 的 mutation 判定从 `changed_files` 扩展到 durable mutation evidence：即使 tool result 没有上报 changed files，只要当前 step 的 tool call 在 durable stream 中产生 `MutationCommitted`、`MutationReconciled`、`CheckpointRestored` 或 `WorkspaceMutationDetected`，也会进入 missing/stale/inconclusive verification 状态。
- 已修正 `/task` durable mutation replay 的作用域：除当前 step tool call 外，readiness 也会纳入 task 开始后、且晚于最新 successful verification receipt 的 durable mutation evidence，避免 terminal_start 在前一 step/run、terminal exit/cancel 在后一 step 时被误判为 clean。
- 已将 `/task` durable mutation replay failure 视为 `UnknownDirty`：corrupt/unreadable event stream 会产生 `ResolveUnknownDirty` action，而不是被误判为无需验证。
- 已收紧 `/task` mutation baseline：只允许当前 task/step scope、当前 policy hash、当前 verification scope 和 required check 匹配的 successful receipt 推进 baseline，避免其他任务或其他 scope 的成功检查遮蔽当前任务 mutation。
- 已覆盖 persistent terminal 自然退出产生的 durable mutation evidence：terminal task 自行退出后，TUI worker 会追加终态并 reconcile `terminal_start` 的 workspace snapshot transition，使后续 verification/readiness 看到 terminal 写入。
- 已覆盖 agent-loop terminal cancel 产生的 durable mutation evidence：模型调用 `terminal_cancel` 返回终态时，kernel 会 reconcile 原始 `terminal_start` snapshot，使 cancel 前的终端写入污染旧 verification。
- 已覆盖 persistent terminal 运行中状态：`terminal_start` 已完成但 terminal task 仍 running 时，readiness 会从 durable `TerminalTask` + `ExecutionMutationProfile` 重建 `running_terminal_task` unknown-dirty evidence，避免长进程执行期间被误判为 clean。
- 已覆盖 MCP server lifecycle 的最小 unknown-dirty readiness 输入：TUI lazy activation、TUI MCP refresh 和 `mcp_activate_server` 可追加 `WorkspaceMutationDetected(tool_call_id=None)`，foreground chat 和 `/task` readiness 会将该 evidence 视为 workspace 污染。
- 已覆盖 TUI eager MCP startup 的最小 unknown-dirty readiness 输入：worker 启动期间 eager MCP server 成功启动后会追加 `WorkspaceMutationDetected(tool_call_id=None)`，后续 foreground chat 和 `/task` readiness 不会把该 session 误判为 clean。
- 已覆盖 MCP server 启动失败/初始化崩溃的 readiness 输入：activation/refresh 尝试启动 MCP server 时会先追加 unknown-dirty mutation evidence，即使 initialize 或 tools/list 失败，旧 verification 也会被污染。
- 已补充 terminal task durable projection：active/exited terminal state 可从 V2 durable event stream 重建，readiness 与恢复路径不再只依赖 in-memory entries-based projection。
- 已补充 changeset durable projection：`ChangeSetProposed` / `ChangeSetApplied` 可从 V2 durable event stream 重建，为后续 child/worktree merge review 与 parent re-check trace 提供 durable projection 基础。
- 已补充 plan/skill/plugin durable projection：plan approval、skill load 和 plugin trust/context 状态可从 V2 durable event stream 重建，减少 workspace trust 与 extension context 对运行时临时状态的依赖。
- 已补充 agent profile trust/policy、agent result continuation 和 conversation queue durable projection：profile trust/policy、child result continuation 和 queued input state 可从 V2 durable event stream 重建，进一步降低 resume 后 verification / trust / child-result UI 对 entries-only projection 的依赖。
- 已将 child/agent merge 类 durable event 接入 `/task` readiness 的 mutation replay：`ChildChangesetMerged` / `AgentMergeApplied` 会使 parent workspace verification stale 或 unknown-dirty，child worktree 的 passed receipt 不会在 merge 后直接转移为 parent passed。
- 已将 foreground chat final answer 接入 readiness：普通 chat run 结束时会追加系统计算的 `ReadinessEvaluated`；无 workspace mutation 时为 `NotApplicable`，存在 mutation 且缺少适用 receipt 时为 `Missing` / `CompletedUnverified`，不会把 final text 视为 verified。
- 已将 `/task` step completion 接入 readiness：final text 不能直接证明 verified，missing check 会阻断/降级，RunCheck action 可执行 trusted check 后重算 readiness。
- 已将 check runner lifecycle 进入 append-only control/audit：`RunCheck` 会记录 queued、running 和 terminal `VerificationCheckRun` entry，projection 保留每个 run 的最新状态；最终 proof 仍由 `VerificationRecorded` receipt 决定。
- 已收紧 repo-local check promotion：workspace trust 不再自动把 CI/Cargo/Makefile discovery 变成 task required checks；默认只要求用户显式配置的 checks，repo-local discovery 需要显式 approval、sandbox decision 或 global policy promotion 后才进入 task policy。
- 已修正 check runner 执行前 workspace trust gate：`run_verification_check` 会同时识别 request 级 approval/sandbox decision 和 `TrustedCheckSpec` promotion 自带的 approval/sandbox decision，避免已审批或已 sandboxed 的 repo-local trusted check 被错误拒绝。
- 已在 session audit 中展示 workspace trust provenance：`WorkspaceTrustDecision` 会显示 trust snapshot、deciding event 和 reason，便于用户追溯 workspace trust 来源。
- 已在 TUI 中展示 verification missing/passed/stale 等状态，并补 slash command 高亮、timeline command token 和 MCP failure 展示回归测试。
- 已将 workspace trust 改为首次进入 workspace 的启动 gate：未信任 workspace 不能进入正式 TUI、加载 repo-local instructions 或执行 repo-local check discovery；`/config` Permissions 只展示 trust 状态、用户配置 checks 和 repo-local candidate checks，不再提供 workspace trust footer action。
- 已在 `/config` 的 Permissions 页补充 repo-local instruction trust 摘要：`SIGIL.md`、`AGENTS.md`、`CLAUDE.md` 和 `SIGIL.local.md` 在 workspace 未信任时显示为 untrusted data，workspace trust 后显示为 trusted instructions。
- 已简化 `/config` Permissions 的 repo-local verification 展示：只展示 repo-local check 数量与长期策略摘要，具体 run/retry/review 入口归属 task sidebar / strip，避免把设置页做成一次性执行审批面。
- 已在 TUI task sidebar / strip / session audit 中补充 workspace trust / check approval 的用户可读解释：`TrustWorkspace` 会显示 `workspace trust required`，`ApproveCheckExecution` 会显示对应 check approval；task sidebar 的 `action:` 行和 session audit 的 required action 摘要都改为用户可读短语，不再暴露内部 action token。
- 已移除 `/config` Permissions 的 repo-local check footer approval UX：底层 approval/sandbox promotion action 保留给 task status surface 和后续真实 sandbox backend / advanced surface，避免当前主流程误导用户。
- 已将 workspace-scope check promotion 接入 task readiness：approved / sandboxed promotion 会生成 `ApprovalOrSandbox` trust requirement，并把 promotion id 绑定到 check run / receipt；check spec 变化时旧 workspace promotion 不再匹配当前候选 check。
- 已在 TUI task sidebar / strip 中展示最新 `VerificationCheckRun` queued/running/terminal 状态和失败原因；当已有 check run lifecycle evidence 时，不再只显示静态 `run_check` action。
- 已在 TUI task sidebar / strip 中补充窄宽度 verification reason compact 展示：显示第一个 stale/actionable reason，并用 `+N more` 汇总其余原因；session audit 仍保留完整 reason labels。
- 已补充 check runner 失败后的最小 retry affordance：queued/running 会隐藏重复 run action，terminal failed/errored/inconclusive/succeeded 等历史 run 不会遮蔽当前 `run_check` required action，用户能看到失败原因和重新运行入口。
- 已补充配置化 check auto-run policy：`manual` 为默认低摩擦策略，只展示 run/retry action；`trusted_only` 才会自动启动 trusted checks；`never` 禁止自动启动。`/config` Permissions 可查看/切换该策略，task materialize 会把策略写入 task/step policy，子 policy 只能收紧不能放宽。
- 已将 check runner 执行路径接入 RFC-0005 `ExecutionBackend`：`run_verification_check` 不再直接 spawn 本地进程，而是通过 runtime 配置的 backend 执行；`/task` orchestrator 持有并传递同一 backend，缺少 backend 时 fail closed。
- 已将 verification receipt 绑定到实际 execution backend 和 capabilities：新 receipt 记录 backend kind、capability summary，并用 `SandboxProfileRequirement + backend + capabilities` 计算 `sandbox_profile_hash`；`Sandboxed` policy 不再接受 legacy receipt 或 LocalBackend receipt 作为 passed evidence。
- 已将 check runner timeout / exit failure reason 写入 `VerificationReceipt`，并由 terminal `VerificationCheckRun` 继承；TUI 可直接展示系统产生的 `check timed out ...` / exit-code reason，而不是只显示泛化 failed 状态。
- 已将 policy timeout 写入 `VerificationCheckRun` queued/running/terminal lifecycle audit，并在 task sidebar / strip / session detail 中展示，用户能看到 check-run 采用的 timeout 配置。
- 已将 workspace snapshot 大文件阈值纳入 `VerificationScope.max_file_bytes`，并作为 policy-bound scope coverage 参与验证范围覆盖判断；默认值仍沿用 `MAX_WORKSPACE_SNAPSHOT_FILE_BYTES`。
- 已补充 verification scope profile MVP：`auto` / `rust` / `node` / `python` / `docs` 预设可生成对应 `VerificationScope`，`[verification.scope]` 可通过 `profile`、`extra_excludes` 和 `generated_roots` 做低频 override；`/config` Permissions 只读展示当前 profile、关键 excludes、generated roots 与 advanced override 数量，不新增普通用户操作面。
- 已完成 child verification / worktree merge 的最小产品链路展示：task sidebar / strip 会在 child merge 导致 parent verification stale 时显示 child task/status 和 `run parent check` 引导；session audit 中 child receipt link 会显示 linked/merged 状态和 parent re-check requirement，避免把 snapshot id / merge event id 当成普通用户动作。
- E03.4 已完成：E14.7 的 merge review product surface 展示 pending/accepted/conflict/rejected/cancelled states，kernel `verification_child` reducer test 证明 child worktree `Passed` receipt 不会继承为 parent `Passed`，parent merge 后仍需 parent workspace evidence。
- 已补齐新增 projected state 的 Session 级 durable replay adoption：session-list、agent-graph 和 dispatch-trace projection 可通过 `Session` 的 V2 durable replay adapter 重建；dispatch trace projection 继续保持 egress payload redaction，腐败 stream/sequence gap 由读取层 fail closed。
- 已完成 plugin verification hook receipt binding：可信 hook command 的 started/finished evidence 与 bounded output envelope 一致时，系统可生成绑定 workspace snapshot、check spec hash、execution backend/capabilities、network receipt 和 sandbox profile hash 的 `VerificationReceipt`；hook stdout/stderr 只作为 output provenance，不能决定系统 verdict；mutating hook result 映射为 `Inconclusive`，不能满足 final passed evidence。plugin-declared MCP/server 长生命周期进程仍未自动进入 active startup/refresh path。

Productization remains：

- 完成 check runner 产品化：更完整的失败重试交互；runner lifecycle audit primitive、核心 trust gate、repo-local approval UI、backend execution plumbing、最小 queue/status UI、timeout visibility、terminal failure reason、retry affordance、配置化 auto-run policy 和 actual backend/capability binding 已落地。跨平台 sandbox backend、persistent terminal sandbox 和 MCP/plugin 进程隔离继续由 RFC-0005 / RFC-0009 后续切片提供。
- plugin hook command runtime 已通过 RFC-0015 E15.5/E15.6 接入 mutation recorder 与 verification receipt binding；后续自动合并 plugin-declared MCP servers 或其他长生命周期 plugin-owned process 时，仍必须复用 RFC-0002 external-process unknown-dirty recorder。
- MCP lifecycle verification UX bridge 已落地：`WorkspaceMutationDetected(tool_name="mcp_server:<name>")` 会投影为用户可读的 `MCP server <name>` source reason 和 `refresh MCP or run check` recovery hint；task sidebar / strip / session detail 不再只显示内部 unknown-dirty token。
- 扩展 verification scope profile 的后续工作只剩真实项目校准：默认/profile presets、配置文件 override 和 TUI 只读摘要已落地；更多语言专用生成目录或依赖缓存应按项目证据追加，避免把普通用户操作面做复杂。
- 完成 workspace trust UX：首次进入 workspace gate、基础 audit provenance、`/config` trust/long-term policy 摘要、repo-local instruction 降级展示、task sidebar/strip 与 session audit 的 trust/approval 解释已落地。
- child verification / worktree merge 产品链路的默认 TUI 展示已完成：child receipt link、merge 后 parent re-check 引导和 session audit trace 已落地；后续如通过 RFC-0014 引入真正 worktree merge review UI，应继续复用该 trace，不得让 child `Passed` 直接继承为 parent `Passed`。
- 继续把后续新增 historical/projected state 接入 RFC-0001 durable replay；现有核心 task、verification、agent thread/agent graph、session list、dispatch trace、terminal、changeset、plan、skill、plugin、profile trust/policy、continuation 和 queue projection 已具备 V2 stream replay 入口。

## 16. Open Questions

None for core semantics. Remaining work is tracked under Productization remains.
