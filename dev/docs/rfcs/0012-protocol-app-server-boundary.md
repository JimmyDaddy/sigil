# RFC-0012 Protocol and App Server Boundary

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Related: [RFC-0008 Thread Projection and Agent Graph Observability](0008-thread-projection-and-agent-graph-observability.md)

## 1. Summary

本 RFC 定义 future `sigil-protocol` / app-server boundary。目标是在 TUI、CLI、未来 IDE/daemon/desktop 之间共享 command/event surface，同时不复制 agent loop、不绕过 permission/approval/sandbox/egress/verification control plane。

## 2. Goals

- 定义 versioned command envelope。
- 区分 durable protocol events 和 transient live events。
- 支持 `Last-Event-ID` durable replay。
- 防止旧客户端批准过期 tool call 或覆盖新状态。
- 让 TUI 未来可以逐步迁移到 command/event bridge。

## 3. Non-goals

- 不立即实现完整 HTTP listener 或 remote access。
- 不把 app-server 变成事实来源。
- 不承诺 transient token/reasoning delta replay。
- 不默认开放远程网络访问。

## 4. Command Envelope

```rust
struct CommandEnvelope<T> {
    protocol_version: u16,
    command_id: CommandId,
    client_id: ClientId,
    session_id: SessionId,
    expected_stream_sequence: Option<u64>,
    correlation_id: Option<EventId>,
    payload: T,
}
```

Rules:

- `command_id` deduplicates retries.
- `expected_stream_sequence` prevents stale clients from mutating newer state.
- `client_id` is audited.
- `correlation_id` links command to durable events.

## 5. Initial Commands

- `StartTurn`
- `ApproveTool`
- `CancelTurn`
- `SpawnAgent`
- `ContinueTask`
- `RestoreCheckpoint`
- `RevertSession`
- `UnrevertSession`

Approval command must include:

- `approval_request_id`
- `tool_call_hash`
- `policy_version`
- `expires_at`

If tool arguments, policy or expiry changed, old approval is rejected.

## 6. Protocol Events

```rust
enum ProtocolEvent {
    Durable(DurableEventView),
    Transient(LiveEventView),
}
```

Durable events get replay cursor. Transient events do not.

Initial event surface:

- `ReasoningDelta`
- `ToolStarted`
- `ToolCompleted`
- `VerificationUpdated`
- `AgentStatusChanged`
- `ContextSourcesUpdated`
- `SandboxDecisionRecorded`

## 7. Server Boundary

`sigil-app-server` owns:

- auth
- local routing
- session/run registry
- command/event transport
- SSE framing

It does not own:

- agent loop
- permission decisions
- verification reducer
- session truth source
- tool execution semantics

## 8. Implementation Slices

1. Stabilize protocol DTOs around current `sigil-http` boundary.
2. Command envelope and approval stale protection.
3. TUI runner command bridge for one or two flows.
4. Localhost app-server listener with auth.
5. OpenAPI/SSE productization.

## 9. Acceptance Criteria

- Client retry cannot execute command twice.
- Stale approval cannot approve changed tool call.
- Durable SSE reconnect can replay cursor after `Last-Event-ID`.
- Transient events are not advertised as replayable.
- App-server uses same kernel/runtime paths as TUI.
- Remote access is disabled by default or separately secured.

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-http
cargo test -p sigil-runtime protocol
cargo test -p sigil-tui runner
```

## 11. Open Questions

- Whether `sigil-protocol` should be a new crate before app-server is real.
- Which TUI flow should migrate first to command/event bridge.
- Whether OpenAPI should cover all commands in MVP or only session/run operations.
