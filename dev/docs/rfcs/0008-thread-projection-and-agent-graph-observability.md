# RFC-0008 Thread Projection and Agent Graph Observability

状态：draft / E08.1-E08.4 and E08.6 implemented / E08.5 gated

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
6. Product view projection adoption for suitable historical/audit views.

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

## 10. Implementation Progress

- 已新增 kernel `ProjectionStore<T>` trait，统一 load、single-record apply 和 stream rebuild 入口。
- 已新增 `ProjectionRebuildReport` / `ProjectionRebuildOutput`，rebuild 可报告 applied / ignored record 数和最终 cursor。
- 已有 file-backed `FileProjectionStore<T>` 实现 trait，projection 与 cursor 仍保存在同一个 envelope 中，并通过 temporary file + atomic rename + parent dir sync 持久化。
- 已补 verification file projection specialization，并保持 JSONL 为 truth source；projection store 可删除后从 durable stream 重建。
- 已补测试覆盖 duplicate replay、sequence gap、cursor ahead、schema/name mismatch、corrupt projection store、trait dispatch 和 rebuild diagnostics。
- 已新增 session list projection：`SessionListProjectionSnapshot` / `SessionListProjectionEntry` 从 mixed legacy/v2 stream 重建 session metadata、首个用户标题、usage、task 和 readiness 摘要。
- 已新增 file-backed session list projection store specialization，保持 projection + cursor 原子保存。
- TUI session history 已接入 projection adapter 读取 v2 title，并保留旧 bounded line scanner 作为 fallback；active approval/tool execution 仍不依赖 projection。
- 已新增 agent graph projection file store specialization，可从 durable mixed stream 重建 `AgentThreadStateProjection`，并通过 cursor rules 保持 duplicate replay idempotent。
- 已新增 `AgentGraphSummary`，为 TUI 和 projection audit 提供 agent count、route count、token budget 和 changed paths 摘要。
- TUI info rail 已显示低噪声 agent graph summary；无 child agent thread 时不额外显示空状态行。
- 已新增 dispatch trace projection：tool trace 以 `call_id` 聚合 approval、execution、egress、observation truncation、changed files 和 error kind；agent trace 以 `thread_id` 聚合 start/status/result、profile、parent thread 和 token usage。
- 已新增 file-backed dispatch trace projection store specialization，支持 duplicate replay idempotence 和 redacted inspect summary。
- Dispatch trace projection 不保存 streaming token deltas、egress payload 或 raw tool result content；仅保存 hash、destination、计数、redaction/truncation metadata 和 bounded summary fields。
- E08.6 已新增 runtime agent graph product-view adapter over durable session logs，并迁移 TUI agent summary consumer；active turn、approval 和 transient progress 仍保持 live runtime state 边界，projection lag 不作为交互真相来源。

本阶段没有引入 SQLite，也没有让 active approval/tool execution 依赖 projection。

2026-06-29 审计补充：

- 当前是 projection infrastructure + selected adapter adoption，不是所有 TUI/product views 全面 projection-first。
- E08.5 SQLite/materialized view 继续 gated：只有桌面端、server 或跨会话查询出现真实查询压力，且 file-backed durable replay 不再足够时才升级。

## 11. Open Questions

- Whether file-backed projection remains enough for TUI-only workflows.
- Which projection family should be the first SQLite candidate.
- Whether dispatch trace should be shown in `/doctor`, session detail, or a dedicated inspect panel.
