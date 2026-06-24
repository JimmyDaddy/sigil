---
name: technical-solution-executor
description: Execute a technical solution end-to-end with strict task decomposition, sub-agent review, implementation, validation, code review, completeness audit, fixes, and a final report. Use when the user provides or points to a 技术方案 / technical solution / execution plan and expects complete implementation without unapproved defers, mocks, stubs, hardcoded shortcuts, or missing tests.
---

# Technical Solution Executor

Use this skill to turn a supplied technical solution into a complete, audited implementation. Treat the technical solution as the source of truth. Do not broaden scope unless required to compile, validate, or satisfy an explicit requirement in the solution.

## Required Alignment

Before editing implementation files, read and follow:

- `AGENTS.md`
- Repository code standards and engineering standards, especially `dev/governance/code-standards.md` and `dev/governance/engineering-standards.md` when present
- The full technical solution supplied by the user
- Any architecture, implementation snapshot, acceptance checklist, or execution plan explicitly referenced by the technical solution

If the solution changes TUI behavior, verify state model, event flow, renderer, key/help hints, docs, and tests together. If the solution changes durable state, session, permissions, tools, provider contracts, or migrations, preserve auditability and recovery semantics.

## Execution Contract

- Implement the complete confirmed scope. Do not introduce "later", "future", "temporary", "MVP", or "defer" items unless the technical solution explicitly marks them out of scope.
- Do not use deceptive implementations: no mocks, stubs, fake integrations, hardcoded success returns, tests-only branches, or shortcuts that satisfy current tests without implementing the requirement.
- Do not weaken, delete, or bypass tests to make validation pass.
- Keep changes scoped to the technical solution and necessary integration/doc/test updates.
- Prefer repository patterns and existing helper APIs over new abstractions.
- Maintain a visible task checklist while working and update it as tasks move through review, implementation, validation, and fixes.
- If the technical solution is internally inconsistent or impossible, stop before editing the affected area, explain the blocker with concrete evidence, and ask for a decision or propose the smallest correction.

## Workflow

### 1. Ingest The Technical Solution

Identify and read the complete source:

- A pasted technical solution
- A file path, commonly under `.repo-local-dev/`
- A task list that references a solution file

Extract:

- Goals and non-goals
- Required behavior and user-facing semantics
- Data models, APIs, config, migrations, docs, and tests
- Acceptance criteria and validation gates
- Explicitly approved future work or defers
- Ambiguities that would lead to incompatible implementations

Ask one concise clarification only when the ambiguity blocks correct implementation. Otherwise make a conservative assumption and record it in the task list.

### 2. Decompose All Work

Create a task list that covers every implementation requirement in the technical solution. Every task must include:

- Source requirement or section reference
- Implementation files/modules likely involved
- Required tests and docs
- Dependencies on earlier tasks
- Acceptance criteria
- Validation command or check

The task list must cover code, tests, docs, migration, config, TUI/CLI/help surfaces, error handling, and cleanup when applicable. Do not leave hidden work in prose; if it must happen, make it a task.

### 3. Review The Decomposition Before Editing

Before implementation, use a sub-agent to review the task decomposition against the technical solution when sub-agent tooling is available.

Sub-agent review prompt must provide only:

- The technical solution path or complete content
- The proposed task decomposition
- The requirement that the reviewer check for missing tasks, unauthorized defers, ordering gaps, missing tests/docs/migrations, and non-goal violations

Do not provide your expected answer. If sub-agent tooling is unavailable, explicitly state that limitation and run a separate local decomposition audit with the same checklist.

Fix every valid decomposition gap before editing code.

### 4. Execute Task By Task

Implement tasks in dependency order. For each task:

1. Read the relevant existing code and tests.
2. Make the minimal complete implementation.
3. Add or update unit tests for new business logic.
4. Update docs when public behavior, config, commands, TUI flow, safety, privacy, or architecture changes.
5. Run the narrow validation that proves the task works.
6. Mark the task done only after code, tests, docs, and validation for that task are complete.

Mandatory rules:

- Follow code and engineering standards strictly.
- Keep `sigil-kernel` provider-neutral in this repository.
- Preserve append-only session/control behavior when touching durable state.
- Keep workspace confinement and permission boundaries intact when touching tools or paths.
- Ensure unit test coverage meets the repository threshold. In this repository, use the repo coverage gate when business code changes and do not rely on superficial compile-only checks.
- Avoid performance regressions such as unbounded scans, repeated full rebuilds, unbounded output, unnecessary blocking in async paths, or cache-destabilizing request materials.

If a task uncovers extra necessary work, add it to the checklist and map it back to a source requirement. If it cannot be mapped, ask before expanding scope.

### 5. Validate The Full Implementation

Run the strongest feasible validation for the touched area:

- Narrow crate/module tests for focused changes
- `cargo fmt --all --check`
- `cargo check`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- Repository coverage gate, such as `./scripts/coverage.sh`, when business logic changed
- Docs/site checks when docs changed
- Manual TUI smoke checks when user-facing TUI behavior changed

If a full gate is too slow or blocked, run the relevant narrower gate and state exactly what was not run and why. Do not call the implementation complete when required coverage or critical validation is missing without reporting it as a blocker.

### 6. Post-Implementation Sub-Agent Reviews

After implementation and local validation, run two independent sub-agent passes when sub-agent tooling is available.

#### 6.1 Code And Project Standards Review

Ask one sub-agent to review the diff against:

- `AGENTS.md`
- Code standards
- Engineering standards
- Module boundaries
- Rust style and async/path/error handling rules
- Tests, docs, TUI help, performance, and safety/privacy risks

#### 6.2 Completeness Audit

Ask another sub-agent to compare:

- Technical solution
- Final task list
- Actual diff
- Validation results

The audit must verify:

- Every requirement is implemented.
- Every task is complete.
- No non-goal was implemented.
- No unapproved defer remains.
- No mock/stub/hardcoded/deceptive implementation exists.
- Tests and docs match the changed behavior.

Do not give reviewers your expected answer. If sub-agent tooling is unavailable, explicitly report that limitation and perform two separate local review passes with the same scopes; do not claim sub-agent review happened.

### 7. Fix Review Findings

For every valid review or audit finding:

1. Add a follow-up task.
2. Fix the issue.
3. Rerun the relevant validation.
4. If the fix changes behavior, update tests/docs.

If rejecting a finding, record the concrete reason. Do not ignore review output silently.

### 8. Final Report

Finish with a concise report that includes:

- Technical solution implemented
- Requirement-to-task completion summary
- Files/modules changed
- Validation commands and results
- Sub-agent decomposition review result
- Sub-agent code review result
- Sub-agent completeness audit result
- Fixes made after reviews
- Important notes or caveats, if any
- Recommended next steps, if any

Do not mark the work complete if required tasks, tests, coverage, docs, validation, or reviews are still pending.
