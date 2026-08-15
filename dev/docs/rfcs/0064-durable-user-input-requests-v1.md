# RFC-0064 Durable User Input Requests V1

状态：implementation-in-progress（协议冻结；代码实施与 release validation 尚未完成）

创建日期：2026-08-15

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0012 Protocol and App Server Boundary](0012-protocol-app-server-boundary.md)
- [RFC-0053 Autonomous Task Routing and Parallel Agent Orchestration V1](0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)
- [RFC-0057 Cache-stable Compaction and Conversation Continuity V3](0057-cache-stable-compaction-and-conversation-continuity-v3.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0063 Automatic Plan Review and Default AI Orchestration V1](0063-automatic-plan-review-and-default-ai-orchestration-v1.md)

## 1. Summary

Sigil 需要一个 kernel-owned、跨 TUI/Desktop/HTTP 一致、可以跨进程重启恢复的“agent 向用户请求输入”
协议。当前模型只能把问题作为 final answer 结束运行；MCP elicitation 又依赖进程内等待，并不拥有普通
agent continuation 的 durable 语义。审批、credential 输入与用户问题也分别拥有不同 authority，不能
通过改名复用。

本文冻结 `Durable User Input Requests V1`：模型通过内部 typed tool 创建 bounded 问题；host 先持久化
request，再让当前 physical worker 以 `AwaitingUserInput` 结束；用户回答通过 exact-identity CAS 命令
持久化，随后由 supervisor 恰好一次启动 continuation。pending request 没有 wall-clock timeout，退出、
重启、session switch、断线与 compaction 均不能丢失或重复消费答案。

RFC-0063 的 Plan revision guidance 是该协议的第一个必须落地的消费者，但协议本身保持 provider、UI 与
PlanReview 中立。

## 2. Confirmed current baseline

当前源码事实：

- `AgentRunDisposition` 没有 waiting-for-user-input 结果；普通 agent 无法暂停后继续；
- TUI MCP elicitation 通过进程内 channel/oneshot 等待，回答后才写终态审计；崩溃前没有 durable pending
  truth；
- HTTP/Desktop 没有普通 agent question 的 snapshot、SSE、answer command 或 strict DTO；
- approval 表示授予一次工具执行 authority，不能表达“补充目标/偏好”；
- MCP elicitation 的 answer 交给外部 server，server 断线后不可安全 replay；
- host secret input 处理 credential，明文不得进入 session 或模型可见 tool result；
- RFC-0053 已有 `AwaitingUser` 产品概念，但未形成 kernel request/answer/continuation 协议。

## 3. Goals

V1 必须达成：

1. 普通 conversation、PlanReview research 与允许交互的 planner 可以请求 1–3 个 typed 用户输入。
2. request、decision、answer receipt、continuation claim 与 terminal outcome append-only、可审计、可恢复。
3. pending request 在 TUI/Desktop/HTTP 使用同一 public projection；renderer state 不成为 authority。
4. physical worker 在等待时退出，不以线程、oneshot 或 300 秒审批 timeout 保活。
5. answer 以 session/run/request/generation/hash/command identity 做 CAS；重复、陈旧和跨 session 请求 fail
   closed。
6. answer 只能启动一次 supervised continuation；重启恢复不得 replay 产生问题的 provider turn。
7. root 与 background agent 的问题都进入 root attention queue，保留真实 source identity。
8. MCP 可共享 normalized form renderer，但不继承普通 agent 的 durable answer replay。
9. secret input 保持独立 host-owned channel，模型不能声明 secret field。
10. compact/aging 不得淘汰未解决 request、已接受但未 claim 的 answer 或恢复 continuation 所需 binding。

## 4. Non-goals

V1 不做：

- 不把用户输入伪装成 approval；
- 不让模型创建密码、token、private key 或任意 secret field；
- 不把自由 Markdown 问句猜成 durable request；
- 不让 UI 直接修改 agent history 或自行恢复 provider continuation；
- 不保证 MCP server 断线后重放明文 answer；
- 不让 background agent 绕过 root session 的 foreground/attention ownership；
- 不用 renderer-local timeout 自动 decline；
- 不在 V1 支持任意 JSON Schema、文件上传、富文本编辑器或动态嵌套表单。

