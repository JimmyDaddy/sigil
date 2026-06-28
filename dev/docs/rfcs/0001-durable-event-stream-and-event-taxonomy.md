# RFC-0001 Durable Event Stream and Event Taxonomy

状态：RFC core semantics implemented / productization remains

创建日期：2026-06-25

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Architecture snapshot: [Sigil Rust Agent 核心技术方案](../sigil-rust-agent-core-technical-solution.md)
- Source baseline: `main` around `d44b2f82a4c6fff330c3b30e878d176dbfe2dc5d`
- Sibling RFC: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Sibling RFC: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 Sigil 的 durable event stream、事件分类、持久 envelope、旧日志兼容读取、checksum、tail recovery、reducer 输入边界和 projection 消费规则。

核心决策：

1. JSONL 继续作为 session 的事实来源。
2. 新写入使用 `StoredEvent` envelope。
3. 旧 `SessionLogEntry` 不重写；读取时稳定 upcast 为 legacy event。
4. 同一个 session stream 内使用严格递增的 `stream_sequence`。
5. Durable event、live runtime event 和 protocol event 分层；流式 token、reasoning delta 和瞬时进度不进入 durable JSONL。
6. Kernel 内部 reducer 消费强类型 `DomainEvent`，不直接消费任意字符串和 JSON。
7. `DomainEvent` 是 `StoredEvent` 成功解码后的强类型形态；durable taxonomy 与 reducer input 必须保持同一事件集合。

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
    UserMessageRecorded(UserMessageRecorded),
    AssistantMessageRecorded(AssistantMessageRecorded),
    ToolResultRecorded(ToolResultRecorded),
    SessionEntryRecorded(SessionEntryRecorded),
    RunStatusChanged(RunStatusChanged),
    RunFinalized(RunFinalized),
    ToolExecutionStarted(ToolExecutionStarted),
    ToolExecutionFinished(ToolExecutionFinished),
    ApprovalResolved(ApprovalResolved),
    MutationPrepared(MutationPrepared),
    MutationCommitted(MutationCommitted),
    MutationReconciled(MutationReconciled),
    MutationBatchStarted(MutationBatchStarted),
    MutationBatchFinished(MutationBatchFinished),
    WriteCommitted(WriteCommitted),
    WorkspaceMutationDetected(WorkspaceMutationDetected),
    CheckpointRestored(CheckpointRestored),
    MutationArtifactLifecycleRecorded(MutationArtifactLifecycleRecorded),
    CommandFinished(CommandFinished),
    CheckFinished(CheckFinished),
    CheckSpecRecorded(CheckSpecRecorded),
    DiagnosticRecorded(DiagnosticRecorded),
    TodoChanged(TodoChanged),
    VerificationRecorded(VerificationRecorded),
    VerificationPolicyChanged(VerificationPolicyChanged),
    VerificationCheckRun(VerificationCheckRun),
    EnvironmentFingerprintRecorded(EnvironmentFingerprintRecorded),
    ReadinessEvaluated(ReadinessEvaluated),
    TaskStatusChanged(TaskStatusChanged),
    ChildVerificationReceiptLinked(ChildVerificationReceiptLinked),
    ChildChangesetMerged(ChildChangesetMerged),
    AgentMergeApplied(AgentMergeApplied),
    WorkspaceTrustDecision(WorkspaceTrustDecision),
    ContextSourceCaptured(ContextSourceCaptured),
    EgressDecisionRecorded(EgressDecisionRecorded),
    ExtensionTrustDecision(ExtensionTrustDecision),
    SandboxDecisionRecorded(SandboxDecisionRecorded),
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
- `DomainEvent` is the reducer-facing decoded event-kind enum with versioned payload. RFC-0001 prevents reducers from dispatching on raw `event_type` strings; later owner RFCs replace generic payloads with event-specific payload structs and upcasters.

Initial mapping from current surfaces:

