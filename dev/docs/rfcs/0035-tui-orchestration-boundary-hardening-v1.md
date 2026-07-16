# RFC-0035 TUI Orchestration Boundary Hardening V1

状态：accepted / R35.0-R35.3 complete; R35.4 in progress

创建日期：2026-07-16

基线：

- Source review: [2026-06-30 Sigil Full Project Architecture and TUI Analysis](../../../.repo-local-dev/review/2026-06-30-sigil-full-project-architecture-and-tui-analysis.md)
- Extends: [RFC-0017 Architecture and TUI Productization Execution Plan](../../../.repo-local-dev/rfcs/0017-architecture-tui-productization-execution-plan.md)
- Extends: [RFC-0019 File Responsibility Split Execution Plan](../../../.repo-local-dev/rfcs/0019-file-responsibility-split-execution-plan.md)
- Acceptance baseline: [RFC-0034 Alpha Dogfood Stabilization V1](0034-alpha-dogfood-stabilization-v1.md)
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)

## 1. Summary

RFC-0017 and RFC-0019 converted the original TUI worker loop, runtime agent tools, HTTP adapter and built-in tools into module facades. That physical split succeeded, but subsequent session, compaction, verification, task, agent, web and lifecycle work concentrated orchestration back into `runner/worker_loop/scheduler.rs`.

The current scheduler is a single long-running function that owns unrelated mutable state and dispatches every `WorkerCommand`. `AppState` has four established domain bundles, while checkpoint/review, timeline/tool activity, agent panel and egress disclosure state still live as independent root fields.

This RFC hardens those two ownership boundaries without changing ordinary product behavior. It introduces explicit worker-loop state ownership, splits scheduler advancement and command dispatch by domain, unifies session transitions and completes the remaining high-churn `AppState` bundles. The durable event stream, command/message protocol, provider/tool behavior, permissions, approval and visible TUI interaction remain unchanged, except that session transitions fail closed while a detached background agent is still owned by the current worker. That narrow correction prevents child results and live status from crossing into another active session.

## 2. Goals

1. Give every long-lived mutable worker-loop value one explicit owner.
2. Keep the scheduler responsible only for polling, delegating state advancement, command dispatch and message publication.
3. Route commands through exhaustively matched domain handlers instead of one monolithic command match.
4. Reuse one session transition path for switch, new-session and other accepted-session reloads while preserving runtime attachments, trust and queue recovery.
5. Move the remaining coherent TUI field groups into `TimelineState`, `ReviewState`, `AgentPanelState` and `EgressDisclosureState`.
6. Prove behavior preservation with existing runner/state tests plus the real-binary stateful PTY acceptance.
7. Prevent detached background-agent results or live events from being attributed to a different active session.

## 3. Non-goals

- No new command, key binding, panel, config field or product workflow.
- No `WorkerCommand` or `WorkerMessage` shape, serialization or ordinary routing change.
- No session/control/event schema change and no migration.
- No provider, tool, permission, approval, sandbox, egress or verification policy change.
- No runtime move into `sigil-kernel` and no new crate.
- No Remote MCP OAuth, persistent semantic graph, SQLite projection, physical worktree or Windows restricted backend work.
- No release or version change.

## 4. Current risk

At the RFC baseline:

- `runner/worker_loop/scheduler.rs` is 3,687 lines.
- `run_worker_loop` contains almost the entire file and dispatches 56 distinct `WorkerCommand` variants.
- the file has been touched by 30 commits since 2026-06-30;
- session switch and new-session paths duplicate attachment, trust, queue reconciliation and message publication steps;
- detached background agents can currently finish after a session transition and be collected into the newly active session;
- `AppState` still owns checkpoint/review, timeline/tool activity, agent panel and egress disclosure fields directly.

Line count is evidence of concentration, not the acceptance target by itself. The target is explicit state ownership and auditable transition boundaries.

## 5. Worker-loop ownership

The worker loop will use one internal state aggregate with focused sub-state:

- session identity, store path, loaded session, runtime attachment resolver and exact prompt cache;
- active run, discarded run ids and run counters;
- manual/automatic/pre-turn compaction preparation and latches;
- queue continuation and agent-result continuation scheduling;
- MCP/provider/terminal refresh state;
- agent supervisor and background agent runs.

Session-scoped state has one explicit reset operation. A transition clears manual/pre-turn compaction, queued pre-turn blocks and idle-auto state, aborts compaction preparation, rebuilds pending agent-result continuations from the target session and preserves exact prompt material only when the durable logical session scope is unchanged.

Immutable dependencies such as root config, provider capabilities, workspace root, run options and message/handler endpoints remain explicit scheduler dependencies. State types stay private to `sigil-tui::runner`; they do not become kernel truth or public protocol types.

## 6. Command dispatch boundary

`WorkerCommand` remains the single runner protocol. Scheduler tick work first moves behind focused advancement functions:

- compaction preparation results;
- foreground run results;
- background agent completion;
- terminal/provider/MCP refresh;
- queue and continuation admission.

An internal exhaustive classifier then routes owned commands to these handler domains:

- run and plan;
- session lifecycle;
- queue and compaction;
- agent and task;
- verification and checkpoint;
- provider and MCP;
- maintenance and shutdown.

Each handler may mutate only the state it requires and must publish existing `WorkerMessage` values. Unknown/default command fallbacks are forbidden; adding a protocol variant must fail compilation until it is classified and handled.

## 7. Session transition contract

