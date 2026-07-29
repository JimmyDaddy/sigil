---
name: technical-solution-executor
description: Execute supplied technical solutions or execution plans with risk-scaled implementation, serial/parallel task decomposition, bounded sub-agent execution when useful and allowed, explicit user opt-out support, minimal necessary validation, and audits. Use when the user provides or points to a 技术方案, technical solution, execution plan, or task list and expects implementation without unapproved defers, mocks, stubs, hardcoded shortcuts, missing tests, or deceptive pass-only changes.
---

# Technical Solution Executor

Use this skill to implement a supplied technical solution completely, but scale the ceremony and validation to the risk of the work. The solution remains the source of truth; the optimization is to avoid unnecessary sub-agent passes, repeated full gates, long validation loops, and oversized reports on small safe changes.

## Always Enforce

- Read `AGENTS.md`, repository code/engineering standards when present, the complete supplied technical solution, and any referenced architecture or acceptance material needed to understand the required behavior.
- For this repository, code implementation must conform to `dev/governance/*` and the relevant `dev/docs/*` architecture/RFC material. If implementation pressure conflicts with those architecture boundaries, stop and resolve the design instead of landing a shortcut.
- Keep changes scoped to the confirmed solution and required integration, test, and doc updates.
- Do not introduce unapproved "later", "future", "temporary", "MVP", or "defer" items.
- Do not use mocks, stubs, fake integrations, hardcoded success returns, tests-only branches, or shortcuts that satisfy tests without implementing the requirement.
- Do not weaken, delete, or bypass tests to make validation pass.
- Do not use validation volume as a substitute for reading the code, understanding the diff, and choosing precise checks.
- Stop before editing an affected area if the solution is internally inconsistent or impossible; explain the blocker with evidence and ask for a decision or propose the smallest correction.
- Preserve repository-specific boundaries, especially provider-neutral kernel APIs, append-only session/control behavior, workspace confinement, approval boundaries, and TUI state/help/docs consistency.
- Avoid both over-abstraction and unreasonable coupling. Prefer the existing crate/module boundary unless a new boundary clearly reduces coupling, protects an architecture invariant, or matches an approved RFC.
- Adding a new crate requires an explicit rationale: ownership boundary, dependency direction, why existing crates/modules are insufficient, public API surface, tests, and required `dev/docs` / `dev/governance` updates.
- When a change affects architecture, public contracts, crate boundaries, configuration, TUI/user flows, durable state, permissions, tools, verification, or execution semantics, update the relevant documents under `dev/` in the same slice.

## Research Before Execution

Before implementing, do enough research to make the execution grounded rather than assumption-driven.

- Start with local source, tests, RFCs, roadmap items, execution slices, governance docs, and prior status records.
- Inspect adjacent implementations and existing tests before adding new abstractions, fields, flows, or crates.
- When the task involves product UX, security posture, sandbox behavior, provider/platform behavior, industry comparison, current standards, external APIs, or any fact likely to have changed, use internet research when helpful and cite or summarize the source basis in the execution notes.
- Prefer primary sources for technical claims: official docs, upstream source, release notes, specs, RFCs, or directly inspected code.
- Do not use research as a reason to expand scope. Research should clarify the smallest correct implementation, expose blockers, or justify design choices.
- If research shows the supplied solution conflicts with current facts or architecture constraints, stop before editing the affected area and report the conflict with evidence.

## Decompose and Delegate

Resolve delegation policy before decomposing the work. Treat an explicit user instruction such as
"do not use sub-agents", "不要用子代理", or "由当前代理自己完成" as a hard opt-out for the entire
run. Do not start, delegate to, hand off to, or use sub-agents during decomposition, implementation,
review, audit, escalation, or validation. Continue normally in the primary agent: build the same task
graph, execute it in dependency order, and preserve the required scope, implementation completeness,
validation, and audit intensity. Do not ask the user to relax the opt-out or treat it as a blocker.

Build a dependency-aware task graph before implementation when the solution is non-trivial:

- Mark a task **serial** when it depends on an earlier design/API result, shares a mutable authority boundary, edits the same ownership-heavy files, or must consume earlier validation evidence.
- Mark tasks **parallel** only when they are independently useful, have non-overlapping or explicitly coordinated write ownership, and can be integrated without guessing another task's unfinished contract.
- Keep integration, shared-file conflict resolution, durable/public contract decisions, final gates, and completion claims with the primary agent.
- Do not create sub-agents merely to appear busy. Delegate concrete bounded work that can make progress while the primary agent continues useful work.

When sub-agent tooling is available and higher-priority policy permits it, use sub-agents for both
implementation and review when parallelism materially helps. Give each sub-agent:

- the exact source requirement or slice;
- allowed files/modules and explicit write ownership;
- relevant architecture invariants and forbidden scope;
- the expected artifact or code result;
- a targeted validation command and evidence to return.

Assume sub-agents share the worktree unless the environment says otherwise. Inspect every returned
diff/result before integration, reconcile conflicts deliberately, and never treat delegation as proof
that the task is complete.

## Slice-Based Execution

Use this mode when the supplied solution is split into RFC execution slices, roadmap items, or `.repo-local-dev/rfcs/*` task files.

- Treat each slice as the unit of work. Move one slice to `in_progress`, implement it, validate it, record the result, then move to the next slice.
- Keep slice state in the relevant execution-plan file and shared status file when they exist. Do not rely on chat history as the only task ledger.
- Each slice should reference its source RFC or roadmap section before implementation starts. If the source link is missing, add it first.
- Do not expand a slice into adjacent RFC work unless the slice cannot be completed without that dependency. Record the dependency instead of silently broadening scope.
- Prefer a small, complete semantic increment over a broad partial implementation. A slice is not done until code, tests, docs/status updates, and its selected validation are complete.
- If a slice exposes a product operation to users, audit whether the operation is too complex for the main flow. Prefer coarse user actions, config files, doctor output, or advanced surfaces over adding low-frequency controls to the default TUI/config path.

## Choose Intensity

Honor an explicit user mode when provided: `quick`, `standard`, or `full-audit`. Otherwise start at `quick` and escalate only for a concrete risk listed below. State the selected mode briefly.

### Quick

Use for low-risk, localized work such as docs-only changes, renderer/layout tweaks, test-only fixes, or small implementation changes that do not affect durable state, permissions, tools, provider contracts, public APIs, migrations, release packaging, or cross-crate behavior.

- Use a compact checklist of 2-5 items: requirement, edit surface, validation.
- Do not start sub-agents by default unless one clearly independent task would materially reduce
  latency without increasing coordination risk.
- Run the smallest useful validation for the touched area. For docs/config/text-only skill changes, static review plus a basic format/frontmatter check is enough. For code changes, prefer one targeted test or check plus formatting when applicable.
- Do not run full workspace tests, clippy, coverage, or broad package gates unless the user asks or a concrete risk appears.
- Finish with a short summary, changed files, validations, and any skipped heavier checks with reason.

### Standard

Use by default for normal implementation work that touches several files or one crate boundary, changes tests/docs, or has moderate user-facing behavior but no high-risk durability, security, migration, or cross-crate contract change.

- Decompose the work into tasks that include source requirement, likely files, tests/docs, and validation. Keep each task compact.
- Execute independent tasks in parallel when ownership and dependencies are explicit; otherwise keep
  them serial.
- Use one independent review pass only when the decomposition is non-trivial, ambiguity remains, or the diff is large enough that a second pass is likely to catch real gaps.
- Validate with narrow tests first, then at most the relevant package/crate/module gate needed for confidence. Avoid repeated full workspace gates unless the changed surface warrants them.
- Default validation budget: one formatting check, targeted tests for changed behavior, and one relevant compile/check gate. Add clippy, coverage, docs-site, or full test suites only when required by acceptance criteria or risk.
- Finish with mode, task completion, changed files, validation results, review/audit result if run, and caveats.

