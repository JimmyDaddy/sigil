# RFC-0068：Durable Recovery Spine and Effect-Scoped Retry V1

状态：已实施（RFC-0069 扩展资格门禁）

创建日期：2026-08-21

> 实施说明（2026-08-22）：provider turn 的 durable recovery、effect-scoped retry、unknown-effect
> reconciliation 与 restart-safe projection 已落地。RFC-0069 在此基础上统一 recovery taxonomy、
> Plan materialization、workspace observation/CAS、execution segment 与 public-event outbox；其
> qualification gate 取代本 RFC 先前未拆分的跨 surface / fault-campaign 收口要求。

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- [RFC-0007 Task DAG and Isolated Agent Workflows](0007-task-dag-and-isolated-agent-workflows.md)
- [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0062 Harness-owned Tool Output Spooling and Result Conformance V1](0062-harness-owned-tool-output-spooling-and-result-conformance-v1.md)
- [RFC-0066 Durable Task Execution Contracts V2](0066-durable-task-execution-contracts-v2.md)
- [RFC-0067 Single Execution Spine and Monotonic Plan-to-Task Adoption V1](0067-single-execution-spine-and-monotonic-plan-to-task-adoption-v1.md)

## 1. 摘要

RFC-0067 解决的是：Plan 如何只经过一次原子 adoption，稳定地成为 durable Task。它没有完整解决：Task
内部已经执行了多个 step、tool 和 provider turn 后，一次瞬时 provider、transport、tool capture 或进程故障
应如何留在最小失败作用域内恢复。

当前实现已经具备 provider physical-attempt、request envelope、Task participant retry、step checkpoint、
Blocked/Paused phase、tool effect audit 和 workspace mutation frontier；但这些能力没有统一成一条恢复脊柱：

- provider turn 错误会先把当前 agent run 结算为 Failed；
- participant retry 主要按整个 step 是否 `SharedReadOnly` 判断，而不是按实际失败的 provider/tool effect 判断；
- write step 即使所有写入已经 durable settle，最后一次纯生成请求发生 TLS EOF，也无法在同一 child session 恢复；
- 未识别为现有 retry proof 的错误被投影为 step Failed，并级联取消依赖步骤；
- 进程重启只能修补悬空 Started，却不能继续已调度但未启动的 provider-turn recovery；
- 普通任务被拆成多个短命 child 和大量 provider request，放大了至少一次瞬时故障发生的概率。

本 RFC 冻结第二条核心脊柱：**Durable Recovery Spine（持久恢复脊柱）**。

```text
provider physical attempt
  -> typed failure observation
  -> effect-scoped recovery decision
  -> retry same logical provider turn
     | resume same participant frontier
     | reconcile uncertain effect
     | block/pause with typed action
     | fail only when irrecoverable
  -> step/task monotonic settlement
```

核心不变量：

> 可恢复的下层故障不能直接升级为上层 `Failed`。重试安全性必须由发生故障的 exact effect boundary 和
> durable evidence 判断，不能由整个 Task/step 是否曾经写入、用户 prompt 文案或错误字符串猜测。

本 RFC 不削弱 permission、approval、sandbox、egress、workspace confinement 或 unknown-outcome 的
fail-closed 边界。它要求安全边界保持不变，同时把“不能盲目重放”从“整个 Task 失败”修正为“在 exact
boundary 重试、观察、阻塞或等待用户处理”。

## 2. 问题陈述

### 2.1 当前系统是局部精确、全局 fail-stop

Sigil 各层都能准确写出自己的失败事实，但缺少一条统一传播规则：

```text
provider stream error
  -> Agent run Failed
  -> participant Failed
  -> step Failed
  -> dependent steps Cancelled
  -> Task Failed
```

该链路对 permanent contract violation 可能正确，对 TLS EOF、502、connection reset、短暂断网等瞬时故障
则过度终结。最终结果是：系统能解释“为什么失败”，却不能利用已经持久化的证据继续执行。

### 2.2 2026-08-20 dogfood failure

真实任务 `plan-task-593329e285cd5523004c6a78` 已经完成前三个提交批次。第四批执行期间，child 已完成
13 个 tool call，随后在新的 DeepSeek Messages generation request 上发生：

```text
peer closed connection without sending TLS close_notify
```

最后一个 `ProviderPhysicalAttemptTerminal` 的事实是：

```text
outcome = transport_outcome_uncertain
durable_output_event_ids = []
durable_side_effect_event_ids = []
```

也就是说，该失败 physical attempt 没有产生模型可见 durable output，也没有触发 tool/workspace/hosted
side effect；此前 13 个工具的结果已经在 child history 中正常结算。正确恢复应是从同一 child history
重建下一次 provider request，而不是重新运行此前工具，更不应失败整个 Task。

当前实现却立即追加：

```text
RunStatusChanged(Failed, provider_stream_error)
RunFinalized(Failed)
TaskParticipantFailed
TaskStepFailed
dependent steps Cancelled
TaskFailed
```

这个事件链证明当前安全判断使用了错误粒度：系统在问“这个 step 是不是写步骤”，而不是问“这次失败的
provider request 有没有未结算副作用”。

### 2.3 当前 retry 只覆盖窄路径

当前已实现的恢复主要覆盖：

- provider wire 前可证明的 connect failure；
- 429/cooldown；
- `ConfirmedNoModelConsumption`；
- shared-read-only step 的 `ProtocolRejectedAfterOutput`；
- admission before dispatch；
- participant replacement attempt。

这些路径有价值，但仍有三个结构性缺口：

1. provider turn 不能在 agent run 内 durable retry；
2. participant retry eligibility 由整个 step mode/isolation 限制；
3. recoverable failure 未被现有 proof 接住时，默认分支仍是 Failed，而不是 Blocked/Paused。

### 2.4 请求数量放大故障概率

将一个顺序型工作流拆成大量 child、短 step 和 provider request，会放大瞬时失败概率。假设单次请求成功率
为 99%，连续 78 次请求全部成功的概率约为 45.7%。这不是 provider 特例，而是串联系统的基本可靠性
问题。

安全审计、DAG 和 step contract 不应删除；但同 role、同 authority、同 isolation 的顺序步骤没有必要为
每个步骤重启完整 participant/provider lifecycle。确定性 verification/VCS 操作也不应反复交给模型重新
解释。

## 3. 与 RFC-0067 的关系

RFC-0067 定义 **Single Execution Spine**：

```text
Plan ready -> atomic adoption -> admission -> Task execution
```

本 RFC 定义 **Durable Recovery Spine**：

```text
Task execution fault -> classify -> retry/resume/reconcile/block/fail
```

两者互补，不互相替代：

- RFC-0067 保证 accepted Run 后 Task 不会消失；
- RFC-0068 保证 Task 内的 recoverable fault 不会错误终结整张 DAG；
- RFC-0067 的 adoption/Plan candidate/intent authority 不被修改；
- RFC-0068 只消费 adopted Task、participant、provider attempt、tool/effect 和 checkpoint 的 durable facts；
- RFC-0067 §11.3 禁止同一 logical attempt 的隐藏重放；本 RFC 的每次 retry 都是新的 durable physical
  attempt，并有显式 schedule/start/terminal，不是透明重放。