## 5. Domain contract

### 5.1 Identity

```rust
pub struct UserInputIdentityV1 {
    pub session_scope_id: SessionScopeId,
    pub root_logical_run_id: LogicalRunId,
    pub source_thread_id: AgentThreadId,
    pub request_id: UserInputRequestId,
    pub generation: u32,
    pub source_binding_hash: String,
}
```

`request_id` 在一次 logical request 内稳定；重新询问必须增加 `generation` 并产生新的
`source_binding_hash`。hash 绑定 source turn、normalized schema、prompt、purpose 与 continuation
frontier，不绑定 renderer 文案。

### 5.2 Request and source

```rust
pub struct UserInputRequestV1 {
    pub identity: UserInputIdentityV1,
    pub source: UserInputSourceV1,
    pub purpose: UserInputPurposeV1,
    pub prompt: String,
    pub questions: Vec<UserInputQuestionV1>,
    pub allowed_actions: Vec<UserInputActionV1>,
    pub requested_at_unix_ms: u64,
}

pub enum UserInputSourceV1 {
    Agent,
    PlanReviewResearch { plan_review_id: PlanReviewId, attempt_id: PlanReviewAttemptId },
    PlanRevision { base_plan_id: PlanId, base_plan_hash: String },
    Planner { task_id: TaskId },
    Mcp { server_id: String, call_id: String },
}

pub enum UserInputPurposeV1 {
    Clarification,
    Choice,
    MissingConstraint,
    RevisionGuidance,
    ExternalElicitation,
}
```

### 5.3 Bounded form

```rust
pub struct UserInputQuestionV1 {
    pub id: String,
    pub header: String,
    pub question: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: UserInputFieldKindV1,
}

pub enum UserInputFieldKindV1 {
    Text { multiline: bool, max_chars: u32 },
    Number,
    Integer,
    Boolean,
    SingleSelect { options: Vec<UserInputOptionV1>, allow_other: bool },
    MultiSelect { options: Vec<UserInputOptionV1>, max_selected: u32 },
}
```

V1 bounds：

- 每个 request 1–3 个 question；
- 每个 root logical run 最多 3 个 agent-owned request；Plan revision host-owned guidance 不消耗模型
  自主提问次数，但同一 revision 只允许一个 pending generation；
- question id 1–48 ASCII 字符且 request 内唯一；header 1–32 Unicode scalar；question 1–512；
  description 0–512；
- select 2–12 个 option；option id 1–48 ASCII 且 field 内唯一；label 1–80；description 0–240；
- text `max_chars` 为 1–4096；multi-select `max_selected` 为 1–options.len；
- normalized request JSON、answer JSON 与 public projection 分别受显式 byte cap；超过上限拒绝整个请求，
  不静默截断 schema 或 answer；
- 不支持的 field kind 返回 typed `UnsupportedFormShape`，不得降级为 text。

### 5.4 Actions and decisions

```rust
pub enum UserInputActionV1 {
    Submit,
    Decline,
    CancelRun,
}

pub enum UserInputDecisionV1 {
    Submitted { answers: Vec<UserInputAnswerV1> },
    Declined,
    RunCancelled,
}
```

`Esc` 不是 domain action，只关闭当前 UI surface；request 仍为 pending。

## 6. Durable state machine

### 6.1 Lifecycle

```text
Requested
  -> DecisionAccepted(Submitted | Declined | RunCancelled)
  -> ContinuationClaimed                [Submitted only]
  -> ContinuationStarted                [Submitted only]
  -> Resolved(Consumed | Declined | RunCancelled | Failed)
```

允许的 crash-stable 中间态：

- `Requested`：等待用户；
- `DecisionAccepted(Submitted)`：答案 durable，但 continuation 尚未 claim；
- `ContinuationClaimed`：单一 supervisor 已取得 lease，尚未写 Started；
- `ContinuationStarted`：按普通 physical-attempt 证据恢复；
- terminal `Resolved`。

不允许从 terminal generation 回到 pending。重新提问使用同 request id 的新 generation，且 previous
generation 必须 terminal。

