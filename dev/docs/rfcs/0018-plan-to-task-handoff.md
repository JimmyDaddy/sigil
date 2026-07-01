# RFC-0018 Plan-to-Task Handoff

状态：core semantics implemented / productization complete for TUI v0

创建日期：2026-07-01

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Depends on: [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- Related: [RFC-0012 Protocol and App Server Boundary](0012-protocol-app-server-boundary.md)

## 1. Summary

This RFC defines the bridge from `/plan` to durable `/task` execution.

Today `/plan` is a read-only planning prompt and `/task` owns durable multi-step execution. That split is correct, but the product lacks a first-class handoff:

```text
/plan result
  -> user accepts plan
  -> durable task is created from that plan
  -> task executes through existing verification, mutation, checkpoint and audit paths
```

The goal is not to make `/plan` a second execution engine. The goal is to make a plan become an auditable task artifact when the user explicitly chooses to execute it.

Core decision:

1. `/plan` remains read-only.
2. Plan acceptance is distinct from tool permission approval.
3. Durable `/task` remains the only execution path for planned work.
4. Plan-derived execution must reuse task, mutation, verification, sandbox and approval semantics.
5. The TUI should expose one simple user operation: create or run a task from the approved plan.

## 2. Source Basis

This RFC is based on Sigil's current internal architecture:

- Existing `PlanApprovedEntry` records a permission decision for a plan hash, not durable task state.
- Existing `TaskPlanEntry { status: Accepted }` is the durable task-plan entry used by `/task` execution.
- Existing `TaskStateProjection` is the TUI source of truth for task visibility.
- Existing verification and mutation semantics already know how to gate task completion after writes.

Therefore the missing capability is a durable plan draft plus a handoff command that feeds the approved plan into the existing `/task` planner. `/plan` must not become a hidden second task-planning format.

## 3. Goals

- Make `/plan` output persist as a typed durable plan artifact.
- Let the user create a durable task from a plan without copying text into `/task`.
- Keep plan acceptance separate from permission grants.
- Let task execution continue through existing `/task` runtime and verification contract.
- Keep the default TUI action coarse and low-friction.
- Preserve append-only auditability for plan creation, decision and task handoff.
- Support later protocol/app-server clients by modeling the handoff as a command, not only as a TUI keypress.

## 4. Non-goals

- Do not make `/plan` automatically execute writes after every approval.
- Do not let plan mode expose writer tools, write-capable subagents or full plugin/MCP capabilities.
- Do not introduce a second task orchestrator.
- Do not use model text as verification evidence.
- Do not make repo-discovered checks required merely because a plan mentions them.
- Do not introduce parallel write agents or physical worktrees in this RFC.
- Do not add a wide permission matrix to the main TUI surface.

## 5. Product Semantics

### 5.1 User Flow

Recommended TUI flow:

```text
/plan fix the release docs wording
  -> Sigil researches with read-only tools
  -> Plan ready card appears

Plan ready · execution plan · 3 paths · checks suggested

Enter       create and run task
Esc         discard
```

`Esc` discard is durable for typed plan artifacts: it appends
`PlanDecisionRecorded { decision: Rejected }` and removes the plan from the pending handoff
projection. Only non-durable fallback text plans may be dismissed locally.

If the plan requests edits, Sigil can ask one coarse follow-up:

```text
Allow scoped file edits for this task?

Once        Ask on first write outside normal allow rules
Ask each    Keep current approval behavior
Cancel
```

This keeps the normal path small. Detailed permission, sandbox and verification state remain visible in task/session detail surfaces, not in the default plan card.

### 5.2 `/plan` vs `/task`

`/plan`:

- read-only research and design
- may inspect files and trusted read-only tools
- produces a human-readable execution plan draft
- does not create task state until the user asks for handoff
- does not mark work complete

`/task`:

- durable execution state
- steps, dependencies, review/verify and subagent routing
- mutation and checkpoint evidence
- verification verdict
- resumable task projection
- owns the executable task plan and step model

### 5.3 Approval Vocabulary

Use separate words for separate meanings:

- `PlanDraftCreated`: model produced a plan artifact.
- `PlanDecisionRecorded`: user accepted, rejected, revised or saved the plan.
- `PlanPermissionGranted`: optional scoped short-lived permission for executing this accepted plan.
- `TaskCreatedFromPlan`: durable task was created from the plan artifact.

Do not overload `PlanApproved` to mean both "the plan is a good idea" and "tools may write without prompting".
In the current codebase, `PlanApproved` is treated as a legacy permission-grant record only; new
handoff flows use `PlanDecisionRecorded` for acceptance/rejection and `TaskCreatedFromPlan` for
task materialization.

## 6. Durable Domain Model

Initial control/domain records:

```rust
struct PlanDraftCreated {
    plan_id: PlanId,
    source_session_id: SessionId,
    source_run_id: Option<RunId>,
    plan_hash: String,
    summary: String,
    full_text_artifact: Option<ArtifactId>,
    inline_text: Option<String>,
    target_paths: Vec<PathBuf>,
    suggested_checks: Vec<CheckSpec>,
    workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    created_at_sequence: u64,
}

enum PlanDecision {
    Accepted,
    Rejected,
    RevisionRequested,
    SavedOnly,
}

struct PlanDecisionRecorded {
    plan_id: PlanId,
    decision: PlanDecision,
    decided_by: PlanDecisionActor,
    reason: Option<String>,
    permission_grant: Option<PlanPermissionGrant>,
}

struct TaskCreatedFromPlan {
    plan_id: PlanId,
    task_id: TaskId,
    task_plan_version: u32,
    step_mapping: Vec<PlanToTaskStepMapping>,
}
```

Implementation may keep these as `ControlEntry` variants first, while RFC-0001 durable event projection remains the replay contract.

`task_plan_version = 0` means the handoff has not materialized a task plan yet. This is the normal TUI v0 behavior: the approved plan text is passed as the input to ordinary `/task` execution, and the `/task` planner records its own `TaskPlanEntry` afterward.

## 7. Plan Draft Text and Metadata

Plan output is a user-readable execution plan, not a hidden machine-readable task schema.

The durable plan record should preserve:

- bounded inline plan text or an artifact reference
- summary text for compact surfaces
- target paths extracted conservatively for scoped approval and drift hints
- suggested check candidates extracted conservatively
- source session/run and workspace snapshot metadata
- plan hash for stale decision protection

Rules:

- Markdown bullets, tables, headings and diff hunks are display text only; they must not be guessed into durable task steps.
- Explicit file paths may become `target_paths`.
- Terms like "test", "lint", "cargo test", "verify" may become suggested checks, not required checks.
- Old or model-generated machine-readable fenced blocks are treated as ordinary plan text unless a later RFC deliberately introduces a typed public plan schema.

## 8. Task Handoff

The handoff creates a durable task link, then starts normal `/task` planning with the approved plan as the task input:

```text
PlanDraftCreated
  -> user accepts
  -> TaskCreatedFromPlan
  -> TaskRunEntry
  -> ordinary /task planner
  -> TaskPlanEntry { status: Accepted }
```

Rules:

- The default TUI action creates and runs the task. Lower-level protocol callers may still request create-paused when they need a saved task without immediate execution.
- `/plan` must not pre-populate `TaskPlanEntry`.
- `TaskCreatedFromPlan.step_mapping` is empty for the plan-as-input handoff.
- The task objective/input is the approved plan text wrapped with a short system-owned instruction that it is user-approved plan input.
- The `/task` planner must create stable step ids when it writes its normal task plan.
- Task execution must use the normal task orchestrator.
- Verification policy comes from existing Verification Contract and configuration.
- Suggested checks remain candidates unless the user/config promotes them.
- Child verification from plan-derived tasks follows existing child/parent verification separation.

## 9. Permission and Trust Rules

Plan mode constraints:

- Built-in read tools: allowed by normal policy.
- LSP/code-intel read-only tools: allowed if workspace trust permits.
- Source writes: denied.
- Write-capable subagents: denied.
- MCP/plugin tools: only trusted read-only tools are eligible.
- Unknown or self-reported read-only third-party tools: ask or deny; non-interactive mode fails closed.
- Workspace not trusted: repo instructions and repo-discovered commands are untrusted data; discovered checks are candidates only.

Post-acceptance constraints:

- A plan permission grant is scoped by plan hash, task id, workspace snapshot and target paths.
- The grant expires after the task run, first out-of-scope write, user cancellation or plan supersession.
- It can reduce repeated file-edit prompts for planned paths, but it must not bypass sandbox, secret egress, network, external-directory or MCP/plugin trust policy.
- Shell commands are not preapproved merely because a plan mentions them.

## 10. Drift Detection

V0 drift guard:

- Writes outside `target_paths` fall back to normal approval.
- If the current workspace snapshot no longer matches the plan's base snapshot, show "plan may be stale" before task creation.
- If task execution modifies files after verification, existing verification staleness rules apply.
- If task execution produces new steps outside the accepted plan, the new plan version must be recorded through `TaskPlanEntry` and visible in the task projection.

V1 drift guard can compare semantic intent, but V0 should stay path/snapshot based.

## 11. TUI Surface

Default plan card should show:

- plan status
- summary
- execution-plan label
- target path count
- suggested check count
- one primary action

Do not show internal hashes, full permission matrices, check promotion details or reducer terminology in the default card.

Detail view can show:

- source run/session
- plan hash
- target paths
- suggested checks
- permission grant scope
- created task id
- supersession/revision history

## 12. Protocol Surface

Future app-server/desktop clients should use command envelopes:

```rust
enum Command {
    CreateTaskFromPlan(CreateTaskFromPlanCommand),
    DecidePlan(DecidePlanCommand),
}

struct CreateTaskFromPlanCommand {
    plan_id: PlanId,
    expected_plan_hash: String,
    start_mode: PlanTaskStartMode,
    permission_grant: Option<PlanPermissionGrantRequest>,
}
```

The command must reject:

- stale plan hash
- missing plan artifact
- already rejected plan
- expected stream sequence mismatch
- changed workspace snapshot when policy requires a fresh plan

## 13. Acceptance Criteria

- `/plan` can produce a durable plan artifact.
- The user can create a `/task` from the plan without copying text.
- The created task appears in the normal task sidebar/projection.
- The handoff does not display or persist precomputed task steps from `/plan`.
- The task's executable steps are produced by normal `/task` planning after acceptance.
- Plan acceptance does not automatically grant broad write, shell, network, MCP or plugin permissions.
- Plan-derived task completion is still governed by `RunStatus` and `VerificationVerdict`.
- Suggested checks are not silently promoted to required checks.
- Out-of-scope writes require normal approval.
- Plan artifacts survive session reload and can be inspected in session detail.

## 14. Validation

Recommended checks by slice:

```bash
cargo test -p sigil-kernel plan
cargo test -p sigil-tui plan
cargo test -p sigil-tui create_task_from_plan
cargo test -p sigil-tui plan_handoff_run_now_uses_normal_task_planner_and_executes_resulting_plan
cargo test -p sigil-tui task_sidebar
cargo fmt --all --check
```

Deterministic runner coverage should include the full TUI worker path:

```text
/plan
  -> PlanDraftCreated
  -> user accepts create-and-run
  -> PlanDecisionRecorded(Accepted)
  -> TaskCreatedFromPlan(task_plan_version = 0)
  -> normal /task planner emits TaskPlanEntry { status: Accepted }
  -> orchestrator executes the resulting task step
```

This is deliberately a provider-injected runner test, not a live model test. It proves the
handoff control plane and task runtime semantics without relying on model behavior or network
availability.

Manual smoke:

1. Run `/plan` for a small docs edit.
2. Create task from plan.
3. Confirm the task appears as a normal durable task without precomputed `/plan` steps.
4. Confirm the normal `/task` planner creates and runs the executable task plan.
5. Confirm missing/passed/stale verification behavior is unchanged.

Opt-in live TUI smoke:

```bash
scripts/tui-plan-task-smoke.py --timeout 240
```

This script launches the real TUI in a pseudo-terminal with isolated
`SIGIL_STATE_HOME` and `SIGIL_CACHE_HOME`, uses the local provider
configuration, accepts the workspace trust gate, submits `/plan`, accepts the
plan-ready handoff with Enter, waits for the normal task runtime to complete,
verifies the file edit, and checks that the session does not contain
unknown-dirty workspace mutation pollution. It is not a default CI gate because
it can spend provider tokens and depends on terminal/provider availability.

The `/task` planner/schema contract also treats ordinary writes and delegated
write proposals as distinct roles: normal main-session edits use
`executor + sequential_workspace_write`, while `subagent_write` is reserved for
`changeset_only` delegated write proposals.

## 15. Open Questions

- Whether a later implementation should move long plan text from bounded inline storage to the general artifact store.
- Whether plan revision should create a new plan id or supersede a version under the same plan id.
- Whether create-paused should remain a lower-level protocol capability or be removed from the public TUI path until there is a clear saved-plan product flow.

## 16. Repo-local Research Notes

Competitor and external-source research for this design is intentionally kept in the repo-local execution plan, not in this formal RFC.
