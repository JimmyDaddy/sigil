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

本 RFC 定义 Sigil 的 durable event stream、事件分类、持久 envelope、V2-only 格式门、checksum、tail recovery、reducer 输入边界和 projection 消费规则。

核心决策：

1. JSONL 继续作为 session 的事实来源。
2. 新写入使用 `StoredEvent` envelope。
3. 顶层裸 `SessionLogEntry` JSONL 行不再支持；读取时明确拒绝，且不得把它当成可恢复尾损坏。
4. 同一个 session stream 内使用严格递增的 `stream_sequence`。
5. Durable event、live runtime event 和 protocol event 分层；流式 token、reasoning delta 和瞬时进度不进入 durable JSONL。
6. Kernel 内部 reducer 消费强类型 `DomainEvent`，不直接消费任意字符串和 JSON。
7. `DomainEvent` 是 `StoredEvent` 成功解码后的强类型形态；durable taxonomy 与 reducer input 必须保持同一事件集合。

## 2. Goals

- 为 verification、checkpoint、projection、task graph 和 crash resume 提供统一事件输入。
- 只接受 `StoredEvent` JSONL；不支持的旧格式必须在任何写入前明确拒绝，并保持文件不变。
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
    CheckpointRestoreConflict(CheckpointRestoreConflict),
    ConversationForked(ConversationForked),
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
    ExtensionProcessLifecycleRecorded(ExtensionProcessLifecycleRecorded),
    SandboxDecisionRecorded(SandboxDecisionRecorded),
    LogTailRecovered(LogTailRecovered),
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
- `DomainEvent` is the reducer-facing decoded event-kind enum with versioned payload. RFC-0001 prevents reducers from dispatching on raw `event_type` strings; owner RFCs progressively replace generic payloads with strict event-specific payload structs. This pre-release build does not read old payload versions through upcasters.

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
- `occurred_at` is optional because an event source may not provide a trustworthy timestamp.
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
- Checksum is verified against the persisted wire form before typed payload deserialization.

Limits:

- Current implementation uses a maximum event byte size of 1 MiB (`MAX_EVENT_BYTES`).
- Current implementation uses a maximum payload nesting depth of 64 (`MAX_PAYLOAD_DEPTH`).
- Reject oversized or over-nested events before append.

## 7. V2-only Session Format Gate

Pre-release cutover decision: a session JSONL stream may contain only `StoredEvent` envelopes. A top-level raw `SessionLogEntry` is an unsupported legacy format, not a durable event and not recoverable corruption.

Rules:

- The reader returns `SessionStreamCompatibilityError` with the physical line and path for a raw legacy entry.
- Writer open, append, projection rebuild, history tail-read and doctor use the same V2-only classification.
- Tail recovery must propagate this compatibility error before creating recovery intent, quarantine, truncation or `LogTailRecovered`.
- The unsupported file is never rewritten, upgraded or silently treated as an empty session by this build. Users must archive the old log and start a new session; this pre-release build intentionally provides no migration path.
- This pre-release build only accepts known payload version `1`; unsupported envelope or payload versions fail closed. The format gate does not install a raw-line or payload migration bridge.

## 8. Unsupported Versions and Unknown Events

This pre-release build has no payload upcaster or migration bridge: readers accept only the
current known schema and fail closed on every unsupported known `event_version`. A pre-release
schema change replaces the current contract rather than retaining a reader-side compatibility
path; any released compatibility policy must be proposed separately.

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

The file-backed implementation uses one process-wide owner per canonical session path. Store and
mutation-recorder clones share that owner; a sidecar writer lease rejects a second process owner,
while the JSONL data-file lock remains limited to reload/recovery/append windows so readers are not
permanently excluded. The owner caches session id, next sequence, durable offset, last-record
identity/checksum, bounded tail fingerprint and a full durable-prefix hash state. Normal append
therefore validates only file identity/metadata and the bounded tail before extending the cached
hash; an external change triggers one full reload and must preserve the exact previous durable
prefix or the writer fails closed.