### 6.2 Durable entries

kernel `ControlEntry` 新增 current-only V1 records：

- `UserInputRequestedV1`：完整 normalized request、source/frontier binding 与 safe continuation
  descriptor；
- `UserInputDecisionAcceptedV1`：command id、request hash、decision 与按来源定义的 safe answer；
- `UserInputContinuationClaimedV1`：claim id、supervisor instance、generation；
- `UserInputContinuationStartedV1`：new logical/physical run identity；
- `UserInputResolvedV1`：terminal class、continuation outcome 或 decline/cancel reason。

reducer 必须验证 stream sequence、identity、generation、request hash、合法 transition、单一 accepted
decision 与单一 live claim。未知 schema/current-version mismatch fail closed；不修改 session file 做隐式迁移。

### 6.3 Answer persistence policy

| Source | durable value | restart behavior |
| --- | --- | --- |
| Agent / Planner / PlanReview research | bounded safe answer | exactly-once continuation |
| Plan revision guidance | bounded full guidance，绑定 base plan/hash | exactly-once revision request/attempt |
| MCP | schema、decision、value hash；默认不保存 value | disconnected server => Stale，不 replay |
| Host secret input | opaque receipt only | 按 secret broker 规则重新索取 |

## 7. Agent and tool protocol

模型可见内部工具名为 `request_user_input`，只接受 normalized V1 subset。成功调用的 tool result 不在同一
physical run 中由 UI 回填；agent loop 返回：

```rust
AgentRunDisposition::AwaitingUserInput(UserInputRequestRefV1)
```

工具可见性：

- ordinary conversation：允许；
- PlanReview research：允许；
- interactive planner：允许；
- routing microturn、verification、integration、plan finalizer、final answer synthesis：禁止；
- read-only permission 不禁止提问，因为该能力不访问 workspace/network；但 capability scope 必须显式
  包含它，不能由 ordinary registry preparer 隐式泄漏。

模型调用后 host 必须先 append `Requested`，成功落盘后才 emit public event 并返回 disposition。durable
append 失败时工具调用失败且 run 继续或显式失败；绝不能展示一个无法恢复的内存问题。

## 8. Suspension and continuation

`AwaitingUserInput` 是逻辑暂停，不是活跃 provider stream：

1. 当前 provider response 已包含完整 typed tool call；
2. host 持久化 request 和 continuation frontier；
3. current physical worker terminalize，释放 provider connection、thread 与 process资源；
4. session foreground ownership 变为 suspended request owner；普通新 user turn 排队，answer/cancel command
   可取得优先级；
5. answer command durable accepted 后，recovery/supervisor CAS claim；
6. host 生成与原 call id 精确绑定的 synthetic tool result，并从已持久化 frontier 启动新 physical run；
7. 不重新请求产生问题的 provider turn，也不重复 append assistant tool call。

answer 并不承诺复用 provider 私有 continuation handle。cache-stable history + exact bound tool result 是
V1 可移植恢复基础；支持 provider continuation 的实现只能作为优化，不能成为正确性依赖。

## 9. Commands, public projection and HTTP

### 9.1 Command

```rust
pub struct UserInputDecisionCommandV1 {
    pub command_id: CommandId,
    pub session_id: SessionId,
    pub root_logical_run_id: LogicalRunId,
    pub request_id: UserInputRequestId,
    pub generation: u32,
    pub expected_request_hash: String,
    pub decision: UserInputDecisionV1,
}
```

同一 `command_id` + 同一 body 返回同 receipt；同 id 不同 body、旧 generation、错误 hash、错误
session/run、非 pending request、重复不同 decision 全部 typed reject。

### 9.2 Public projection

conversation snapshot 与 SSE 暴露 bounded `PublicUserInputRequestV1`：identity、source、purpose、prompt、
normalized questions、allowed actions、status、source agent label 与 stale/retry fact。答案值默认不回显；
receipt 只提供 decision、answered field ids 与 hash。

SSE 顺序：

```text
UserInputRequested -> WaitingForUserInput ->
UserInputDecisionAccepted -> ContinuationStarted -> ordinary run events -> UserInputResolved
```

