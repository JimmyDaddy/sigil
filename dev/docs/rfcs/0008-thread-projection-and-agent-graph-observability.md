# RFC-0008 Thread Projection and Agent Graph Observability

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)

## 1. Summary

本 RFC 定义 Sigil 的 projection / observability layer。JSONL 继续作为 truth source，projection store 只提供可重建、可查询的产品视图：thread index、task state、agent graph、verification、cost/token、checkpoint、context source 和 dispatch trace。

## 2. Goals

- 为 TUI、future HTTP/IDE/daemon 提供稳定查询视图。
- 避免产品层直接扫描 kernel 内部对象或重建局部索引。
- 统一 agent graph、task graph、tool dispatch、sandbox/network/egress decision 和 token/cost trace。
- 保持 projection 可删除、可重建、可版本化。

## 3. Non-goals

- 不把 SQLite 或文件 projection store 变成事实来源。
- 不让 active approval/tool execution 依赖可能滞后的 projection。
- 不在本 RFC 中实现完整 app-server。

## 4. Projection Contract

Every projection stores:

```rust
struct ProjectionEnvelope<T> {
    projection_schema_version: u16,
    session_id: SessionId,
    cursor: ProjectionCursor,
    snapshot: T,
}
```

Apply rules:

- `sequence <= last_applied` with same event id/checksum -> ignore.
- same sequence with different event id/checksum -> fail closed.
- `sequence == last + 1` -> apply.
- `sequence > last + 1` -> report gap and stop.
- event apply and cursor update must be transactional for persistent stores.

## 5. Views

Initial projection families:

- Thread index
- Task projection
- Agent graph projection
- Verification projection
- Cost/token projection
- Checkpoint projection
- Context source projection
- Tool/agent dispatch trace

Dispatch trace should connect:

- turn id
- model request id
- tool call id
- sandbox decision
- network approval
- egress receipt
- observation size/truncation
- token usage
- final run status and verification verdict

## 6. Live State Boundary

- Active turn/tool/approval state uses live runtime state and event bus.
- Historical session/task/cost/agent graph uses projection.
- Resume reconstructs live state from durable stream plus projection/reducer.

Projection lag must be visible when it matters. UI must not treat lagging projection as stronger than durable events.

## 7. Implementation Slices

1. Projection trait and file-backed store hardening.
2. Thread index and session list projection.
3. Agent graph projection.
4. Dispatch trace projection.
5. Optional SQLite/materialized view when product query volume requires it.

## 8. Acceptance Criteria

- Deleting projection store and rebuilding from JSONL produces equivalent view.
- Duplicate replay does not duplicate cost, agents or checkpoints.
- Sequence gaps fail closed.
- Projection schema mismatch has clear diagnostic.
- Active approval and tool execution do not rely solely on projection.
- Projection redacts or references large/secret tool output.

## 9. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel projection
cargo test -p sigil-runtime projection
cargo test -p sigil-tui session
```

## 10. Open Questions

- Whether file-backed projection remains enough for TUI-only workflows.
- Which projection family should be the first SQLite candidate.
- Whether dispatch trace should be shown in `/doctor`, session detail, or a dedicated inspect panel.