### Full-Audit

Use when the user explicitly asks for sub-agent review/audit, or when the solution touches high-risk areas:

- durable state, session/control logs, recovery, permissions, approval, tools, workspace confinement, provider contracts, migrations, public APIs, release/distribution, or multi-crate behavior
- broad TUI workflows where state model, event flow, renderer, key/help hints, docs, and tests must move together
- large technical solutions with multiple dependent tasks or explicit acceptance gates

Full-audit mode uses the original strict workflow:

1. Build a full requirement-to-task decomposition with source section, files/modules, tests/docs, dependencies, acceptance criteria, and validation command for each task.
2. Classify the task graph into serial dependencies and parallel workstreams. When tooling and policy
   permit, use a sub-agent to challenge the decomposition; otherwise perform a separate local
   decomposition audit.
3. Dispatch bounded independent implementation workstreams to sub-agents when that materially helps,
   while the primary agent owns serial foundations and integration. Mark a task done only after code,
   tests, docs, and its validation are complete.
4. Run the strongest useful validation for the touched area. Before long gates such as full workspace tests, clippy, coverage, docs-site, or manual TUI checks, confirm they are required by the solution, repository standards, user request, or changed risk surface.
5. Run two post-implementation independent passes when tooling is available: code/project standards review and completeness audit against solution, final task list, diff, and validation results.
6. Fix every valid finding, rerun only the affected or previously failed validation, and record concrete reasons for any rejected findings.
7. Finish with the full report: implemented solution, requirement-to-task completion, files/modules, validation, sub-agent results, fixes after review, caveats, and next steps.

## Validation Discipline

- Keep a validation ledger: command, purpose, result. Do not rerun a passing command unless files covered by that command changed afterward.
- Prefer exact targeted filters over broad suites. For Cargo, use one real test-name substring per invocation; if several tests are needed, run them deliberately rather than as a reflexive full suite.
- Do not run multiple heavy Cargo commands in parallel against the same target dir.
- Treat slow gates as scarce. If a gate is expected to be slow, explain why it is needed before running it; if it is optional, defer it and report that clearly.
- If a task is static-only or docs/config-only, say so. Do not imply test-backed confidence when no runtime validation was run.
- For multi-slice work, validate each completed slice with the smallest command that proves the touched semantics. Save broader gates for changed-risk boundaries, final hardening, or explicitly requested delivery, rather than rerunning them after every small edit.
- When a targeted validation already passed and only docs/status files changed afterward, do not rerun the code validation. Record that the code-covered files were unchanged after the passing gate.

## Escalation Rules

Escalate to a higher intensity if implementation reveals unplanned public behavior, missing requirements, cross-crate coupling, migrations, permission/session/tool/provider risk, failing targeted tests, or review findings that change the design. Do not downgrade below an explicit user request for review, sub-agents, coverage, or full validation.

## Execution Flow

1. Ingest the solution and extract goals, non-goals, required behavior, APIs/data/config, docs/tests, acceptance criteria, validation gates, approved defers, and blocking ambiguities.
2. Complete the research pass required for the slice: local code/RFC inspection first, internet research when useful for volatile facts, product/security comparisons, external APIs, or standards.
3. State the selected intensity, identify serial and parallel work, and maintain a visible checklist
   sized to that intensity.
4. Read relevant existing code/tests before editing. Prefer repository helpers and patterns over new abstractions.
5. Implement the smallest complete change. Add or update unit tests for new business logic, and update docs when public behavior, config, commands, TUI flow, safety/privacy, or architecture changes.
6. Validate at the selected intensity using the smallest checks that prove the changed behavior. If a required full gate is too slow or blocked, run the best narrower gate and report exactly what was not run and why.
7. Review/audit according to the selected intensity. Never claim a sub-agent review happened if it did not.
8. Fix valid findings and rerun relevant validation.
9. Final response should be concise. Include intensity used, research basis, what changed, validations, review/audit status, and remaining caveats.