snapshot 是重连 authority；SSE 可以丢失，客户端必须按 sequence/cursor 补齐。

### 9.3 HTTP/Desktop contract

本地 bearer 认证接口：

```text
POST /sessions/{session_id}/user-input/{request_id}/decision
GET  /sessions/{session_id}/user-input/{request_id}
```

GET 需要 `generation` 与 `expected_request_hash`，返回 immutable request schema 与最新 status，带 ETag。
Desktop Rust client 使用 `deny_unknown_fields` DTO；Tauri IPC 只投影收窄 DTO，不暴露 bearer、session path
或 generic HTTP。OpenAPI、generated TypeScript schema 与 React view model 同步更新并做 drift check。

## 10. Product surfaces

### 10.1 Shared form view model

kernel/adapter 将 V1 request 与可支持的 MCP schema 归一化为同一 provider-neutral form view model。
TUI 与 Desktop 只拥有 focus、cursor、scroll、draft answer 等 ephemeral state；request/status/allowed action
来自 durable projection。

### 10.2 TUI

- pending request 进入 shell attention queue；foreground 与 background source 均显示真实 agent/thread；
- 打开后支持 question/field 导航、文本编辑、select toggle、validation 与 Submit/Decline/Cancel run；
- `Esc` 关闭 form，`Shift-Tab` 或 attention key 重新打开；
- printable key 只在表单字段聚焦时输入，不被 plan/global hotkey 抢占；
- reconnect/restart 从 snapshot 重建，不能依赖旧 oneshot sender。

### 10.3 Desktop

- conversation attention 区与 plan workbench 显示相同 request；
- draft answer 保持 renderer-local，提交后以 exact command receipt 收敛；
- stale command typed error 后 refresh snapshot，不乐观伪造 resolved；
- background agent request 必须能从 root conversation 定位 source agent。

### 10.4 Headless

HTTP client 可以列出 pending request 并提交 decision；无交互客户端遇到 pending 时得到 typed terminal
`AwaitingUserInput`/exit mapping，而不是挂住或等待 300 秒。

## 11. Recovery, compaction and deletion

- session load reducer 重建所有 unresolved requests，并验证最多一个 foreground-blocking owner；
- `Requested` 无 answer：重新展示；
- answer accepted 未 claim：任一 recovery supervisor 可以 CAS claim，一次成功；
- claimed owner crash：基于 lease/physical-attempt durable evidence 接管，不盲目创建第二 continuation；
- continuation started：复用 RFC-0058 physical-attempt recovery；
- session cancel/delete：append cancel/terminal entry 后关闭 request；删除遵循 session lifecycle；
- fork 默认复制 resolved conversation history，不复制 pending authority；若产品允许 fork pending request，必须
  生成新 session-scoped request identity；
- compaction pin unresolved request、bound assistant tool call、accepted answer/tool result 与 continuation
  frontier；resolved 后可按 RFC-0057 summary/aging，但 hash/decision/lineage 保留。

## 12. Plan revision integration

RFC-0063 Revise 使用 host-owned `RevisionGuidance` request：

1. 用户选择 Revise，只 append `UserInputRequestedV1`；不得提前 append `RevisionRequested` 或启动
   provider；
2. guidance Submitted 后，application transaction 同时接受 answer 并 append
   `PlanRevisionRequestedV1`；
3. revision request id、execution attempt id/ordinal 与 resulting plan id 分离；
4. base plan 在 revision 成功提交并校验前保持 active；
5. guidance pending/revision running 时 Run/Save 暂停，Review original/Answer/Cancel 可用；
6. Failed/Interrupted/Cancelled/CompletedWithoutDraft/submit-only protocol violation 都关闭 revision 并恢复
   base plan actions；
7. retry 复用 revision request identity，但创建新 attempt identity。

## 13. Security and privacy

- model/adapter 不得标记 secret；包含 credential 意图的请求由 host policy 拒绝并引导 secret broker；
- public logs、telemetry、SSE 与 notification 不记录 answer value；
- agent/plan safe answer 在私有 session 中持久化，并受现有 owner-only session storage/retention 约束；
- MCP answer 默认只记录 hash，避免断开的第三方 server answer 被误 replay；
- question/answer 均经过内容和 byte bounds、Unicode scalar/control-character 与结构验证；canonical
  JSON 只用于稳定 hash，不声称改写用户文本或执行 NFC/NFKC normalization；
