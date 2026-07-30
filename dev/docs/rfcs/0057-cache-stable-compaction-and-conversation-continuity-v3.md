# RFC-0057 Cache-stable Compaction and Conversation Continuity V3

状态：accepted / portable V3 implemented（R57.6 exact-route native resume deferred）

创建日期：2026-07-28

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0010 Structured Compaction and Task Memory](0010-structured-compaction-and-task-memory.md)
- [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)
- [RFC-0009 Extension Trust Plane](0009-extension-trust-plane.md)
- [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- [RFC-0028 Real-model Acceptance and Provider Conformance V1](0028-real-model-acceptance-and-provider-conformance-v1.md)
- [RFC-0051 Intent Stack / 意图级版本控制 V1](0051-intent-stack-and-intent-level-version-control-v1.md)
- [RFC-0052 Desktop Conversation Continuity and Control V1](0052-desktop-conversation-continuity-and-control-v1.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0056 Provider Connections, Credential Storage and Model Catalog V1](0056-provider-connections-credentials-and-model-catalog-v1.md)

## 1. Problem statement

Sigil 当前的高层方向是正确的：把 compaction 视为一次稀有的 cache epoch reset，而不是上下文稍大
就不断改写历史。当前默认配置在 50% 发出 soft signal、80% 进入 hard admission，并保留最近 6 条消息；
DeepSeek V4 Flash 路径还会用 pinned tokenizer 做 exact target admission。原始 JSONL 不被覆盖，压缩通过
`CompactionStarted`、`TaskMemoryRecordedV1`、`CompactionAppliedV2` 等 append-only 事件生效。

但当前 V2 同时存在连续性风险和成本模型缺口：

1. **重复压缩没有从上一个 checkpoint 继承 active intent。** 当前 portable checkpoint 的
   `pinned_user_constraints` 只从本次 `folded_event_ids` 建 catalog；旧 boundary 被保护、不会再次进入
   catalog。第一次压缩保住的根任务或约束，可能在第二次压缩后从 provider projection 消失。
2. **`latest user message` 不是 root objective。** 没有 durable task 时，以最新用户消息推导 objective
   会把“继续”“跑一下测试”之类局部指令误当成整个会话目标。
3. **固定 `tail_messages = 6` 不代表六个完整 turn。** 一次 tool-heavy turn 可能有几十条消息；六条
   也可能从 tool result 中间切开，或不足以保住仍在执行的用户请求。
4. **压缩 admission 主要判断 token fit，没有核算 cache epoch 的真实账单。** 对 DeepSeek 这类 cache-hit
   输入远低于 cache-miss 输入的后端，提前把一个很长但高命中的前缀改写成短前缀，短期内可能反而更贵。
5. **工具输出 shrink 与语义压缩的 cache 影响没有统一建模。** 在旧消息位置替换大 tool result 虽然减少
   token，却会从替换点起破坏 exact-prefix cache；它不一定是“免费清理”。
6. **provider-native compaction 不能成为唯一真相。** OpenAI 和 Anthropic 已提供 opaque 或 native
   compaction block，但这些载体绑定 provider、model、API shape、retention contract 和 beta/version。
   Sigil 还必须支持 provider/model 切换、离线 replay、审计与恢复。
7. **“第一条用户消息永远原样保留”不是正确不变量。** 第一条消息可能是已被后续明确替代的需求，也可能
   是几十万 token 的粘贴内容或图片。真正应当保留的是仍然生效的目标、约束、授权边界和可恢复引用。
8. **纯确定性抽取不足以恢复长任务的语义脉络。** durable projection 能可靠保住目标、约束、授权、
   receipt 和精确实体，但它不擅长概括“为什么采用当前方案”“哪些尝试已被排除”“几个文件之间如何
   协作”等因果和技术上下文。没有额外模型归纳时，checkpoint 虽然安全，却可能让后续模型重复探索。

本 RFC 设计 V3：在尽量复用模型后端 prompt cache 的同时，让会话能够跨多次压缩、provider 切换和
进程恢复持续进行。

## 2. Decision summary

V3 采用以下方案：

1. compaction 从单一“摘要动作”升级为 **Context Epoch Rotation**。正常 turn 只追加，不改写已发送
   前缀；只有 fit 或经济性 admission 通过时才切换 epoch。
2. 每个 epoch 的 request projection 固定分为五层：
   `ProviderStatic -> SessionAnchor -> ContinuityCheckpoint -> VerbatimTail -> DynamicOverlay`。
   变化越慢、复用价值越高的内容越靠前。
3. `SessionAnchorV1` 是 accepted Intent Stack、Task/control state 和 exact active user constraints
   的只读 request projection；它机械引用现有 authority，不新建第二套 objective/intent 真相，也不由
   摘要模型自由改写。
4. `ConversationContinuityV2` 采用 **确定性事实底座 + LLM 语义叙事**。authority-bearing 部分每次从
   durable truth、前一版 active ledger 与本次 delta 重新 materialize；额外 LLM 只能补充
   `ModelGeneratedUnverified` narrative，不能创建或修改 objective、constraint、authorization、
   completion 或 verification truth，也不做无来源的 summary-of-summary。
5. recent tail 改为按 token 预算选择完整 turn group，至少保住 active user turn、未闭合 tool group、
   queued input 和必要 attachment refs；废弃把“最近 N 条消息”作为核心语义。
6. 每次正常 semantic compaction 必须向当前同一 provider/model route 发送一次额外 LLM 摘要请求。
   请求保持旧 epoch provider request 作为原样前缀，只在末尾追加 bounded compaction instruction；
   不创建子 agent/session，不执行 client tool，不启用 hosted tool。candidate 只有通过 source、
   authority、fit、实际 usage/economics 和 lifecycle validation 才能原子激活。
7. 工具输出先生成 recoverable shrink candidate，不立即改写当前 epoch。默认在同一次 epoch rotation
   应用；只有它能避免更昂贵的 semantic compaction 且经济性明确为正时，才允许单独触发 cache reset。
8. provider-native compaction 是可选 **acceleration carrier**，不是 durable truth。每次 native
   compaction 仍必须有 portable checkpoint；provider/model 不兼容时回退 portable projection。
9. 自动压缩 trigger 从固定百分比升级为 `fit required OR expected total cost wins`。模型同时计算
   reserved output、下一 turn/tool 增长预测、cache read/write/miss 单价和 break-even turns。
10. 第一条用户消息不做无条件永久 pin。只有其中仍 active 的 exact spans 进入 `SessionAnchorV1`；
    大附件/粘贴内容保留 durable artifact ref，需要时可恢复读取。
11. kernel 只暴露 provider-neutral 的 epoch、checkpoint、cache layout 和 economics contract；
    `cache_control`、`prompt_cache_key`、native compaction block 等留在 provider crate。
12. V3 先以 telemetry 和 shadow plan 上线，再修复 repeated-compaction continuity，最后逐 provider
    开启显式 cache/native compaction。后续实施授权已给出；native carrier 仍默认关闭，只有用户显式
    开启且 exact route capability 通过运行时复核时才允许额外 provider 请求。
13. 默认只做一次摘要调用，不再追加“让第二个模型检查摘要”的 probe。严格 schema、closed source
    catalog、deterministic authority reconstruction 和 compare-and-publish 是首版 completeness gate；
    第二次 probe 只能作为未来经过独立成本 admission 的可选质量模式。

## 3. Goals and non-goals

### 3.1 Goals

- 同一 session 在三次以上 compaction 后仍保住当前有效目标、约束、授权、未完成工作和验证状态。
- 通过额外 LLM 归纳保住仅靠结构化 projection 难以表达的技术脉络、因果关系和已排除路径。
- 正常 turn 使用 byte-stable、append-only request prefix，最大化 exact-prefix cache hit。
- 压缩前后都能解释 token、cache hit/write/miss、摘要调用的实际 usage/cost、break-even 和触发原因。
- provider-native 能力可用时获得收益；切换 provider/model 时不丢连续性。
- TUI 能预览“会保留什么、会折叠什么、为什么现在值得切换 epoch”。
- raw durable stream 始终是会话真相；摘要不能伪造完成、验证、授权或用户约束。

### 3.2 Non-goals

- 不承诺 lossless 重建模型看过的每一个 token；原始事件和 artifact 可审计、可恢复，但 active request
  projection 必然是有损选择。
- 不在 kernel 统一模拟所有 provider 的缓存实现。
- 不靠 background ping 或虚构请求维持 cache TTL。
- 不把超长 context window 当成“不需要 compaction”的理由；focus degradation 和成本仍需治理。
- 不在本 RFC 固定任何 provider 的长期价格。价格和 capability 必须来自 model catalog snapshot。

## 4. Current baseline and confirmed gap

### 4.1 Current strengths to keep

| Contract | V3 treatment |
| --- | --- |
| raw JSONL append-only，不覆盖历史 | 保留，epoch 只是 projection boundary |
| control / approval / tool pair 受 fold protection | 保留并扩大到 active turn group |
| exact tokenizer admission on pinned route | 提升为所有 capability-supported route 的优先路径 |
| `TaskMemoryV1` structured fields | 演进为 source-bound `ConversationContinuityV2` |
| portable/native driver 分层 | 保留，但明确 portable truth + native carrier |
| overflow recovery 必须确认没有 model consumption | 保留，最多一次 bounded recovery |
| soft threshold 不立即改写前缀 | 保留为 observation state |

### 4.2 Repeated-compaction failure mode

当前问题可以用以下事件序列复现：

```text
U1: “实现 X；不要提交；只改 crates/a”
A1/T1...                         ┐
compact #1 -> checkpoint C1      │ C1 pin 了 U1
U2: “继续，先跑定向测试”
A2/T2...                         ┐
compact #2 -> checkpoint C2      │ 本次 catalog 只看 C1 之后的新 folded events
request projection after C2      -> U1 的 active constraints 可能不再出现
```

V2 测试已经验证旧 boundary、supersedes 和 fold range，但尚未把“C2 必须继承 C1 中仍 active 的约束”
作为语义不变量。

V3 的 required invariant 是：

```text
active(Cn) =
  apply_supersession(
    active(Cn-1)
    + exact_user_delta(Cn-1.cursor, Cn.cursor)
    + durable_control_delta(...)
  )
```

其中 model-generated narrative 不得参与 authority/supersession 判定。

## 5. Competitor repository research

本节基于 `~/study/sigil-competitor-repos` 在 2026-07-28 的本地 snapshot。源码用于识别可复用机制，
不把竞品默认值直接当成 Sigil contract。

| Project / revision | Observed mechanism | What Sigil adopts | What Sigil rejects |
| --- | --- | --- | --- |
| [OpenAI Codex `4808c16`](https://github.com/openai/codex/tree/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/core/src) | local compact 把总结请求追加到完整历史；remote compact 在发送前裁剪 function output，记录 `cached_input_tokens`；remote tail 有 64K token budget；auto-compact window 区分 prefix prefill 与 body growth | 用旧 epoch 的完整前缀发起 compaction；记录 server-observed cache usage；按 token 而非条数保 tail；把 compaction 视为新 window | 只靠“保留一些用户消息 + narrative summary”承担 durable authority |
| [OpenCode `884c256`](https://github.com/anomalyco/opencode/tree/884c256033958475be4feba69b7e6bf72caaf0ed/packages/opencode/src/session) | 20K prune minimum、whole-turn tail、`tail_start_id`、previous summary、structured sections；另有 context epoch 与 provider cache policy | whole-turn tail、epoch identity、previous checkpoint lineage、结构化 summary、cache breakpoint adapter | 让 previous free-form summary 自己决定哪些旧约束仍生效 |
| [Gemini CLI `ae0a3aa`](https://github.com/google-gemini/gemini-cli/tree/ae0a3aa7b928cc73bb09604bb9c2c020e6b647db/packages/core/src) | 50% trigger、保留约 30% recent context；大工具输出写入临时文件后注入 preview；summary 后发第二次 probe 检查遗漏；新 context graph 记录被抽象 node IDs 和 active tasks | recoverable tool offload、summary completeness probe、source IDs、active task 不以“看起来完成”自动关闭 | 临时文件作为长期 durable artifact；用第二个模型回答替代确定性 continuity validation |
| [Aider `5dc9490`](https://github.com/Aider-AI/aider/tree/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider) | 递归 summary 直到 fit；background summary 只在 source messages 未变化时发布；prompt 被拆成稳定 chunks；可用小请求维持 cache warm | compare-and-publish candidate、稳定 request chunks | 为保 TTL 发送 max-token ping；它产生额外调用、网络和不可预期账单 |
| [Goose `fe7f16b`](https://github.com/aaif-goose/goose/tree/fe7f16b727fa1ecccac15c7eaab593b13347058f/crates/goose/src/context_mgmt) | 80% trigger；非手动压缩保留最近 text user message；summary overflow 时逐步移除中间 tool responses；另有后台 tool-pair summary | safe-boundary trigger、oversized tool fallback、不得从 tool pair 中间切 | 把完整 transcript 包进一个新 system prompt 做 summary；这会放弃原 prefix 的直接复用 |
| [Crush `d8fc48a`](https://github.com/charmbracelet/crush/tree/d8fc48a03c36f3268b4013d3a72ef7091c43d712/internal/agent) | context 剩余 20% 时压缩；在 system/最近消息设置 cache control；summary 成为新 history 起点 | provider cache marker 与 compact trigger 分开 | 单个无 source refs 的 summary 成为唯一 continuity state |
| [DeepSeek Reasonix `a2a44a7`](https://github.com/esengine/DeepSeek-Reasonix/tree/a2a44a772c7c954763255ab4752cc47473a73cac/internal/agent) | 50% 只提示、60% tool snip、80% compact、90% force；token tail、fold economics、adaptive token ratio；小 user turn 和旧 digest 被原样积累，第一条用户消息有 1,500-token pin 上限 | cache-first high-water states、fold economics、adaptive tail、第一次消息不应无限大 | 永久累积所有小 user turns/old digests；过期或冲突指令会持续膨胀且缺少 supersession |
| [Claude Code `01f1617`](https://github.com/anthropics/claude-code/blob/01f1617f14452ac78bf319cef2236d87c0fe05cb/CHANGELOG.md) | changelog 记录 compaction 后 cache miss、1h TTL、tool schema bytes 导致 miss、dynamic system content 被移出 prefix、大 tool result 持久化等修复 | cache-shape regression 必须是一等测试；动态字段后置；tool schema canonicalization | 仓库不含产品主体源码，因此不从 changelog推断未公开实现 |

竞品调研给出的共同结论不是“取一个最佳百分比”，而是：

- OpenAI Codex、Gemini CLI、OpenCode、Aider、Goose、Crush 和 DeepSeek Reasonix 的公开实现都在
  semantic compaction 路径发起额外 LLM 请求；OpenCode/Crush 虽使用内部 agent wrapper，但并不要求
  创建可独立恢复、调度或持久化的子 agent session；
- compaction、tool pruning、cache placement、continuity state 是四个相关但不同的子问题；
- recent history 应按 token 和完整 turn group 选取；
- repeated compaction 必须有 lineage；
- tool output 应可恢复，而不是只截断后永久丢失；
- cache telemetry 必须进入 admission，而不是只在账单页面事后观察；
- 无 source refs 的 rolling prose 越压越容易漂移。

## 6. Official backend constraints

以下约束按 2026-07-28 官方文档核对；运行时仍以 connection/model capability snapshot 为准。

### 6.1 DeepSeek

[DeepSeek Context Caching](https://api-docs.deepseek.com/guides/kv_cache/) 默认开启磁盘缓存。命中要求
后续请求完整匹配一个已经持久化的 prefix unit；请求输入末尾、模型输出末尾、公共前缀和固定 token
interval 都可能形成 unit。usage 提供 `prompt_cache_hit_tokens` 和
`prompt_cache_miss_tokens`。

设计含义：

- DeepSeek adapter 不假设显式 breakpoint；稳定、完全一致的 serialized prefix 是核心。
- 正常 turn 必须只追加；在旧 tool result 处 shrink 也是一次真实 cache reset。
- economics 必须使用 hit/miss 两条输入价格，不能只比较压缩前后总 token。

### 6.2 OpenAI

[Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching) 要求 exact prefix，建议把
静态内容放前面、动态内容放后面；usage 能报告 cache read/write。支持该能力的较新模型还可用 explicit
breakpoint 和 `prompt_cache_key` 改善 routing。

[Compaction](https://developers.openai.com/api/docs/guides/compaction) 提供 Responses 自动
`context_management` 和独立 `/responses/compact`。返回的 encrypted compaction item 必须按原样
round-trip；它适合做 route-bound native carrier，但不是跨 provider portable memory。

设计含义：

- `prompt_cache_key` 是 routing/partition hint，不是 continuity ID，也不能替代 exact bytes。
- explicit mode 下只在真正稳定边界写 cache，避免每个变动尾部都产生付费 write。
- stateless Responses 必须保留官方要求的 response output/reasoning/compaction items。

### 6.3 Anthropic

[Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) 的 prefix 顺序为
`tools -> system -> messages`，支持 automatic 或最多四个 explicit breakpoints、默认 5 分钟和可选
1 小时 TTL；exact match 包括 text 和 image。显式 breakpoint 有 20 content-block lookback 约束。
[Tool-use caching](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching)
说明 deferred tool definition 可以追加为 `tool_reference` 而不改变原 system prefix。

[Server-side compaction](https://platform.claude.com/docs/en/build-with-claude/compaction) 当前是 beta，
用 `compact_20260112` strategy 生成 `compaction` block；后续请求必须回传该 block，其之前内容会被
忽略。`pause_after_compaction` 可让 client 在继续执行前插入需保留的 recent/instruction blocks。

设计含义：

- A0 tool/system breakpoint 与 A2 conversation breakpoint 分开；
- 1h TTL 必须经过 idle-gap 与 write premium 的经济性判断；
- native block 必须绑定 beta/version/model 并和 portable checkpoint 同时持久化。

### 6.4 Gemini

[Gemini context caching](https://ai.google.dev/gemini-api/docs/caching) 对 Gemini 2.5+ 默认启用 implicit
cache；Interactions API 的 `previous_interaction_id` 可用于 stateful continuity 和更高 cache hit。
[Optimization guide](https://ai.google.dev/gemini-api/docs/optimization#context-caching-input-savings)
另说明 generateContent 可以创建带 TTL 和 storage charge 的 explicit cache object。

设计含义：

- 对普通会话优先 stable prefix + implicit cache；使用 stateful API 时单独满足 retention contract。
- explicit cache object 只适合真正大且跨多次请求复用的 A0/corpus，不用于频繁变化的 recent tail。
- Sigil durable session 不能只保存 `previous_interaction_id`；server state 到期后仍需 portable replay。

## 7. Core model: Context Epoch

### 7.1 Definition

`ContextEpoch` 是一段对 provider 呈现为 byte-stable prefix + append-only tail 的生命周期。一次
semantic compact、旧消息 shrink、incompatible model switch、tool schema reorder 或 system
instruction change，都可能开始新 epoch。

```rust
pub struct ContextEpochV1 {
    pub epoch_id: ContextEpochId,
    pub parent_epoch_id: Option<ContextEpochId>,
    pub source_cursor: DurableCursor,
    pub route_binding: RouteBinding,
    pub anchor_hash: ContentHash,
    pub checkpoint_hash: ContentHash,
    pub cache_layout_hash: ContentHash,
    pub activation_reason: EpochActivationReason,
    pub created_at: Timestamp,
}
```

`created_at` 是 durable metadata，禁止进入 cached prompt prefix。

### 7.2 Five-layer request projection

```text
┌──────────────────────────────────────────────────────────────┐
│ A0 ProviderStatic                                           │
│ base instructions, canonical tool schemas, stable examples  │
├──────────────────────────────────────────────────────────────┤
│ A1 SessionAnchor                                            │
│ active objective, exact constraints, authority/safety        │
├──────────────────────────────────────────────────────────────┤
│ A2 ContinuityCheckpoint                                     │
│ structured progress, decisions, evidence refs, pending work  │
├──────────────────────────────────────────────────────────────┤
│ A3 VerbatimTail                                             │
│ complete recent turns, active tool group, queued input       │
├──────────────────────────────────────────────────────────────┤
│ A4 DynamicOverlay                                           │
│ current input, ephemeral env/status/time/budget hints        │
└──────────────────────────────────────────────────────────────┘
```

排序不代表所有 provider wire format 都能把它们放在同一种 role。provider adapter 负责把逻辑 segment
映射到 `tools/system/messages/input items`，但必须保持：

- A0/A1/A2 的 canonical bytes 在 epoch 内不变；
- A3 只向后追加；
- timestamp、随机 ID、实时 token/cost、progress spinner、volatile environment state 只能在 A4；
- map key、tool schema、tool order、空字段、newline 和 image encoding 都按版本化规则 canonicalize。

### 7.3 Cache layout proof

每次实际 request 生成 provider-neutral proof：

```rust
pub struct CacheLayoutProofV1 {
    pub layout_version: u32,
    pub epoch_id: ContextEpochId,
    pub segments: Vec<CacheSegmentProofV1>,
    pub serialized_prefix_hash: ContentHash,
    pub stable_prefix_tokens: TokenProof,
    pub dynamic_suffix_tokens: TokenProof,
    pub breakpoints: Vec<LogicalCacheBreakpoint>,
    pub mutation_from_previous: Option<CacheMutationProof>,
}
```

proof 记录 logical content hash，不持久化 secret 或完整 provider payload。provider conformance test 再证明
logical breakpoint 被正确映射到 wire request。

## 8. Durable continuity model

### 8.1 SessionAnchorV1

`SessionAnchorV1` 只投影仍具约束力的用户意图，不保存任意聊天摘要。它不是新的 authority：
accepted objective 优先引用 RFC-0051 当前 `IntentVersionRef`；执行状态引用 durable Task/control
projection；旧 session 没有 Intent facts 时才回退 exact source turn，并明确标为 legacy provenance。

```rust
pub struct SessionAnchorV1 {
    pub root_objective: AnchoredStatement,
    pub active_subgoal: Option<AnchoredStatement>,
    pub constraints: Vec<ActiveConstraintV1>,
    pub authorization_boundary: Vec<ActiveConstraintV1>,
    pub attachment_refs: Vec<DurableArtifactRef>,
    pub source_cursor: DurableCursor,
    pub canonical_hash: ContentHash,
}

pub struct AnchoredStatement {
    pub exact_text: String,
    pub authority: ObjectiveAuthorityRef,
    pub source: SourceSpanRef,
}

pub enum ObjectiveAuthorityRef {
    AcceptedIntent(IntentVersionRef),
    DurableTask { task_id: TaskId, plan_version: u32 },
    LegacySourceTurn(SourceTurnRef),
}

pub struct ActiveConstraintV1 {
    pub constraint_id: ConstraintId,
    pub exact_text: String,
    pub source: SourceSpanRef,
    pub status: ConstraintStatus, // active | superseded | satisfied | revoked
    pub supersedes: Vec<ConstraintId>,
}
```

`SessionAnchorV1` event 只记录 projection hash 和来源，不能创建或接受 Intent、Task、approval 或
constraint。只有既有 durable Intent/user/control authority 能创建、替代或撤销 authority-bearing
statement。模型可以提出 `candidate interpretation`，但不能自行把“不要提交”标为过期，也不能把
一条 summary 变成 accepted Intent。

### 8.2 ConversationContinuityV2

```rust
pub struct ConversationContinuityV2 {
    pub source_cursor: DurableCursor,
    pub previous_checkpoint_id: Option<CheckpointId>,
    pub anchor_ref: SessionAnchorRef,
    pub decisions: Vec<GroundedDecision>,
    pub progress: Vec<GroundedProgress>,
    pub pending_work: Vec<GroundedPendingWork>,
    pub files_and_artifacts: Vec<GroundedArtifact>,
    pub commands: Vec<GroundedCommand>,
    pub verification: Vec<VerificationReceiptRef>,
    pub failures_and_dead_ends: Vec<GroundedFailure>,
    pub risks: Vec<GroundedRisk>,
    pub unresolved_questions: Vec<GroundedQuestion>,
    pub narrative: Option<UntrustedModelNarrative>,
}
```

所有 `Grounded*` 项都必须带 event/artifact/receipt source ref。以下字段禁止只凭 narrative 更新：

- 用户目标、约束和授权；
- 文件是否已经修改；
- 命令是否成功；
- 测试是否通过；
- 任务是否完成；
- approval 是否授予；
- tool output 中的安全关键事实。

### 8.3 Repeated compaction materialization

每次 candidate 从四类输入构建：

1. 当前 accepted Intent Stack、Task/control projection 和上一个 accepted checkpoint 的 derived
   active ledger；
2. 上一个 `source_cursor` 之后的 raw durable delta 与 Intent supersession lineage；
3. durable artifact/verification projection；
4. 当前 fold plan 中仍需模型归纳的非权威 evidence。

输出是新的完整 snapshot，而不是把旧 summary 当作一条普通消息再次总结。旧 checkpoint 只提供
已验证 lineage 和 active set，不提供不可核对的自由文本真相。

### 8.4 Is the first user message preserved verbatim?

结论：**不做无条件永久原样保留。**

使用以下规则：

| First-message content | V3 treatment |
| --- | --- |
| 仍 active 的根目标 | exact source span 进入 A1 |
| 仍 active 的硬约束/偏好/授权边界 | exact source span 进入 A1 |
| 已被后续明确替代的目标或约束 | durable stream 保留；active projection 标记 superseded，不再注入 |
| 大段 pasted corpus | corpus 存 durable artifact；A1/A2 保存 content hash、type、retrieval ref 和仍 active 的精确指令 |
| image/file attachment | 保存 attachment metadata、content hash 和可恢复引用；需要视觉语义时按 capability 重新注入 |
| 寒暄、过期背景、已经完成的局部要求 | 不永久 pin |

因此，“第一条原样保留”是一个过粗且会无限膨胀的 heuristic；“active source spans 原样保留”才是
连续性 contract。

## 9. Tail selection

### 9.1 Unit

tail selector 操作 `TurnGroup`，不操作单条 message：

```text
user input
  + assistant reasoning/output items required by provider
  + zero or more tool call/result pairs
  + assistant continuation/final
```

以下 group 不允许拆开：

- 未闭合的 assistant tool call 与其 tool result；
- 当前执行中的 user turn；
- waiting approval 与对应 preview；
- promoted/queued user input 与它将要影响的 turn；
- provider 要求 round-trip 的 reasoning/native state items。

### 9.2 Proposed defaults

```toml
[compaction.v3]
tail_min_complete_turns = 2
tail_target_min_tokens = 8192
tail_target_max_tokens = 65536
tail_recent_turn_p95_multiplier = 2.0
tail_max_usable_context_ratio = 0.25
```

effective target：

```text
clamp(
  2 * p95(tokens of recent complete turns),
  8K,
  min(64K, 25% * usable_context)
)
```

如果一个 active turn 本身超过 target：

1. 先把已消费的大 tool output 变成 durable artifact + head/tail preview；
2. 保留用户原文和仍需要的 tool pair；
3. target 可向上扩展到 exact fit limit；
4. 仍不 fit 时拒绝普通 compaction，转 bounded overflow recovery 或要求用户拆分任务。

`tail_messages` 在迁移期只用于 legacy config translation，不再是 V3 核心参数。

## 10. Tool-output lifecycle

### 10.1 Three representations

每个 large tool result 可有：

1. `RawDurableResult`：原始结果或受保护 artifact，审计与恢复使用；
2. `EpochVisibleResult`：当前 epoch 已经发送给 provider 的 immutable bytes；
3. `ShrinkCandidate`：下一个 epoch 可采用的 bounded preview + artifact ref。

### 10.2 No invisible cache break

在当前 epoch 中，把十轮之前的 100K tool output 改成 2K preview，会让 provider 从那个位置开始 cache
miss。V3 因此默认：

- 异步/idle 时只准备 `ShrinkCandidate`，不改变 request projection；
- semantic epoch rotation 时一并应用；
- emergency standalone shrink 必须生成新的 `ContextEpoch`，不得冒充“仍在原 cache epoch”；
- shrink admission 同样比较 cache reset 成本和避免 semantic summary 的收益。

### 10.3 Recoverability

preview 至少包含：

- tool name、call ID、exit/status；
- 原始 byte/token 量；
- head/tail bounded excerpt；
- content hash；
- artifact ref；
- redaction/truncation reason；
- 明确的“需要时重新读取”提示。

临时文件路径不能作为唯一 durable ref；artifact lifecycle 必须随 session fork/export/delete 正确迁移。

## 11. Compaction pipeline

### 11.1 Prepare

在 idle/safe boundary：

1. freeze `source_cursor` 和当前 epoch；
2. 生成 fold plan、whole-turn tail plan 和 shrink candidates；
3. materialize deterministic `SessionAnchorV1`；
4. 从 durable projections 构造 `ConversationContinuityV2` baseline；
5. 生成只包含本次 fold plan 中 durable event 的 closed source index；
6. freeze 摘要调用前的旧 epoch provider request；
7. 计算 pre-compaction cache layout、摘要调用上界和 provisional economics snapshot。

prepare 可以在后台做 CPU/local IO 工作，但不得发送 provider 请求、修改 visible projection 或发布
半成品 checkpoint。

### 11.2 Generate

正常 semantic compaction 必须向当前同一 provider/model route 发送一次摘要请求：

```text
byte-identical existing epoch provider request
+ one final semantic-compaction instruction
```

最后一条 instruction 包含 strict JSON schema、section/item/byte 上限，以及按旧 transcript 顺序排列的
closed source index。index 只携带 bounded source identity/role，不重复 stringify 整份 transcript。
这样摘要请求自身仍可复用旧 prefix cache。不得把整个 transcript 塞进一个全新 system message，除非
provider contract 只能如此且 economics 已把全量 miss 算入。

调用约束：

- provider、model、connection、traffic partition 与当前 route 相同；首版不切换“便宜摘要模型”；
- `max_tokens` 使用独立 bounded summary budget，`background = false`；
- 普通 client tool schema 可为保持旧前缀 identity 而保留，但 Sigil 不执行任何 tool call；一旦模型
  请求 tool，整份 candidate 拒绝；
- hosted tools 必须移除，因为它们可在 provider 侧产生外部副作用；因此产生的 cache layout mutation
  必须进入 proof 和成本估算；
- 摘要响应中的 response handle、continuation state 和 reasoning artifact 不进入主会话连续性；
- 物理调用以 `ProviderPhysicalAttemptPurpose::SemanticCompaction` 在发送前、结束后分别 durable
  记录；`logical_run_id` 绑定对应 portable compaction attempt。

模型只返回 schema-constrained candidate：

- `in_progress`；
- `pending_actions`；
- `provider_continuity`；
- `model_notes`；
- 每项必须引用一个或多个 closed source event IDs；
- 不得返回 authority mutation。

### 11.3 Validate

candidate 必须依次通过：

1. schema/canonical validation；
2. section/item/source-ref/aggregate byte limits；
3. source-ref existence 和 source scope validation；
4. model narrative 一律降级为 `ModelGeneratedUnverified`，并拒绝 completion、verification、approval
   等 authority claim；
   narrative 的 closed event citation 只 materialize 为 checksum-bound whole-event provenance，
   不伪装成模型文本与 durable field 逐字相等的 source span；
5. active objective/constraint/authorization 由 deterministic baseline 独立补全；
6. unresolved tool/approval/queued input completeness；
7. verification receipt consistency；
8. attachment/artifact reachability；
9. 用本次摘要调用实际 usage 重新计算 economics admission；
10. exact target token admission；
11. compare-and-publish：source cursor、route、tool catalog 和 epoch 未在生成期间变化。

首版不做第二次 model probe。后续若增加，probe 只能发现 narrative/technical detail 可能遗漏，不能
覆盖上述确定性 gate，也必须单独计费、审计和 admission。

### 11.4 Activate

通过 validation 后，一次事务性 lifecycle 发布：

```text
ContextEpochPrepared
ProviderPhysicalAttemptStarted(SemanticCompaction)
SemanticCompactionUsageRecorded
ProviderPhysicalAttemptTerminal
ContinuityCheckpointRecordedV2
CacheLayoutRecordedV1
CompactionEconomicsRecordedV1
[NativeCompactionCarrierRecordedV1]
ContextEpochActivatedV1
```

任何失败都追加 `ContextEpochRejectedV1`，旧 epoch 继续有效。禁止出现 checkpoint 已写入但 projection
未切换，或 projection 已切换但 durable terminal 未写入的中间状态。

新 epoch 不主动发送“cache warming”请求；下一次真实用户/agent turn 自然建立 cache。

### 11.5 Failure and fallback

- manual compact：摘要调用失败、超时、返回 tool call、JSON/schema 不合法或 source ref 伪造时，不得
  静默激活纯确定性 checkpoint；UI 明确显示失败原因，用户可重新发起。
- idle/cost-only automatic：摘要失败即放弃本次 rotation，旧 epoch 继续有效，并应用 circuit breaker。
  timeout、inflated output 和 invalid/schema/tool-call failure 都必须进入 durable compaction failure
  lifecycle，不能只留在进程内错误字符串中。
- fit-required / overflow emergency：为保证可用性，允许显式记录
  `deterministic_emergency_fallback` 后使用确定性 baseline 激活；UI/audit 必须说明本次没有成功生成
  LLM narrative，不能伪装为正常 semantic summary。若 provider 未返回完整 usage，token/cost 必须显示
  unknown，且不得据此生成零成本 projection。
- 任何已发送但终态不确定的摘要调用都不得无条件重试。只有 provider 证明 pre-dispatch /
  pre-generation rejection 时，才可按现有 bounded physical-attempt policy 重试。

## 12. Fit and cost admission

### 12.1 Safety budget

```text
usable_context =
  context_window
  - reserved_output_tokens
  - tool_growth_p95_tokens
  - provider_state_tokens
  - safety_buffer_tokens

projected_next_input =
  current_exact_input
  + next_turn_p95_tokens
```

优先使用 provider tokenizer 和 server-observed usage；没有 exact tokenizer 时使用 calibrated estimate，
并把误差分位数计入 safety buffer。

### 12.2 Trigger states

| State | Initial default | Action |
| --- | --- | --- |
| Observe | current input >= 50% window | 提示、采集 forecast，不改写 prefix |
| Prepare | projected next input >= 70% usable context，或 bulky shrink candidate 足够大 | 本地生成 shadow plan |
| Admit | projected next input > usable context，或 expected cost wins | safe boundary 执行 compact |
| Emergency | current input >= 90% context，或 provider overflow | bounded recovery；fit 优先于经济性 |

70%/90% 是初始 telemetry default，不是永久业务 contract。真正 activation 由 projected fit 和 economics
决定；80% legacy hard threshold 仅在 V3 capability/forecast 不可用时作为 compatibility fallback。

### 12.3 Cost model

对预计未来 `N` 个真实请求：

```text
C_keep(N) =
  Σ_i [
    hit_tokens_i  * price_cache_read
    + miss_tokens_i * price_input
    + write_tokens_i * price_cache_write
  ]

C_rotate(N) =
  observed_or_upper_bound_compactor_cache_read_cost
  + observed_or_upper_bound_compactor_uncached_input_cost
  + observed_or_upper_bound_compactor_output_cost
  + first_new_epoch_write_or_miss_cost
  + Σ_(i=2..N) new_epoch_read_and_tail_cost_i

admit when:
  fit_required
  OR (
    C_keep(N) - C_rotate(N) >= min_absolute_savings
    AND relative_savings >= min_savings_ratio
    AND break_even_turns <= expected_remaining_turns
  )
```

admission 分两阶段：

1. provider 调用前用 instruction 上限、summary output budget 和最保守 cache shape 做 provisional gate，
   cost-only candidate 若连该上界都无法回本则不发起摘要请求；
2. provider 调用后必须以实际 `UsageStats` 的 cache read/miss/write、output tokens 和 pricing snapshot
   重算最终 gate。实际成本超过 provisional 上界或使 break-even 超限时，candidate 被拒，旧 epoch
   保持有效；已发生的摘要调用成本仍进入 session stats 和 audit。

proposed initial defaults：

```toml
economics_horizon_turns = 3
min_savings_ratio = 0.05
min_savings_tokens_equivalent = 4096
max_break_even_turns = 3
```

`expected_remaining_turns` 的来源按可信度排序：

1. accepted TaskPlan 中未完成 step、active tool loop 和明确 queued input；
2. 当前 Intent/active subgoal 的未完成 acceptance criteria；
3. 本 session 最近 turn 的 agent-loop shape；
4. 无结构化信号时使用保守 fallback `3`。

不得为了让 compaction 看起来划算而从用户身份做跨 session engagement profiling；forecast source 和
confidence 必须进入 economics record。

`min_savings_tokens_equivalent` 只用于没有可靠价格 catalog 时；有价格时一律使用 connection/model
snapshot 的 cache read/write/miss/output 单价。

### 12.4 Why token reduction can cost more

以下用归一化价格说明为什么不能“超过 50% 就压”。假设：

- cache miss input 单价为 `1.0`；
- cache read 单价为 `0.02`；
- 当前有 400K-token stable prefix 和每 turn 10K-token dynamic suffix；
- compact 后 checkpoint + tail 为 60K tokens；
- 暂不计 summary output，只看对 compaction 最有利的下界。

继续当前 epoch 的单 turn input 成本约为：

```text
400K * 0.02 + 10K * 1.0 = 18K miss-token-equivalent
```

切换 epoch 后，第一轮至少要为新 60K prefix 付一次 write/miss：

```text
old-prefix compaction read 8K + new-prefix miss 60K + suffix miss 10K
= 78K miss-token-equivalent
```

后续命中新 prefix 时约为：

```text
60K * 0.02 + 10K * 1.0 = 11.2K miss-token-equivalent
```

首次 reset 比继续多约 60K，每个后续 turn 只省约 6.8K，至少接近 9 个 future turns 才 break even，
还没计 summary output。若 session 只剩两三轮，提前 compact 虽然让“token 数”变小，却会让真实账单
变大。反过来，若下一轮已经无法 fit，safety admission 必须覆盖 cost gate。

该示例只展示价格形状，不把 `0.02` 固化为某个 provider 的长期价格。

### 12.5 CompactionEconomicsV1

```rust
pub struct CompactionEconomicsV1 {
    pub model_ref: ModelRef,
    pub pricing_snapshot_id: PricingSnapshotId,
    pub input_tokens_before: TokenProof,
    pub input_tokens_after: TokenProof,
    pub stable_prefix_tokens_before: u64,
    pub stable_prefix_tokens_after: u64,
    pub observed_cache_read_tokens: Option<u64>,
    pub observed_cache_write_tokens: Option<u64>,
    pub observed_cache_miss_tokens: Option<u64>,
    pub compaction_call_cost: Option<Money>,
    pub first_new_epoch_cost: Option<Money>,
    pub break_even_turns: Option<f32>,
    pub forecast_horizon_turns: u32,
    pub forecast_confidence: ForecastConfidence,
    pub admission_reason: CompactionAdmissionReason,
}
```

### 12.6 Circuit breakers

- 同一 `source_cursor + layout_hash` 不重复尝试；
- candidate 没有至少 5% 且 4K token reduction 时拒绝，fit-required 除外；
- 连续两次 summary inflated/timeout 后禁用该 route 的 semantic summarizer，直到用户手动重试或 route 变化；
- 新 epoch 第一轮仍超过 emergency threshold 时停止自动循环，报告 system/tools/active turn 哪一层不可压；
- compaction 后至少完成一个真实 turn，才允许非 emergency 再次 semantic compact。
- semantic compaction usage 不得覆盖“最近一次正常 conversation generation usage”；两者分别用于
  当前 epoch cache 预测和总账单统计。

## 13. Provider adapter policy

### 13.1 Kernel capability

```rust
pub struct ProviderContextCapabilities {
    pub cache_mode: CacheMode,
    pub explicit_breakpoint_limit: Option<u8>,
    pub cache_ttls: Vec<CacheTtl>,
    pub cache_usage_fields: CacheUsageCapabilities,
    pub stateful_continuation: Option<StatefulContinuationCapability>,
    pub native_compaction: Option<NativeCompactionCapability>,
    pub native_carrier_portability: NativeCarrierPortability,
}
```

公共字段不得出现 `cache_control`、`compact_20260112`、Responses encrypted item 等 provider 术语。

### 13.2 DeepSeek adapter

- 保持 `tools/system/messages` 序列化完全稳定；
- 不发显式 breakpoint；
- 从 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 校准 forecast；
- public/private proxy 或 OpenAI-compatible endpoint 不能仅凭 model ID 继承 DeepSeek cache capability。

### 13.3 OpenAI Responses adapter

- capability 支持时，在 A0 和 A2 末端设置 explicit logical breakpoint；single-turn 不为 A4 写 cache；
- `prompt_cache_key = HMAC(tenant_partition, session_id, route_family, layout_version)`，不包含原始路径和 secret；
- 高并发时按官方 routing limit 把 hot session key 做稳定分片，不能每请求随机；
- native compaction carrier 绑定 connection、model snapshot、API version、ZDR/store mode 和 source cursor；
- route/model 变化或 carrier validation 失败时，使用 portable A1/A2/A3。

### 13.4 Anthropic adapter

- A0 last tool/system 设长期稳定 breakpoint；A2 设 conversation breakpoint；
- 自动 cache 与 explicit breakpoint 合用时预留四个 slot，避免 400；
- wire contract 接受默认/5m 与 1h，校验最多四个 breakpoint、20-block lookback 和 mixed TTL 必须按
  1h -> 5m 顺序；portable V3 首版生产请求只发默认 5m。只有后续有可信 idle-gap forecast，且预测
  idle gap 在 5m 到 1h、write premium 能 break even 时，才允许启用 1h；
- 高频 tool loop 可以让最新稳定 user/tool prefix 成为短 TTL breakpoint；
- deferred tool schema capability 可用时，避免把全部 MCP tools 塞进 A0；
- native compaction 只有 beta/version/model exact match 时开启，portable checkpoint 始终存在。

### 13.5 Gemini adapter

- 当前 `sigil-provider-gemini` 使用 `streamGenerateContent`：只保证 canonical prefix，并从
  `usageMetadata.cachedContentTokenCount` 读取实际 implicit-cache usage；
- 当前 capability 对 stateful continuation 返回 `None`，不得把 GenerateContent 猜测成
  Interactions API，也不得发送 `previous_interaction_id`；
- 后续 Interactions adapter 才能把 `previous_interaction_id` 作为 route-bound native handle；它仍须
  同时保留 portable projection，并在每次请求重发 interaction-scoped 的 tools、system instruction 和
  generation config；
- connection retention policy 不允许 server storage 时必须使用 stateless；handle 过期、删除或 route
  不兼容时机械回退 portable replay；
- explicit `CachedContent` 仅用于超大、稳定且有明确 TTL reuse 的 corpus/A0，不用于 A3，并与
  Interactions adapter 分开做 capability gate。

### 13.6 Generic OpenAI-compatible adapter

- 默认 `CacheMode::ObservedImplicitOrNone`；
- 只记录 endpoint 实际返回的 cache usage fields；
- 未经 conformance 不能假设 OpenAI/DeepSeek 的 explicit/native 语义；
- portable checkpoint 是唯一可用 carrier。

## 14. Native carrier and portability

```rust
pub struct NativeCompactionCarrierV1 {
    pub provider_family: ProviderFamily,
    pub connection_id: ConnectionId,
    pub model_snapshot_id: ModelSnapshotId,
    pub protocol_version: String,
    pub source_cursor: DurableCursor,
    pub portable_checkpoint_id: CheckpointId,
    pub opaque_payload_ref: ProtectedArtifactRef,
    pub expires_or_invalidates_on: Vec<CarrierInvalidation>,
}
```

规则：

- native carrier 不能被其他 provider crate 解读；
- carrier 丢失、过期或不兼容不影响 portable resume；
- opaque payload 遵守 credential/session redaction、export 和 delete policy；
- native summary 中的事实不自动升级为 durable verification；
- server-side state retention 与 ZDR 必须在 connection policy 中显式可见。

## 15. TUI-first product behavior

### 15.1 Status

TUI 使用用户可理解的状态，不直接显示 provider 内部术语：

- `上下文稳定 · 正在复用缓存`
- `正在准备整理候选`
- `等待当前工具步骤结束`
- `即将切换上下文阶段`
- `已整理 · 连续性检查通过`
- `整理收益不足 · 保持当前上下文`
- `无法安全整理 · 需要处理超大当前步骤`

### 15.2 Progress and receipt

手动 `/compact` 将命令本身视为用户对一次 semantic compact 的明确意图，不再增加 modal confirmation。执行过程必须展示非阻塞进度；终态回执至少包含 folded message 数、compaction id，以及失败时的精确拒绝原因。内部 admission evidence 继续覆盖：

- 当前 active objective 与 active constraints 的来源；
- fold range、完整 turn 数和 token；
- recent tail 的 turn/token；
- tool artifacts/shrink 数量；
- 当前 cache hit 量；
- 预计新 epoch 首轮成本、break-even turns 和触发原因；
- native carrier 是否可用；
- unresolved/approval/queued input 是否被保护。

该 evidence 用于 fail-closed 校验、审计和诊断，不要求用户在正常路径上重复确认。大型工具输出清理保持独立的确定性维护能力，不与一次 semantic `/compact` 混为同一个选择弹窗。

### 15.3 Activation

- manual semantic compact：单次 `/compact` 生成、校验并原子激活；无可折叠历史、摘要失败、token/economics 不准入或 source frontier 过期时保持当前 epoch 不变；
- fit-required automatic compact：只在 idle safe boundary 执行并发出可展开通知；
- tool call 正在运行或 approval 未决：延迟，不弹出打断式 modal；
- overflow recovery：明确告诉用户这是一次恢复动作及是否发生 provider consumption；

Desktop 如展示相同功能，必须消费与 TUI 相同的 typed state，不另造 compact semantics。

## 16. Persistence and audit

### 16.1 New/updated durable events

建议在现有 V2 lifecycle 上增量演进：

| Event | Purpose |
| --- | --- |
| `context_epoch_prepared_v1` | freeze source cursor、route、fold/tail plan |
| `session_anchor_recorded_v1` | accepted Intent/Task/control 的 derived active source spans 和 projection hash；不创建 authority |
| `continuity_checkpoint_recorded_v2` | portable structured checkpoint |
| `cache_layout_recorded_v1` | logical segment hash/token/breakpoint proof |
| `provider_physical_attempt_*` (`semantic_compaction`) | 摘要 provider 调用的发送前 barrier、usage lineage 和 terminal |
| `semantic_compaction_usage_recorded_v1` | 与普通 conversation usage 分离的摘要 cache/miss/output/cost |
| `compaction_economics_recorded_v1` | admission 和 break-even evidence |
| `native_compaction_carrier_recorded_v1` | protected opaque carrier ref |
| `context_epoch_activated_v1` | 唯一改变 active provider projection 的 terminal |
| `context_epoch_rejected_v1` | validation/admission/publish failure |

旧 `CompactionStarted` / `CompactionAppliedV2` 在兼容期可映射到 V3 lifecycle，但 resolver 必须只让一个
terminal activation 生效。

### 16.2 Fork/resume

- fork 继承 fork cursor 当时 active anchor/checkpoint，创建自己的 epoch lineage；
- 父 session 后续 supersession 不反向修改子 session；
- resume 重新验证 artifact reachability、native carrier compatibility 和 route/model snapshot；
- 无 native carrier 时机械生成 portable request，不调用模型“修复记忆”；
- corrupted checkpoint 不能静默 fallback 到 free-form summary，应回退上一个 valid epoch 或 raw replay。

## 17. Security and trust

- cached prefix 不得包含 secret、bearer、credential ID、真实 home path 或无必要 PII；
- `prompt_cache_key` 使用不可逆 partition key，不使用 session title/raw user text；
- model/plugin 生成的 summary 永远是 untrusted derived context；
- 摘要调用没有可执行 client tool path，hosted tools 被移除；模型返回 tool call 时 fail closed；
- 摘要 instruction 和输出都受独立 byte/token 上限，避免借 compact 放大 prompt injection 或账单；
- `SessionAnchorV1` 是 RFC-0051 Intent / Task / user-control authority 的 projection，不能反向修改
  Intent Stack、Task graph 或 acceptance state；
- `PreCompact` hook 只能补充 narrative focus 或候选 refs，不能创建 approval、constraint supersession、
  verification receipt 或 task completion；
- artifact preview 必须沿用 tool output redaction policy；
- native server state/compaction 的 retention 与 deletion contract 必须进入 connection settings 和 export；
- cache telemetry 不记录 payload，只记录 token、hash、route snapshot 和命中原因。

## 18. Validation and evaluation

### 18.1 Deterministic tests

1. 三次以上 compaction 后，初始 active constraint 仍存在；
2. 后续用户明确替代旧 constraint 后，只保留新 constraint，旧项可审计但不再注入；
3. 第一条消息为 200K pasted corpus，anchor 只保留 active spans + artifact ref；
4. 第一条消息含 image/file，portable resume 仍有 attachment ref；
5. whole-turn selector 不拆 tool call/result、approval、queued input；
6. source cursor 在 candidate 生成中变化时 compare-and-publish 失败；
7. empty、inflated、malformed、fabricated source ref summary 全部被拒；
8. raw JSONL、artifact 和 V2/V3 resolver 在 crash point 下保持一致；
9. map/tool order/timestamp/environment 变化分别产生预期 cache mutation reason；
10. provider switch 时 native carrier 被拒，portable checkpoint 正常继续。
11. mock provider 证明摘要请求的旧 request prefix canonical bytes 原样保留，且只在末尾追加 instruction；
12. 摘要请求不执行 client tool、移除 hosted tool，任何 tool-call chunk 都拒绝；
13. 摘要 usage 以 semantic purpose 持久化，不覆盖最近正常 turn usage；
14. manual 摘要失败不静默降级，overflow emergency 的确定性 fallback 有显式 audit；
15. 模型引用 catalog 外 event、试图伪造授权/完成/验证或在生成期间发生 source cursor drift 时均拒绝；
16. repeated compaction 不把上一次 model narrative 当 authority source。

### 18.2 Economics simulations

每个 provider price shape 至少覆盖：

- 0、1、2、3、10 个 future turns；
- 0%、50%、90%、99% observed cache hit；
- cache write 价格高于普通 input；
- 5m TTL 过期和 1h TTL break-even；
- semantic compact 调用本身 cache hit / miss；
- shrink-only、portable compact、native compact 三个方案；
- 当前 fit、不 fit、只差一个 tool-heavy turn 三种压力。

### 18.3 Provider contract tests

- DeepSeek：连续三 turn exact prefix 的 hit/miss usage；
- OpenAI：logical breakpoint、`prompt_cache_key`、stateless item round-trip、native compact fallback；
- Anthropic：四 breakpoint 限制、20-block lookback、mixed TTL 顺序、native compaction block replay；
- Gemini：当前 GenerateContent adapter 覆盖 implicit usage、canonical stateless replay 和
  `stateful_continuation = None` 的 fail-closed contract；Interactions adapter 落地后，必须再覆盖
  stateful/stateless parity、interaction-scoped 参数重发和 server handle expiry fallback；
- compatible proxy：缺 capability 时绝不发送 vendor-only fields。

这里的 Gemini Interactions/stateful 和 provider-native carrier 同属 R57.6 exact-route resume 范围；
不把尚未存在的 transport capability 伪装为 portable V3 已交付能力。

real-provider tests 必须只在 opt-in/secret gate 下运行并设置成本上限。

DeepSeek 的真实 cache smoke 以 ignored test 交付；默认 gate 不触网。发布负责人只有在同时设置
`SIGIL_REAL_PROVIDER_CACHE_CONFORMANCE=1`、`SIGIL_API_KEY` 和
`SIGIL_REAL_PROVIDER_MAX_COST_USD` 后才能显式运行
`cargo test -p sigil-provider-deepseek real_provider_three_exact_prefix_turns_report_cache_hit_and_miss_usage -- --ignored --exact`。
用例固定为三次请求、64K conservative input reservation、每次最多 32 output token，并从受信 pricing
snapshot 计算本地 admission；该值不是 provider 侧账单硬上限，因此不应在普通 CI 或开发 gate 中运行。

### 18.4 Quality evals

eval corpus 必须包含：

- 长编码任务中反复“继续”；
- 初始 `不要提交`，后续改成 `可以提交但不要 push`；
- 路径、版本、issue/PR ID 等精确实体；
- 失败尝试和已验证成功结果同时存在；
- 大 test log、大 search result、多图片；
- 中途切 model/provider；
- 用户恢复一周前 session；
- prompt injection 试图让 summary 伪造 approval/completion。

核心指标：

| Metric | Target direction |
| --- | --- |
| active constraint retention after 3 compactions | 100% |
| superseded constraint leakage | 0 |
| unsupported completion/verification claims | 0 |
| cache read ratio within an epoch | 上升 |
| semantic epoch resets per 100 turns | 下降 |
| effective input cost per useful turn | 下降 |
| break-even prediction error | 可校准且持续下降 |
| overflow recovery success without duplicate consumption | 上升 |
| post-compact task-quality regression | 不劣于 V2 baseline |

## 19. Ownership map

| Owner | Responsibilities | Must not own |
| --- | --- | --- |
| `sigil-kernel::session` | epoch lifecycle、anchor/checkpoint schema、fold/tail plan、resolver、append-only events、source validation | vendor cache/native wire fields |
| `sigil-kernel::task_memory` / Intent projection | 从既有 Task/Intent/control truth 提供 derived active state | 让 compaction 创建 Task/Intent authority |
| `sigil-runtime` | route capability、pricing snapshot、tokenizer/forecast selection、feature gates | durable session truth |
| `sigil-provider-deepseek` | exact serialized prefix、hit/miss usage mapping | 公共 DeepSeek 专属类型 |
| `sigil-provider-openai-responses` | cache key/breakpoint、Responses item/native carrier、usage mapping | portable continuity truth |
| `sigil-provider-anthropic` | cache control/TTL、native compaction block、usage mapping | kernel compaction policy |
| Gemini/OpenAI-compatible provider crates | implicit/stateful/cache-object adapter 和保守 capability | 按 model 名猜 capability |
| `sigil-tools-builtin` + artifact owner | recoverable tool-result artifact、preview/redaction/re-read | 修改当前 epoch 的 cache identity 而不报告 |
| `sigil-tui` | preview、confirm、status、manual recovery | 重新实现 admission 或 authority |
| `sigil-http` / `sigil-desktop` / `apps/desktop` | bounded typed DTO/event rendering | raw provider payload、opaque carrier、artifact path |

首个 implementation slice 应优先修改现有 `portable_compaction`、`compaction_sidecar`、
`context_projection` 和相关 tests；不能另建一条绕过 V2 resolver 的 parallel compact path。

## 20. Rollout plan

### R57.1 Telemetry and cache-shape proof

- 只增加 logical segment hash、actual cache usage 和 mutation reason；
- 不改变当前 compact behavior；
- 建立 provider/model pricing snapshot 和 effective input cost dashboard。

Exit criteria：能解释一次 cache miss 是 system、tool schema、old-message rewrite、TTL 还是 route change。

### R57.2 Continuity correctness

- 引入 `SessionAnchorV1` 和 constraint supersession ledger；
- repeated compaction 从 previous active checkpoint + durable delta materialize；
- 增加三次压缩、约束替代、large-first-message tests；
- 仍使用 portable checkpoint，不开启 native carrier。

Exit criteria：连续性 eval 通过，且 summary 不能改变 authority-bearing fields。

### R57.2B Cache-preserving LLM semantic summary

- 用同一 current route 和旧 epoch frozen request 发起一次额外摘要调用；
- 最后一条 instruction 携带 strict schema 和 closed source index；
- 引入 semantic physical-attempt、独立 usage 分类、严格 parser 和 tool-call rejection；
- manual/cost-only/emergency 采用不同且显式的失败策略；
- provisional economics 在调用前阻止明显亏损请求，observed economics 在调用后决定是否激活。

Exit criteria：mock contract test 证明旧 prefix 未重写、摘要 usage 被计入真实成本、伪造 source/authority
不能激活 checkpoint，且默认路径没有第二次摘要请求。

### R57.3 Adaptive tail and recoverable tool output

- `tail_messages` 翻译为 whole-turn token policy；
- 引入 durable artifact/shrink candidate；
- standalone shrink 显式创建 epoch；
- TUI preview 展示 protected tail 和 artifact refs。

Exit criteria：tool-heavy active turn 不被拆，artifact 在 resume/fork 可恢复。

### R57.4 Economics admission

- 引入 `CompactionEconomicsV1`、forecast 和 circuit breaker；
- fixed hard ratio 降级为 fallback；
- shadow mode 对比 V2 decision，达到稳定后再控制 automatic activation。

Exit criteria：至少在 DeepSeek、OpenAI、Anthropic 三种价格形状上证明不会因提前 reset 系统性增费。

### R57.5 Explicit cache adapters

- OpenAI/Anthropic logical breakpoints；
- Gemini/DeepSeek implicit telemetry 校准；
- tool schema canonicalization 和 dynamic overlay regression gate。

Exit criteria：real-provider conformance 能观察到预期 cache hit，且 vendor field 不泄漏 kernel。

### R57.6 Native compaction carriers

- OpenAI Responses、Anthropic beta 分别 feature-gated；
- portable/native dual-write；
- model switch、expiry、ZDR/store policy fallback。
- `compaction.native_carrier_enabled = false` 为默认值；
- materialization 与 artifact validation 已具备，但产品路径保持 fail-closed；在 carrier 能按相同
  route/source cursor 接回下一次请求前，显式开启也不得发起额外计费请求。

Exit criteria：实现并验证 exact-route resume；删除 native carrier 后仍可从 portable checkpoint
无模型调用恢复。当前此阶段明确 deferred，不计入 V3 portable compact 的完成条件。

### R57.7 Default flip and legacy cleanup

- `strategy = "cache_aware_v3"` 成为默认；
- legacy `tail_messages`、soft/hard ratio 继续读但提示迁移；
- V2 session 保持 replay，禁止 destructive migration；
- 完成 TUI/Desktop/serve DTO 和文档同步。

## 21. Rejected alternatives

### 21.1 Always preserve the first user message verbatim

拒绝。它能缓解第二次 compaction 丢根任务，却不能处理后续 supersession、大粘贴内容、附件和第一条消息
本来就不是长期目标的情况。V3 保留 active source spans，而不是保留消息位置。

### 21.2 Preserve every user message and every previous digest

拒绝。它类似 DeepSeek Reasonix 当前的 deterministic floor，短期连续性强，但会累积已过期/冲突指令，
稳定 prefix 仍不断膨胀，且没有明确 authority resolution。

### 21.3 Compact at a fixed 50% or 80%

拒绝作为主 admission。百分比适合 observation/fallback，但无法反映 output reserve、tool growth、模型
context、cache hit discount、write premium 和预计剩余 turn。

### 21.4 Prune large tool outputs as soon as possible

拒绝。早期 old-message mutation 可能摧毁一个价值很高的 prefix cache。先准备 candidate，在同一 epoch
rotation 一并应用；只有独立经济性成立才单独 reset。

### 21.5 Trust provider-native compaction only

拒绝。opaque/native state 缺少跨 provider portability，也无法单独满足 Sigil 的 append-only audit、
fork、offline replay、verification 和 trust contract。

### 21.6 Send background pings to keep caches warm

拒绝。它把推测中的未来收益变成确定的额外请求、计费、rate-limit、隐私与失败面。只让真实 turn 建 cache。

### 21.7 Use a cheaper separate summarizer by default

拒绝作为默认。把 transcript 发送给另一 route 通常失去旧 prefix cache，还引入能力、隐私和语义差异。
只有 `C_rotate(N)` 明确更低且 connection policy 允许时，才可作为经过 admission 的 provider-local option。

## 22. Initial implementation decisions

以下决定冻结首版实现边界；后续若要放宽，必须以兼容 schema 或独立 RFC 演进：

1. constraint supersession 只接受既有 durable accepted Intent、显式 user-control authority 或用户确认的
   candidate。模型输出本身不能替代或撤销约束；没有可证明 supersession 时旧约束继续 active。
2. R57.3 首版复用当前 durable transcript、compaction sidecar 和受保护 mutation/session artifact
   能力；在通用 artifact export/delete/encryption contract 闭合前，不宣称外部 artifact lifecycle
   已完整覆盖。
3. 64K 是普通 tail target cap，不是 active-turn hard cap。完整 active turn 可扩展到 exact fit limit；
   多图片或 provider-native state 另计 provider state/safety budget。
4. 低 confidence economics 不触发自动 cost-only compact。fit-required 可以自动进入安全 admission；
   cost-only candidate 先进入 preview/confirmation。
5. 没有受信任 pricing snapshot 时，自动 compact 只允许 fit-required；token-equivalent heuristic 只用于
   manual preview，不升级为自动成本结论。
6. 首版不合并 provider-native summary 和 portable narrative。portable checkpoint 是 deterministic
   truth；native carrier 只作为可丢弃加速层。
7. cache layout 的持久化 hash 使用独立、确定性、domain-separated SHA-256；现有 process-keyed request
   fingerprint 继续只用于单次进程内 request integrity，不能充当跨重启 cache-shape identity。
8. layout 未发生本地 mutation 但 provider 报告 miss 时，只记录
   `provider_miss_without_local_mutation`。TTL、eviction、provider routing/sharding 只能作为候选解释，
   不能作为已证明原因。
9. 正常 semantic compaction 必须成功完成一次额外 LLM 摘要调用；不再把空
   `ContinuationModelOutputV1` 当成正常成功。只有 fit-required / overflow emergency 可以在显式
   `deterministic_emergency_fallback` 记录下激活纯确定性 baseline。
10. 摘要调用默认使用当前同一 route/model，不创建子 agent、不生成可恢复子 session、不执行工具；
    OpenCode/Crush 式内部 agent wrapper 不是本设计所需的 durable agent abstraction。
11. 首版只做一次摘要调用。strict schema + deterministic validation 代替 Gemini CLI 式默认第二次
    verification probe；未来开启 probe 必须有独立 feature gate 和 economics admission。

## 23. Acceptance criteria

本 RFC 只有在以下条件全部满足后才能从 `proposed` 升为 `accepted`：

- repeated-compaction continuity bug 有独立 regression spec；
- `SessionAnchorV1` 与 RFC-0051 Intent、Task/control authority 的只读投影和 supersession contract
  冻结，且不会创建第二套 objective truth；
- cache layout、economics 和 epoch lifecycle 的 kernel/provider ownership 冻结；
- artifact/shrink recoverability 有明确 owner；
- DeepSeek、OpenAI、Anthropic、Gemini adapter capability matrix 通过 provider owner 复核；
- TUI preview/automatic behavior 通过产品复核；
- 正常 manual/automatic semantic compact 都有额外 LLM 摘要调用的 mock contract 覆盖，且
  emergency deterministic fallback 可见、可审计；
- rollout 每一阶段都有可单独回滚的 feature gate；
- 不把 RFC 中 2026-07-28 的 provider beta、价格或 TTL 当成永久 hard-code。

实施期间 V2 作为 production baseline，每个切片保持可单独回滚。R57.7 已完成 portable V3 默认翻转；
legacy `tail_messages`、ratio 和 V2 replay 继续可读，用于兼容与回滚，不做 destructive migration。

## 24. Implementation evidence

portable V3 于 2026-07-28 完成首版实现，交付边界如下：

| Slice | Delivered evidence |
| --- | --- |
| R57.1 | `cache_layout` 提供 domain-separated stable hash、五段 layout proof、mutation reason 和 provider-observed cache read/write/miss telemetry；普通 generation 与 semantic-compaction usage 分账。 |
| R57.2 | `ConversationContinuityV2` 从 durable authority、previous active ledger 与本次 delta materialize；三次压缩、constraint supersession、large-first-message、attachment ref、source drift 和 compare-and-publish 均有 regression coverage。 |
| R57.2B | 正常 compact 在同一 connection/provider/model route 发起一次额外 LLM 请求；frozen old request 保持原样前缀，只追加 strict instruction/source catalog；请求移除 hosted tools、没有 client-tool execution，tool-call output 与越权/source fabrication 均 fail closed；manual/idle 不静默降级，只有 fit-required emergency 可记录 deterministic fallback。 |
| R57.3 | recent tail 按 token 预算选择完整 turn group；大型 tool output 形成 recoverable artifact/shrink candidate；manual prepare 为 local-only，用户可独立选择 keep current、standalone shrink 或 full semantic summary，activation 仍是单独 CAS transition。 |
| R57.4 | economics engine 覆盖 2160 个 DeepSeek/OpenAI/Anthropic price-shape、cache-hit、TTL、future-turn、pressure 和 candidate-mode 组合；没有可信 pricing 时仅允许 fit-required 自动路径。 |
| R57.5 | DeepSeek/Gemini implicit usage、OpenAI Responses logical cache routing、Anthropic explicit breakpoint/TTL wire contract 和 generic compatible fail-closed capability 已落地；DeepSeek exact-prefix mock 与 budget-gated ignored real-provider smoke 已交付。Anthropic 首版仅发默认 5m；Gemini 当前 transport 仍是 stateless GenerateContent。 |
| R57.6 | native carrier schema、materialization 与 artifact validation 已具备，但 product request path 保持 `NATIVE_COMPACTION_RESUME_ENABLED = false` 且 fail closed。exact-route next-request resume、portable/native dual-write、expiry/ZDR fallback 和 Gemini Interactions transport 明确 deferred，不计入 portable V3 完成条件。 |
| R57.7 | `cache_aware_v3` 已成为默认；TUI、serve/OpenAPI、Desktop Rust IPC 与 React 消费同一 typed prepare/candidate/activation state；中英文配置、用户指南、changelog 与核心技术方案已同步。 |

验证证据：

- `cargo fmt --all --check`、`cargo check`、`cargo clippy --all-targets -- -D warnings` 与全 workspace
  `cargo test` 通过；
- Desktop OpenAPI generate/check、TypeScript typecheck 与 245 个前端测试通过；
- RFC 相关 10 个 Rust package 的 scoped coverage 为 line 82.52%、branch 81.70%、region 83.53%；
- real-provider cache conformance 保持 opt-in、secret gate 与预算 gate；本次默认验证未发起任何真实付费
  provider 请求，因此不把 ignored live smoke 误报为已执行。
