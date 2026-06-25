# RFC-0003 Verification Contract and Workspace Snapshot

状态：Draft

创建日期：2026-06-25

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Future dependency: RFC-0002 Crash-consistent Mutation Protocol

## 1. Summary

本 RFC 定义 Sigil 的 verification contract：系统如何判定一次 run、task step 或 child agent result 是否已经验证，如何绑定 workspace snapshot，如何处理写入后验证失效，以及如何避免模型 final answer 被误当成完成证明。

核心决策：

1. 执行状态和验证结论分离。
2. Verification verdict 必须由系统 reducer 根据 evidence 计算，不能由模型自报。
3. `Passed` 必须绑定当前验证范围的 `WorkspaceSnapshotId`。
4. 未信任 workspace 只能发现候选检查，不能自动执行仓库脚本或配置。
5. 任何 verification scope 内的 mutation 都会使旧 verification stale。

## 2. Goals

- 修正“模型停止调用工具 = 完成”的语义风险。
- 修正 `/task` step 只要 final text 非空就可能 completed 的语义风险。
- 提供 foreground chat、`/task`、subagent 和未来 protocol surface 共享的 completion semantics。
- 定义 check discovery、policy inheritance、workspace snapshot binding 和 stale invalidation。
- 让 TUI 能独立展示 run status 和 verification verdict。

## 3. Non-goals

- 不定义具体测试命令自动推断的完整启发式。
- 不实现 sandbox backend；verification 只引用 sandbox profile hash。
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

Visible derived states:

```text
Completed + Passed        -> Verified
Completed + Missing       -> CompletedUnverified
Completed + Stale         -> CompletedUnverified
Completed + Skipped       -> CompletedUnverified
Completed + NotApplicable -> Completed
Failed + Failed           -> FailedVerification
Blocked + Pending         -> NeedsUser
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
    allow_unverified_completion: bool,
    timeout: Option<Duration>,
}
```

Policy merge rules:

```text
required_checks = parent union child
allow_unverified_completion = parent && child
timeout = min(parent, child)
```

Child scope may tighten parent policy. It may not relax required checks or enable unverified completion if the parent disallows it.

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
    check_spec_hash: String,
    environment_fingerprint: String,
    sandbox_profile_hash: String,
}
```

Default scope:

- Git tracked files.
- Unignored new source files.
- User explicit include paths.
- Exclude `.git`, Sigil state directories, common build/cache directories, dependency caches and generated roots.

Rules:

- `WorkspaceRevision` is scoped to one workspace, worktree or snapshot stream.
- `WorkspaceRevision` is not a global order across sessions or worktrees.
- Verification validity should use content-bound `WorkspaceSnapshotId`.
- A check that modifies files in verification scope produces mutation evidence, not final passed evidence.
- After a writing check such as formatter, fixer, snapshot update or codegen, Sigil must re-run a non-writing check before `Passed`.

## 8. Evidence Model

Evidence is derived from durable events. It is not a second mutable truth source.

Relevant events:

```text
WriteRecorded
CommandFinished
CheckFinished
DiagnosticRecorded
TodoChanged
ReadinessEvaluated
WorkspaceMutationDetected
```

Receipt minimum fields:

```rust
struct EvidenceReceipt {
    receipt_id: ReceiptId,
    scope: EvidenceScope,
    producer_tool_call: Option<ToolCallId>,
    workspace_revision: Option<WorkspaceRevision>,
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
- Unknown dirty workspace yields `Stale` or `Inconclusive`, depending on whether prior evidence existed.
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

## 12. TUI and Protocol Surface

TUI should show:

- run badge
- verification badge
- latest applicable check
- stale reason
- missing required checks
- workspace trust warning
- whether command execution required approval or sandbox

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
- formatter modifies source and cannot produce final `Passed`
- user skips check with policy support maps to `Skipped`
- required check failure maps to `Failed`
- untrusted workspace discovers but does not auto-run Makefile/script checks
- unknown shell mutation maps to `UnknownDirty` and stale verification
- restore invalidates previous passed verification
- child worktree passed evidence does not transfer after parent merge
- final text cannot force `Passed`
- `/task` step with recovered tool error is not silently verified
- cancellation preserves independent verification verdict
- policy inheritance cannot relax parent required checks

## 15. Open Questions

- Exact `WorkspaceSnapshotId` hash algorithm.
- Exact default exclude list for build/cache/generated roots.
- How much of CI check discovery should run before workspace trust.
- Whether `Inconclusive` should be used for unknown dirty with no prior evidence or only for ambiguous check outputs.
- How TUI should compact multiple stale reasons in narrow terminal width.