- answer command 不授予 write/network/tool permission；后续工具仍走原审批/permission path。

## 14. Implementation slices

### R64.0 — Kernel contract and reducer

- V1 types、bounds、hash/canonicalization；
- append-only entries、reducer、projection；
- `AgentRunDisposition::AwaitingUserInput`；
- serialization/reducer/property tests。

### R64.1 — Agent tool and continuation coordinator

- internal `request_user_input` tool；
- capability visibility；
- durable-before-visible suspension；
- answer CAS、synthetic tool result、claim/start/resolve 与 restart recovery；
- root/background attention ownership。

### R64.2 — HTTP and Desktop

- decision/detail endpoints、idempotency、SSE/snapshot；
- OpenAPI、Desktop strict DTO、Tauri IPC、React form；
- real `sigil serve` contract tests 与 reconnect tests。

### R64.3 — TUI and MCP form convergence

- provider-neutral form VM；
- TUI attention/form/key routing；
- MCP supported subset normalization 与 unsupported-shape failure；
- real PTY small/wide/reconnect/restart acceptance。

### R64.4 — Plan revision adoption and release closure

- RFC-0063 guidance transaction/revision identity/state recovery；
- old PlanReview session migration policy；
- deterministic + real-model + Desktop/TUI acceptance；
- docs/Doctor/release notes and full gate。

## 15. Validation plan

按 slice 执行定向测试，最终至少包括：

```bash
cargo test -p sigil-kernel user_input
cargo test -p sigil-runtime user_input
cargo test -p sigil-http user_input
cargo test -p sigil-desktop user_input
cargo test -p sigil-tui user_input
cargo test -p sigil-mcp elicitation
pnpm --dir apps/desktop check
./scripts/generate-desktop-contract.sh --check
./scripts/check-docs.sh
./scripts/check-touched.sh --tier full
```

必须增加：

- reducer transition/property/duplicate/generation/hash tests；
- Requested、answer-before-claim、claim-before-start、active continuation 四个 crash window；
- duplicate/stale/cross-session/cross-run command tests；
- background agent -> root attention queue；
- compaction pin 与 resolved aging；
- HTTP SSE ordering/snapshot reconnect；
- TUI real PTY 与 Desktop strict DTO/E2E；
- Plan revision guidance + all terminal failure base-plan restoration。

## 16. Acceptance criteria

RFC-0064 只有同时满足以下条件才可标记 implemented：

1. 普通 agent 通过 typed tool 请求用户输入，不用 final answer 模拟问题。
2. request 在 UI 展示前已 durable；进程退出后同一 pending request 可恢复。
3. 等待期间没有存活 provider stream/worker/oneshot 正确性依赖，也没有 300 秒 timeout。
4. answer exact-bound，duplicate/stale/cross-session/cross-run 全部 fail closed。
5. accepted answer 只启动一次 continuation，产生问题的 provider turn 不 replay。
6. unanswered/answered/claimed/started 四个 crash window 都有 deterministic recovery test。
7. TUI/Desktop/HTTP 从同一 projection 展示相同 schema、status 与 action。
8. background agent question 在 root attention queue 可发现、可回答、可定位 source。
9. MCP 只共享 renderer；server 断线后的 answer 不被 durable replay。
10. secret value 不进入 request schema、session answer、public DTO、telemetry 或模型 tool result。
11. Plan Revise 在 guidance 提交前不写 RevisionRequested；所有失败恢复 base plan actions。
12. targeted、full、PTY、real serve contract、Desktop E2E、docs/contract drift gates 全部通过。

## 17. Rejected alternatives

### 17.1 复用 approval

拒绝。approval 授予一次外部副作用 authority；用户输入补充目标或偏好。混用会让“回答问题”被误解为
批准工具，也无法表达多字段 typed answer。

### 17.2 复用 MCP oneshot

