<!-- public-doc-role: reference; authority: command-key-path-authority; sections: tui-keys,slash-commands,cli-commands,machine-output-and-local-server,config-resolution,important-paths,web-tool-inputs,approval-outcomes,session-recovery-facts; cta: return-user-guide -->

# Command And Key Reference

[Docs home](README.md) · [User guide](user-guide.md) · [Configuration Reference](configuration-reference.md) · [简体中文](../zh-CN/reference.md)

Use this page for exact user-facing commands, keys, paths, outputs, and recovery behavior.

## TUI Keys

| Action | Key |
| --- | --- |
| Open help / slash selector | `F1` / `/` |
| Submit | `Enter` |
| Show or hide info rail | `F2` |
| Switch visible rail compact/detail | `Shift-F2` |
| Scroll transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| Cycle default permission mode | `Shift-Tab` |
| Insert composer newline | `Ctrl-J`; `Shift-Enter` / `Alt-Enter` when supported |
| Move composer cursor | `Ctrl-A/E`, `Ctrl-B/F`, `Alt-B/F`, arrows |
| Delete composer text | `Backspace/Delete`, `Ctrl-H/W`, modified Backspace/Delete |
| Kill/yank line tail | `Ctrl-K/Y` |
| Restore the last draft cleared with `Esc` | `Ctrl-Z` |
| Copy selected transcript text | `Ctrl-C` when a selection is active |
| Copy selection, or latest assistant reply when no selection is active | `Ctrl-L`; info rail is excluded |
| Cancel current run / close overlay | `Ctrl-C` with no selection / `Esc` |
| Focus and move through activity | `Ctrl-G`, `Alt-J` / `Alt-K` |
| Focus task verification | `Alt-V`; `Enter` runs, `I` inspects |
| Open latest checkpoint restore | `Ctrl-R`; `Enter` restores, `F` forks, `Esc` closes |
| Open saved-session actions | Select `/resume` row, then `Ctrl-O` or right-click |
| Cycle visible agent transcript | Agent panel, `Alt-A`, `Shift-Alt-A` |
| Expand/collapse thinking or activity | `Ctrl-T` |
| Run diagnostics on changed source | `Alt-D` |
| Cancel focused terminal task | `Alt-X` |

`Up/Down` first handles composer history or multiline movement. `Ctrl-Z` restores one cleared draft; it is not a general undo stack.

## Slash Commands

| Command | Purpose |
| --- | --- |
| `/config` | Open configuration |
| `/doctor` | Run diagnostics |
| `/feedback` | Preview and export a local support report |
| `/new` | Start a fresh session |
| `/resume` | Select a saved session |
| `/agent <main|child-id>` | Switch visible transcript |
| `/agent rename <child-id|current> <name>` | Name a child transcript |
| `/agent cancel <child-id|current>` | Cancel a running child with a live handle |
| `/queue` | Show advanced follow-up controls |
| `/queue next|interrupt|edit|delete [item]` | Reorder, interrupt for, edit, or remove a follow-up |
| `/plan [prompt]` | Run a read-only plan; accept its card to start a task |
| `/task <task>` | Start multi-step execution |
| `/task continue` | Continue the latest unfinished task |
| `/model <flash|pro|id>` | Switch model for the next run and start a fresh session |
| `/effort <low|medium|high|max>` | Change reasoning effort for the next run |
| `/compact` | Review and, when ready, apply one context reduction |
| `/quit` | Quit the TUI |

Aliases: `/m` for `/model`, `/e` for `/effort`, and `/q` or `/exit` for `/quit`. Candidate commands use `Up/Down`, `Tab`, and `Enter`.

## CLI Commands

| Command | Use |
| --- | --- |
| `sigil` | Open the TUI in the current workspace |
| `sigil doctor [--output text|json]` | Run local diagnostics |
| `sigil run "<task>" [--output text|json|jsonl]` | Run a non-interactive task |
| `sigil resume [session-id]` | Open the TUI and restore a session |
| `sigil serve` | Start the authenticated loopback-only local service |
| `sigil --version` | Print the installed version |
| `sigil --config <path> doctor` | Diagnose an explicit config |

## Machine Output And Local Server

`sigil run --output json` writes one result to stdout. `jsonl` writes ordered events followed by one result or error. Human progress and safe network notices stay on stderr. Exit codes are `0` success, `1` execution failure, `2` invalid invocation/configuration, and `130` cancellation.

