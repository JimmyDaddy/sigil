<!-- public-doc-role: workflows; authority: task-workflow-authority; sections: explore-an-unfamiliar-repository,make-a-small-change-safely,plan-a-larger-feature-or-refactor,debug-a-failing-command,review-local-changes,resume-previous-work,use-code-intelligence,connect-external-tools-with-mcp,what-to-review-yourself; cta: use-cookbook -->

# Common Workflows

[Docs home](README.md) · [Cookbook](cookbook.md) · [简体中文](../zh-CN/workflows.md)

These workflows describe the checkpoints and decisions around a task. Copyable prompt-only versions live in the [Cookbook](cookbook.md).

## Explore An Unfamiliar Repository

Ask Sigil to stay read-only and identify entrypoints, tests, configuration, and user docs. A useful result cites concrete files and makes uncertainty visible. Narrow the next question to one directory or behavior before asking for changes.

## Make A Small Change Safely

State the goal, allowed files, what must stay untouched, and how to verify the result. Review the approval diff before allowing an edit. Finish with `git diff` and the smallest relevant project check; deny and narrow any proposal that grows beyond the stated scope.

## Plan A Larger Feature Or Refactor

Use `/plan <prompt>` when you want a read-only plan before committing to execution. Accept the Plan ready card only after the steps and boundaries look right. Use `/task <task>` when you already want a multi-step task, and `/task continue` when the latest task should proceed without new guidance.

Keep steering instructions concrete, for example:

```text
Keep this docs-only. Update English and Chinese together. Do not edit Rust code.
```

## Debug A Failing Command

Provide the command, relevant output, and expected behavior. Ask Sigil to read the failing test and implementation, explain the likely cause, and wait before editing. Once the cause is clear, request the smallest fix and rerun the same failing check.

## Review Local Changes

Ask for findings by severity with file references. Decide which findings to fix, and keep unrelated working-tree changes out of scope. After fixes, review the live diff again rather than relying on the earlier report.

## Resume Previous Work

Open `/resume`, choose a session, and read the restored context before continuing. Interrupted tools remain visibly interrupted and are not rerun automatically. Give new guidance in the composer or use `/task continue` for an unfinished task.

## Use Code Intelligence

When enabled, code intelligence can help with symbols, definitions, references, diagnostics, code actions, and rename previews. Press `Alt-D` for diagnostics on changed source files. If no language server is available, normal chat and file tools still work; use [Configuration](configuration.md) and [Troubleshooting](troubleshooting.md) for setup.

## Connect External Tools With MCP

Configure one server, start with conservative trust, run `/doctor`, and inspect what the server can access before allowing calls or credentials. Setup and authentication belong in the [MCP guide](mcp.md).

## What To Review Yourself

Always review the final diff, changed tests, command output, configuration files that may contain secrets, and any external service allowed to receive data.

<!-- public-doc-cta: use-cookbook -->
Next: [Open the Cookbook for copyable prompts](cookbook.md).