拒绝。oneshot 依赖活进程和 server connection，不提供 durable pending、CAS answer、restart recovery 或
ordinary agent continuation ownership。

### 17.3 在同一 provider stream 中等待

拒绝。它占用 connection/worker，受网络 timeout 影响，无法跨重启恢复，并把 UI 响应时间错误地计入
provider 运行时间。

### 17.4 把回答追加成普通 user message

拒绝。它丢失 tool call id、request generation/hash 与 source frontier，容易重复消费，也允许无关 user
turn 抢占 pending continuation。

### 17.5 renderer 自行保存 pending state

拒绝。TUI/Desktop 状态会在退出、刷新和 session switch 时丢失，且两个表面会产生不同 authority。

## 18. Completion boundary

本文状态保持 `implementation-in-progress`，直到 R64.0–R64.4、§16 所有 acceptance 与 release validation
完成。局部 DTO、单一 UI 表单或 Plan-only guidance 不得被描述为 RFC 已实现。

## 19. Implementation ledger（2026-08-15）

本轮已落地但不足以把 RFC 标记为 `implemented` 的能力：

- kernel 已有 provider-neutral V1 request/answer/lifecycle contract、稳定 hash/bounds、append-only reducer、
  `AwaitingUserInput` disposition 与 typed `request_user_input` tool；request durable 后当前 worker 才退出；
- ordinary root conversation 与 PlanReview research 已接入同一 decision/continuation coordinator；answer 以
  session/root-run/thread/request/generation/hash/command identity exact-bound，public event/DTO 不包含 answer
  value；
- HTTP 已提供 exact detail/decision contract，detail 使用 request hash ETag；session reopen 会从私有 durable
  `DecisionAccepted` frontier 恢复同一 continuation，不依赖已经完成的旧 command receipt；
- TUI 与 Desktop 已使用同一 public projection 展示表单；Desktop 的 accepted recovery 只显示 answered field
  ids 并提供 Resume，不回显私有值；TUI restore 同样恢复 exact resume action；
- RFC-0063 Revise 已先创建 durable guidance request，提交 guidance 后才创建新的 revision attempt；失败恢复
  base plan，finalizer 只暴露 `submit_plan_draft`，非 submit tool call 不执行并产生 typed protocol violation；
- OpenAPI/generated TypeScript/strict Desktop DTO/SSE event variants 已同步；Plan detail 使用 hash/ETag 绑定，
  UI action authority 来自 canonical reducer 而不是 renderer 本地猜测。
- background child 的 pending request 已以 public bounded form + child session binding 镜像到 root session；
  root answer 会重新加载 authoritative child session、校验原 profile/read-only surface 并启动新 physical attempt，
  已有授权不会跨 attempt 提升；process restart 回归证明只消费一次 continuation；
- stdio 与 Streamable HTTP MCP form 已收敛到同一 bounded normalized contract；TUI 把它转换为同一个
  `UserInputFormViewModel`，复用 agent question renderer/键位/校验，支持 multi-select。未知或 nested shape
  显式返回 `UnsupportedFormShape`，MCP owner 断开/被清理时只发送 Cancel，绝不生成 durable replay command。
- canonical conversation snapshot 现在同时投影 bounded、oldest-first `user_inputs` attention queue，并为旧
  client 保留队首 `user_input`；ordinary、PlanReview 与 background route 按 exact identity/hash 去重。TUI
  用 `Ctrl-N/P` 切换且保留逐请求草稿，Desktop 同时挂载各请求表单并提供显式 Previous/Next；MCP live
  elicitation 不进入 durable queue。

仍未关闭的 release blocker：

1. `ContinuationStarted` 后、provider dispatch evidence 尚未落地时的 crash window 仍需与 RFC-0058 physical
   attempt recovery 完整收敛；
2. unresolved request 的 compaction pin/property campaign、真实 TUI PTY、真实 `sigil serve` + Desktop E2E、
   real-model campaign 尚未全部执行；
3. migration/compatibility 与 release notes/Doctor 仍需在 release closure slice 完成。

因此本节只记录当前事实，不放宽 §16 acceptance，也不把已通过定向 restart/transport 回归的能力外推为
release 全链路完成。