Start the local service with a high-entropy environment token:

```bash
export SIGIL_HTTP_TOKEN="$(openssl rand -hex 32)"
sigil serve
```

The service prints its selected loopback address. `GET /health` is unauthenticated; OpenAPI, disclosure, session, run, event, cancellation, approval, and historical catalog routes require `Authorization: Bearer <token>`. It is not a remote or multi-user service, does not use cookie auth or wildcard CORS, and shuts down on `Ctrl-C`.

Trusted local launchers can request a one-line, secret-free readiness object and tie the child lifetime to a private stdin pipe:

```bash
sigil serve --startup-output json --shutdown-on-stdin-close
```

The authenticated `GET /server-info` response uses the same versioned schema. Run one server per workspace; the bearer token belongs in the child environment, never in arguments or logs. Closing the owner pipe starts the same graceful drain as `Ctrl-C`; without the flag, terminal stdin does not control server lifetime.

`GET /sessions` lists live handles owned by the current server process. For restart-durable workspace history, use `GET /session-catalog?limit=50&q=...&provider=...&pinned=true&state=ready`. The catalog returns an OpenAPI-defined allowlist of compact, safely projected metadata with an opaque `next_cursor`; storage hashes, record checksums, active runs, approvals, and progress are not part of this response. If history changes between pages, a `409 stale_cursor` response means the client must restart from the first page. The catalog is a rebuildable index over session logs, so a catalog failure does not stop runs or session recording.

To continue a ready catalog entry after a server restart, send its relative `session_ref` and expected durable `session_id` to authenticated `POST /sessions/open`. The server revalidates the session log rather than trusting SQLite, creates no run or provider request, and returns one process-local session handle. Repeating the same open in one server process returns that same handle. Missing, non-ready, or identity-changed sources fail closed; clients should query the catalog again instead of constructing filesystem paths.

## Config Resolution

Sigil uses `--config <path>` when supplied; otherwise it loads `~/.sigil/sigil.toml`. A workspace-root `sigil.toml` is not loaded automatically.

## Important Paths

| Path | Meaning |
| --- | --- |
| State root `workspaces/<workspace-id>/sessions/` | Session logs |
| State root `workspaces/<workspace-id>/input-history.jsonl` | Composer history |
| State root `workspaces/<workspace-id>/artifacts/` | Terminal and change artifacts |
| Cache root `workspaces/<workspace-id>/tmp/` | `$SIGIL_SCRATCH_DIR` |
| User config `~/.sigil/sigil.toml` | Default local config |
| `.sigil/agents`, `.sigil/commands`, `.sigil/skills`, `.sigil/plugins` | Workspace resources |
| `SIGIL.md`, `AGENTS.md`, `SIGIL.local.md` | Workspace instructions |

Do not commit real secrets in config or local instruction files.

## Web Tool Inputs

| Tool | Input | Boundary |
| --- | --- | --- |
| `websearch` | `query`; optional `max_results` | Uses the selected provider-hosted, configured MCP, or bundled route. |
| `webfetch` | observed `source_id`; optional `format`, `max_content_bytes` | Opens only a URL already observed in the current session. |

Both also follow `[web].network_mode`. `deny` blocks them; an unresolved `ask` cannot proceed headlessly.

## Approval Outcomes

| Outcome | Meaning |
| --- | --- |
| `allow` | Run the action |
| `deny` | Reject it |
| `timeout` | Deny after no decision |
| `approval_required` | A non-interactive run needed a decision it could not request |

## Session Recovery Facts

- Restart restores supported visible session and task state.
- An unfinished tool returns as interrupted and is not silently rerun.
- `/new` starts a fresh session; `/resume` selects an older one.
- Saved-session actions include resume, conversation fork, safe export, pin/unpin, and reviewed delete.
- Retention cleanup requires an explicit preview and confirmation under `/config` → **Storage**.
- Exiting prints the session id and `sigil resume <session-id>`.
- `/task continue` continues the latest unfinished task when one exists.

Provider credentials belong in [Providers](providers.md); config fields belong in [Configuration Reference](configuration-reference.md).

<!-- public-doc-cta: return-user-guide -->
Next: [Return to the User Guide](user-guide.md).
