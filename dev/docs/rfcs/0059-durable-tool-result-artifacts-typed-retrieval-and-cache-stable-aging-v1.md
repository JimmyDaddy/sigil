# RFC-0059 Durable Tool-result Artifacts, Typed Retrieval and Cache-stable Aging V1

状态：draft / implementation deferred

创建日期：2026-07-29

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)
- [RFC-0009 Extension Trust Plane](0009-extension-trust-plane.md)
- [RFC-0010 Structured Compaction and Task Memory](0010-structured-compaction-and-task-memory.md)
- [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- [RFC-0057 Cache-stable Compaction and Conversation Continuity V3](0057-cache-stable-compaction-and-conversation-continuity-v3.md)
- [RFC-0058 Event-driven Worker and Incremental Durable Session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)

## 1. Problem statement

Sigil 已经具备 append-only session、typed tool result、context epoch、recoverable shrink candidate 和
event-driven worker 方案，但通用工具输出仍存在一个结构性缺口：

```text
tool implementation
  -> ToolResult { content: String }
  -> 32 KiB head/tail model envelope
  -> durable session message
  -> next-epoch 8 KiB projection
```

这条链路同时制造了四类问题：

1. 工具实现必须先把完整输出 materialize 成一个 `String`。超大 stdout、搜索结果或 MCP response 会先
   占用进程内存，然后才在 model boundary 被截断。
2. durable session 保存的是已经截断的 model envelope，不是完整原始结果。当前
   `DurableTranscriptEvent` ref 只能恢复这个 bounded envelope，不能恢复最初被省略的字节。
3. 当前 `ToolResult` 同时承担 raw data、model context 和 UI display 三种职责。一个字段的大小策略会
   同时影响 JSONL、provider token、TUI、run event 和恢复。
4. 同一个大输出既可能触发单条 stored event 的 1 MiB 上限，也会让长期会话 token 几乎完全被历史
   tool result 占据。随后再做 whole-conversation LLM compaction，会承担不必要的模型调用、cache
   reset 和语义漂移风险。

在触发本 RFC 的实际 session 中，durable JSONL 约 12.9 MB；67 条 tool result message 的 content
约 933 KB，占全部 durable user/assistant/tool message content 的约 98.8%。最新 provider usage
已经达到约 240K prompt token，其中约 226K 是 cache hit。这个形状说明：

- provider cache 正在降低账单，但不能消除长上下文、attention 和 fit 压力；
- 频繁改写旧 tool result 会破坏高价值 prefix；
- 只保留当前 32 KiB/8 KiB preview 又会永久丢失模型后续可能需要的细节；
- raw output、durable audit、model projection 和 display projection 必须解耦。

本 RFC 设计一条完整的 tool-result lifecycle，使超大输出：

```text
stream capture
  -> policy-safe immutable artifact
  -> small durable descriptor + structured facts
  -> bounded model view / display view
  -> typed on-demand retrieval
  -> deterministic batch aging
  -> context epoch activation
```

## 2. Decision summary

本 RFC 冻结以下五项决定。

### 2.1 Tool result 拆成三种 view

每次工具执行形成三个彼此独立的表示：

1. `RawArtifact`：完整的、经过 persistence policy 允许的原始字节，供审计和按需读取；
2. `ModelView`：结构化事实、bounded preview、状态和 opaque artifact ref；
3. `DisplayView`：TUI/Desktop 使用的 summary、分页能力和展示状态。

工具输出通过 bounded streaming sink 写入 artifact store；通用执行路径不再要求先构造无限大的
`String`。

### 2.2 增加 model-callable typed artifact retrieval

模型只拿到 session-scoped opaque ref，不拿本地路径。内置只读工具
`read_tool_artifact` 提供 bounded slice、line page 和 literal search；每次读取校验 session、
artifact hash、selector 和输出预算，并产生不含正文的 audit receipt。

### 2.3 Tool output 独立于 whole-conversation compaction 进行 aging

已完成、历史性的 tool output 在保护窗口之外可降级为小型 facts/preview。current turn、未闭合 tool
pair、错误、审批、mutation receipt、changed files 和 verification evidence 采用更强保留策略。

### 2.4 deterministic aging 先于 LLM semantic compaction

上下文治理固定为：

```text
externalize raw output
  -> tool-specific deterministic projection
  -> batch aging
  -> repeated-read dedupe
  -> LLM conversation compaction
```

只有前四层仍不能满足 fit 或 continuity/economics admission 时，才进入 RFC-0057 semantic
compaction。

### 2.5 复用 RFC-0058 event-driven worker 和增量 durable projection

artifact descriptor append 后只更新增量 projection 和 coalesced wake。worker 不扫描完整 JSONL，
也不按固定 50 ms 轮询。aging candidate 绑定稳定 frontier，并通过 RFC-0057 context epoch 的
compare-and-publish 激活。

## 3. Relationship to RFC-0057 and RFC-0058

本 RFC 不建立新的 scheduler、compaction resolver 或 session authority。

| Existing contract | RFC-0059 refinement |
| --- | --- |
| RFC-0057 `RawDurableResult` | 明确为独立 artifact bytes；durable transcript descriptor 不是 raw body |
| RFC-0057 `ShrinkCandidate` | 使用 artifact ref 和 structured facts，不再依赖已截断 transcript event 恢复 |
| RFC-0057 context epoch | aging 只在新 epoch 激活，不在当前 cached prefix 原地改写 |
| RFC-0058 source-change wake | tool result commit 更新 projection slot 并发送一个 coalesced wake |
| RFC-0058 incremental projection | 维护 token pressure、retention class、artifact reachability 和 aging eligibility |
| RFC-0058 single writer/CAS | artifact descriptor、aging plan和 epoch activation 都通过同一 authority boundary |

若本 RFC 与 RFC-0057 第 10 节对 raw tool result 的解释冲突，以本 RFC 为准。若与 RFC-0058 的
worker/writer contract 冲突，以 RFC-0058 为准。

当前 `ToolOutputArtifactRefV1::DurableTranscriptEvent` 和
`model_retrieval_available=false` 是过渡实现。新 session schema 启用后，它们不再作为通用 raw
artifact contract。

## 4. Goals and non-goals

### 4.1 Goals

- 任意单个工具输出不能直接制造超过 stored-event 上限的 JSONL record。
- ordinary tool execution 的峰值内存不随 stdout 总大小线性增长。
- provider 首次看到的 tool result 有明确 token 上界，同时保留后续精准读取能力。
- 历史 tool output 可按批次老化，且不修改当前 context epoch 的 immutable prefix。
- tool result 在 session resume、fork、export 和删除时有明确生命周期。
- 模型、TUI、Desktop 和 HTTP 都不能获得 artifact 的物理路径。
- repeated retrieval 不重复向 durable JSONL 写入大正文，也不在 model context 中无限复制相同页面。
- aging 和 artifact GC 不引入 idle polling、完整 JSONL 热扫描或 hot writer lock 竞争。
- output preservation 遵守现有 SafePersist、secret carrier、external trust 和 approval 约束。

### 4.2 Non-goals

- 不保证无限大小的输出被永久、无上限保存。
- 不让模型通过 artifact ref 获得任意文件读取能力。
- 不让 artifact 替代 mutation receipt、verification receipt、external provenance 或 Task authority。
- 不在本 RFC 统一所有工具的业务语义；首版定义通用 fallback 和少量高价值 projector。
- 不把 SQLite catalog 变成 artifact body 或 live session authority。
- 不为保住 cache TTL 发送额外 provider ping。
- 不兼容没有 V2 artifact descriptor 的旧 session log。

## 5. Current baseline and confirmed gap

### 5.1 Current implementation

当前 `sigil-kernel::tool` 的重要行为是：

- `ToolResult` 用一个 `content: String` 保存完整执行结果；
- `to_model_content()` 在 provider boundary 应用 32 KiB byte cap；
- `summary()` 又复制完整 `content` 到 `content_preview`；
- `agent::tool_results` 先调用 `to_model_message()`，再把该 message 写入 session；
- next-epoch `ToolOutputProjectionPolicy` 默认再收缩为 8 KiB；
- recoverable shrink ref 指向 durable transcript event，但该 event 已经只含 bounded model envelope。

因此当前链路是：

```text
tool raw bytes
  -> unbounded String in memory
  -> 32 KiB provider envelope
  -> same bounded envelope persisted
  -> 8 KiB next-epoch view
```

“可以从 durable event 恢复”只表示可恢复 32 KiB envelope，不表示可恢复最初 raw output。

### 5.2 Why increasing `MAX_EVENT_BYTES` is not a fix

当前 stored-event hard limit 为 1 MiB。提高上限只会：

- 把故障推迟到更大的输出；
- 增加 crash recovery、checksum、JSON parse 和 projection rebuild 成本；
- 继续把 UI/model/audit 的尺寸策略绑在同一个字段；
- 让每次 provider request 和 compaction 处理更多无差别历史文本。

正确不变量是：**durable event 只包含 O(1) descriptor、facts 和 bounded view，不包含 unbounded body。**

### 5.3 Terminology

| Term | Meaning |
| --- | --- |
| observed bytes | 工具实际产生、sink 实际观察到的字节数 |
| persisted bytes | 经过 policy 和 hard cap 后进入 artifact 的字节数 |
| complete | observed bytes 全部按原顺序进入 artifact |
| policy redacted | secret/persistence policy 替换或移除了禁止持久化内容 |
| storage truncated | 输出超过 artifact hard cap，只保存受控部分 |
| model view | 某 context epoch 内给 provider 的 bounded representation |
| display view | 给 TUI/Desktop 的 bounded DTO，不是 provider message |
| aging | 用更小的确定性 projection 替代下一个 epoch 中的历史 model view |
| retrieval page | 从 artifact 按 selector 读取的一段 bounded transient body |

“raw”在本 RFC 中指 **policy-safe raw artifact**。artifact store 不是绕过 SafePersist、credential 或
URL capability 规则的后门。

## 6. Core invariants

1. artifact body 不进入 JSONL event、control entry、run event 或 Desktop IPC。
2. public/model-visible ref 不包含 absolute path、workspace path、username 或 content-addressed filename。
3. 一个 artifact ref 只能在创建它的 logical session scope 内解析。
4. descriptor 的 `content_sha256` 绑定最终 persisted bytes；读取时必须重新校验或使用已验证 immutable
   handle。
5. tool result durable append 前必须完成 artifact publish；反向顺序禁止。
6. descriptor append 失败时 artifact 可以成为 orphan，但不能出现“durable ref 指向尚未发布 temp
   file”。
7. artifact 缺失或损坏不能触发工具重跑；只返回 typed unavailable/corrupt state。
8. current context epoch 内已经发送的 model view 不原地改变。
9. aging candidate 必须绑定 exact durable frontier、active epoch、policy version 和 artifact hash。
10. old tool output 的 body 可 aging；其 status、error、approval、mutation、changed-file、verification
    和 provenance facts 不能随正文一起丢失。
11. model retrieval 的正文不会被作为第二份大 tool result 永久复制到 JSONL。
12. steady-state append、pressure observation 和 idle wait 均不读取完整 session JSONL。

## 7. Data model

### 7.1 Opaque reference and descriptor

```rust
pub struct ToolArtifactRefV1 {
    /// Random, non-path, session-scoped capability identifier.
    pub artifact_id: ToolArtifactId,
}

pub struct ToolArtifactDescriptorV1 {
    pub schema_version: u16,
    pub artifact_ref: ToolArtifactRefV1,
    pub session_scope_id_hash: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub content_sha256: String,
    pub observed_bytes: u64,
    pub persisted_bytes: u64,
    pub media_type: String,
    pub encoding: ToolArtifactEncoding,
    pub completeness: ToolArtifactCompleteness,
    pub sensitivity: PersistenceSensitivity,
    pub retention_class: ToolArtifactRetentionClass,
    pub retrieval_policy: ToolArtifactRetrievalPolicyV1,
}

pub enum ToolArtifactCompleteness {
    Complete,
    PolicyRedacted { redaction_count: u32 },
    StorageTruncated {
        omitted_bytes: u64,
        retained_head_bytes: u64,
        retained_tail_bytes: u64,
    },
    EphemeralUnavailableAfterRestart,
}
```

`artifact_id` 使用不可预测随机值；`content_sha256` 用于 integrity 和 dedupe proof，不作为模型可直接
拼接到文件路径的 identifier。`session_scope_id_hash` 使用 domain-separated hash，不持久化 raw
session path。

### 7.2 Structured facts

```rust
pub struct ToolResultFactsV1 {
    pub status: ToolResultStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub changed_files: Vec<BoundedWorkspacePath>,
    pub error: Option<BoundedToolErrorV1>,
    pub mutation_receipt_refs: Vec<MutationReceiptRef>,
    pub verification_receipt_refs: Vec<VerificationReceiptRef>,
    pub external_provenance_refs: Vec<ExternalProvenanceRef>,
    pub tool_specific: BoundedJsonValue,
}
```

`tool_specific` 必须经过 schema、depth、key count 和 byte cap 校验。它不能成为另一个可塞入任意大
JSON 的逃生口。

### 7.3 Three views

```rust
pub struct ToolResultViewsV2 {
    pub artifact: ToolArtifactDescriptorV1,
    pub model: ToolModelViewV1,
    pub display: ToolDisplayViewV1,
}

pub struct ToolModelViewV1 {
    pub facts: ToolResultFactsV1,
    pub preview: BoundedText,
    pub preview_kind: ToolPreviewKind,
    pub artifact_ref: Option<ToolArtifactRefV1>,
    pub retrieval_hint: Option<BoundedText>,
    pub token_upper_bound: u64,
    pub projection_version: u16,
}

pub struct ToolDisplayViewV1 {
    pub status_label: String,
    pub summary: BoundedText,
    pub preview: BoundedText,
    pub observed_bytes: u64,
    pub persisted_bytes: u64,
    pub has_more: bool,
    pub artifact_ref: Option<ToolArtifactRefV1>,
    pub display_capabilities: Vec<ToolDisplayCapability>,
}
```

三种 view 的生命周期不同：

- artifact descriptor 和 facts 进入 durable `ToolResultRecordedV2`；
- initial model view 的 canonical bytes/hash 进入 durable event，证明模型首次可见内容；
- display view 从 descriptor、facts 和 bounded preview 增量 materialize，不作为第二份 raw truth。

### 7.4 Durable event

```rust
pub struct ToolResultRecordedV2 {
    pub schema_version: u16,
    pub call_id: String,
    pub tool_name: String,
    pub artifact: Option<ToolArtifactDescriptorV1>,
    pub facts: ToolResultFactsV1,
    pub initial_model_view: ToolModelViewV1,
    pub initial_model_view_sha256: String,
    pub recorded_at: Timestamp,
}
```

首版约束：

- canonical serialized event target 不超过 64 KiB；
- hard fail 仍使用全局 `MAX_EVENT_BYTES`，但普通 tool result 不应接近 1 MiB；
- `initial_model_view` 默认不超过 8K estimated tokens，且同时受 32 KiB byte cap；
- generic preview 默认 16 KiB head + 8 KiB tail；tool-specific projector 可以在同一总预算内使用更高
  信息密度的结构化结果；
- event 超过 64 KiB target 时再次 deterministic shrink；仍超过 hard limit 才返回
  `tool_result_descriptor_too_large`，不能回退为 inline raw body。

### 7.5 Token and storage accounting

三种 view 必须分账：

```text
session disk bytes =
  immutable artifact bytes
  + bounded JSONL descriptor/facts/view bytes

provider input tokens =
  active model views
  + explicitly retrieved artifact pages
  + other conversation/context layers

display bytes =
  bounded display DTO/pages
```

因此：

- artifact body 即使是 10 MiB，只要未被读取，就贡献 0 provider token；
- display pagination 不自动进入 model context；
- durable JSONL 中的 descriptor 会影响 replay/storage，但不应因 body 体积线性增长；
- initial/aged model view 和每次 retrieval page 才进入 token/cost 计算；
- provider cache hit 会降低部分输入价格，但这些 token 仍占 context window，因此 aging admission 同时
  看 fit、cache economics 和任务质量；
- token telemetry 必须按 `initial_view`、`aged_view`、`artifact_read` 和 `semantic_compaction`
  分 bucket，不能继续把所有 tool token 合成一个不可解释的总数。

## 8. Streaming capture and artifact store

### 8.1 Tool output sink

`ToolContext` 增加 session-owned `ToolOutputSinkFactory`。能够产生大输出的工具改用：

```rust
let mut sink = ctx.create_tool_output_sink(call_id, metadata)?;
sink.write_stdout(chunk)?;
sink.write_stderr(chunk)?;
sink.record_fact(fact)?;
let captured = sink.finish()?;
return ToolResultV2::from_capture(captured);
```

sink 同时执行：

- incremental SHA-256；
- observed/persisted byte accounting；
- UTF-8/binary media detection；
- bounded head/tail preview；
- cancellation 和 total-budget enforcement；
- persistence redaction/classification；
- store-owned temp file 写入。

通用路径不得用 `read_to_string` 或无上限 `Vec<u8>` 收集完整子进程输出。

### 8.2 Transitional adapter

首版允许现有小工具继续返回 `ToolResult { content: String }`。agent boundary 立即把它导入 sink，再
生成 V2 record。该 adapter：

- 只用于迁移，不是长期大输出 API；
- 对 `content` 设置较小 hard guard，防止已有工具继续 materialize 任意大字符串；
- 在 telemetry 中记录 `legacy_inline_capture`，推动高流量工具迁移；
- 必须先迁移 shell、search、file read、test runner、MCP 和 agent-result 读取路径。

### 8.3 Store layout

物理布局属于 session state，不属于 workspace：

```text
<sigil-state>/workspaces/<workspace-id>/sessions/<session-id>/artifacts/
  blobs/<sha256-prefix>/<sha256>
  staging/<random>.part
```

模型、TUI、HTTP 和 Desktop 永远看不到这个路径。首版不新增 crate；kernel 定义 provider-neutral
contract，session store 实现本地 filesystem backend。若未来需要 remote/object backend，再通过
`ToolArtifactStore` trait 替换。

### 8.4 Publish protocol

```text
1. create store-owned staging file with owner-only permissions
2. stream + classify + hash + enforce caps
3. fsync staging file when durability policy requires
4. atomically rename/link immutable blob
5. build descriptor and bounded views
6. append ToolResultRecordedV2 + related controls through the single writer
7. apply incremental projection
8. publish RunEvent with bounded display DTO
9. coalesce SessionProjectionWake
```

第 4 步成功、第 6 步失败会产生 orphan blob。它由带 grace period 的 mark-and-sweep 回收；不得为了
模拟跨文件和 JSONL 的虚假原子性而把 body 重新塞进 event。

### 8.5 Initial storage limits

首版内部默认值：

```toml
[tool_artifacts]
max_artifact_bytes = 16777216       # 16 MiB
max_live_session_bytes = 268435456  # 256 MiB
event_target_bytes = 65536          # 64 KiB
model_view_max_bytes = 32768        # 32 KiB
display_preview_max_bytes = 32768   # 32 KiB
```

规则：

- 超过单 artifact cap 时继续 drain/terminate 按工具 contract 决定，但 descriptor 必须记录
  `StorageTruncated` 和 observed/persisted bytes；
- session budget 先驱动 eligible artifact GC；仍不足时新结果使用 bounded
  `StorageTruncated`，不能删除 active/protected artifact；
- mutation/verification/approval receipt 使用自己的 durable contract，不受 raw body GC 连带删除；
- 默认值先作为 internal policy 和 telemetry 字段，不立即增加普通用户设置。

## 9. Tool-specific projection

generic head/tail 是 fallback，不是所有工具的最佳 model view。

| Tool class | Required deterministic facts | Preferred preview |
| --- | --- | --- |
| shell/process | exit code、signal、duration、stdout/stderr bytes、changed files | failing tail + bounded head |
| tests | suite/case counts、failed names、first failure、receipt refs | failure blocks，成功时极短 |
| search/grep | query hash、match/file counts、top matches、truncation | representative matches |
| file read | workspace-relative path、content hash、range、encoding | requested range |
| file mutation | changed paths、diff/receipt refs、verification state | compact diff summary |
| MCP | server/tool identity、trust class、status、content type | schema-aware bounded JSON/text |
| web/external | source IDs、trust label、citation/provenance refs | bounded sourced facts |
| agent result | child/run identity、terminal state、artifact ref | result summary，不复制完整 transcript |

projector contract：

```rust
pub trait ToolResultProjector: Send + Sync {
    fn project(
        &self,
        descriptor: &ToolArtifactDescriptorV1,
        generic_preview: &BoundedPreview,
        execution_meta: &ToolExecutionMeta,
    ) -> Result<ToolResultProjectionV1>;
}
```

- projector 是纯确定性、版本化、bounded 的函数；
- projector 不执行网络、不修改 workspace、不读取 artifact 之外的任意路径；
- projector 失败时回退 generic projection，并记录原因；
- output 必须通过统一 facts/preview size validator；
- model-generated summary 不进入首次 tool result projection。

## 10. Typed artifact retrieval

### 10.1 Public tool contract

模型可见内置工具：

```json
{
  "name": "read_tool_artifact",
  "arguments": {
    "artifact_ref": "ta1_...",
    "selector": {
      "kind": "line_page",
      "start_line": 120,
      "line_count": 80
    }
  }
}
```

V1 selector 只允许：

```rust
pub enum ToolArtifactSelectorV1 {
    ByteSlice { offset: u64, limit: u32 },
    LinePage { start_line: u64, line_count: u32 },
    SearchLiteral {
        query: BoundedLiteral,
        start_offset: u64,
        max_matches: u16,
        context_lines: u16,
    },
}
```

首版不支持任意 regex、glob、shell expression、absolute path 或“列出所有 artifact”。

### 10.2 Authorization

每次读取必须同时证明：

1. ref schema/version 合法；
2. ref 存在于当前 logical session scope 的 durable descriptor projection；
3. descriptor 没有 expired/revoked；
4. current agent/tool registry 允许 `read_tool_artifact`；
5. selector 在 byte/line/match budget 内；
6. blob 路径由 store 内部解析并保持 root confinement；
7. content hash 与 descriptor 一致；
8. sensitivity/retrieval policy 允许当前 surface/model 读取。

artifact ref 不是 bearer path。即使用户猜到另一个 session 的 ref，也必须 fail closed。

### 10.3 Retrieval result

模型看到：

```rust
pub struct ToolArtifactPageV1 {
    pub artifact_ref: ToolArtifactRefV1,
    pub selector: ToolArtifactSelectorV1,
    pub content: BoundedTextOrBytes,
    pub returned_bytes: u32,
    pub next_selector: Option<ToolArtifactSelectorV1>,
    pub content_sha256: String,
    pub complete_for_selector: bool,
}
```

默认单次 retrieval body：

- hard byte cap 16 KiB；
- estimated token cap 4K；
- line page 最多 200 行；
- literal search 最多 20 matches、每个 match 最多 3 行上下文；
- 一个 model turn 最多 8 次 artifact read、累计 64 KiB body。

这些限制属于 root-owned budget；子 agent 继承更小的剩余预算，不能各自重置。

### 10.4 Durable receipt without duplicate body

artifact read 的 durable event 只记录：

```rust
pub struct ToolArtifactReadRecordedV1 {
    pub call_id: String,
    pub artifact_ref: ToolArtifactRefV1,
    pub selector: ToolArtifactSelectorV1,
    pub returned_bytes: u32,
    pub page_sha256: String,
    pub artifact_sha256: String,
    pub outcome: ToolArtifactReadOutcome,
}
```

正文不写入 event。当前 provider request 的 tool result body 由 artifact + selector
deterministically materialize；crash recovery 若仍需重建未闭合 pair，也用 receipt 重新读取同一页面。
artifact 不可达时 pair materialize 为 typed `artifact_unavailable`，禁止重跑原工具。

### 10.5 Repeated-read dedupe

dedupe key：

```text
(artifact_sha256, canonical_selector, page_sha256, active_epoch_id)
```

同一 epoch 内重复读取同一页时：

- durable receipt 仍记录调用和 outcome；
- model view 默认返回短消息：`unchanged_from_read=<call-id>`、page hash 和 artifact ref；
- 若 artifact hash 或 selector 变化则正常返回正文；
- context epoch rotation 后 dedupe ledger 可保留 hash lineage，但不假设 provider 仍记得旧 page。

文件读取工具可采用同样机制，以 `(workspace file hash, range, selector)` 避免反复注入未变化内容。

## 11. Output aging policy

### 11.1 Why aging is separate

Tool output 通常是低连续性密度、高 token 体积的数据。先做 deterministic aging 可以：

- 不调用 LLM；
- 不让摘要模型解释或改写 verification/approval truth；
- 精确计算能回收多少 token；
- 在需要细节时通过 artifact 恢复；
- 把 semantic compaction 留给目标、决策和技术脉络。

### 11.2 Eligibility

一个 tool result 只有同时满足以下条件才可 aging：

- tool call/result pair 已闭合；
- 不属于 current active user turn；
- 没有 pending approval、continuation 或 unresolved provider round-trip state；
- artifact descriptor 和 structured facts 已 durable；
- result 不在 recent protected token window；
- projection 能保留该 result 的 required high-signal facts；
- candidate 能在同一 batch 中达到最小 reclaim threshold。

### 11.3 Retention classes

```rust
pub enum ToolOutputRetentionClassV1 {
    ActivePair,
    CurrentTurn,
    Recent,
    HighSignal,
    Ageable,
    Aged,
}
```

`HighSignal` 至少包括：

- 未解决 error 和 retry decision；
- approval request/decision；
- workspace mutation receipt、changed files 和 diff identity；
- verification result；
- active plan/skill/task state；
- external source provenance 和 citation identity；
- 用户明确要求原样保留的 evidence。

`HighSignal` 不表示永远保留完整 raw body；它表示 aged model view 必须保留更丰富 structured facts，
且只有在 evidence 可通过 artifact/repository/receipt 恢复时才可缩小正文。

### 11.4 Initial token policy

```toml
[context.tool_output_aging]
recent_protected_tool_tokens = 32768
minimum_reclaimable_tool_tokens = 16384
initial_model_view_target_tokens = 4096
initial_model_view_hard_tokens = 8192
aged_result_target_tokens = 1024
aged_turn_aggregate_tokens = 2048
max_results_per_batch = 128
```

selection 顺序：

1. 从 newest 向 oldest 保护 current turn、active pair 和最近 32K tool tokens；
2. 对剩余结果按 age、token、signal density 和 recoverability 排序；
3. 只有同一 batch 预计至少回收 16K token 时才准备 candidate；
4. 单个 aged result 目标不超过 1K token；
5. 同一完整历史 turn 的 aged tool facts 聚合目标不超过 2K token；
6. fit-required 可以绕过 16K economics threshold，但不能绕过 safety/protection rules。

32K/16K/1K 是首版 telemetry defaults，不是 provider-specific 永久常量。exact tokenizer 可用时使用
exact token；否则使用 calibrated upper bound。

### 11.5 Aged model view

aged view 至少包含：

- tool name、call ID、status；
- artifact completeness、observed/persisted bytes 和 content hash；
- artifact ref 和 retrieval availability；
- error/exit/changed-files/receipt/provenance facts；
- projector 提取的关键计数或结论；
- bounded preview，或在 facts 已充分时省略 preview；
- aging reason、source view hash 和 projection version。

它不能声称“完整输出仍在 transcript”；必须明确完整度和 artifact 状态。

## 12. Deterministic lifecycle before semantic compaction

每次 provider request 前的治理顺序固定为：

```text
Stage 0  append-only raw artifact + ToolResultRecordedV2
Stage 1  tool-specific initial projection
Stage 2  eligible historical output batch aging
Stage 3  repeated artifact/file read dedupe
Stage 4  RFC-0057 semantic compaction, only if still admitted
```

Stage 0/1 是 tool completion 的同步 durability boundary。Stage 2/3 只准备 next-epoch candidate。
Stage 4 才允许 LLM 生成 continuity narrative。

禁止：

- 每个 turn 都重写整个 history；
- 每个大结果立即独立切一个 epoch；
- 为了减少 token 在未达到 batch threshold 时持续产生小 cache reset；
- 把 deterministic tool facts 交给 compaction model 猜测；
- aging 后删除 raw artifact；
- artifact GC 后静默重跑原工具恢复内容。

## 13. Event-driven worker integration

### 13.1 Incremental projection

RFC-0058 projection 增加：

```rust
pub struct ToolOutputPressureProjectionV1 {
    pub frontier: DurableCursor,
    pub active_epoch_id: ContextEpochId,
    pub total_visible_tool_tokens: u64,
    pub protected_tool_tokens: u64,
    pub reclaimable_tool_tokens: u64,
    pub ageable_results: VecDeque<AgeableToolResultRef>,
    pub high_signal_results: BTreeSet<ToolCallId>,
    pub artifact_states: BTreeMap<ToolArtifactId, ArtifactProjectionState>,
    pub policy_version: u16,
}
```

projection 只保存 counters、IDs、hash、token proof 和 bounded facts；不把 artifact body 克隆进常驻内存。

### 13.2 Wake semantics

`ToolResultRecordedV2` append 完成后：

1. single writer 推进 durable cursor；
2. incremental reducer 更新 pressure projection；
3. observer slot 合并为 `Invalidated(ToolOutputPressure, frontier)`；
4. 最多发送一个 `SessionProjectionWake`。

不新增每 turn 一个 `ContextPressureChanged` durable event。pressure 是从 durable tool result 和 active
epoch 可确定重建的 projection state；wake 只是 hint。

### 13.3 Cheap preflight

worker 被唤醒后只读取 active projection snapshot：

```text
if no ageable result
  or reclaimable < minimum
  or active pair/current turn unsafe
  or another candidate already covers same frontier:
    return idle
```

cheap preflight 不 reload JSONL、不获取 data-file shared lock、不读取 artifact body。只有 eligible
candidate 才克隆 bounded descriptor/facts snapshot，并按需读取小型 preview。

### 13.4 Candidate and activation

```rust
pub struct ToolOutputAgingCandidateV1 {
    pub source_frontier: DurableCursor,
    pub source_epoch_id: ContextEpochId,
    pub policy_version: u16,
    pub source_layout_hash: String,
    pub result_plans: Vec<ToolOutputAgingPlanV1>,
    pub tokens_before: u64,
    pub tokens_after_upper_bound: u64,
    pub estimated_cache_reset_cost: Option<Money>,
    pub activation_reason: ToolOutputAgingReason,
}
```

activation 复用 RFC-0057：

1. validate source frontier、epoch、artifact hash、tool pair safety；
2. materialize next request projection；
3. exact/upper-bound token admission；
4. compare cache reset cost 与避免 semantic compaction 的收益；
5. append candidate/activation lifecycle；
6. `ContextEpochActivatedV1` 原子切换 active projection。

source frontier 或 epoch 变化时 candidate stale。worker 重新读取增量 projection，而不是 full replay。

### 13.5 Priority

worker priority 保持：

```text
active run continuation
  > blocking user/control decision
  > ordinary queued work
  > TaskGuidance
  > fit-required tool aging
  > cost-only tool aging
  > semantic compaction preparation
  > artifact GC
```

tool streaming chunk 不进入 authority inbox；只有 terminal durable append 触发 projection wake。

## 14. Cache-stable context epoch behavior

### 14.1 No in-place rewrite

当前 epoch 已经发给 provider 的 tool result bytes 保持不变。aging candidate 即使提前准备完成，也只在
下一次 context epoch activation 后生效。

### 14.2 Batch over eager shrink

一个 100K-token tool result 很大，不代表应立即 reset cache。单独 aging 只有在以下任一条件成立时
激活：

- 下一次真实 request 不 aging 就无法 fit；
- 本 batch 能避免更昂贵的 semantic compaction；
- RFC-0057 economics 证明预期 future turns 内回本；
- 用户手动确认“只清理大工具输出”。

否则 candidate 等待与后续 semantic rotation 或更大 aging batch 一起激活。

### 14.3 Cache telemetry

每次 activation 记录：

- stable prefix tokens before/after；
- aged tool tokens and retained facts tokens；
- estimated/observed cache read、write、miss；
- first-new-epoch cost；
- break-even turns；
- candidate waiting duration；
- activation reason；
- semantic compaction 是否因此避免或延后。

成功标准不是“token 越少越好”，而是 fit、任务质量和 effective cost 同时改善。

## 15. Artifact retention, fork, export and GC

### 15.1 Default lifecycle

默认 `SessionBound`：

- active session、fork boundary、checkpoint、verification 或 review 仍引用时保留；
- model view aging 不影响 artifact retention；
- session 删除进入 tombstone/grace period 后才允许删除；
- export 必须显式选择包含 artifact、只含 bounded transcript，或拒绝不完整 export；
- fork 建立独立 logical ref lineage，可复用 immutable bytes，但不能共享可越权解析的 public ref。

### 15.2 Mark-and-sweep

GC root：

- active `ToolResultRecordedV2` descriptors；
- active/previous context epoch checkpoint refs；
- unresolved read receipts/current turn；
- fork/export pins；
- verification/review pins；
- explicit retention holds。

GC 流程：

1. 从 incremental descriptor projection 获取 mark set；
2. 比较 store manifest，不扫描 session JSONL；
3. orphan 至少经过 24 小时 grace；
4. move-to-trash/tombstone 后再异步 unlink；
5. deletion outcome 写 bounded audit event；
6. GC 不持有 JSONL writer lock 执行大目录 IO。

### 15.3 Missing or corrupt artifact

```rust
pub enum ToolArtifactAvailability {
    Available,
    Expired,
    Missing,
    HashMismatch,
    PolicyRevoked,
    LegacyUnavailable,
}
```

model/display view 始终如实显示状态。缺失 artifact 不影响 session event audit，但会使 retrieval
fail closed。禁止把 `Missing` 自动转换成工具重跑。

## 16. Security, privacy and trust

### 16.1 Persistence policy

artifact capture 必须先经过专用 `SafePersistArtifact` boundary：

- 已有“不得持久化”的 bearer、signed URL、credential、provider-private carrier 仍不得进入 artifact；
- 可持久化但敏感的本地输出使用 owner-only permissions，并从 telemetry/support bundle 排除；
- redaction 后 descriptor 标记 `PolicyRedacted`，不能声称 `Complete`；
- 只能进 process-local capability store 的内容标记
  `EphemeralUnavailableAfterRestart`，不伪造 durable recoverability。

### 16.2 Filesystem hardening

- store root 在 trusted state directory 下 canonicalize；
- staging/blob create 使用 no-follow 和 owner-only mode；
- resolver 不接受用户输入的 path component；
- immutable blob publish 后不原地修改；
- archive/binary artifact 不自动解压或执行；
- line/search reader 有 CPU、memory、byte 和 match budget；
- literal search 首版避免 regex denial-of-service；
- hash mismatch 立即隔离，不返回部分内容。

### 16.3 Surface boundaries

- kernel/provider message 只含 opaque ref；
- TUI 通过 typed action 读取 page；
- `sigil-http` 暴露 bounded DTO 和 page endpoint，不暴露 filesystem path；
- `sigil-desktop` 只桥接 allowlisted command/event；
- renderer 没有 generic filesystem、HTTP 或 bearer 权限；
- MCP server/tool 不能自行声明一个本地 path 为 trusted artifact ref。

### 16.4 Trust labels

artifact 保存内容不改变其 provenance：

- external/web/MCP 内容仍为 `ExternalUntrusted`；
- model-generated projector 禁止；所有 V1 projector 必须 deterministic；
- artifact retrieval body 在 provider context 中保留原 trust label；
- artifact ref 只能证明 identity/integrity，不能证明内容真实、命令成功或测试通过。

## 17. Crash consistency and failure matrix

| Failure point | Durable state | Required recovery |
| --- | --- | --- |
| staging write 前 | 无 descriptor、无 blob | 返回 tool execution error |
| staging write 中断 | `.part` | grace-period orphan cleanup |
| blob publish 后、event append 前 | immutable orphan blob | GC；不得向模型返回未持久化 ref |
| event append 中断 | writer recovery 决定 record 是否 committed | committed descriptor 必须可解析；否则 event 不生效 |
| event append 后、projection apply 前 | durable descriptor 存在 | incremental catch-up/rebuild descriptor projection |
| projection apply 后、wake 前 | state 已可推导 | resume 时 observer re-arm；wake 可丢 |
| retrieval 中进程崩溃 | read receipt 可能不存在 | 不产生 mutation；用户/模型可重新读取 |
| aging prepare 中崩溃 | old epoch active | 丢弃 candidate |
| aging activation append 中断 | append-only lifecycle | resolver 只接受 complete terminal |
| blob 后续缺失/损坏 | descriptor 仍可审计 | typed unavailable；不重跑原 tool |

side effect boundary 保持不变：artifact 持久化失败不能让一个已执行 mutation 被“当作未执行再试”。
mutation tool 仍以 receipt/physical-attempt lifecycle 处理 unknown outcome。

## 18. TUI-first product behavior

### 18.1 Tool card

默认 tool card 展示：

```text
✓ cargo test
  142 passed · 2 failed · 18.4 s
  输出 3.8 MiB，当前显示 12 KiB
  [查看下一页] [搜索完整输出] [复制摘要]
```

若 artifact 不完整：

```text
输出超过 16 MiB 保存上限；已保留开头、结尾和结构化结果。
```

若 artifact 已过期/不可用：

```text
完整输出不可用；状态、receipt 和已保存摘要仍可审计。
```

### 18.2 Context status

不向普通用户暴露“projection reducer”等内部术语。可见状态：

- `已保存完整工具输出`
- `历史工具输出已整理，可按需读取`
- `等待当前工具步骤结束后整理`
- `整理收益不足，保持当前上下文`
- `完整输出已过期`

### 18.3 Manual control

RFC-0057 compact preview 增加：

- tool tokens before/after；
- protected/ageable result 数；
- artifact reachability；
- 预计 cache reset 和 break-even；
- “只整理工具输出”选项。

不新增日常 slash command；复用 compact/context management surface。高级诊断可展示 artifact ID 和
hash，但仍不显示物理路径。

## 19. Ownership map

| Owner | Responsibilities | Must not own |
| --- | --- | --- |
| `sigil-kernel::tool` | V2 result/view/facts contract、sink interface、bounded validation | filesystem path 暴露、provider fields |
| `sigil-kernel::session` | artifact store、descriptor events、recovery、GC roots、incremental projection | tool-specific business parsing |
| `sigil-kernel::agent` | artifact commit-before-append、model view materialization、read budget | 跳过 approval/mutation lifecycle |
| `sigil-runtime` | projector registry、tokenizer/cost policy、feature gates | durable truth |
| `sigil-tools-builtin` | shell/search/read/test projector、streaming output | session authority |
| `sigil-mcp` | bounded streaming adapter、trust/content metadata | 把 MCP path 当 trusted ref |
| provider crates | bounded model-view wire mapping和 usage telemetry | raw artifact store、aging policy |
| `sigil-tui` | display view、pagination/search action、compact preview | 直接读 artifact 文件 |
| `sigil-http` / `sigil-desktop` / `apps/desktop` | narrowed DTO/command/event | path、bearer、generic filesystem |
| RFC-0058 worker | wake、priority、stable frontier candidate scheduling | tool body、second authority |

## 20. Session schema cutover

用户已明确旧日志无需兼容。本 RFC 采用 clean cutover：

1. 新 session schema 使用 `ToolResultRecordedV2` 和 real artifact ref；
2. 不把旧 `DurableTranscriptEvent` envelope 迁移成“完整 artifact”；
3. 没有 V2 descriptor 的 old result 标为 `LegacyUnavailable`；
4. 不做启动期全量历史扫描、artifact backfill 或 destructive JSONL rewrite；
5. old session 无法通过新 schema validation 时可以停止加载，并给出 bounded diagnostic；
6. 不为兼容旧 session 保留双写路径；
7. cutover 前的 current development-only projection schema 可直接替换。

这项决定显著降低实现复杂度，但不能降低新 schema 的 crash、fork、export 和 deletion tests。

## 21. Rollout plan

### R59.1 Contract and evidence baseline

- 冻结 descriptor、facts、view、read receipt 和 availability schema；
- 记录 current session 的 event bytes、tool token share、peak memory 和 cache hit baseline；
- 为 1 MiB stored-event error 增加独立 regression；
- V2 feature gate 默认关闭。

Exit：schema 和 failure matrix 通过 review，未改变 production behavior。

### R59.2 Artifact store and streaming sink

- 实现 session-scoped local store、staging/publish、hash、caps 和 orphan cleanup；
- `ToolContext` 注入 sink factory；
- 迁移 shell/process 和 MCP 大输出路径；
- legacy inline adapter 加 telemetry 和 guard。

Exit：10 MiB/100 MiB synthetic output 不进入单个 `String`，JSONL event 保持 bounded。

### R59.3 ToolResult V2 and three views

- durable append 改为 descriptor + facts + initial model view；
- 增加 generic projector；
- 迁移 tests/search/file read/mutation projector；
- TUI/RunEvent 改为 bounded display DTO。

Exit：model、display、artifact 三者尺寸策略互不影响；无 path leakage。

### R59.4 Typed retrieval

- 增加 `read_tool_artifact`、session authorization、selector budgets 和 hash validation；
- read receipt 不含正文；
- crash recovery 可重建未闭合 artifact-read tool pair；
- repeated-read dedupe。

Exit：模型能按 ref 找到需要的页；cross-session、expired、corrupt 和 oversized selector 全部 fail closed。

### R59.5 Deterministic aging

- pressure projection、retention class 和 batch selector；
- artifact-aware aged model view；
- 复用 RFC-0057 epoch prepare/activate/economics；
- TUI compact preview 增加“只整理工具输出”。

Exit：latest/current/high-signal contract 通过，aging 不在当前 epoch 原地改写。

### R59.6 Event-driven integration and GC

- RFC-0058 source-change wake、coalescing slot、cheap preflight；
- full/incremental projection equivalence；
- mark-and-sweep、fork/export/delete pins；
- long-session resource telemetry。

Exit：idle 时零 polling、零 full replay、零 data-file lock；legitimate append 的工作量与 delta 成正比。

### R59.7 Default flip and cleanup

- 新 session 默认 V2 tool result；
- 删除 old `content: String` 大输出主路径和 fake transcript artifact ref；
- 同步 README、governance、core solution、TUI/Desktop/HTTP schema；
- 运行全量 gate 和真实长会话 acceptance。

Exit：所有 acceptance criteria 满足后状态从 `draft` 升为 `accepted`。

## 22. Validation plan

### 22.1 Deterministic unit tests

1. UTF-8 boundary、binary、empty output、exact cap、cap+1；
2. SHA-256、observed/persisted bytes 和 completeness 一致；
3. facts/depth/key/byte cap；
4. descriptor 和 initial model view canonical hash；
5. public ref 不包含 path；
6. cross-session ref 被拒；
7. missing、expired、hash mismatch、policy revoked；
8. byte slice、line page、literal search bounds；
9. repeated-read dedupe 的 unchanged/changed case；
10. current turn、active pair、error、approval、mutation、verification retention；
11. exact tokenizer 和 upper-bound selector；
12. candidate frontier/epoch/policy/hash drift；
13. full rebuild 与 incremental pressure projection 等价；
14. orphan grace 和 GC root；
15. legacy session 返回 unsupported/legacy unavailable，不尝试伪造 artifact。

### 22.2 Fault injection

每个第 17 节 crash point 都必须可注入。特别覆盖：

- rename 成功后 append 失败；
- append fsync 后 projection 未 apply；
- read receipt append 前/后 crash；
- epoch candidate 完成后 source cursor 变化；
- GC 与 active read 并发；
- artifact budget 耗尽；
- process cancellation 时 stdout pipe 仍有数据；
- mutation tool 已执行但 artifact persistence 失败。

### 22.3 Scale tests

| Scenario | Target |
| --- | --- |
| single 10 MiB shell output | stored event < 64 KiB target；model view <= 8K tokens |
| single 100 MiB output | bounded memory；artifact 明确 storage-truncated |
| 100 × 10 MiB outputs | JSONL growth 与 descriptor/view 成正比，不与 raw bytes 成正比 |
| 1000 small tool results | 不因 artifact abstraction 显著增加 latency/IO |
| 240K prompt with 90%+ cache hit | aging 只在 fit/economics admission 后 reset epoch |
| repeated same page reads | 第二次起不重复注入正文 |
| idle 10 minutes | zero JSONL reload、zero lock retry、zero aging poll |

### 22.4 Cross-surface tests

- TUI tool card、pagination、search、unavailable state；
- CLI/non-TUI run event 只含 bounded DTO；
- HTTP OpenAPI drift check；
- Desktop Rust IPC、TypeScript types 和 renderer tests；
- provider request materialization 保持 tool call/result pair；
- fork/resume/export/delete lifecycle；
- no path/secret leakage snapshot。

### 22.5 Quality evals

eval corpus 至少包含：

- 大编译日志中只需最后一个 error；
- test log 中需要回读中间 failure；
- grep 结果中模型先看 top matches，再搜索 artifact；
- MCP 返回大 JSON，projector 保住 key facts；
- mutation 成功但 stdout 很大；
- tool error 后三轮仍需引用 exact evidence；
- 三次 aging + 一次 semantic compaction；
- artifact 过期后模型不虚构细节、不自动重跑 mutation；
- prompt injection 藏在 artifact 中，retrieval 后仍保留 external/untrusted label。

核心指标：

| Metric | Target direction |
| --- | --- |
| max durable tool-result event bytes | <= 64 KiB target |
| peak memory / observed output bytes | 不再线性增长 |
| historical tool tokens per useful turn | 下降 |
| artifact retrieval success on needed evidence | 上升 |
| repeated page duplication | 接近 0 |
| semantic compactions per 100 turns | 下降 |
| cache epoch resets per 100 turns | 不增加 |
| unsupported completion/verification claims | 0 |
| cross-session/path leakage | 0 |
| idle full replay / lock attempts | 0 |

## 23. Rejected alternatives

### 23.1 Only lower the current 32 KiB cap

拒绝。它减少首次 token，但永久丢失细节，仍保留 unbounded in-memory `String`，也没有 typed re-read。

### 23.2 Only use the current 8 KiB next-epoch projection

拒绝。当前 ref 指向已经截断的 transcript envelope，不是完整 artifact；“recoverable”语义不成立。

### 23.3 Store full output inline in JSONL

拒绝。它继续受 1 MiB event limit、JSON encoding、replay、checksum、memory 和 projection 成本影响。

### 23.4 Give the model a local file path

拒绝。路径泄漏 home/workspace layout，扩大任意文件读取和 cross-session authority，且 fork/export 后
不可移植。

### 23.5 Summarize every large tool result with an LLM

拒绝。它增加请求、账单、latency、prompt injection 和事实漂移。deterministic projector 是默认；
LLM 只用于 RFC-0057 conversation continuity。

### 23.6 Rewrite old history immediately after every tool call

拒绝。它持续破坏 exact-prefix cache，并让 cache identity、audit 和 crash recovery复杂化。

### 23.7 Depend on provider prompt cache instead of aging

拒绝。cache 降低部分输入账单，不解决 context fit、attention dilution、provider portability 和模型可见
噪声。

### 23.8 Rely only on whole-conversation compaction

拒绝。tool output 是可确定处理的高体积数据，不应先交给模型语义总结。

### 23.9 Put artifact truth in SQLite catalog

拒绝。catalog 可索引 descriptor，但 append-only session + immutable blob 才是 session-local durable
truth。SQLite 不进入 live writer authority。

## 24. Competitor and official research

调研基于 `~/study/sigil-competitor-repos` 的固定 revision，并用官方文档交叉核对。竞品默认值用于理解
机制，不直接成为 Sigil contract。

| Project | Observed mechanism | Sigil conclusion |
| --- | --- | --- |
| OpenAI Codex `4808c162` | exec 保留 raw bytes，provider view 按 token/byte 做 middle truncation；pre-turn 检查 compaction | raw capture 与 model view 应分层；truncation 必须 token-aware |
| OpenCode `884c2560` | 超过 2000 行或 50 KiB 写 managed file，并给 bounded head/tail；compaction 先处理旧 tool result | durable managed output + deterministic pruning 应早于 semantic summary |
| Gemini CLI `ae0a3aa7` | large tool distillation 和 masking 分层；保护最近约 50K tool tokens，在有足够可回收量时批处理 | recent protection + minimum reclaim threshold 比逐条 eager shrink 稳定 |
| Claude Code `01f1617f` | 官方说明先清旧 tool output 再 summary；changelog 记录 large result disk refs 和 repeated Read dedupe | output aging、disk artifact 和 dedupe 是互补机制 |
| Goose `fe7f16b7` | oversized response 写临时文件；旧 tool output 在 compaction 前有独立 cutoff/summarization | offload 有价值，但 temp path 不能成为 durable public ref |
| DeepSeek Reasonix `a2a44a77` | 分层阈值先 tool snip/prune，后 compact，并先 archive | deterministic tool cleanup 应早于 LLM compaction |
| Crush `d8fc48a0` | Bash 对超大输出做 bounded lossy middle truncation | bounded provider view 是底线，但还需要 durable retrieval |

关键差异：

- Sigil 使用 session-scoped opaque ref，不把物理路径交给模型；
- artifact read 有 typed selector、root-owned budget、hash 校验和 durable receipt；
- aging 通过 cache-stable context epoch 批量激活；
- worker 通过 source-change wake 和 incremental projection 调度；
- artifact body、session event、model view 和 display view 有明确不同的 durability/size contract。

## 25. Research references

### Official documentation

- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [Anthropic prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [Claude Code: how Claude Code works](https://code.claude.com/docs/en/how-claude-code-works)
- [Claude Code prompt caching](https://code.claude.com/docs/en/prompt-caching)
- [Gemini CLI configuration](https://geminicli.com/docs/reference/configuration/)
- [OpenCode compaction](https://opencode.ai/v2/docs/compaction)
- [Microsoft Event Sourcing pattern](https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing)
- [Microsoft Materialized View pattern](https://learn.microsoft.com/en-us/azure/architecture/patterns/materialized-view)

### Exact source snapshots

- [OpenAI Codex output truncation](https://github.com/openai/codex/blob/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/utils/output-truncation/src/lib.rs#L12-L144)
- [OpenAI Codex exec output context](https://github.com/openai/codex/blob/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/core/src/tools/context.rs#L310-L409)
- [OpenAI Codex pre-turn compaction](https://github.com/openai/codex/blob/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/core/src/session/turn.rs#L796-L820)
- [OpenCode tool output store](https://github.com/anomalyco/opencode/blob/884c256033958475be4feba69b7e6bf72caaf0ed/packages/core/src/tool-output-store.ts#L13-L188)
- [OpenCode compaction implementation](https://github.com/anomalyco/opencode/blob/884c256033958475be4feba69b7e6bf72caaf0ed/packages/core/src/session/compaction.ts#L12-L188)
- [Gemini CLI tool output masking](https://github.com/google-gemini/gemini-cli/blob/ae0a3aa7b928cc73bb09604bb9c2c020e6b647db/packages/core/src/context/toolOutputMaskingService.ts#L24-L269)
- [Gemini CLI tool distillation](https://github.com/google-gemini/gemini-cli/blob/ae0a3aa7b928cc73bb09604bb9c2c020e6b647db/packages/core/src/context/toolDistillationService.ts#L109-L177)
- [Claude Code large tool-result persistence changelog](https://github.com/anthropics/claude-code/blob/01f1617f14452ac78bf319cef2236d87c0fe05cb/CHANGELOG.md#L2735-L2765)
- [Claude Code repeated Read dedupe changelog](https://github.com/anthropics/claude-code/blob/01f1617f14452ac78bf319cef2236d87c0fe05cb/CHANGELOG.md#L2006-L2015)
- [Goose large response handler](https://github.com/aaif-goose/goose/blob/fe7f16b727fa1ecccac15c7eaab593b13347058f/crates/goose/src/agents/large_response_handler.rs)
- [Goose context management](https://github.com/aaif-goose/goose/blob/fe7f16b727fa1ecccac15c7eaab593b13347058f/crates/goose/src/context_mgmt/mod.rs#L460-L610)
- [DeepSeek Reasonix pruning](https://github.com/esengine/DeepSeek-Reasonix/blob/a2a44a772c7c954763255ab4752cc47473a73cac/internal/agent/prune.go)
- [DeepSeek Reasonix compaction](https://github.com/esengine/DeepSeek-Reasonix/blob/a2a44a772c7c954763255ab4752cc47473a73cac/internal/agent/compact.go#L20-L130)
- [Crush Bash output truncation](https://github.com/charmbracelet/crush/blob/d8fc48a03c36f3268b4013d3a72ef7091c43d712/internal/agent/tools/bash.go#L389-L440)

## 26. Open questions

以下问题不阻塞 V1 contract，但必须在相应 rollout slice 前关闭：

1. artifact at-rest encryption 是 workspace policy、connection policy 还是全局 installation policy；
2. 16 MiB/256 MiB 默认值在真实 Rust build、MCP 和 agent-result workload 上是否需要调整；
3. export 是否默认包含 artifact，还是默认只含 bounded transcript 并显示不完整性；
4. literal search 稳定后是否增加受限 regex engine；
5. content-addressed blob 是否允许同 workspace 多 session 物理去重，以及如何保持 logical ref 隔离；
6. binary/image artifact 的 model retrieval 是否只返回 metadata，还是通过 provider capability 重新注入；
7. artifact GC 的磁盘压力 admission 如何与 session deletion/tombstone 统一。

任何答案都不能放宽 opaque ref、session scope、bounded retrieval、no-body-in-event 和 no-in-place-rewrite
五项核心不变量。

## 27. Acceptance criteria

本 RFC 只有在以下条件全部满足后才能升级为 `accepted`：

- `ToolResultRecordedV2` event target、artifact descriptor、facts 和三种 view schema 冻结；
- shell/MCP/file/search/test 至少五类高流量工具不再 materialize unlimited output；
- 10 MiB/100 MiB/100×10 MiB scale tests 满足 event、memory 和 artifact cap；
- `read_tool_artifact` 的 session scope、selector budget、hash、receipt 和 crash recovery 全部通过；
- latest turn、active pair、error、approval、mutation、verification 和 provenance retention spec 通过；
- deterministic aging 在 semantic compaction 之前，并复用 RFC-0057 context epoch activation；
- RFC-0058 worker steady state 为 event-driven、incremental、idle-quiescent；
- no path、secret、cross-session、renderer capability leakage tests 通过；
- fork/resume/export/delete/GC lifecycle 通过；
- TUI、CLI、HTTP、Desktop 使用同一 typed artifact/display state；
- `cargo fmt --all --check`、`cargo check`、`cargo test` 和
  `cargo clippy --all-targets -- -D warnings` 通过；
- telemetry 证明 semantic compaction 次数、历史 tool token 和 effective cost 至少不劣于当前 baseline；
- 旧 session 不兼容策略已在 release note 和错误提示中明确，不存在静默误读。
