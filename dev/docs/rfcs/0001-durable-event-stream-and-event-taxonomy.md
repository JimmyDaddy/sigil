# RFC-0001 Durable Event Stream and Event Taxonomy

状态：Draft

创建日期：2026-06-25

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Source baseline: `main` around `d44b2f82a4c6fff330c3b30e878d176dbfe2dc5d`

## 1. Summary

本 RFC 定义 Sigil 的 durable event stream、事件分类、持久 envelope、旧日志兼容读取、checksum、tail recovery、reducer 输入边界和 projection 消费规则。

核心决策：

1. JSONL 继续作为 session 的事实来源。
2. 新写入使用 `StoredEvent` envelope。
3. 旧 `SessionLogEntry` 不重写；读取时稳定 upcast 为 legacy event。
4. 同一个 session stream 内使用严格递增的 `stream_sequence`。
5. Durable event、live runtime event 和 protocol event 分层；流式 token、reasoning delta 和瞬时进度不进入 durable JSONL。
6. Kernel 内部 reducer 消费强类型 `DomainEvent`，不直接消费任意字符串和 JSON。

## 2. Goals

- 为 verification、checkpoint、projection、task graph 和 crash resume 提供统一事件输入。
- 保证旧 session log 可读取，且 legacy event id 稳定。
- 明确 session stream 的单写者、flush/sync、tail recovery 和 checksum 语义。
- 支持 projection store 删除后从 JSONL 重建。
- 避免把 transient UI / provider streaming noise 永久写入事实日志。

## 3. Non-goals

- 不实现完整 distributed event bus。
- 不保证 `record_checksum` 防恶意篡改；它只检测意外损坏。
- 不在 RFC-0001 中实现 checkpoint artifact store。
- 不定义 verification policy 细节；见 RFC-0003。
- 不定义 crash-consistent file mutation 全流程；该部分由 RFC-0002 细化。
- 不引入 SQLite 作为事实来源；SQLite 或其他索引只能是 projection。

## 4. Event Taxonomy

Sigil 必须区分三类事件。

```rust
enum DurableDomainEvent {
    ToolExecutionStarted(ToolExecutionStarted),
    ToolExecutionFinished(ToolExecutionFinished),
    ApprovalResolved(ApprovalResolved),
    WriteCommitted(WriteCommitted),
    VerificationRecorded(VerificationRecorded),
    TaskStatusChanged(TaskStatusChanged),
    LogTailRecovered(LogTailRecovered),
    Legacy(LegacyEvent),
}

enum LiveRuntimeEvent {
    ReasoningDelta,
    TextDelta,
    ToolProgress,
    SpinnerUpdated,
}

enum ProtocolEvent {
    Durable(DurableEventView),
    Transient(LiveEventView),
}
```

Rules:

- Durable events are replayable facts required for recovery, audit or projection.
- Live runtime events are process-local and not required for recovery.
- Protocol events are client-facing views derived from durable or live events.
- Protocol transient events are not guaranteed to replay after reconnect.

## 5. Stored Event Envelope

Persistent v2 event lines use this envelope:

```rust
struct StoredEvent {
    schema_version: u16,
    event_type: String,
    event_version: u16,
    event_id: EventId,
    session_id: SessionId,
    stream_sequence: u64,
    occurred_at: Option<DateTime<Utc>>,
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
    parent_session_id: Option<SessionId>,
    record_checksum: String,
    payload: serde_json::Value,
}
```

Field rules:

- `schema_version` versions the envelope.
- `event_version` versions the payload for `event_type`.
- `(session_id, stream_sequence)` is unique.
- `stream_sequence` is scoped to one session stream only.
- Cross-session ordering must use `event_id`, `correlation_id`, `causation_id`, `occurred_at` and `parent_session_id`.
- `occurred_at` is optional because legacy records may not have a trustworthy timestamp.
- `payload` is the persisted wire form; reducer input must be decoded into a strong `DomainEvent`.

## 6. Checksum

`record_checksum` covers the immutable event body except the checksum field itself.

Canonical input:

```text
canonical_json(
  schema_version,
  event_type,
  event_version,
  event_id,
  session_id,
  stream_sequence,
  occurred_at,
  correlation_id,
  causation_id,
  parent_session_id,
  payload
)
```

Rules:

- Canonical serialization must be deterministic.
- JSON key order must not affect the computed checksum.
- Checksum mismatch is not the same error as JSON parse failure.
- A checksum mismatch means the event cannot be trusted for projection.
- `record_checksum` is not tamper-proof and must not be described as a security signature.

Limits:

- Define a maximum event byte size.
- Define a maximum payload nesting depth.
- Reject oversized or over-nested events before append.

## 7. Legacy Log Upcast

Existing logs contain mixed `SessionLogEntry` records without the v2 envelope. They must remain readable.

Decision: use mixed-format append in the same session JSONL file.

Rationale:

- Keeps one session stream as the truth source.
- Avoids coordinating a v2 sidecar with the old log.
- Avoids rewriting old logs during read.

Rules:

