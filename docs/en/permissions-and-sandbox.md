<!-- public-doc-role: permissions-and-sandbox; authority: permission-network-sandbox-authority; sections: choose-a-permission-mode,review-before-an-action-runs,narrow-command-and-path-rules,network-and-web-tools,sandbox-expectations; cta: review-safety -->

# Permissions And Sandbox

[Docs home](README.md) · [Configuration](configuration.md) · [Safety](safety.md) · [Privacy](privacy.md) · [简体中文](../zh-CN/permissions-and-sandbox.md)

This page is the operational authority for local permissions, external paths, network access, and sandbox expectations.

## Choose A Permission Mode

```toml
[permission]
mode = "manual"
```

| Mode | Use | Default behavior |
| --- | --- | --- |
| `read-only` | Exploration and review | Workspace reads and recognized read-only commands can run; writes and mutating or unclassified commands are denied. Network still follows its own policy. |
| `manual` | Normal interactive work | Reads proceed; changes and commands usually ask. |
| `auto-edit` | Supervised file editing | Workspace edits can proceed. Recognized workspace validation can proceed only when the selected execution backend proves every requested containment capability; otherwise Sigil asks. |
| `danger-full-access` | Closely supervised automation | Local access is broad, but network, protected paths, and other hard limits still apply. |

`manual` is the recommended starting point. A specific deny always remains stricter than a broad mode.

## Review Before An Action Runs

Check the summary, path or destination, command, and diff before choosing a decision. A plan or earlier approval is not permission for a different action. Headless `sigil run` cannot open an approval modal; an unresolved `ask` action fails.

Interactive approval surfaces show the safely projected command or tool input, the effects Sigil detected, the affected targets, and any containment capability the current backend cannot prove. Risk labels explain what an action may do; they do not decide approval by themselves. A command can therefore be medium risk and run automatically under proven containment, or require approval when the same containment is unavailable.

In the TUI, a request without a file diff puts the command or tool content in a dedicated high-contrast review area. The header keeps only the tool type, tool name, and risk; policy, effect, and containment details stay collapsed behind `M` by default. File writes use the exact diff as the primary review content. When there are no file changes, the TUI omits empty Diff and Details panels. The approval receipt repeats the allowed action instead of exposing an internal call identifier.

When the workspace is a Git worktree, the TUI keeps the current branch and compact change counts in the info rail. If the info rail is hidden or does not fit, the same status moves below the composer. The snapshot refreshes after shell, terminal, and file-changing tool results.

After the control route accepts a decision, Desktop and TUI immediately remove the decision buttons and show that execution is resuming. The later execution event changes that state to running. If the server cannot confirm whether the active run received the decision, the surface shows delivery as uncertain, disables duplicate decisions, and converges from the authoritative run snapshot instead of treating the action as pending again. Interactive tool approvals do not expire after five minutes: they remain pending until you decide, cancel the run, close the route, or the run/session ends. Retrying a temporarily interrupted decision reuses the same exact command id and request identity; a cancelled or stale request, changed command, changed policy, or changed execution profile must be reviewed again.

**Allow for session** appears only when Sigil can derive a bounded semantic grant for equivalent requests. The grant is append-only session state and survives leaving and reopening that same session. For recognized workspace validation, it binds the executable validation command and arguments while ignoring presentation-only output pipes such as `tail`, `head`, or `grep`; every concrete execution still receives a new exact plan/hash check. The grant also binds subjects, effect ceiling, workspace, normalized policy scope, execution backend, containment profile, and environment profile. It does not authorize arbitrary Shell commands, a different validation step or validation arguments, a changed destination, remote mutation, destructive or dynamic code, or a different risk class.

Sigil analyzes POSIX compound Shell commands one child at a time. Operators such as `&&`, `||`, `;`, and pipelines do not make an otherwise recognized validation chain “unknown”. Redirections, wrappers, dangerous flags, dynamic expansion, and nested executors are still evaluated separately; incomplete or unsupported analysis fails closed to `ask` (or `deny` in a headless run).

## Narrow Command And Path Rules

```toml
[permission.commands]
allow = ["cargo test *", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

Prefer a few narrow patterns. When several rules match, deny wins over ask, and ask wins over allow. A raw command pattern records user intent; it is not a sandbox. An `allow` pattern cannot override a protected target, dynamic or invalid Shell analysis, unresolved destinations, privilege escalation, or missing mandatory containment.

In the interactive approval prompt, **Allow family** derives a conservative first-two-token pattern (`cargo test -p x 2>&1 | tail -60` becomes `cargo test*`) and appends it to `permission.commands.allow` for future runs. The action is offered only when the same bounded grant conditions hold (complete analysis, grantable risk and effects); it refuses destructive programs, `git` mutating subcommands, redirections, and command chains. **Allow session** covers argument variants of the same command family for the current session without writing configuration; unknown-family commands share the derived first-two-token prefix.

<!-- public-doc-topic: external-directory -->

Workspace-external paths are disabled by default:

```toml
[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

Enabling this section does not make every external path safe or accessible; each path still follows its rule and protected-path checks. Use `$SIGIL_SCRATCH_DIR` for command scratch files when possible. The scratch directory is scoped to the current session, private to this user, capped by a size quota, and reclaimed after a TTL; do not rely on it for long-term storage.

## Network And Web Tools

<!-- public-doc-topic: network-control -->

Network policy is independent of local permission mode:

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"
```

`allow` lets supported read-only search and fetch calls proceed while destination checks and limits still run. `ask` offers a one-time or same-tool session decision. `deny` disables Web access. A session decision never grants another tool, a write-like request, or a denied destination. Read [Privacy](privacy.md) before choosing a third-party route or sending sensitive queries.

Remote MCP and MCP OAuth follow this independent network boundary too. `auto-edit` does not silently authorize OAuth discovery, token exchange, refresh, or revocation. One sign-in can contact the MCP resource and a separate authorization server, so Sigil can show more than one destination disclosure. A session approval does not expose token values, authorize another kind of request, or bypass destination checks.

## Sandbox Expectations

<!-- public-doc-topic: sandbox-limit -->

Permission answers whether Sigil may attempt an action. A sandbox is an optional operating-system boundary applied afterward. The default local strategy is not an OS sandbox and does not guarantee filesystem, network, credential, or process isolation.

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt" # or linux_bubblewrap / docker
profile = "workspace_write"
fallback = "deny"
```

Availability and protection depend on the host, backend, profile, and action. Sigil binds every automatic execution or session grant to the backend's actual capability receipt; requested isolation is never treated as proof by itself. In particular, the current macOS Seatbelt backend does not claim network isolation, so commands that execute workspace code, such as `cargo check`, `cargo test`, or `cargo clippy`, require explicit one-time/session authority unless another selected backend proves the requested network denial. A sandboxed command does not make remote services, MCP servers, plugins, containers, or every process path safe. With `fallback = "deny"`, an unavailable backend stops the action instead of silently running it locally. Run `sigil doctor` after changing execution settings.

Finite checks and builds run through the foreground Shell tool and produce one final result. Persistent servers and interactive programs use an explicit terminal task. Terminal tasks publish readiness, output-generation, exit, cancellation, and interruption changes to Desktop and TUI; an agent that needs to wait uses one event-driven wait instead of repeatedly reading the log. Log reads remain explicit inspection operations.

Verification commands have their own declared behavior and approval needs. Configure them through [Advanced configuration](advanced-configuration.md#verification); field defaults are in [Configuration Reference](configuration-reference.md#permission).

<!-- public-doc-cta: review-safety -->
Next: [Review the Safety decision checklist](safety.md).
