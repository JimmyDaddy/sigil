---
name: technical-solution-executor
description: Execute an input technical solution end-to-end for this repository. Use when the user provides or points to a 技术方案 / technical solution / execution plan and wants task decomposition, implementation, self-checking, tests, sub-agent code review, implementation-completeness audit, fixes, and a final execution summary.
---

# Technical Solution Executor

Use this skill to turn a technical solution into a complete, audited implementation. Treat the technical solution as the source of truth. Do not broaden scope unless required to compile or to satisfy an explicit requirement in the solution.

## Required Repository Alignment

Before editing implementation files in this repository, read and follow:

- `AGENTS.md`
- `dev/governance/code-standards.md`
- `dev/governance/engineering-standards.md`
- The technical solution supplied by the user
- Any implementation snapshot or architecture document explicitly referenced by the solution

If the task is docs-only, still check the relevant docs standards and paths. If the task changes TUI behavior, verify state model, event flow, renderer, key/help hints, docs, and tests together.

## Workflow

### 1. Ingest The Technical Solution

Identify the solution source:

- A pasted technical solution
- A file path, commonly under `.repo-local-dev/`
- A task list that references a solution file

Read the complete solution before decomposing work. If the input references sections, tasks, diagrams, acceptance criteria, or non-goals, keep those boundaries intact.

If a requirement is ambiguous enough that multiple incompatible implementations are possible, ask one concise clarification before editing. Otherwise make a conservative assumption and record it.

### 2. Decompose Tasks Completely

Create an implementation task list that covers every requirement in the solution.

Each task should include:

- The requirement or section it satisfies
- Expected code/docs/test files or modules
- Dependencies on prior tasks
- Acceptance criteria
- Suggested validation command

The decomposition must include tests, docs, migration/compatibility handling, and final gates when applicable. Do not create "later", "future", or "defer" items unless the technical solution itself explicitly marks them as out of scope or future work.

### 3. Self-Check The Decomposition

Before implementation, audit the task list:

- Every solution requirement maps to at least one task.
- Every task maps back to a solution requirement.
- Non-goals are not implemented.
- Ordering is executable without hidden prerequisites.
- Validation covers the riskiest behavior, not just formatting.
- No requirement is satisfied by a placeholder, mock, stub, hardcoded shortcut, or "pretend" behavior.
- No extra defer appears outside the supplied technical solution.

If the self-check finds a gap, fix the task list before editing.

### 4. Execute Task By Task

Implement one coherent task at a time. Prefer existing repository patterns and local helper APIs over new abstractions.

Mandatory execution rules:

- Follow `dev/governance/code-standards.md` and `dev/governance/engineering-standards.md`.
- Keep `sigil-kernel` provider-neutral; do not leak provider-private terms into public kernel APIs.
- Preserve append-only session/control semantics when touching durable state.
- Add or update unit tests for new business logic.
- Update user/developer docs when behavior, config, TUI flow, or public contracts change.
- Run the narrow relevant validation after each risky task, then broader gates at the end.
- Never use deceptive implementations: no mock/stub placeholders, hardcoded success paths, fake integrations, or functions that only satisfy current tests without implementing the requirement.
- Do not silently skip performance concerns; avoid repeated expensive work, unbounded output, unnecessary full rebuilds, and avoidable blocking in async paths.

If implementation reveals the technical solution is invalid or impossible, stop, explain the concrete blocker with file/line evidence where possible, and propose the smallest correction.

### 5. Review With Sub-Agents

After implementation and local validation, use sub-agents before finalizing.

Run at least these independent review passes when sub-agent tooling is available:

1. Code/project standards review:
   - Check against `AGENTS.md`, code standards, engineering standards, module boundaries, Rust style, tests, docs, and performance risk.
2. Implementation completeness audit:
   - Compare the technical solution, task decomposition, and actual diff.
   - Verify every requirement is implemented.
   - Verify no non-goal or unauthorized defer was introduced.
   - Verify there are no mock/stub/hardcoded/deceptive implementations.

Give each sub-agent the technical solution path or content, the task decomposition, and the relevant diff. Do not give them your expected answer.

If sub-agents find valid issues, fix them and rerun the relevant validation. If sub-agent tooling is unavailable, explicitly report that limitation and perform two separate local review passes with the same scopes; do not claim sub-agent review happened.

### 6. Final Verification

Run the strongest feasible validation for the touched area:

- Narrow crate/module tests for focused changes
- `cargo fmt --all --check`
- `cargo check`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- Docs/site checks when docs are touched

If a full gate is too slow or unavailable, run the relevant narrower gate and state exactly what was not run and why.

### 7. Final Summary

Report:

- Implemented tasks and the requirements they satisfy
- Files or modules changed
- Validation run and results
- Sub-agent review results and fixes applied
- Important caveats, performance notes, compatibility notes, or follow-up improvements

Do not mark the work complete if required tasks, validation, or review are still pending.
