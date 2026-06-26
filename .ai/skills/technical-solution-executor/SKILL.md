---
name: technical-solution-executor
description: Execute supplied technical solutions or execution plans with risk-scaled implementation, validation, and audits. Use when the user provides or points to a 技术方案, technical solution, execution plan, or task list and expects implementation without unapproved defers, mocks, stubs, hardcoded shortcuts, missing tests, or deceptive pass-only changes.
---

# Technical Solution Executor

Use this skill to implement a supplied technical solution completely, but scale the ceremony to the risk of the work. The solution remains the source of truth; the optimization is to avoid unnecessary sub-agent passes, repeated full gates, and oversized reports on small safe changes.

## Always Enforce

- Read `AGENTS.md`, repository code/engineering standards when present, the complete supplied technical solution, and any referenced architecture or acceptance material needed to understand the required behavior.
- Keep changes scoped to the confirmed solution and required integration, test, and doc updates.
- Do not introduce unapproved "later", "future", "temporary", "MVP", or "defer" items.
- Do not use mocks, stubs, fake integrations, hardcoded success returns, tests-only branches, or shortcuts that satisfy tests without implementing the requirement.
- Do not weaken, delete, or bypass tests to make validation pass.
- Stop before editing an affected area if the solution is internally inconsistent or impossible; explain the blocker with evidence and ask for a decision or propose the smallest correction.
- Preserve repository-specific boundaries, especially provider-neutral kernel APIs, append-only session/control behavior, workspace confinement, approval boundaries, and TUI state/help/docs consistency.

## Choose Intensity

Honor an explicit user mode when provided: `quick`, `standard`, or `full-audit`. Otherwise choose the lowest safe mode and state the choice briefly.

### Quick

Use for low-risk, localized work such as docs-only changes, renderer/layout tweaks, test-only fixes, or small implementation changes that do not affect durable state, permissions, tools, provider contracts, public APIs, migrations, release packaging, or cross-crate behavior.

- Use a compact checklist of 2-5 items: requirement, edit surface, validation.
- Do not start sub-agents by default.
- Run targeted validation for the touched area, plus formatting for code changes when applicable.
- Finish with a short summary, changed files, validations, and any skipped heavier checks with reason.

### Standard

Use by default for normal implementation work that touches several files or one crate boundary, changes tests/docs, or has moderate user-facing behavior but no high-risk durability, security, migration, or cross-crate contract change.

- Decompose the work into tasks that include source requirement, likely files, tests/docs, and validation. Keep each task compact.
- Use one independent review pass only when the decomposition is non-trivial, ambiguity remains, or the diff is large enough that a second pass is likely to catch real gaps.
- Validate with narrow tests first, then the relevant crate/module gates. Avoid repeated full workspace gates unless the changed surface warrants them.
- Finish with mode, task completion, changed files, validation results, review/audit result if run, and caveats.

### Full-Audit

Use when the user explicitly asks for sub-agent review/audit, or when the solution touches high-risk areas:

- durable state, session/control logs, recovery, permissions, approval, tools, workspace confinement, provider contracts, migrations, public APIs, release/distribution, or multi-crate behavior
- broad TUI workflows where state model, event flow, renderer, key/help hints, docs, and tests must move together
- large technical solutions with multiple dependent tasks or explicit acceptance gates

Full-audit mode uses the original strict workflow:

1. Build a full requirement-to-task decomposition with source section, files/modules, tests/docs, dependencies, acceptance criteria, and validation command for each task.
2. Run a pre-implementation sub-agent decomposition review when tooling is available; otherwise perform a separate local decomposition audit and report that no sub-agent was available.
3. Implement task by task. Mark a task done only after code, tests, docs, and its validation are complete.
4. Run the strongest feasible validation for the touched area, including coverage/docs/site/manual TUI checks when required by the solution or repository standards.
5. Run two post-implementation independent passes when tooling is available: code/project standards review and completeness audit against solution, final task list, diff, and validation results.
6. Fix every valid finding, rerun relevant validation, and record concrete reasons for any rejected findings.
7. Finish with the full report: implemented solution, requirement-to-task completion, files/modules, validation, sub-agent results, fixes after review, caveats, and next steps.

## Escalation Rules

Escalate to a higher intensity if implementation reveals unplanned public behavior, missing requirements, cross-crate coupling, migrations, permission/session/tool/provider risk, failing broad tests, or review findings that change the design. Do not downgrade below an explicit user request for review, sub-agents, coverage, or full validation.

## Execution Flow

1. Ingest the solution and extract goals, non-goals, required behavior, APIs/data/config, docs/tests, acceptance criteria, validation gates, approved defers, and blocking ambiguities.
2. State the selected intensity and maintain a visible checklist sized to that intensity.
3. Read relevant existing code/tests before editing. Prefer repository helpers and patterns over new abstractions.
4. Implement the smallest complete change. Add or update unit tests for new business logic, and update docs when public behavior, config, commands, TUI flow, safety/privacy, or architecture changes.
5. Validate at the selected intensity. If a required full gate is too slow or blocked, run the best narrower gate and report exactly what was not run and why.
6. Review/audit according to the selected intensity. Never claim a sub-agent review happened if it did not.
7. Fix valid findings and rerun relevant validation.
8. Final response should be concise. Include intensity used, what changed, validations, review/audit status, and remaining caveats.