RFC-0067 的 implementation summary 已将 provider disconnect、tool error、disk pressure、session append
failure 和 process restart fault campaign 标为 qualification pending。本 RFC 把这些 qualification 项从
[测试清单](0067-implementation-summary.md)提升为正式恢复协议。

## 4. 设计目标

V1 必须达成：

1. 无 durable output、无 tool/hosted effect 的 transient provider failure 可以在同一 logical provider
   turn 内创建新的 durable physical attempt。
2. provider-turn retry 不重新运行此前已经 settled 的 tool call、workspace mutation、verification 或
   commit。
3. partial generation、tool result、hosted effect 和 workspace effect 使用不同恢复规则，不压成一个
   `retryable` bool。
4. provider/tool/participant 的 recoverable terminal 只能向上投影为 retry、Blocked 或 Paused，不能直接
   Task Failed。
5. retry budget 耗尽后保留 Task、accepted plan、completed steps、child session 和 downstream Pending
   状态。
6. process restart 能恢复 scheduled retry；若材料不可重建，则形成 typed blocker，不猜测或重发。
7. effect outcome uncertain 时先观察外部状态，不盲目重放。
8. 所有 retry/recovery 都有 durable identity、source attempt、request/effect digest、budget 和 terminal。
9. TUI、Desktop、HTTP 消费同一 recovery projection；普通用户不需要理解 attempt id 或内部 proof 名称。
10. 语义决定仍由模型通过 typed tool/schema 表达；host 不匹配用户 prompt、assistant prose 或本地化短语。
11. 同 role/authority/isolation 的顺序步骤可以复用 participant session，减少无必要 provider request 和
    context restart。
12. release gate 必须用真实 provider 与 fault injection 证明“最终完成或可行动阻塞”，而不是只证明
    event schema 可 replay。

## 5. 非目标

V1 不做：

- 不把所有错误都无限重试；
- 不在 provider adapter 内对已发送请求做隐藏 retry；
- 不把模型 generation 当作确认未计费；transport outcome uncertain 可能产生重复模型消费，必须计入预算；
- 不自动重放 generic shell、MCP mutation、provider-hosted tool、external API mutation 或未知写 effect；
- 不让 retry 绕过新的 provider request authorization、egress disclosure、budget、route lease 或 cancellation；
- 不修改已完成 step、tool result、ChangeSet、verification receipt 或 commit；
- 不从 error message string、HTTP body 文案或用户 prompt 推导恢复类别；provider 私有解析留在 provider
  crate，并输出 provider-neutral typed observation；
- 不把 `danger-full-access` 解释为允许 duplicate/unknown mutation；
- 不用一个依赖 kernel、provider、TUI、persistence 的“大 recovery manager”接管所有 owner；
- 不复制 Harness 的 everything-is-plugin 架构；
- 不承诺瞬时故障后永远自动完成。需要 credential、余额、网络恢复或 effect reconciliation 时，正确结果
  是可行动的 Blocked/Paused，而不是静默循环。

## 6. 竞品对照

本节基于 2026-08-21 的本地源码快照：

| 项目 | 本地 commit | 恢复边界 |
| --- | --- | --- |
| DeepSeek Harness | `47f943859bef` | agent step 内 durable retry turn |
| OpenAI Codex | `4808c162eeb7` | sampling request loop，必要时 WS -> HTTP fallback |
| Gemini CLI | `ae0a3aa7b928` | connection/mid-stream 分型重试 |
| OpenCode | `884c25603395` | session processor 内 provider-aware retry |

### 6.1 DeepSeek Harness

Harness 的
[`agent.step()`](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/core/agent-loop/src/agent.ts)
在 closed step 内循环构造 request；request error 交给 waterfall recovery，返回
`retry` 时继续同一 step。`@deepseek-ai/dsh-llm-retry` 的 normal policy 默认：

- `EMPTY_RESPONSE`、`RATE_LIMIT`、`SERVER`、`TIMEOUT`、`TRANSPORT` 最多重试两次；
- 500 ms 起始、10 秒上限、10% jitter；
- provider `Retry-After` 可替换本地 delay；
- retry schedule/start 都写 durable event；
- 每次 adapter 调用仍是独立 provider attempt；
- failed partial output 不进入 deriveMessages；
- retry request 从 durable surface history 重建；
- cancellation/disposal 会终止 backoff 并 drain recovery。

策略、事件与默认预算的源码锚点见
[`llm-retry`](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/llm/llm-retry/src/index.ts)。

应学习：retry 属于 provider step，不属于整个业务 Task；retry 本身可审计；失败 chunk 与 model-visible
history 分离。

不应复制：无限 always retry、everything-is-plugin、有限 sandbox 边界。

### 6.2 OpenAI Codex

