# Common Workflows

[Docs home](README.md) · [简体中文](../zh-CN/workflows.md)

These examples are starting points for day-to-day repository work. They assume you are running the TUI with:

```bash
cd /path/to/workspace
sigil
```

## Explore An Unfamiliar Repository

Start read-only:

```text
Explain the repository structure. Identify the main entrypoints, test layout, configuration files, and user documentation.
```

Follow up with a focused question:

```text
Trace how a user prompt moves from the TUI composer into the agent runtime. Include the main files and state transitions.
```

Good signs:

- Sigil cites concrete files it read.
- Tool activity stays read-only.
- You can ask it to narrow the answer to a crate, module, or path.

## Make A Small Change Safely

Give the goal, scope, and verification expectation:

```text
Update docs/en/quickstart.md so the first-run path is clearer for new users.
Keep the change docs-only.
After editing, check links and run the static docs checks if available.
```

When approval appears, review the diff before allowing it. After the run:

```bash
git diff
```

If the edit is too broad, deny it and restate the scope.

## Plan A Larger Feature Or Refactor

Use `/plan` when the task crosses multiple files or needs sequencing:

```text
/plan add a troubleshooting section for terminal copy failures and link it from the TUI guide
```

Then review the plan before letting the executor continue. You can steer the next step:

```text
Keep this docs-only. Do not edit Rust code. Update both English and Chinese docs.
```

Use:

```text
/plan continue
```

when the latest task should continue without extra guidance.

## Debug A Failing Command

Paste the failing command and the relevant output:

```text
cargo test failed in crates/sigil-tui. The failing assertion says the help text is missing Alt-D.
Find the source of that help text, explain the likely cause, and propose the smallest fix before editing.
```

For safer debugging, ask Sigil to inspect first and edit second:

```text
Read the failing test and implementation first. Summarize the root cause and wait before changing files.
```

## Review Local Changes

Ask for a review stance:

```text
Review the current diff for user-facing regressions, stale docs, and missing validation. List findings by severity with file references.
```

Then decide whether to apply fixes:

```text
Fix the high-severity docs findings only. Leave unrelated Rust changes untouched.
```

## Resume Previous Work

Use:

```text
/resume
```

Select a session from the list. Restored sessions rebuild visible conversation and durable task state. Tools that were interrupted are shown as interrupted; Sigil does not silently replay them.

If the latest planned task is still unfinished:

```text
/plan continue
```

or type guidance in the composer.

## Use Code Intelligence

Enable it in config:

```toml
[code_intelligence]
enabled = true
startup = "lazy"
```

In the TUI, use:

```text
Alt-D
```

to run diagnostics over changed source files. Code intelligence can also provide symbols, definitions, references, code actions, and rename previews when an LSP server is available.

If no LSP server is available, normal chat and file tools still work. See [configuration.md](configuration.md) and [troubleshooting.md](troubleshooting.md).

## Connect External Tools With MCP

Use MCP when Sigil needs tool-backed access to external systems or specialized local capabilities.

Typical pattern:

1. Configure a server in `[[mcp_servers]]`.
2. Set a conservative trust policy.
3. Start with `approval_default = "ask"`.
4. Use `/doctor` to check command and trust configuration.
5. Let Sigil list and call MCP tools only after you understand what the server can access.

See [mcp.md](mcp.md).

## Prompt Patterns That Work Well

Good prompts include:

- The exact goal.
- Relevant files, modules, or commands.
- What not to touch.
- How to verify the result.
- Whether Sigil should propose first or edit immediately.

Example:

```text
Improve docs/en/configuration.md for new users.
Keep provider-specific advanced fields, but move the common path before the full reference.
Update the Chinese mirror if needed.
Run docs link/path checks after editing.
```

## What To Review Yourself

Sigil can inspect, edit, and run commands, but you should still review:

- `git diff`
- generated or changed tests
- command output
- approval diffs
- config files that may contain secrets
- MCP servers before allowing secret egress or write actions