| Current surface | Durable event | Live runtime event | Protocol event |
| --- | --- | --- | --- |
| Completed user message | `UserMessageRecorded` | none | durable view |
| Completed assistant message | `AssistantMessageRecorded` | none | durable view |
| Reasoning or text delta | none | `ReasoningDelta` / `TextDelta` | transient view |
| Tool call started | `ToolExecutionStarted` | optional `ToolProgress` | durable view plus optional transient progress |
| Tool call finished or failed | `ToolExecutionFinished` and optionally `CommandFinished` / `CheckFinished` | none | durable view |
| Provider-visible tool result message | `ToolResultRecorded` | none | durable view |
| Approval decision | `ApprovalResolved` | none | durable view |
| Run cancelled, interrupted, max-turn stopped or finalized | `RunStatusChanged` / `RunFinalized` | none | durable view |
| Readiness calculation | `ReadinessEvaluated` | none | durable view |
| Trust, egress, sandbox or context source decision | matching trust / egress / sandbox / context event | none | durable view |
| Existing control entry without a precise RFC-0001 domain mapping | `SessionEntryRecorded` | none | durable compatibility view |

## 5. Stored Event Envelope

Persistent v2 event lines use this envelope:

```rust
struct StoredEvent {
    schema_version: u16,
    event_type: String,
    event_version: u16,
    event_class: EventClass,
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

enum EventClass {
    Critical,
    NonCritical,
}
```

Field rules:

- `schema_version` versions the envelope.
- `event_version` versions the payload for `event_type`.
- `event_class` allows older readers to distinguish unknown non-critical events from unknown events that must fail closed.
- `(session_id, stream_sequence)` is unique.
- `stream_sequence` is scoped to one session stream only.
- Cross-session ordering must use `event_id`, `correlation_id`, `causation_id`, `occurred_at` and `parent_session_id`.
- `occurred_at` is optional because legacy records may not have a trustworthy timestamp.
- `payload` is the persisted wire form; reducer input must be decoded into the `DomainEvent` event-kind enum before projection logic runs.

## 6. Checksum

`record_checksum` covers the immutable event body except the checksum field itself.

Canonical input:

```text
record_checksum = "sha256:jcs-v1:" + hex(SHA256(
  canonical_json(
  schema_version,
  event_type,
  event_version,
  event_class,
  event_id,
  session_id,
  stream_sequence,
  occurred_at,
  correlation_id,
  causation_id,
  parent_session_id,
  payload
)))
```

Rules:

- Canonical serialization uses JSON Canonicalization Scheme style ordering for object keys, UTF-8 bytes, deterministic string escaping and normalized number representation.
- JSON key order must not affect the computed checksum.
- Checksum mismatch is not the same error as JSON parse failure.
- A checksum mismatch means the event cannot be trusted for projection.
- `record_checksum` is not tamper-proof and must not be described as a security signature.
- Checksum is verified against the persisted wire form before payload upcasting.

Limits:

- Current implementation uses a maximum event byte size of 1 MiB (`MAX_EVENT_BYTES`).
- Current implementation uses a maximum payload nesting depth of 64 (`MAX_PAYLOAD_DEPTH`).
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
legacy session_id = UUIDv5(SigilLegacySessionNamespace, legacy_prefix_hash)
legacy_prefix_hash = SHA256(ordered raw legacy record lines before the first v2 line)
legacy stream_sequence = effective JSONL record ordinal
legacy event_id = UUIDv5(legacy_session_id, record_ordinal + raw_line_hash)
legacy occurred_at = None, unless the legacy record already has a trustworthy timestamp
```

The same legacy session must produce the same legacy event ids on every rebuild.

Rules:

- Empty lines do not consume a legacy `stream_sequence`.
- Invalid middle lines are not skipped; they fail load.
- New v2 append after legacy records uses `max(valid_event.stream_sequence) + 1`.
- If a legacy-only stream is later appended with v2 events, existing legacy event ids remain stable because the `legacy_prefix_hash` only covers the legacy prefix before the first v2 line.
- If no stable legacy prefix can be derived, the reader must fail closed rather than generate random event ids.

## 8. Upcasters and Unknown Events

Event payload evolution uses explicit upcasters once an event owner defines payload-specific structs:

```text
v1 -> v2 -> v3
```

Until an upcaster exists for a known `event_type`, readers fail closed on unsupported `event_version`.

Unknown event rules:

- Unknown events with `event_class = NonCritical` are preserved and skipped by projections that do not understand them.
- Unknown events with `event_class = Critical` fail closed.
- Unknown events without a parseable or trusted `event_class` fail closed.
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
- A read-only loader may read without appending reconciliation events.
- A writer-mode loader must hold the OS file lock across tail validation, max sequence calculation and all load-time reconciliation appends.
- Load-time reconciliation events must have deterministic ids or idempotency keys so repeated writer-mode opens do not duplicate recovery records.

Initial sync classes:

```text
NormalEvent       append + flush
RecoveryCritical  append + flush + sync file
TailRecovery      backup + sync backup + truncate + sync file + append recovery + sync file
```

Initial event-to-sync mapping:

| Event type | Sync class |
| --- | --- |
| `UserMessageRecorded` / `AssistantMessageRecorded` | `NormalEvent` |
| `ToolResultRecorded` / `SessionEntryRecorded` | `RecoveryCritical` |
| `ToolExecutionStarted` / `ToolExecutionFinished` | `RecoveryCritical` |
| `ApprovalResolved` | `RecoveryCritical` |
| `MutationPrepared` / `MutationCommitted` / `MutationReconciled` | `RecoveryCritical` |
| `MutationBatchStarted` / `MutationBatchFinished` | `RecoveryCritical` |
| `WriteCommitted` / `WorkspaceMutationDetected` / `CheckpointRestored` / `MutationArtifactLifecycleRecorded` | `RecoveryCritical` |
| `CommandFinished` / `CheckFinished` / `CheckSpecRecorded` | `RecoveryCritical` |
| `DiagnosticRecorded` / `TodoChanged` | `RecoveryCritical` |
| `VerificationRecorded` / `VerificationPolicyChanged` / `VerificationCheckRun` / `EnvironmentFingerprintRecorded` / `ReadinessEvaluated` | `RecoveryCritical` |
| `TaskStatusChanged` / `RunStatusChanged` / `RunFinalized` | `RecoveryCritical` |
| `ChildVerificationReceiptLinked` / `ChildChangesetMerged` / `AgentMergeApplied` | `RecoveryCritical` |
| `WorkspaceTrustDecision` / `EgressDecisionRecorded` / `ExtensionTrustDecision` / `SandboxDecisionRecorded` | `RecoveryCritical` |
| `ContextSourceCaptured` | `NormalEvent`, unless it grants trust or secret/egress access |
| `LogTailRecovered` | `TailRecovery` |

`Legacy` is an upcast view over existing legacy log records. A v2 writer must not append new `Legacy` events, so it has no new append sync class.

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
write and sync TailRecoveryIntent manifest
truncate original to last complete event
sync original file
append LogTailRecovered
sync original file
mark TailRecoveryIntent completed
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

If the process crashes after truncate but before `LogTailRecovered`, the next writer-mode load must find the recovery intent and either append/reconstruct `LogTailRecovered` or fail closed. It must not silently accept a shortened log without a recovery event.

## 11. Reducer Contract

Reducers consume strong domain events:

```rust
type DomainEvent = DurableDomainEvent;
```

Rules:

- Persistence can use `event_type + serde_json::Value`.
- Kernel reducers must decode into `DomainEvent` first.
- Reducers must not branch on arbitrary strings and raw JSON.
- Reducers must be deterministic.
- Reducers must be side-effect free.
- Every event in `DurableDomainEvent` must be either consumed by at least one reducer or explicitly ignored by a named reducer with a documented reason.

## 12. Projection Consumption

Each projection stores:

```rust
struct ProjectionCursor {
    session_id: SessionId,
    projection_schema_version: u16,
    last_applied_stream_sequence: u64,
    last_applied_event_id: EventId,
    last_applied_record_checksum: String,
}
```

Apply rules:

```text
sequence < last_applied       ignore only if already applied by event id/checksum
sequence == last_applied      ignore only if event id and checksum match cursor
sequence == last_applied + 1  apply
sequence > last_applied + 1   report gap and stop
```

Event apply and cursor update must happen in the same database transaction.

If a projection sees the same sequence with a different `event_id` or `record_checksum`, it must fail closed and request rebuild/diagnostics.

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
- legacy stream with blank lines uses effective record ordinal without sequence gaps
- v2 append after legacy starts at `max(valid_event.sequence) + 1`
- legacy session id remains stable after appending v2 events
- `stream_sequence` strictly increases within one session
- cross-session events do not require global sequence
- checksum mismatch differs from JSON parse failure
- checksum uses `sha256:jcs-v1:<hex>` canonical form
- unknown non-critical event is preserved and skipped
- unknown critical event fails closed
- unknown event without trusted `event_class` fails closed
- tail half-line recovery appends `LogTailRecovered`
- crash after tail truncate but before `LogTailRecovered` is recovered from intent or fails closed
- middle corruption fails load
- writer-mode loader holds lock through reconciliation appends
- OS lock conflict prevents a second writer-mode loader
- projection ignores duplicate sequence only when event id and checksum also match
- projection fails on same sequence with different event id or checksum
- projection reports sequence gap
- projection cursor ahead of recovered stream fails closed
- transient event is not written to JSONL
- durable `Last-Event-ID` replay does not promise transient replay
- recovery-critical append uses stronger sync path
- event-to-sync-class mapping covers approval, tool lifecycle, mutation, command/check, diagnostics, todo, verification, sandbox and trust events

## 16. Implementation Progress

当前进度：

- 已新增 `StoredEvent` envelope，包含 `schema_version`、`event_type`、`event_version`、`event_class`、`event_id`、`session_id`、`stream_sequence`、可空 `occurred_at`、causation/correlation、`record_checksum` 和 JSON payload。
- 已实现 canonical checksum、1 MiB event size / 64 层 payload depth 限制、checksum mismatch 与 JSON parse failure 的区分。
- 已强制 known durable event 的 `event_class` 与事件语义匹配，避免 recovery-critical event 被错误追加为 non-critical。
- 已实现 legacy / v2 / mixed JSONL 读取；legacy record 使用稳定 id 和 stream ordinal upcast，不重写旧日志。
- 已实现 v2 append、session-scoped `stream_sequence`、legacy 后混合格式追加、middle corruption / sequence gap fail-closed。
- 已实现 tail recovery quarantine 和 `LogTailRecovered` 审计路径，避免静默截断。
- 已区分 durable、live runtime 和 protocol event 边界；流式 reasoning/text delta 不作为 durable 事实写入。
- 已新增 `DomainEvent` / durable event type 解码和 reducer disposition 覆盖测试，kernel reducer 不直接消费任意字符串。
- 已落地基础 projection cursor/idempotence 规则，并接入 session entry projection 的 replay / cursor 应用测试。
- 已新增 verification projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `VerificationStateProjection`，并应用 projection cursor/idempotence 规则；原有 entries-based API 保持兼容。
- 已新增 `VerificationCheckRun` durable/control event，用于审计 check runner queued/running/terminal lifecycle；proof 仍只由 `VerificationRecorded` receipt 决定。
- 已新增 task projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `TaskStateProjection`，并应用 projection cursor/idempotence 规则；原有 entries-based API 保持兼容。
- 已新增 agent thread projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `AgentThreadStateProjection`，并应用 projection cursor/idempotence 规则；原有 entries-based API 保持兼容。
- 已新增 changeset projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `ChangeSetProjection`，用于 merge/review/changeset 审计链路。
- 已新增 usage / cost projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `SessionStats`，并应用 projection cursor/idempotence 规则；`CompactionApplied` 会继续使 `last_prompt_tokens` 失效。
- 已新增 terminal task projection 的 durable replay API：`Session` 可从 mixed-format event stream 直接重建 `TerminalTaskProjection`，用于 active terminal / long-running process 状态恢复与审计。
- 已新增 plan approval、skill 和 plugin projection 的 durable replay API：`Session` 可从 mixed-format event stream 重建 `PlanApprovalProjection`、`SkillStateProjection` 与 `PluginStateProjection`，支撑 workspace trust / extension context 审计。
- 已新增 agent profile trust/policy、agent result continuation 和 conversation queue projection 的 durable replay API：`Session` 可从 mixed-format event stream 重建 profile trust/policy、child result continuation 与 queued user input 状态，进一步减少 resume 后对运行时 entries-only projection 的依赖。
- 已新增 `MutationArtifactLifecycleRecorded` durable event，用于审计 RFC-0002 artifact 删除、过期和内容不可用状态。
- 已将 `/task` readiness 的 durable replay bridge 接到 store-backed session snapshot，避免非阻塞 readiness 只从 in-memory legacy entries 判断 RFC-0002 mutation evidence。
- 已将 foreground chat 和 `/task` synthetic readiness evidence 的 `source_stream_sequence` 切到 mixed-format durable stream 的 next sequence；durable-only events 不进入 `Session::entries()` 时，也不会低估后续 readiness / run-check ordering。
- 已将 `/task` durable mutation replay failure 处理为 fail-closed unknown-dirty evidence，避免 corrupt/unreadable stream 被当作空 mutation evidence。
- 已新增 `RunStatusChanged` / `RunFinalized` 基础 durable event，并在 agent terminal/max-turn 路径中记录。
- 已加强 session append / tail recovery 的 sync 策略：recovery-critical event 写入会 sync session file；新建 session log、tail recovery quarantine/intent 创建和清理会同步父目录，降低目录项丢失造成的恢复不一致窗口。
- 已将 session stream 健康诊断接入共享 doctor 面：CLI `sigil doctor` 与 TUI `/doctor` 会扫描当前 session log dir 中最近的 JSONL stream，使用 RFC-0001 reader 校验 checksum/sequence/session id，并展示 record、legacy/stored、last sequence 与 tail recovery 摘要；损坏流会作为 error 暴露而不是静默跳过。
- 已新增 typed durable decode seam：`decode_typed_stored_event` 会把 mutation、verification、task、agent thread、terminal 和 changeset family 收敛为强类型 `TypedDomainEvent`，并继续对 unknown critical event fail closed。
- 已新增 projection-facing typed record API：`SessionStreamRecord::typed_domain_event_record` 输出 typed event 和 `ProjectionCursor`，后续 typed reducer / projection store 可在不重新解析 JSON 的情况下消费 cursor-bound event。

Productization remains：

- 将 projection cursor 规则接入未来持久 projection store 的事务边界，例如 SQLite/materialized view。
- 为尚未收敛的 durable event owner 继续补齐强类型 payload struct 与 upcaster，逐步减少泛型 JSON payload 的 reducer 接触面；当前 payload version 仍只有 v1，第一次引入 v2 payload 时必须补对应 version upcaster 测试。
- 在 protocol/server 阶段实现 durable cursor / `Last-Event-ID` replay；transient event replay 保持非承诺。
- 如果真实 session 中出现合理的大型 durable payload，再单独评估 1 MiB / 64 层 event limit 是否需要配置化；当前限制已作为 core guard 生效。

## 17. Open Questions

None for core semantics. Productization questions:

- Whether `record_checksum` should later become a hash chain for stronger tamper-evidence.
- Whether additional platform-specific file locking or sync backends are needed beyond the current local JSONL implementation.
- Whether old binaries need graceful failure messaging for sessions containing v2 lines.
