<!-- public-doc-role: cookbook; authority: copyable-prompts; sections: explore-a-repository,make-a-small-docs-change,make-a-small-code-change,review-a-diff,fix-a-failing-test,improve-documentation-structure,use-planning,work-with-mcp,investigate-terminal-issues,ask-for-proposal-first,guard-rails-to-add-to-prompts; cta: apply-prompt -->

# Cookbook

[Docs home](README.md) · [Workflows](workflows.md) · [简体中文](../zh-CN/cookbook.md)

Use these prompts as starting points. Adjust file paths, scope, and verification commands for your repository.

## Explore A Repository

```text
Explain this repository structure for a new contributor. Identify the important components, test layout, configuration files, user docs, and likely starting points.
```

```text
Investigate how <feature or command> behaves. List the files you read, what you found, and where a user would see errors.
```

## Make A Small Docs Change

```text
Improve docs/en/quickstart.md for first-time users.
Keep this docs-only.
Do not change Rust code.
After editing, check Markdown links and run the Pages check if available.
```

## Make A Small Code Change

```text
Implement <specific behavior> in <specific module>.
Before editing, read the current tests and explain the smallest change.
After editing, run the narrow relevant test first.
```

## Review A Diff

```text
Review the current diff for bugs, user-facing regressions, stale docs, and missing tests.
Lead with findings by severity and include file references.
Do not edit files during this review.
```

## Fix A Failing Test

```text
The command `<command>` fails with this output:

<paste output>

Find the root cause from the implementation and tests. Explain it first, then apply the smallest fix and rerun the failing command.
```

## Improve Documentation Structure

```text
Review the user docs as if you were a new user.
Identify where the reading path is unclear, where information is duplicated, and where an example would reduce confusion.
Then update both English and Chinese docs consistently.
```

## Use Planning

```text
/plan split the configuration guide into first-run setup, common tasks, and full reference without changing product behavior
```

Follow up:

```text
Keep this docs-only. Update English and Chinese mirrors. Run docs checks after each structural change.
```

## Work With MCP

```text
Inspect the configured MCP servers and explain which tools are available, what trust class they use, and which actions would require approval.
Do not call external tools yet.
```

## Investigate Terminal Issues

```text
Use doctor output to explain why OSC52 copy or mouse capture is not working in this terminal. Recommend the smallest config change and point to the terminal compatibility checklist.
```

## Ask For Proposal First

```text
Propose a small implementation plan before editing. Include files, expected tests, and risks. Wait for confirmation before applying changes.
```

## Guard Rails To Add To Prompts

Add these lines when useful:

```text
Scope only: <paths>.
Do not touch unrelated files.
Do not commit.
Prefer existing patterns in this repository.
Run only docs checks; this is a docs-only change.
If a tool needs write access, show the diff before applying it.
```

<!-- public-doc-cta: apply-prompt -->
Next: [Choose the matching workflow](workflows.md).