Recovery-critical pre-effect records use the sealed blocking `DurableAuditWriter`. It returns a
non-cloneable, non-serializable receipt only after complete write, flush, `sync_all` and offset
verification. The receipt binds writer generation, session, event kind/id/checksum, sequence,
optional correlation/causation ids, record/authorization identities, batch identity and durable byte
ranges. Recovery coordinators may preallocate an event id before append; receipt expectations bind
that exact id. A preallocated correlation is either its own root event id or an earlier same-session
event id; causation is an earlier same-correlation event id. The writer keeps this link index under
the writer lease, rebuilds it only on reload, and updates it after each successful append. If append
or sync acknowledgement is ambiguous, an explicit writer-mode
reconciliation re-reads and syncs the stream under the single-writer lease and returns one of exact
present, confirmed absent, conflicting identity/content, or indeterminate. Only exact present is a
durable success; conflict and indeterminate never authorize a retry or protected effect.
Record and authorization identities must occur in the checksum-covered payload, and receipt
construction validates the event's registered session-entry or typed-domain payload schema.
Direct-JSON event types without a typed schema cannot use the strict pre-effect writer. Receipt
consumption re-reads the recorded byte ranges before returning a one-shot permit. In-memory
sessions return `MissingDurableStore`; ordinary `append_durable_event` results are not authorization
proofs. Because this interface performs blocking file synchronization, async callers must use the
runtime blocking-I/O bridge.

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
| `WriteCommitted` / `WorkspaceMutationDetected` / `CheckpointRestored` / `CheckpointRestoreConflict` / `ConversationForked` / `MutationArtifactLifecycleRecorded` | `RecoveryCritical` |
| `CommandFinished` / `CheckFinished` / `CheckSpecRecorded` | `RecoveryCritical` |
| `DiagnosticRecorded` / `TodoChanged` | `RecoveryCritical` |
| `VerificationRecorded` / `VerificationPolicyChanged` / `VerificationCheckRun` / `EnvironmentFingerprintRecorded` / `ReadinessEvaluated` | `RecoveryCritical` |
| `TaskStatusChanged` / `RunStatusChanged` / `RunFinalized` | `RecoveryCritical` |
| `ChildVerificationReceiptLinked` / `ChildChangesetMerged` / `AgentMergeApplied` | `RecoveryCritical` |
| `WorkspaceTrustDecision` / `EgressDecisionRecorded` / `ExtensionTrustDecision` / `ExtensionProcessLifecycleRecorded` / `SandboxDecisionRecorded` | `RecoveryCritical` |
| `ContextSourceCaptured` | `NormalEvent`, unless it grants trust or secret/egress access |
| `LogTailRecovered` | `TailRecovery` |

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

Pre-release migration path:

1. Remove the raw `SessionLogEntry` reader and mixed-stream reducer branch.
2. Require `StoredEvent` for every non-empty session JSONL record.
3. Reject unsupported legacy records before tail recovery or append.
4. Rebuild projections only from V2 streams.

This build does not auto-migrate or rewrite old JSONL files in place.

## 15. Test Matrix

Required deterministic tests:

- reject legacy-only session without modifying it
- load v2-only session
- reject legacy + v2 and v2 + legacy streams with a compatibility error
- reject one legacy JSONL line through the tail/history decoder
- reject legacy while a pending tail-recovery intent exists without truncating or clearing the intent
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
- 已完成 V2-only cutover：顶层裸 `SessionLogEntry` JSONL 行返回结构化 compatibility error，不再 upcast 为 legacy event。
- 已实现 v2 append、session-scoped `stream_sequence`、middle corruption / sequence gap fail-closed；不支持旧格式时不会创建 recovery intent、quarantine 或截断日志。
- 已实现 tail recovery quarantine 和 `LogTailRecovered` 审计路径，避免静默截断。
- 已区分 durable、live runtime 和 protocol event 边界；流式 reasoning/text delta 不作为 durable 事实写入。
- 已新增 `DomainEvent` / durable event type 解码和 reducer disposition 覆盖测试，kernel reducer 不直接消费任意字符串。
- 已落地基础 projection cursor/idempotence 规则，并接入 session entry projection 的 replay / cursor 应用测试。
- 已新增 verification projection 的 durable replay API：`Session` 可从 V2 durable stream 直接重建 `VerificationStateProjection`，并应用 projection cursor/idempotence 规则；原有 entries-based API 保持兼容。
- 已新增 `VerificationCheckRun` durable/control event，用于审计 check runner queued/running/terminal lifecycle；proof 仍只由 `VerificationRecorded` receipt 决定。
- 已新增 task、agent thread、changeset、usage/cost、terminal、plan approval、skill、plugin、profile trust/policy、agent result continuation 和 conversation queue projection 的 V2 durable replay API，进一步减少 resume 后对运行时 entries-only projection 的依赖。
- 已新增 `MutationArtifactLifecycleRecorded` durable event，用于审计 RFC-0002 artifact 删除、过期和内容不可用状态。
- 已新增 `CheckpointRestoreConflict` 与 `ConversationForked` recovery-critical durable event：
  前者记录 exact restore 在首个写入前的 fail-closed 原因，后者记录新 session 的 parent/turn
  provenance；两者都不保存原始文件内容或 prompt。