The shared session transition helper must:

1. reject transitions while a foreground or detached background run is active; the background guard is the only newly visible fail-closed correction in this RFC;
2. cancel stale compaction preparation and clear request-local pending state;
3. load the target session through the existing runtime-attachment-aware loader;
4. clear exact prompt material only when the logical session identity changes;
5. rebuild pending agent-result continuation state from the target session;
6. reconcile stale dispatching queue items using the existing best-effort recovery contract: projection/append failure emits the existing notice and does not turn the transition into a new transactional operation;
7. reset idle-auto compaction, pending manual/pre-turn preparation and queued pre-turn block state;
8. preserve workspace trust only through the current exact-workspace durable trust rule;
9. update the active log path only after required fallible preparation succeeds; best-effort queue reconciliation is explicitly outside this guarantee;
10. publish the command-specific existing `SessionSwitched` or `NewSessionStarted` message.

No helper may rewrite existing session bytes or manufacture durable evidence.

## 8. AppState ownership

`AppState` remains the TUI facade. The following groups move into private structs in `app/state.rs`:

- `TimelineState`: non-public streaming indexes, render store, selection, revision and tool-activity presentation state. The existing public `timeline`, `events`, `timeline_scroll_back` and `activity_scroll_back` fields remain on `AppState` to preserve the current Rust API;
- `ReviewState`: checkpoint restore request/preview lifecycle, latest restore/readiness sequence state and verification card focus/inspect state;
- `AgentPanelState`: sidebar selection and active child transcript/view;
- `EgressDisclosureState`: pending/recent disclosure and rendered acknowledgement state.

Renderer and flow modules continue consuming `AppState`; they do not gain independent domain truth. Field moves must not alter focus, layout, key routing or event ordering.

## 9. Implementation slices

1. R35.0: RFC, execution ledger, baseline measurements, behavior matrix and decomposition audit.
2. R35.1: worker-loop state ownership and focused state tests.
3. R35.2: focused scheduler advancement plus exhaustive domain command dispatch split.
4. R35.3: unified session transition/reload path and regression tests.
5. R35.4: remaining `AppState` domain bundles and state-transition regression tests.
6. R35.5: architecture/status calibration, stateful PTY acceptance, full gate and two independent audits.

## 10. Acceptance criteria

- `WorkerCommand` and `WorkerMessage` public shapes and behavior are unchanged.
- scheduler-owned mutable values are grouped under explicit private state owners.
- command dispatch is exhaustively classified; no wildcard silently accepts new variants.
- the scheduler loop no longer contains domain-sized advancement or command implementations.
- switch/new-session behavior shares one transition implementation and retains distinct existing messages.
- `AppState` facade retains top-level composition and its existing public compatibility fields; remaining private fields that belong to the four new bundles move to their owner.
- session switch/new-session rejects while detached background handles exist, so child events and result evidence cannot cross session scope.
- runner, compaction, session lifecycle, app, view and stateful PTY tests pass.
- no public user-guide update is required because no new workflow is added; the narrow background-run transition guard remains documented as a safety correction in this RFC.
- architecture and repo-local execution ledgers describe the resulting ownership accurately.

## 11. Validation

```bash
cargo fmt --all --check
cargo check -p sigil-tui
cargo test -p sigil-tui runner -- --format terse
cargo test -p sigil-tui app -- --format terse
cargo test -p sigil-tui view -- --format terse
cargo clippy -p sigil-tui --all-targets -- -D warnings
python3 scripts/test-tui-stateful-pty-acceptance.py
cargo build --release -p sigil
shasum -a 256 target/release/sigil
python3 scripts/tui-stateful-pty-acceptance.py \
  --binary target/release/sigil \
  --tokenizer-json <checksum-pinned-tokenizer-json> \
  --expected-binary-sha256 <fresh-build-sha256> \
  --expected-tokenizer-sha256 <pinned-tokenizer-sha256> \
  --output-dir .repo-local-dev/tui-stateful-acceptance/r35-final
./scripts/check-touched.sh --scope range --base <r35-baseline-commit> --tier full
git diff --check
```

The stateful campaign uses only its loopback fixtures. No paid provider or public network request is part of this RFC.

## 12. Progress

- R35.0 complete. Baseline runner and session-lifecycle tests pass. Independent decomposition review found no P0/P1 and identified five P2 contract gaps plus one P3 compatibility risk; the accepted design now freezes background-agent transition safety, complete session-scoped reset, best-effort queue recovery semantics, tick advancement extraction, fresh-binary admission and public `AppState` field compatibility before implementation begins.
- R35.1 complete. The scheduler now owns its mutable session, run, compaction,
  refresh, continuation and background-agent state through one private
  `WorkerLoopState`; the change preserves command/event order and is covered by
  focused construction plus full runner regression tests.
- R35.2 complete. The 3,687-line scheduler hotspot is now a 251-line loop
  coordinator. Seven focused advancement functions own refresh, compaction,
  run-result, idle-compaction, background-agent, continuation and queue
  progress; an exhaustive classifier routes all 56 commands into seven domain
  handlers with no classifier wildcard.
- R35.3 complete. Switch and new-session commands now share one transition
  implementation. It fails closed for foreground and detached background runs,
  commits the target path only after fallible loading/trust work, preserves
  exact prompts only for the same durable scope and resets/rebuilds all frozen
  session-scoped scheduler state while retaining best-effort queue recovery.
- R35.4 in progress.