Codex 在
[`run_sampling_request`](https://github.com/openai/codex/blob/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/core/src/session/turn.rs)
内维护 retry loop。retryable stream error 先消费 max retry/backoff；
WebSocket 路径耗尽后可以切换 HTTP transport，并重置 retry counter。UI 收到 `Reconnecting... n/max`，
而不是看到整条 thread 立即失败。

transport fallback 与用户可见 retry 通知见
[`responses_retry.rs`](https://github.com/openai/codex/blob/4808c162eeb767b389f13b7cb2730f32c8563dba/codex-rs/core/src/responses_retry.rs)。

应学习：transport fallback 仍在同一 sampling turn 内；retry 状态属于正式产品事件。

### 6.3 Gemini CLI

Gemini 在
[`geminiChat.ts`](https://github.com/google-gemini/gemini-cli/blob/ae0a3aa7b928cc73bb09604bb9c2c020e6b647db/packages/core/src/core/geminiChat.ts)
中将 connection phase 与 stream iteration phase 分开：连接阶段由通用 backoff 处理，mid-stream 的
SSL/API/invalid-stream error 使用独立有界预算，并发出 typed `RETRY` event。测试覆盖 SSL bad record、
ECONNRESET 和 premature stream closure；对应 fault coverage 见
[`geminiChat_network_retry.test.ts`](https://github.com/google-gemini/gemini-cli/blob/ae0a3aa7b928cc73bb09604bb9c2c020e6b647db/packages/core/src/core/geminiChat_network_retry.test.ts)。

应学习：连接失败、空响应、partial stream 和 content protocol failure 不能共享一个 retry bool。

### 6.4 OpenCode

OpenCode 在 session processor 内用
[`SessionRetry`](https://github.com/anomalyco/opencode/blob/884c256033958475be4feba69b7e6bf72caaf0ed/packages/opencode/src/session/retry.ts)
识别 5xx、SDK retryable error 和 Retry-After，
把 session status 投影为 typed retry，最终才进入 idle/error。

policy 的执行与 retry status 投影见
[`processor.ts`](https://github.com/anomalyco/opencode/blob/884c256033958475be4feba69b7e6bf72caaf0ed/packages/opencode/src/session/processor.ts)。

应学习：retry policy 简洁集中，产品面明确展示 next retry；不应复制 error body 字符串匹配，Sigil 必须
使用 typed provider error。

### 6.5 对照结论

竞品共同遵守：

1. transport/provider failure 首先在当前 generation turn 内消化；
2. retry 有界、可取消、可展示；
3. failed partial output 不污染下一请求；
4. 只有本地恢复预算耗尽或错误永久化后，才向上层报告停止；
5. tool/effect 与 provider generation 的 replay safety 分开判断。

Sigil 应在这些基础上增加竞品通常较弱的部分：append-only authority、effect evidence、cross-restart
schedule、Task DAG 和 world-state reconciliation。

## 7. 核心不变量

### 7.1 Failure containment

每层只能向上返回以下 disposition 之一：

```text
Recovered
RetryScheduled
Blocked
Paused
Irrecoverable
Cancelled
Interrupted
```

只有 `Irrecoverable` 可以映射上层 `Failed`。`RetryScheduled`、`Blocked`、`Paused` 不得经 generic error
分支降级为 Failed。

### 7.2 Effect-scoped safety

是否允许 retry 由以下 exact facts共同决定：

- failure 所属 physical attempt/tool call；
- request/effect material digest；
- durable model output 是否已 commit；
- tool call/result 是否已完整配对；
- local/network/hosted/workspace effect 是否存在或 outcome uncertain；
- retry request 是否可从 durable frontier 重建；
- route/config/tool schema 是否仍匹配；
- root cancellation 和 retry/cost budget 是否允许。

“当前 step 是 Write”不是禁止重试 provider generation 的充分条件；“当前 step 是 Read”也不是自动重放
任意 hosted/network effect 的充分条件。

### 7.3 One terminal owner per layer

沿用 RFC-0067 §11.1：provider、tool、participant、step、Task 各有一个 terminal owner。新增 recovery
不会生成第二个并列 owner：

- provider turn owner 追加 retry schedule/start/attempt terminal；
- participant owner只在 provider-turn recovery exhausted/blocked 后结算 participant；
- Task orchestrator 只消费 participant recovery disposition；
- 产品面只投影 durable state，不反向修改 execution owner。

### 7.4 No hidden retry

每次 retry 必须：

1. 关闭旧 physical attempt；
2. durable append `ProviderTurnRecoveryScheduledV1`；
3. 等待可取消 backoff；
4. durable append `ProviderTurnRecoveryStartedV1`；
5. 重新取得 provider request/egress/budget/cancellation permit；
6. 创建新的 `ProviderPhysicalAttemptStarted`；
7. 以新的 terminal 收口。

不得在旧 physical attempt id、旧 authorization 或旧 route lease 上再次发送 bytes。

### 7.5 Monotonic progress

- 已完成 Task step 不回退；
- 已 settled tool call 不重放；
- child session history 只 append；
- failed generation 的审计事实保留，但只有 committed surface output 进入 deriveMessages；
- retry ordinal 单调增加；
- recovery budget 只减少，不因重启或 route replacement 重置；
- downstream step 在 recoverable blocker 下保持 Pending，不改成 Cancelled；
- terminal Task 不自动复活。

### 7.6 Prompt semantic boundary

Recovery supervisor 只读取 typed provider/tool/session/effect facts。它禁止读取：

- 用户 prompt；
- assistant reasoning/prose；
- 本地化 UI 文案；
- tool error 的自然语言 message；
- “继续”“重试”“网络错误”等关键词。

用户是否取消、切换 route、确认 reconciliation 或修改计划，由 typed command/tool/schema 表达；host 只
校验 durable authority。

## 8. 失败与证据模型

### 8.1 Provider failure observation

Provider adapter 负责将私有协议错误映射为 provider-neutral observation：

```rust
pub enum ProviderFailureClassV1 {
    RejectedBeforeDispatch,
    RateLimited,
    TransientServer,
    TransportInterrupted,
    StreamEndedUnexpectedly,
    ProtocolViolation,
    ContextCapacity,
    Authentication,
    BillingOrQuota,
    RouteUnavailable,
    PermanentRequest,
    Cancelled,
}

pub struct ProviderFailureObservationV1 {
    pub class: ProviderFailureClassV1,
    pub retry_after_ms: Option<u64>,
    pub wire_state: ProviderWireStateV1,
    pub provider_retry_hint: ProviderRetryHintV1,
    pub safe_diagnostic_code: String,
}
```

`ProviderWireStateV1` 至少区分：

```text
NoBytesSent
RequestBytesMayHaveBeenSent
ResponseStarted
```

provider-specific HTTP status、error body、TLS/SDK error 的解释保留在 provider crate；kernel 不出现
DeepSeek、Anthropic、Gemini 或 OpenAI 私有字段。

### 8.2 Durable recovery evidence

Kernel 使用 physical-attempt projection 生成：

```rust
pub struct ProviderTurnRecoveryEvidenceV1 {
    pub logical_run_id: String,
    pub failed_physical_attempt_id: ProviderPhysicalAttemptId,
    pub request_material_fingerprint: String,
    pub request_envelope_digest: String,
    pub source_frontier: DurableFrontierV1,
    pub failure: ProviderFailureObservationV1,
    pub output_state: ProviderOutputStateV1,
    pub local_tool_effect_state: EffectSettlementStateV1,
    pub hosted_effect_state: EffectSettlementStateV1,
    pub request_reconstruction: RequestReconstructionDispositionV1,
}
```

`ProviderOutputStateV1`：

```text
None
TransientOnly
DurableSurfaceCommitted
```

`EffectSettlementStateV1`：

```text
None
Settled
OutcomeUncertain
```

这些字段由 event/result identity 推导，不由调用方传入任意 bool。已有
`ProviderRequestEnvelopeV1::verify_reconstructed_request_at_frontier` 是 reconstruction proof 的基础。

### 8.3 Recovery disposition

统一纯决策函数：

```rust
pub enum RecoveryDispositionV1 {
    RetryProviderTurn(ProviderTurnRetryPlanV1),
    ResumeParticipant(ParticipantResumePlanV1),
    ReconcileEffect(EffectReconciliationPlanV1),
    Block(RecoveryBlockerV1),
    Pause(RecoveryPauseV1),
    Irrecoverable(RecoveryFailureV1),
}
```

```rust
pub trait RecoveryPolicyV1: Send + Sync {
    fn decide(
        &self,
        evidence: &ProviderTurnRecoveryEvidenceV1,
        budget: &RecoveryBudgetProjectionV1,
        cancellation: &RunCancellationHandle,
    ) -> Result<RecoveryDispositionV1>;
}
```

该 policy 是无 I/O 纯函数。它不执行 provider、tool、workspace read 或持久化；原 owner 根据 disposition
执行并 append event。这样不会形成新的“大总管”。

## 9. Provider-turn recovery

### 9.1 Logical turn 与 physical attempt

一个 agent provider generation 定义为稳定 `logical_run_id` 下的 logical turn。它可以包含多个 physical
attempt：

```text
logical provider turn T
  attempt T.1 -> transport uncertain
  recovery scheduled #1
  attempt T.2 -> 503
  recovery scheduled #2
  attempt T.3 -> completed
```

所有 attempt 必须绑定：

- 同一 provider/model semantic route；
- 同一 canonical request envelope；
- 同一 source durable frontier；
- 同一 tool schema/system/conversation/dynamic material hash；
- 独立 authorization、egress disclosure、route lease、physical attempt id 和 usage。

任何 request material 漂移都会停止自动 retry，进入 `Blocked(request_material_changed)`；不能把变化后的
请求冒充同一 retry。

### 9.2 V1 自动 retry matrix

| 条件 | 自动动作 |
| --- | --- |
| `NoBytesSent` + confirmed rejection + zero output/effect | retry same logical turn |
| 429 + typed Retry-After + zero committed output/effect | 等待后 retry |
| 5xx/timeout/TLS EOF/connection reset + no durable output + no local/hosted effect + reconstructable | 新 physical attempt |
| previous tool results fully settled，当前 generation 自身无新 effect | 新 physical attempt；不重放 tools |
| transient partial text only，未 durable commit，zero tool/hosted effect | V1.1 discard marker 后最多 retry 一次 |
| durable surface output 已 commit | 不自动 retry；Block/continue through explicit recovery |
| provider-hosted tool request bytes may have been sent | 不自动 retry；Reconcile/Blocked |
| tool/workspace/network mutation outcome uncertain | 不自动 retry；Reconcile/Blocked |
| auth/credential/billing/quota | Blocked，提供 typed action |
| context capacity | 进入现有 compaction/fit proof；无 proof 则 Blocked |
| permanent request/protocol incompatibility | route fallback 或 Blocked；不级联 Task Failed |
| root cancellation | 停止创建新 attempt，按 cancellation contract 收口 |

“transport outcome uncertain”不等于“确认没有 provider 消费”。普通 generation 的 bounded reissue 允许
产生重复 token/cost，但必须满足 zero world-state effect、预算和用户配置；telemetry 必须记录 duplicate
consumption risk。它不能复用 `ProviderConfirmedNoConsumption` 这一更强 proof 名称。

### 9.3 Partial output

V1.0 只强制实现 `output_state = None` 的恢复。V1.1 才允许 `TransientOnly`：

- stream delta 可以 live 展示，但在 assistant message terminal 前不成为 model-visible durable history；
- failure append `ProviderGenerationDiscardedV1`，绑定 chunk/event range 和 digest；
- derived messages 排除 discarded generation；
- UI 将旧 live fragment 标记为“连接中断，已重新生成”，不能与新结果拼接；
- 已出现 tool call、hosted evidence、external citation 或 durable assistant message 后禁止该路径；
- append-only audit 保留旧 chunk，不删除或覆盖。

### 9.4 Retry budget

默认 normal profile：

```text
transport/server retries: 2
partial-output retry:     1
initial delay:            500 ms
max delay:                10 s
jitter:                   10%
rate-limit total wait:    沿用现有 120 s 上限
```

预算按 logical turn 和 root Task tree 同时计数：

- attempt count；
- cumulative delay；
- provider request count；
- estimated duplicate input cost；
- root cancellation；
- existing web/provider budget。

预算状态必须 durable；process restart、participant restore 或 route-pressure registry 重建不能把 attempt ordinal
清零。高级策略留在配置/doctor，不增加默认 TUI 主流程开关。

### 9.5 Transport fallback

若 runtime route profile 明确声明多个等价 transport，例如同一官方 provider 的 WebSocket 和 HTTPS：

- fallback 只能在同 provider/model/tenant/semantic request identity 内发生；
- fallback transport 必须重新取得 authorization 和 budget；
- `ProviderTurnTransportFallbackSelectedV1` durable 后才能 dispatch；
- endpoint/tenant/protocol semantic fingerprint 不等价时不是 fallback，而是 route rebind，需要 Blocked/typed
  user action；
- provider 私有 fallback 规则留在 provider/runtime，不进入 kernel 枚举。

V1 不要求 DeepSeek route 实现 transport fallback；bounded new HTTPS physical attempt 已能覆盖当前故障。

## 10. Agent run 与 participant continuity

### 10.1 Provider error 不能先终结 Agent run

当前 agent loop 在 `collect_provider_turn` 返回 error 后立即 append `RunStatusChanged(Failed)` 和
`RunFinalized(Failed)`。新顺序必须是：

```text
collect provider attempt error
  -> close physical attempt
  -> derive recovery evidence
  -> RecoveryPolicyV1::decide
  -> retry/backoff OR participant blocker/failure
  -> only then append agent run terminal
```

当 disposition 是 `RetryProviderTurn` 时，agent run 保持 Running；pending joined-result delivery、tool result
pairing、usage 和 current request frontier 保持不变。

### 10.2 Same child session, same participant attempt

provider-turn retry 使用当前 child session：

- 不创建新的 participant attempt；
- 不重发 child objective/step prompt；
- 不重跑 settled tools；
- 不重置 max-turn/no-progress/approval budget；
- 不改变 workspace invocation grant；
- 只重建失败 generation request。

只有 agent process 已结束、需要跨重启恢复，或者 recovery material 无法在当前 run 内继续时，才创建更高
participant continuation ordinal。该 continuation 仍引用同一 child session ref 和 last safe frontier，不新建
空 child session。

### 10.3 Participant replacement 与 continuation 分离

保留现有 `TaskParticipantRetryScheduled`，但收窄职责：

- admission-before-dispatch、route drift 或 child session 无法继续时创建 replacement participant；
- provider turn 内可恢复错误不再升级到 participant replacement；
- replacement 必须引用 prior child ref 和 continuation frontier；
- 只有 exact retry-stable input、route、step contract 和 effect evidence匹配时才能继续；
- 已完成 tool/step evidence通过 child history恢复，不重新执行。

### 10.4 Final synthesis

Task synthesis 也是普通 logical provider turn，使用同一恢复策略。TLS EOF、5xx 或 timeout 不能因为“这是最后
一步”绕过 retry。只有 final answer durable commit 后才允许 `Task Completed`。

## 11. Tool/effect recovery

### 11.1 Runtime-only replay contract

每个 tool runtime contract 增加不进入 provider-visible `ToolSpec` 的字段：

```rust
pub enum ToolReplayClassV1 {
    PureRead,
    Idempotent,
    Reconciliable,
    NonReplayable,
}

pub struct ToolReplayContractV1 {
    pub class: ToolReplayClassV1,
    pub idempotency_key_kind: Option<String>,
    pub reconciliation_probe_kind: Option<String>,
}
```

该 contract 必须进入 registry runtime contract fingerprint，但不能进入 provider tool-schema hash，避免 wire
未变化却误报 `ToolSchemaChanged`。

默认 `NonReplayable`。只有工具 owner 显式声明并有测试时才能放宽。

### 11.2 分类规则

- 文件读取、list/glob/grep、纯 symbol query：`PureRead`；
- prepared file mutation：不自动 replay；先根据 expected content hash/ChangeSet evidence reconcile；
- VCS commit：使用 expected parent/tree/message digest 形成 idempotency/reconciliation proof；
- verification command：若 exact CheckSpec 证明只读，可作为 host-owned idempotent job；
- generic Bash/terminal/MCP/Agent/Web mutation：默认 `NonReplayable`；
- provider-hosted tool：独立 hosted effect，request bytes may have been sent 后默认 `NonReplayable`；
- tool output capture/storage failure不等于 tool body failure；body 必须 drain，capture 标记 unavailable，不能
  因 artifact writer 失败重跑命令。

### 11.3 Unknown outcome

对于 `OutcomeUncertain`：

```text
effect attempt terminal uncertain
  -> append EffectReconciliationRequiredV1
  -> run typed observation probe with separate authority
  -> ObservedApplied | ObservedNotApplied | StillUncertain
```

- `ObservedApplied`：补 terminal/result/checkpoint，不重放；
- `ObservedNotApplied`：只有 replay contract 允许时创建新 effect attempt；
- `StillUncertain`：step Blocked，Task Paused/Blocked，用户可 review/cancel/reconcile；
- 观察 probe 只能读取 world state，不能顺便修复；
- reconciliation result 绑定 exact effect digest、workspace snapshot/frontier 和 probe receipt。

## 12. Step 与 Task failure propagation

### 12.1 Step terminal semantics

`TaskStepStatus` 保留现有枚举，但严格解释：

- `Blocked`：存在可解决 blocker、retry budget exhausted、需要 reconciliation 或环境修复；
- `Interrupted`：process/cancellation/owner loss，恢复时需要显式 continuation；
- `Failed`：step contract 在当前 plan version 下不可恢复、hard safety violation、reconciliation 证明不可逆错误
  或显式 repair policy 已确定失败；
- `Cancelled`：用户/Task 明确取消，不能由普通 dependent failure 自动滥用；
- `Completed`：acceptance/check/result evidence 完整。

### 12.2 Dependency behavior

前置 step：

- `Blocked` / `Interrupted`：依赖步骤保持 `Pending`；
- retry scheduled：当前 step 保持 Running 或进入专门 recovery projection，不产生 step terminal；
- `Failed`：依赖步骤可以投影 `Blocked(upstream_failed)`；只有整个 Task 明确取消时才标记 Cancelled；
- `Cancelled`：依赖步骤根据 Task cancellation scope 收口。

用 `Blocked(upstream_failed)` 代替“父步骤 Failed 就把全部 downstream Cancelled”，能保留 replan/recovery 和
审计语义。

### 12.3 Task terminal semantics

`TaskExecutionPhaseV1::Failed` 只允许：

- accepted plan/step contract 结构损坏；
- hard safety invariant 被破坏；
- effect reconciliation 证明不可逆错误且 plan 无合法 repair；
- final verification 证明目标不可满足，且模型/用户明确结束 repair；
- current-schema/session authority corruption；
- 用户通过 typed action 选择“结束并标记失败”。

以下情况不得直接 Task Failed：

- TLS/HTTP/timeout/5xx；
- retry budget exhausted；
- provider credential/余额/配额不可用；
- route cooldown；
- disk pressure/artifact/session reserve；
- approval pending/denied but alternate path exists；
- tool NotFound/invalid input；
- process restart；
- external writer active；
- recoverable verification failure；
- child result delivery中断。

这些情况进入 Blocked/Paused，并包含 typed actions。

## 13. Durable event contract

### 13.1 新事件

```rust
ProviderTurnRecoveryScheduledV1 {
    recovery_id,
    logical_run_id,
    failed_physical_attempt_id,
    next_physical_attempt_ordinal,
    request_envelope_digest,
    source_frontier,
    failure_class,
    not_before_unix_ms,
    retry_after_ms,
    budget_snapshot,
    recovery_policy_fingerprint,
}

ProviderTurnRecoveryStartedV1 {
    recovery_id,
    logical_run_id,
    physical_attempt_id,
    started_at_unix_ms,
}

ProviderTurnRecoveryExhaustedV1 {
    logical_run_id,
    last_physical_attempt_id,
    reason_code,
    budget_snapshot,
    terminal_disposition,
}

ProviderGenerationDiscardedV1 {
    logical_run_id,
    physical_attempt_id,
    output_event_range_digest,
    reason_code,
}

EffectReconciliationRequiredV1 { ... }
EffectReconciliationTerminalV1 { ... }
```

这些是 recovery-critical direct JSON durable events，不使用自由文本 `Note`。

### 13.2 Event invariants

1. Schedule 只能引用一个已 terminal 的 failed physical attempt。
2. 一个 schedule 最多对应一个 Started。
3. Started 的 physical attempt id 必须与紧随其后的 `ProviderPhysicalAttemptStarted` 一致。
4. retry ordinal 对 logical turn 单调增加。
5. request envelope、route、provider/model、source frontier 必须完全匹配。
6. terminal attempt 成功后不得再出现新 schedule。
7. cancellation 在 backoff 期间胜出时，不写 RecoveryStarted。
8. restart repair 不得为同一 schedule 启动两个 attempt。
9. `ProviderGenerationDiscardedV1` 只可引用未 durable surface commit、无 effect 的 attempt。
10. recovery/exhausted/blocker 不能包含 raw prompt、secret、provider body、URL query 或 tool output body。

### 13.3 Projection

新增 provider-turn recovery projection：

```text
Idle
Running(attempt n)
Waiting(recovery id, not_before)
Recovering(attempt n+1)
Succeeded
Blocked(reason/actions)
Interrupted
```

这是 provider turn 的投影，不增加新的 Task 顶层状态。Task 继续使用 RFC-0067 的 execution phase。

## 14. Restart recovery

### 14.1 Scheduled but not started

startup reducer 发现 durable Schedule、没有 Started 时：

- 校验 schedule identity、预算、not-before 和 source attempt terminal；
- 校验 request envelope 可从 durable frontier 重建；
- 校验不存在 process-local-only overlay、hosted carrier 或失效 authorization；
- 到期后由唯一 owner CAS claim；
- 重新执行 authorization/disclosure/budget，再创建 physical attempt。

无法重建时：

```text
Blocked(recovery_material_unavailable)
```

不得从 assistant prose、旧 prompt 文本或 compaction summary猜请求。

### 14.2 Started without terminal

physical attempt Started 但无 terminal：

- append `Interrupted` 或 `TransportOutcomeUncertain` repair terminal；
- 若 wire state/hosted/effect 无法证明，默认 Blocked/Reconcile；
- 不自动假定 provider 未消费；
- 用户显式 resume 也只能通过 typed recovery policy，不能绕过 evidence。

### 14.3 Backoff 与 process restart

backoff deadline 使用 absolute `not_before_unix_ms`。重启后：

- deadline 未到：继续 Waiting；
- deadline 已到：立即进入 claim，不额外重复完整 delay；
- retry count/cumulative delay/cost 不重置；
- route config fingerprint 漂移：Blocked(route_rebind_required)；
- root Task 已取消/terminal：schedule 收口 Cancelled，不 dispatch。

### 14.4 Child continuation

participant child session reload 后，从以下 frontier 继续：

```text
last committed assistant/tool-result boundary
+ settled tool execution evidence
+ current provider-turn recovery state
```

不能创建空 child session并重发整个 step。若 child material已不可用，step Blocked，提供 replan/restart-step
typed action；potential mutation step 不自动从头执行。

## 15. 简化 Task execution

Recovery spine解决“故障不毒死 Task”；还需要减少不必要故障面。

### 15.1 Execution segment

runtime 可将满足以下条件的相邻、拓扑连续步骤派生为 `TaskExecutionSegmentV1`：

- 相同 role/profile/provider route；
- 相同 workspace isolation 和 invocation authority；
- 没有跨越独立 approval/integration/replan boundary；
- 前一步 terminal 后下一步可从同一 child session安全继续；
- 每一步仍有独立 durable start/result/checkpoint/terminal。

Segment 是 runtime 派生执行计划，不是新的 Plan authority；它不能合并、删除或改变用户批准的 step。

收益：

- child session/tool schema/system prefix 复用；
- 减少重新 materialize context 和 provider warm-up；
- step 之间保留模型工作记忆；
- crash 后从最后一个已 commit step继续；
- 降低总 provider request 和 child lifecycle 数量。

### 15.2 Host-owned deterministic jobs

以下工作在 Plan 已给出 exact typed contract 后，应优先由 runtime job 执行，而不是开启新的模型 participant：

- CheckSpec verification；
- workspace snapshot/status/diff inventory；
- typed VCS stage/commit observation与幂等确认；
- artifact/session/disk resource preflight；
- exact route/capability admission；
- checkpoint publish。

模型继续负责：

- 理解目标；
- 设计/修改代码；
- 解释 verification failure并修复；
- 判断 semantic readiness；
- 生成最终用户总结。

### 15.3 Plan 粒度

Plan 应描述用户可理解的语义阶段和依赖，不把每个 `git status`、`read_file`、格式检查或 provider turn 拆成
独立 step。Compiler/runtime 可以附加 typed CheckSpec 和 deterministic jobs，但不能把内部调度细节暴露为
用户计划标题。

## 16. Permission、egress 与安全

### 16.1 Retry 不复用旧 permission/effect permit

每个新 physical provider attempt 都重新取得：

- root cancellation forward-effect permit；
- provider route lease；
- request budget；
- hosted authorization/disclosure（如果存在）；
- current config/route fingerprint validation。

普通 provider generation retry 不重新请求已 settled local tool approval；因为工具没有重放。若模型在新
generation中提出新的 tool call，则该调用照常走新 permission plan/approval。

### 16.2 Hosted/network boundary

provider-hosted tool、MCP、WebFetch、OAuth 和其他 durable egress 遵守现有规则：request bytes 已可能发送后
不透明 retry。若 upper-layer 新建 attempt：

- 必须生成新的 authorization/disclosure/attempt id；
- 只有 effect evidence 明确允许才可 dispatch；
- hosted tool outcome uncertain 默认 Reconcile/Blocked；
- query、URL、credential/raw carrier 不进入 recovery event。

### 16.3 danger-full-access

`danger-full-access` 消除普通 Ask，不改变：

- replay safety；
- duplicate mutation guard；
- unknown-outcome reconciliation；
- root cancellation；
- protected target/hard deny；
- egress budget与 disclosure；
- registry/config/request identity校验。

## 17. 产品语义

### 17.1 用户可见状态

默认产品文案：

```text
正在重新连接 · 第 1/2 次
服务暂时不可用 · 8 秒后重试
任务已暂停 · 模型请求重试次数已用完
需要确认外部操作结果后才能继续
凭据不可用 · 更新连接设置后继续
```

不显示：

- `provider-attempt-...`；
- `transport_outcome_uncertain`；
- `TaskParticipantRetryProof`；
- `awaiting durable projection`；
- raw rustls/reqwest error chain 作为主标题。

内部 identity 和 safe diagnostic 放在 Audit/Doctor/details。

### 17.2 Actions

Blocked/Paused 提供由 durable state决定的 typed action：

```text
Retry now
Wait for scheduled retry
Change connection/model
Update credential
Review uncertain effect
Re-run verification
Replan remaining steps
Cancel task
```

产品 action 直接发送 typed command；不生成“请继续”“重试一下”自然语言 prompt，也不要求模型再次理解用户
按钮意图。

### 17.3 Surface parity

TUI/Desktop/HTTP DTO 至少包含：

- task/step semantic identity；
- recovery phase；
- safe reason code/summary；
- current/max attempt；
- next retry timestamp；
- available typed actions；
- whether user attention is required；
- detail/audit handle。

HTTP/CLI automation 可以选择等待 Blocked/Paused 或返回结构化 exit，不得只返回 generic 1。

## 18. 配置与策略

V1 不在普通设置页暴露 retry matrix。provider/runtime profile 使用 advanced config：

```toml
[recovery.provider]
max_transport_retries = 2
max_partial_output_retries = 1
initial_delay_ms = 500
max_delay_ms = 10000
jitter_ratio = 0.10
max_cumulative_delay_ms = 120000
```

约束：

- 数值有编译期 hard cap；
- `0` 显式关闭对应自动 retry，但 exhausted 仍进入 Blocked/Paused；
- 不允许 unlimited；
- provider-specific Retry-After/transport hint 只能收窄或在 hard cap 内替换；
- config fingerprint 进入 recovery policy fingerprint；
- retry schedule 后配置漂移不静默应用，新 attempt 前进入 Blocked/re-admit；
- TUI 主流程只展示当前恢复状态，不要求普通用户理解策略矩阵。

## 19. Telemetry、Doctor 与 SLO

最低指标：

```text
provider_turn_recovery_scheduled_total{class,provider,model}
provider_turn_recovery_succeeded_total{attempt_ordinal}
provider_turn_recovery_exhausted_total{class}
provider_turn_recovery_delay_ms
provider_turn_duplicate_consumption_risk_total
provider_turn_discarded_partial_output_total
effect_reconciliation_required_total{tool_class}
effect_reconciliation_outcome_total{applied,not_applied,uncertain}
task_blocked_by_recoverable_failure_total{reason}
task_failed_due_to_transient_provider_total        == 0
task_dependent_cancelled_by_recoverable_failure    == 0
duplicate_mutation_after_recovery_total            == 0
orphan_recovery_schedule_total                     == 0 after repair window
provider_requests_per_completed_task
child_sessions_per_completed_task
```

Doctor 检查：

- schedule/start/physical-attempt 配对；
- retry ordinal 和 budget；
- request envelope/frontier/route一致；
- success 后无多余 schedule；
- discarded partial没有进入 derived history；
- effect reconciliation closure；
- Blocked/Paused Task 保留 accepted plan/completed step；
- recoverable failure没有 downstream Cancelled；
- restart 后不存在双重 claim。

Release SLO：

1. transient provider fault 单独不能产生 Task Failed；
2. 每次 injected fault 最终必须是 Completed 或带行动建议的 Blocked/Paused；
3. duplicate workspace mutation/commit 为 0；
4. tool call/result pairing 缺口为 0；
5. silent failure/Task missing 为 0；
6. retry UI 状态在 schedule 后一个 frame/event 周期内可见。

## 20. Schema 与 cutover

### 20.1 Current-schema cutover

本 RFC 采用 current-schema clean cutover：

- 新 physical attempt recovery 必须写 V1 recovery event；
- 不通过缺字段默认值或 error string重建旧 proof；
- 旧 session 可以按原有 terminal语义只读展示，但不获得自动 retry authority；
- 明确 resume 旧 Blocked/Failed Task 时，新 attempt必须先建立 current recovery/admission binding；
- provider/runtime wire私有字段不进入 kernel schema。

### 20.2 Projection authority

- physical attempt terminal 是旧 attempt 事实；
- recovery schedule 是允许下一 attempt 的唯一 authority；
- recovery started 是 schedule 被消费的唯一事实；
- provider-turn success/exhausted/blocker 是 logical turn terminal；
- participant/step/Task 只消费 logical turn terminal，不自行扫描 error message。

## 21. 实施分期

### R68.0：Incident fixture 与 invariant companion

- 将 2026-08-20 TLS close failure 安全化为 deterministic replay fixture；
- 记录当前 provider error -> run/participant/step/task failure propagation；
- Doctor 增加“transient zero-effect attempt upgraded to Task Failed”诊断；
- 为 agent loop 增加 recovery decision spy，不改变生产语义。

退出条件：fixture 稳定复现当前错误级联，且所有 event/effect事实可验证。

### R68.1：Provider-turn durable retry

- 新增 typed provider failure observation；
- 新增 recovery evidence/policy/disposition；
- 新增 schedule/start/exhausted事件与 reducer；
- 在 agent run terminal 前执行 recovery decision；
- 实现 `output=None + effect=None + reconstructable` 的 bounded retry；
- UI/HTTP 显示 waiting/reconnecting。

退出条件：dogfood TLS fixture 在同一 participant内完成新 physical attempt，不重跑 13 个 settled tools。

### R68.2：Failure propagation correction

- participant 把 recovery exhausted映射为 Blocked/Paused；
- step Blocked/Interrupted不取消 downstream；
- Failed downstream投影为 Blocked(upstream_failed)；
- Task Failed只接受 §12.3 原因；
- final synthesis使用同一 provider-turn recovery。

退出条件：transient provider/route/resource failure不能产生 Task Failed；resume保持 completed steps。

### R68.3：Cross-restart recovery

- scheduled retry CAS claim；
- absolute not-before 和 durable budget；
- child session/frontier continuation；
- route/material漂移 blocker；
- Started-without-terminal repair与 hosted/uncertain guard。

退出条件：在 backoff、schedule/start之间、physical Started后分别 kill process，重启后不重复 dispatch/effect。

### R68.4：Effect replay/reconciliation contract

- tool runtime-only replay contract/fingerprint；
- prepared file/VCS/check effect observation probe；
- generic shell/MCP/hosted fail-closed；
- capture storage failure与body failure分离；
- reconciliation product actions。

退出条件：unknown write outcome从不盲目 replay；可证明 applied/not-applied时正确收口。

### R68.5：Execution surface reduction

- execution segment派生与同 child session续跑；
- host-owned CheckSpec/VCS/resource jobs；
- plan粒度审计；
- provider requests/child sessions telemetry；
- “简单提交任务”real E2E。

退出条件：相同计划完成语义与审计不变，同时显著减少 child/provider lifecycle；具体 release threshold以
R68.0 baseline 后冻结，不在无基线时伪造百分比。

### R68.6：Partial output 与 transport fallback

- discarded partial output sidecar；
- derived-message exclusion；
- live UI replacement；
- equivalent transport fallback contract；
- provider-specific qualification。

退出条件：mid-stream TLS/EOF retry不会拼接旧输出、污染cache history或重复tool effect。

### R68.7：Qualification

- real DeepSeek至少一次 6+ provider request任务；
- provider disconnect/429/5xx/TLS EOF/partial stream；
- tool error/approval deny/disk/session append/process restart；
- exact fixture连续20次；
- 单 transient fault fixture连续100次；
- TUI PTY、Desktop、HTTP contract；
- 外部 world-state验证。

退出条件：§23 全部 gate通过后才能标记 implemented。

## 22. 代码落点

建议职责：

| 模块 | 变更 |
| --- | --- |
| `sigil-kernel/src/provider_error.rs` | provider-neutral failure observation/hint |
| `sigil-kernel/src/session/provider_attempt.rs` | recovery evidence、schedule/start/exhausted domain event与projection |
| `sigil-kernel/src/provider_request_material.rs` | exact reconstruction/frontier verification |
| `sigil-kernel/src/agent/provider_stream.rs` | physical attempt terminal后调用 recovery policy；不隐藏 retry |
| `sigil-kernel/src/agent.rs` | recovery exhausted前不结算 RunFailed |
| `sigil-kernel/src/task.rs` | participant continuation/recovery projection和step/Task terminal约束 |
| `sigil-kernel/src/task_orchestrator/runner.rs` | recovery disposition传播、downstream Pending/Blocked语义 |
| `sigil-kernel/src/tool.rs` | runtime-only replay contract，不进入provider schema hash |
| `sigil-runtime/src/provider_pressure.rs` | retry/backoff/route pressure与hard budget装配 |
| `sigil-runtime/src/agent_supervisor/task_runner.rs` | same-child provider recovery、replacement职责收窄 |
| `sigil-runtime/src/doctor/session.rs` | recovery pairing、budget、duplicate effect和failure escalation审计 |
| `sigil-runtime` application service | typed retry/reconcile/replan/cancel action |
| `sigil-tui` | recovery ViewModel、waiting/blocked action；不显示内部id |
| `sigil-http` / `sigil-desktop` | provider-neutral recovery DTO和typed command parity |
| provider crates | 私有错误到`ProviderFailureObservationV1`映射和fixture |
| model eval / PTY / Desktop E2E | fault campaign、world-state和cross-surface qualification |

禁止创建同时持有 provider client、Task writer、tool registry、workspace和UI的 `RecoveryManager`。Domain type和
proof在kernel，provider私有分类在provider crate，policy装配和backoff在runtime，append由原owner执行，
renderer只消费projection。

## 23. 验收标准

### 23.1 Provider turn

1. TLS EOF/ECONNRESET/timeout/502/503 在 zero durable output/effect时创建新的 physical attempt。
2. 每次 retry有 schedule/start/attempt terminal完整配对。
3. retry request envelope、source frontier、route、tool schema和system/conversation hash一致。
4. retry不重新执行此前settled tool call。
5. retry成功后agent run继续，不留下RunFailed/RunFinalized(Failed)。
6. max retry耗尽后logical turn为Blocked/Paused，不是Task Failed。
7. Retry-After和cumulative delay/hard cap正确。
8. cancellation期间不启动新attempt，active attempt按现有quiescence收口。

### 23.2 Effect safety

9. generic shell/hosted/MCP/unknown mutation outcome uncertain时provider/tool自动重放次数为0。
10. prepared file mutation/VCS operation已应用时通过observation补terminal，不重复写/commit。
11. observed not-applied只有在replay contract允许时创建新effect attempt。
12. capture storage failure继续drain child并保留真实exit status，不重跑命令。
13. danger-full-access不绕过replay/effect guard。

### 23.3 Task continuity

14. provider transient failure不会把step标记Failed。
15. Blocked/Interrupted前置step的downstream保持Pending。
16. completed step、accepted plan、intent、verification receipt在retry/resume/compact后不变。
17. process restart后继续同child ref和safe frontier。
18. final synthesis断线可恢复；Task只有final answer/readiness durable后Completed。
19. terminal Task从active panel退出；Blocked/Paused保留action。

### 23.4 Product parity

20. TUI/Desktop/HTTP显示同一attempt count、next retry、reason和actions。
21. 普通界面不显示physical attempt id、proof enum或raw provider error。
22. keyboard/mouse/history scroll/resize/modal不触发recovery取消。
23. typed Retry/ChangeRoute/Reconcile/Replan/Cancel receipt幂等。

### 23.5 Fault campaign

24. 对2026-08-20 fixture在最后generation注入TLS close：Task最终完成且不重复前三个commit/13个tool call。
25. partial text后注入EOF：discarded内容不进入下一request或最终assistant message。
26. provider retry schedule后kill process：重启只启动一次replacement attempt。
27. physical Started后kill process：无proof时Blocked，不盲目重发。
28. hosted tool bytes发送后断线：Blocked/Reconcile，provider request自动重放为0。
29. exact failure fixture连续20次，无Task missing、silent failure、duplicate mutation、unpaired result。
30. 单次transient provider fault注入连续100次：100次均Completed或actionable Blocked/Paused；因transient
    provider直接Task Failed为0。

## 24. 被拒绝的方案

### 24.1 简单扩大 participant retry 到所有 write step

拒绝。它仍会从头重跑整个step和潜在mutation，只是把过严改成过松。正确方案是provider-turn/effect-scoped
recovery。

### 24.2 把 transport outcome uncertain 改成 confirmed no consumption

拒绝。TLS断线不能证明provider没有收到或计费。新设计承认duplicate consumption risk，只证明没有Sigil
durable output/world-state effect，并受独立预算约束。

### 24.3 所有错误都变成 Blocked

拒绝。contract corruption、hard safety、不可逆错误仍必须Failed。关键是typed taxonomy和传播规则，不是
永不失败。

### 24.4 删除 Task/DAG，完全复制 Harness loop

拒绝。会丢失Intent、step contract、capability、verification、isolation、cross-surface和long-running
recovery。应复制Harness的turn-level recovery，不复制其完整架构。

### 24.5 provider adapter 内部自动重连

拒绝。已发送bytes后的透明retry会绕过durable attempt、authorization、budget和audit，也违反RFC-0067
no-hidden-fallback。retry必须由上层创建新attempt。

### 24.6 根据错误文案判断能否重试

拒绝。错误文案不稳定且容易误分类。provider crate输出typed observation，host只验证durable evidence。

### 24.7 在UI增加“强制继续”按钮绕过proof

拒绝。typed action可以请求retry/reconcile/replan，但不能跳过effect、route、permission和reconstruction
authority。

### 24.8 只提高provider稳定性或减少请求

拒绝。减少请求只能降低概率，不能修复一次故障就终结Task的错误语义；两者都应做，但recovery spine是
正确性的基础。

## 25. 风险与缓解

### 25.1 重复模型消费与成本

transport uncertain retry可能重复provider计算或计费。缓解：有界次数、duplicate-risk telemetry、root cost
budget、Retry-After/backoff、普通generation与hosted effect分离。产品面可在Audit显示安全成本摘要，但默认
主流程不弹确认。

### 25.2 Partial output与cache/history一致性

若旧partial进入durable assistant message，下一request会污染语义。缓解：V1.0先只支持zero-output；V1.1
引入explicit discard sidecar、derived-history exclusion和跨surface测试后再开放。

### 25.3 Recovery policy变成复杂矩阵

缓解：policy只接受统一evidence并返回小型disposition；provider私有分类留在provider crate；产品主流程只
显示coarse state/action；高级策略留在配置/Doctor。

### 25.4 Restart自动dispatch风险

缓解：只有durable schedule、exact reconstruction、current route/config、CAS claim和fresh authorization全部
成立才允许；任一缺失都Blocked。

### 25.5 Execution segment扩大child authority

缓解：segment不改变step contract；每个step仍单独admit/checkpoint/terminal；跨approval/isolation/route
边界禁止合并；R68.5在recovery正确性完成后实施。

### 25.6 Failed语义收窄导致Blocked积累

缓解：产品面区分active attention和历史Task；提供retry/change route/reconcile/replan/cancel；retention只清理
terminal/abandoned，Doctor报告长期blocked原因分布。

## 26. 最终决策

Sigil 不删除已有精密控制面，而是改变复杂度的组织方式：

1. RFC-0067继续拥有Plan-to-Task单一执行脊柱；
2. RFC-0068新增Task内部单一恢复脊柱；
3. provider故障先在logical generation turn内恢复；
4. replay安全性按exact effect evidence判断，不按整个write step判断；
5. 每次retry是新的durable physical attempt，不隐藏重放；
6. settled tool/mutation/commit永不重跑；
7. unknown effect先reconcile，不盲目继续；
8. recoverable error只能上升为retry、Blocked或Paused；
9. Task Failed只保留给真正不可恢复的语义/安全/world-state失败；
10. process restart从durable schedule、child frontier和budget继续；
11. 相邻同authority步骤复用execution segment，确定性工作由host-owned job承担；
12. real-model fault campaign与外部world-state验证成为release gate。

目标不是让Sigil“更敢重试”，而是让每次故障都在最小、可证明的边界内得到正确结果：恢复、等待、观察、
阻塞或失败。这样才能同时保留Sigil的安全与证据优势，并获得coding agent应有的任务完成鲁棒性。