- 已将 `/task` readiness 的 durable replay bridge 接到 store-backed session snapshot，避免非阻塞 readiness 只从 in-memory session entries 判断 RFC-0002 mutation evidence。
- 已将 foreground chat 和 `/task` synthetic readiness evidence 的 `source_stream_sequence` 切到 V2 durable stream 的 next sequence；durable-only events 不进入 `Session::entries()` 时，也不会低估后续 readiness / run-check ordering。
- 已将 `/task` durable mutation replay failure 处理为 fail-closed unknown-dirty evidence，避免 corrupt/unreadable stream 被当作空 mutation evidence。
- 已新增 `RunStatusChanged` / `RunFinalized` 基础 durable event，并在 agent terminal/max-turn 路径中记录。
- 已加强 session append / tail recovery 的 sync 策略：recovery-critical event 写入会 sync session file；新建 session log、tail recovery quarantine/intent 创建和清理会同步父目录，降低目录项丢失造成的恢复不一致窗口。
- 已将 session stream 健康诊断接入共享 doctor 面：CLI `sigil doctor` 与 TUI `/doctor` 会扫描当前 session log dir 中最近的 JSONL stream，使用 RFC-0001 reader 校验 checksum/sequence/session id，并展示 record、last sequence 与 tail recovery 摘要；不支持旧格式会给出“不修改文件”的专用 remediation。
- 已实现 E21.3 canonical per-session linear writer、跨进程 sidecar lease、热路径 O(1) tail 校验、外部修改精确 prefix reload/fail-closed，以及 store-backed strict durable append/sync receipt；无 durable store、sync 失败或 receipt mismatch 均不能授权后续 effect。
- strict writer 已支持 caller 预分配 event id，并把 correlation/causation 纳入持久 envelope、receipt 与消费 expectation；append acknowledgement 不确定时可在单写者 lease 下重读并同步，区分 exact present、confirmed absent、conflict 与 indeterminate，为 compaction 等多事件因果链提供恢复前置条件。
- 已注册 `CompactionStarted`、`CompactionAppliedV2`、`CompactionFailed`、`CompactionSkipped`、`TaskMemoryRecordedV1` 和 `TaskMemoryInvalidated` 的 direct-JSON taxonomy；lifecycle/memory events 是 Critical + RecoveryCritical，skip 是 NonCritical + NormalEvent。
- 已实现 K25.3 initiated compaction lifecycle projection：strict Start/Applied/Failed payload、唯一 terminal、Start-root correlation/causation、fail-closed fallback lineage、只读投影与显式幂等 `RecoveryInterrupted` recovery。该基础尚未改变 chat context 或用户可见 compaction 行为。
- 已新增 typed durable decode seam：`decode_typed_stored_event` 会把 mutation、verification、task、agent thread、terminal 和 changeset family 收敛为强类型 `TypedDomainEvent`，并继续对 unknown critical event fail closed。
- 已新增 projection-facing typed record API：`SessionStreamRecord::typed_domain_event_record` 输出 typed event 和 `ProjectionCursor`，后续 typed reducer / projection store 可在不重新解析 JSON 的情况下消费 cursor-bound event。
- 已新增文件型 persistent projection store：`FileProjectionStore` 将 projection snapshot 与 `ProjectionCursor` 写入同一 envelope，并通过 temporary file + atomic replace 持久化，首个真实 projection 为 `VerificationStateProjectionSnapshot`；duplicate replay、sequence gap、cursor ahead 和 JSONL rebuild 已有测试覆盖。
- 已新增 HTTP protocol event boundary：`sigil-http` 将 public run event 包装为 durable / transient protocol envelope，durable SSE frame 带 `Last-Event-ID` cursor，transient delta 不带 replay id；`HttpProtocolEventBuffer` 只重放 cursor 之后的 durable event，并对 malformed、scope mismatch 和 cursor-ahead fail closed。

Productization remains：

- 如果未来需要跨客户端高频查询，再将文件型 projection store 替换或扩展为 SQLite/materialized view；JSONL 仍是事实来源，projection 仍必须可重建。
- 为尚未收敛的 durable event owner 继续补齐强类型、严格字段的 payload struct，逐步减少泛型 JSON payload 的 reducer 接触面；在预发布阶段，任何不支持的 payload version 一律 fail closed，不保留 reader-side upcaster。
- 完整 HTTP listener / app-server 仍属于未来 server productization；当前已完成 protocol DTO / SSE cursor / reconnect replay helper，不承诺 transient event replay。
- 如果真实 session 中出现合理的大型 durable payload，再单独评估 1 MiB / 64 层 event limit 是否需要配置化；当前限制已作为 core guard 生效。

## 17. Open Questions

None for core semantics. Productization questions:

- Whether `record_checksum` should later become a hash chain for stronger tamper-evidence.
- Whether additional platform-specific file locking or sync backends are needed beyond the current local JSONL implementation.
- Whether old binaries need graceful failure messaging for sessions containing v2 lines.
