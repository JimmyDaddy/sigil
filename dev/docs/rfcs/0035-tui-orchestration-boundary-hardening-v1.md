# RFC-0035 TUI Orchestration Boundary Hardening V1

状态：complete / R35.0-R35.5

创建日期：2026-07-16

基线：

- Local source review evidence: `.repo-local-dev/review/2026-06-30-sigil-full-project-architecture-and-tui-analysis.md`
- Local predecessor ledger: `.repo-local-dev/rfcs/0017-architecture-tui-productization-execution-plan.md`
- Local predecessor ledger: `.repo-local-dev/rfcs/0019-file-responsibility-split-execution-plan.md`
- Acceptance baseline: [RFC-0034 Alpha Dogfood Stabilization V1](0034-alpha-dogfood-stabilization-v1.md)
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)

## 1. Summary

RFC-0017 and RFC-0019 converted the original TUI worker loop, runtime agent tools, HTTP adapter and built-in tools into module facades. That physical split succeeded, but subsequent session, compaction, verification, task, agent, web and lifecycle work concentrated orchestration back into `runner/worker_loop/scheduler.rs`.

The current scheduler is a single long-running function that owns unrelated mutable state and dispatches every `WorkerCommand`. `AppState` has four established domain bundles, while checkpoint/review, timeline/tool activity, agent panel and egress disclosure state still live as independent root fields.

This RFC hardens those two ownership boundaries without changing ordinary product behavior. It introduces explicit worker-loop state ownership, splits scheduler advancement and command dispatch by domain, unifies session transitions and completes the remaining high-churn `AppState` bundles. The durable event stream, command/message protocol, provider/tool behavior, permissions, approval and visible TUI interaction remain unchanged, except that session transitions fail closed while a detached background agent is still owned by the current worker and target-session agent trust/policy projections replace source-session runtime surfaces before activation. Those narrow corrections prevent child results, live status or agent permissions from crossing into another active session.

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

An internal exhaustive classifier consumes the public command and converts it
to one of seven private, domain-typed command enums:

- run and plan;
- session lifecycle;
- queue and compaction;
- agent and task;
- verification and checkpoint;
- provider and MCP;
- maintenance and shutdown.

Each handler accepts only its domain enum, may mutate only the state it requires
and must publish existing `WorkerMessage` values. Unknown/default command
fallbacks are forbidden; adding a protocol variant fails compilation until the
classifier converts it and the destination handler exhaustively handles its
typed command.

## 7. Session transition contract

The shared session transition helper covers switch, new-session, local-session
fork activation and checkpoint-fork activation. It must:

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

Before committing the target session, the helper also rebuilds the
session-projected `AgentSupervisor` and replaces the seven agent-tool surfaces
with the target session's trust/policy projection. Fork callers retain their
existing `LocalSessionForked` or `ConversationForked` message.

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
- switch/new-session/local-fork/checkpoint-fork activation shares one transition implementation and retains distinct existing messages.
- `AppState` facade retains top-level composition and its existing public compatibility fields; remaining private fields that belong to the four new bundles move to their owner.
- switch/new-session/local-fork/checkpoint-fork activation rejects while detached background handles exist, so child events and result evidence cannot cross session scope.
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
./scripts/check-touched.sh --scope base --base <r35-baseline-commit> --tier full
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
  progress; an exhaustive classifier converts all 56 public commands to seven
  private domain-typed enums whose handlers have no wildcard or runtime
  misroute fallback.
- R35.3 complete. Switch, new-session, local-session fork activation and
  checkpoint-fork activation now share one transition implementation. It fails
  closed for foreground and detached background runs, commits the target path
  only after fallible loading/trust work, preserves exact prompts only for the
  same durable scope, rebuilds the target session's agent supervisor and
  model-visible agent-tool surface, and resets/rebuilds all frozen
  session-scoped scheduler state while retaining best-effort queue recovery.
- R35.4 complete. Private timeline presentation, review/checkpoint,
  agent-panel and egress-disclosure state now live in four focused bundles.
  The public timeline/event/scroll fields remain on `AppState`, and focused
  app/view/tool/checkpoint/egress regression suites preserve UI behavior.
- R35.5 complete. The final base-range full gate passed all workspace tests
  (including TUI 1,377 passed and 4 expected ignored) plus workspace Clippy
  with warnings denied. A freshly built release-profile binary from
  `30b7141d` passed the checksum-pinned stateful PTY campaign. Independent
  code-quality and solution-completeness re-audits found no remaining P1/P2.

## 13. Completion evidence

- Baseline commit: `b6e43ebb1073e415a11696dc83aee9a89ff6e518`.
- Slice commits: R35.0 `2d8627e4`; R35.1 `58accd0b`; R35.2
  `adda7f45`; R35.3 `6ca359d4`; R35.4 `40c40663`.
- Final-audit correction commit: `30b7141d`; it closes local/checkpoint fork
  transition bypass, target-session agent supervisor/tool-surface rebinding
  and typed handler exhaustiveness.
- Hotspot result: `scheduler.rs` moved from 3,687 lines containing one
  domain-sized loop to 251 lines coordinating seven advancement functions and
  seven typed command domains.
- Final full gate:
  `./scripts/check-touched.sh --scope base --base b6e43ebb1073e415a11696dc83aee9a89ff6e518 --tier full`;
  diff/fmt/check, all workspace tests and workspace Clippy passed.
- Fresh local acceptance binary: unchanged version `0.0.1-alpha.4`, commit
  `30b7141da522`, SHA-256
  `d90980cb1f16560b416785d4e1e4a67c837bc16178dcd764ad97738f9a138cab`.
- Pinned tokenizer snapshot:
  `60d8d70770c6776ff598c94bb586a859a38244f1`, SHA-256
  `8f9f37ca37fdc4f5fd36d5cf4d3b0e8392edb4e894fd10cc0d70b4957c8633cf`.
- Stateful PTY manifest (local ignored evidence):
  `.repo-local-dev/tui-stateful-acceptance/r35-final-30b7141d/manifest.json`,
  SHA-256
  `67517d73c5023e876d6652aa3007304de026f4b6afe5e6aea1f51ce1dad55fd8`.
  It proves one live/resumed final reply, one durable source/fork final answer,
  one V2 compaction, one checkpoint restore, one fork, successful fork resume
  and unchanged controlled-file state.
- The first two final audits found transition/trust/typed-dispatch gaps before
  completion; after `30b7141d` and the refreshed gates, both independent
  re-audits reported no remaining P1/P2 finding.
- No version, tag, release or public distribution action was performed.