- Existing lines are read as legacy records.
- New writes append v2 `StoredEvent` lines.
- The reader detects line shape and upcasts legacy lines.
- The writer never rewrites old lines in place.
- Once a v2 event is appended, older binaries may not understand the session. This is accepted as a forward schema migration.

Legacy mapping:

```text
legacy stream_sequence = original physical line number
legacy event_id = UUIDv5(session_id, line_number + raw_line_hash)
legacy occurred_at = None, unless the legacy record already has a trustworthy timestamp
```

The same legacy session must produce the same legacy event ids on every rebuild.

## 8. Upcasters and Unknown Events

Event payload evolution uses explicit upcasters:

```text
v1 -> v2 -> v3
```

Unknown event rules:

- Unknown non-critical events are preserved and skipped by projections that do not understand them.
- Unknown events that affect permission, approval, write mutation, verification or recovery fail closed.
- Unknown events are never silently discarded.

Critical event classes:

- approval decisions
- tool execution lifecycle
- file mutation lifecycle
- verification evidence
- checkpoint restore
- sandbox decisions
- trust and egress decisions

## 9. Append and Sync Policy

The session stream has a single writer.

Rules:

- Each append assigns the next `stream_sequence`.
- The stream writer must hold the session write lock while assigning sequence and writing the line.
- Cross-process writers require an OS file lock or must fail.
- Append policy must define when to flush and when to `sync_all`.
- Recovery-critical events use the strongest sync policy.

Initial sync classes:

```text
NormalEvent       append + flush
RecoveryCritical  append + flush + sync file
TailRecovery      backup + sync backup + truncate + sync file + append recovery + sync file
```

## 10. Tail Recovery

Tail corruption policy:

- Last incomplete or invalid line may be recovered.
- Middle corruption fails session load and returns diagnostics.
- Recovery cannot silently discard bytes.

Recovery flow:

```text
acquire exclusive lock
create corrupt copy in RecoveryQuarantineStore
sync corrupt copy
truncate original to last complete event
sync original file
append LogTailRecovered
sync original file
```

`LogTailRecovered` records:

```rust
struct LogTailRecovered {
    original_size: u64,
    recovered_size: u64,
    discarded_bytes: u64,
    quarantine_path: PathBuf,
    original_hash: String,
}
```

`RecoveryQuarantineStore` is minimal and only stores damaged log copies plus metadata. General artifact storage is out of scope for RFC-0001.

## 11. Reducer Contract

Reducers consume strong domain events:

```rust
enum DomainEvent {
    ToolExecutionStarted(ToolExecutionStarted),
    ToolExecutionFinished(ToolExecutionFinished),
    ApprovalResolved(ApprovalResolved),
    VerificationRecorded(VerificationRecorded),
    LogTailRecovered(LogTailRecovered),
    Legacy(LegacyEvent),
}
```

Rules:

- Persistence can use `event_type + serde_json::Value`.
- Kernel reducers must decode into `DomainEvent` first.
- Reducers must not branch on arbitrary strings and raw JSON.
- Reducers must be deterministic.
- Reducers must be side-effect free.

## 12. Projection Consumption

Each projection stores:

```rust
struct ProjectionCursor {
    session_id: SessionId,
    projection_schema_version: u16,
    last_applied_stream_sequence: u64,
}
```

Apply rules:

```text
sequence <= last_applied      ignore
sequence == last_applied + 1  apply
sequence > last_applied + 1   report gap and stop
```

Event apply and cursor update must happen in the same database transaction.

## 13. Protocol Boundary

Durable protocol events can be replayed from JSONL or projection.

Transient protocol events come from live runtime state and are not replayable. Example transient events:

- reasoning delta
- text delta
- spinner update
- fine-grained tool progress

SSE or similar transports should support `Last-Event-ID` for durable events. They must not promise replay for transient events.

## 14. Compatibility and Migration

Migration path:

1. Add read support for legacy and v2 event lines.
2. Add deterministic legacy event id generation.
3. Add v2 append support.
4. Start appending v2 events without rewriting old lines.
5. Add projection rebuild from mixed-format session streams.

No phase should require rewriting old JSONL files in place.

## 15. Test Matrix

Required deterministic tests:

- load legacy-only session
- load v2-only session
- load mixed legacy + v2 session
- legacy event ids stable across two rebuilds
- `stream_sequence` strictly increases within one session
- cross-session events do not require global sequence
- checksum mismatch differs from JSON parse failure
- unknown non-critical event is preserved and skipped
- unknown critical event fails closed
- tail half-line recovery appends `LogTailRecovered`
- middle corruption fails load
- projection ignores duplicate sequence
- projection reports sequence gap
- transient event is not written to JSONL
- recovery-critical append uses stronger sync path

## 16. Open Questions

- Exact canonical JSON implementation.
- Exact byte and nesting limits for events.
- Whether `record_checksum` should later become a hash chain for stronger tamper-evidence.
- Exact file locking backend per platform.
- Whether old binaries need graceful failure messaging for sessions containing v2 lines.
