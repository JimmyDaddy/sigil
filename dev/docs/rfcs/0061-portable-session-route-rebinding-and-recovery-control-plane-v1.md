# RFC-0061 Portable Session Route Rebinding and Recovery Control Plane V1

状态：proposed / base design audit complete / startup ownership addendum re-audit pending / implementation deferred

创建日期：2026-08-03

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0011 Crash Resume and Job Reconciliation](0011-crash-resume-and-job-reconciliation.md)
- [RFC-0026 Stable Machine Protocol and Real Serve](0026-stable-machine-protocol-and-real-serve.md)
- [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- [RFC-0035 TUI Orchestration Boundary Hardening V1](0035-tui-orchestration-boundary-hardening-v1.md)
- [RFC-0052 Desktop Conversation Continuity and Control V1](0052-desktop-conversation-continuity-and-control-v1.md)
- [RFC-0056 Provider Connections, Credential Storage and Model Catalog V1](0056-provider-connections-credentials-and-model-catalog-v1.md)
- [RFC-0057 Cache-stable Compaction and Conversation Continuity V3](0057-cache-stable-compaction-and-conversation-continuity-v3.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)

## 1. Summary

Sigil 当前把 durable session route 的 semantic fingerprint 当成 session restore 的硬门禁。只要
connection 的 endpoint、protocol 或 wire-semantic options 在 session 创建后发生变化，恢复就返回
`session_route_drift`。TUI 又在启动时自动恢复 latest session，并把 `/new`、session switch 和 fork
等生命周期操作交给同一个 agent worker；worker 在 route 校验阶段退出后，这些恢复动作会再次尝试启动
同一个不可恢复 session，最终形成 bootstrap deadlock。

本 RFC 将 session continuity 与 provider-private continuation compatibility 拆成两个概念：

1. **portable session truth**：安全持久化的 user/assistant/tool-result 历史、任务状态、标题、usage、
   portable compaction truth 和可恢复 control state；它属于 session，不因 route 配置变化而失效；
2. **route-private acceleration state**：response handle、provider continuation、native carrier、route-private
   proof、prefix/cache material；它只能在 exact compatible route epoch 内复用。

semantic fingerprint 继续保留，但职责从“是否允许打开 session”降级为“是否允许复用 route-private
state”。当原 `ModelRef` 在当前配置中仍可解析、fingerprint 漂移且 durable egress trust binding 仍匹配
时，runtime 默认在同一 session 追加一个 append-only route-rebind boundary，保留 portable truth，隔离旧
route-private state，并使用当前配置继续。Provider family、protocol、origin/tenant trust boundary 变化或旧
session 无法证明该 binding 时，session 仍可打开，但必须由用户显式确认当前 route 或选择 replacement。
Connection 缺失、当前配置不可解析或 durable session 本身损坏也只进入需要用户处理的恢复状态；即使如此，
Desktop/TUI shell、配置、session picker、`new` 和合法的 fork 都必须可用。

默认 TUI 启动语义同时从“恢复当前 workspace 最近会话”改为“创建一个 fresh session”。恢复历史会话必须
来自显式 `sigil resume [selector]` 或 TUI `/resume` 选择；同一 session 任何时刻最多只能有一个
write-capable interactive attachment。这样用户在同一工作目录再次运行 `sigil` 时会自然开始另一个任务，
不会把已经由另一 TUI 打开的最近会话再次加载并启动第二个 worker。

核心产品原则：

> Provider-private continuation 不能安全复用，不等于 session、Desktop 或 TUI 不能打开。

## 2. Motivation and confirmed failure

### 2.1 Reproduction

已确认的真实路径：

1. 用户配置 OpenAI Responses-compatible connection；
2. endpoint 写错；
3. 新 session 在首次 provider request 前已经持久化 `SessionIdentity` 和 frozen route；
4. provider 返回 HTTP 404；
5. 用户退出 TUI，直接修正配置文件中的 endpoint；
6. 再次启动 TUI，latest session 的 persisted fingerprint 与当前 connection fingerprint 不同；
7. worker 返回 `session_route_drift` 并在 `WorkerReady` 前退出；
8. `/new` 被排到同一个 worker 后面，launcher 又针对旧 session 启动 worker，随后再次失败并清空 pending
   command。

404 没有损坏 session。真正的问题是：一次合法配置修复被解释成不可恢复的 session identity 破坏，且
session lifecycle control 错误地依赖 agent execution worker 健康。

### 2.2 Second-window reproduction

同一 workspace 的另一条已确认路径：

1. 用户运行 plain `sigil`，TUI 恢复最近 session A 并启动 worker；
2. 用户希望并行处理另一个任务，在同一工作目录打开第二个 terminal，再次运行 plain `sigil`；
3. 第二个 invocation 仍把 launch intent解析为 `Latest`，再次选择 session A，而不是 fresh session B；
4. 两个 TUI 因而可能同时持有 A 的独立 in-memory projection、composer 和 worker/control path；
5. 单次 append serialization 或 process-local foreground-run lease不能表达“这个 session 已由另一个
   interactive surface 打开”，因此无法从产品入口阻止双开。

即使某个后续 write 恰好返回 busy，第二个 TUI 已经错误地选择了 A；用户看到的是两个窗口指向同一个任务，
而不是一个明确的新任务空间。这既是默认启动语义错误，也是 session attachment ownership 缺失。

### 2.3 Current implementation chain

当前实现中的关键事实：

- `connection_semantic_fingerprint` 包含 provider family、protocol、normalized endpoint 和 provider
  options，不包含 secret、credential identity、label 或 rotation generation；
- `validate_persisted_model_route` 对 provider family、protocol 或 fingerprint 任一差异返回
  `ResolvedRouteError::SemanticDrift`；
- TUI worker 在 provider build 和 `WorkerReady` 之前调用该 strict validator；
- plain TUI startup 与 `sigil resume` 无 selector 当前都使用 `InitialSessionTarget::Latest`；
- startup `RunFailed` 会退休 worker 并清空 pending worker commands；
- `/new` 只生成 `StartNewSession` worker command，因此无法从 pre-ready worker failure 中逃生；
- application foreground lease 当前是 process-local、run-scoped，不是跨 TUI 进程的 interactive attachment；
- kernel 已有 append-only `SessionModelSelected` boundary，并且普通 continuation state、response handle
  与 prefix snapshot 已按 latest boundary 隔离。

因此本 RFC 不需要删除 route identity，也不需要复制整个 session。缺失的是共享的 resume decision、
自动 route rebind 事件，以及独立于 agent worker 的 session recovery control plane。

### 2.4 Why RFC-0056 amendment is insufficient

RFC-0056 主要定义 provider connection、credential、catalog 与 initial route identity。其 `10.3 Restore
and drift` 当前要求 endpoint/protocol/options drift fail closed，并只提供 restore-config 或 fork。

本变更同时修改：

- durable event taxonomy；
- session route epoch 与 continuation eligibility；
- runtime/application restore contract；
- TUI launcher 与 worker ownership；
- HTTP/OpenAPI typed recovery surface；
- Desktop continuity and recovery UX；
- crash/idempotency、privacy 与跨表面 acceptance。

因此由独立 RFC 冻结 contract。RFC-0061 accepted 后，RFC-0056 `10.3`、route-drift error table 和
相关 acceptance 由本 RFC supersede；RFC-0056 的 connection identity、credential rotation、catalog
与 explicit model-switch 规则继续有效。

## 3. Goals

1. endpoint 或 options 修复后，原 session 可以在同一 session ID 下继续；egress trust binding 未变化时
   自动 rebind，无法证明或发生 trust-boundary 变化时只需显式确认，不强制 fork。
2. semantic fingerprint 继续保护 provider-private continuation、native carrier 和 cache material。
3. 所有 route 变化保持 append-only、secret-free、可恢复、可审计。
4. TUI 与 Desktop 即使没有健康 agent worker，也能打开、浏览、配置、新建和选择 session。
5. `/new`、session switch 和恢复选择不再形成 worker bootstrap dependency。
6. Desktop、TUI、HTTP 和 headless adapter 使用同一个 runtime resume decision model。
7. connection 缺失或同名 connection 的 egress trust boundary 变化时，不把完整历史静默发送到新
   destination。
8. 将 startup/configuration failure 与真正的 post-ready run failure 分开表达。
9. 通过 deterministic、session、TUI state、HTTP contract、Desktop interaction 和真实 PTY 测试证明
   整条恢复链路。
10. 无显式 resume intent 的 TUI 启动总是创建 fresh session，不读取“最近打开”作为 active session。
11. 同一 durable session 不能被两个 TUI 或其他 write-capable interactive surface 同时 attach。

## 4. Non-goals

- 删除 semantic fingerprint 或允许跨 route 复用 provider-private continuation；
- 在 active provider stream、tool、approval、foreground run 或 detached background run 中 mid-turn 改 route；
- 自动重试可能已经发送 request bytes 的 provider request；
- 在原 connection 缺失时静默选择一个不相关的 connection；
- 把 route fingerprint、endpoint、credential、raw provider error body 暴露给普通用户；
- 新增用户必须理解的 `route epoch`、`semantic drift` 或 `continuation` 设置项；
- 引入全局 Auto provider router；
- 兼容旧 binary 读取 RFC-0061 新增的 current session event；
- 把 TUI 专用状态或 provider endpoint 放入 `sigil-kernel` 公共 API；
- 自动 fork。fork 继续是用户主动创建 conversation branch 的操作，不是配置修复的默认代价。
- 允许多个 interactive surface 同时以可写模式 attach 同一 session。
- V1 不提供 force takeover、跨终端 focus handoff 或 read-only spectator mode；busy session 只提供安全恢复动作。

## 5. Terminology

### 5.1 Portable session truth

能够在 route boundary 后继续使用的 provider-neutral durable truth：

- safe-persisted user、assistant 和 tool-result message；
- external provenance 的安全投影；
- session ID、title、usage history；
- finalized task/plan/intent state；
- conversation queue 中仍可证明安全且未 dispatch 的用户输入；
- portable compaction checkpoint 与其 exact cursor/provenance；
- approval、tool execution、mutation、verification 等已经发生的审计事实。

Portable 不表示所有字段都必须进入下一次 provider request。它表示这些事实不会因为 route drift 被
删除、覆盖或强制 fork；已有各域的 projection、budget、safe-persist 与 stale 规则继续适用。

### 5.2 Route-private state

只能在 exact route epoch 内复用的材料：

- provider response handle / response ID；
- provider-specific continuation state；
- provider-native compaction carrier 或 opaque handle；
- route-private proof、request fingerprint 与 cache binding；
- old provider/model-specific prefix snapshot；
- provider 进程内的 paused/background continuation owner。

### 5.3 Route epoch

从 `SessionIdentity`、`SessionModelSelected` 或 `SessionRouteRebound` 起，到下一个 route boundary 之前的
append-only session 区间。epoch 是 projection 概念，不要求新增用户可见编号，也不要求把 endpoint 放进
kernel。

### 5.4 Egress trust binding

Runtime 从 provider-neutral、secret-free 的 route security material 计算 opaque
`RouteEgressTrustBinding`。它至少绑定 provider family、protocol family、normalized network origin 和会改变
destination/tenant trust boundary 的 provider options；不绑定 label、connection ID、credential secret、普通
credential rotation generation 或 endpoint path 中仅影响 API routing 而不改变 origin/tenant 的部分。

Kernel 只持久化 bounded opaque binding，不持有 endpoint。Connection ID 是配置引用，不是 egress trust
identity。新 session 在 initial route 后追加 trust-binding control；旧 session 或缺失该 control 的 route epoch
将 trust 状态投影为 `Unproven`，不能 automatic rebind，但仍可显式确认后继续同一 session。

### 5.5 Exact resume

Persisted `ResolvedModelRoute` 与当前配置解析出的 route 在 provider family、protocol、ModelRef 和
semantic fingerprint 上一致。Route-private state 只进入 existing lifecycle、expiry、restart、payload 与
profile gates 的 eligibility evaluation；exact route 是必要条件，不保证每种 private state 一定可复用。

### 5.6 Portable rebind

Persisted `ModelRef` 在当前配置中仍可解析，但 route 不再 exact compatible。只有 source epoch 与 target
route 的 `RouteEgressTrustBinding` 可证明相同时，runtime 才能 automatic rebind；trust binding 不同或
`Unproven` 时必须先取得 explicit recovery selection。Commit 后 runtime 在同一 session 追加
`SessionRouteRebound`，随后只使用 portable session truth 和新 epoch 中产生的 route-private state。

### 5.7 Replacement selection

Persisted connection 已不存在或不能解析。系统不得自动把历史发送给任意 default；用户可以选择当前
ready connection 继续同一 session、修复原 connection、创建新 session 或打开其他 session。

### 5.8 Interactive session attachment

一个 TUI、Desktop conversation controller 或其他会启动 session worker/接受用户输入的 write-capable surface
对 durable session 的长生命周期占用。Attachment 从 active session 被选定、worker 启动之前开始，到 session
switch、正常退出或进程死亡时结束；它不同于单次 append writer lock，也不同于仅在 run 期间持有的
process-local foreground lease。

## 6. Normative invariants

1. Route drift 不能阻止 Desktop/TUI shell 或 session transcript 打开。
2. Exact route compatibility 是 route-private state reuse 的必要条件，不是 portable transcript access 的
   必要条件。
3. 自动 portable rebind 只能发生在 persisted `ModelRef` 仍能从当前 config 精确解析且 source/target egress
   trust binding 可证明一致时；connection ID 相同本身不是充分条件。
4. Portable rebind 必须先 durable append/sync route boundary，再构建任何可能读取新 route state 的
   provider request。
5. Route boundary 前的 response handle、continuation state、native carrier、route-private proof 和
   prefix snapshot不得进入 boundary 后的 provider request。
6. Portable transcript、session identity、title、usage、task state 和 portable compaction truth 不因 rebind
   被复制、删除或就地改写。
7. Route planning 是纯函数，并与 provider build 绑定同一个 immutable、secret-free resolved-config snapshot；
   磁盘 config 的后续变化只在下一次显式 reload 生效，不能被描述为当前 apply 的 TOCTOU stale guard。
8. Route apply 必须消费 session-scoped quiescence permit；只持有 `&mut Session` 或一次 append writer 不能证明
   active foreground/background run、transition 或跨进程 provider owner 已停止。
9. Active foreground/background work 存在时 route change 仍是 zero mutation。
10. `new`、config、session picker 和 replacement selection 不依赖 agent worker 达到 `WorkerReady`。
11. Pre-ready startup failure 不得清空 composer draft，也不得静默丢弃已经 durable 入队的用户输入。
12. 原 connection 缺失时，saved default 只能作为可见候选，不能作为隐式 replacement。
13. Rebind 本身不发送网络请求；下一次用户 run 或显式 headless invocation 才允许 provider egress。
14. 用户表面不得展示 raw fingerprint、credential、query-bearing endpoint 或 provider-private state。
15. Desktop 与 TUI 的交互可以不同，但 route decision、durable event 和 continuation eligibility 不得分叉。
16. 无显式 resume intent 的 TUI 启动必须创建 fresh session；最近会话只可用于 picker 排序或显式 resume。
17. Write-capable interactive attachment 必须先取得 session-scoped、cross-process、non-blocking exclusive
    lease；lease busy 时不得启动第二个 worker、append input 或把该 session 设为当前可写 session。

## 7. Route resume decision model

### 7.1 Shared runtime types

`sigil-runtime` 新增 provider-neutral decision surface：

```rust
pub enum SessionRouteResumePlan {
    Exact {
        provider_name: String,
        route: ResolvedModelRoute,
    },
    RebindCurrentModel {
        provider_name: String,
        source_route: ResolvedModelRoute,
        target_route: ResolvedModelRoute,
        egress_trust_binding: RouteEgressTrustBinding,
        reason: SessionRouteRebindReason,
    },
    NeedsConfirmation {
        provider_name: String,
        source_route: ResolvedModelRoute,
        target_route: ResolvedModelRoute,
        target_egress_trust_binding: RouteEgressTrustBinding,
        reason: SessionRouteConfirmationReason,
    },
    NeedsReplacement {
        source_route: ResolvedModelRoute,
        reason: SessionRouteUnavailableReason,
    },
    NeedsSetup {
        reason: ModelRouteSetupReason,
    },
}

pub enum SessionRouteRebindReason {
    ConnectionSemanticsChanged,
}

pub enum SessionRouteConfirmationReason {
    EgressTrustChanged,
    EgressTrustUnproven,
}

pub enum SessionRouteUnavailableReason {
    ConnectionNotFound,
    ConnectionConfigInvalid,
}
```

类型名可以在实现前按现有 naming 收敛，但必须保留五种 product disposition。不得把它重新压回
`Result<String, SemanticDrift>` 或让入口按 error string 猜分支。

### 7.2 Pure planning

```rust
pub fn plan_session_route_resume(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    persisted: &SessionRouteResumeInput,
) -> SessionRouteResumePlan;
```

`ResolvedRouteConfigSnapshot` 是一次 config load 生成的 immutable、secret-free route resolution view；planner、
apply revalidation 和 provider build 消费同一 snapshot。进程运行期间外部替换 config 文件不会偷偷改变该
snapshot；显式 reload/save 会生成新 snapshot 并重新 planning。

决策顺序：

1. 使用 persisted `ModelRef` 加载 exact connection；
2. connection 缺失：`NeedsReplacement(ConnectionNotFound)`；
3. connection 或 config 无法通过当前 schema：对应 `NeedsReplacement` 或 `NeedsSetup`；
4. 构建当前 secret-free `ResolvedModelRoute` 与 target `RouteEgressTrustBinding`；
5. family、protocol、fingerprint 全部一致：`Exact`；
6. source trust binding 缺失或 `Unproven`：`NeedsConfirmation(EgressTrustUnproven)`；
7. source/target trust binding 不同：`NeedsConfirmation(EgressTrustChanged)`；
8. trust binding 相同且 fingerprint 不同：
   `RebindCurrentModel(ConnectionSemanticsChanged)`。

Credential rotation 不进入 semantic fingerprint，因此仍为 `Exact`。

### 7.3 Strict validator remains

现有 `validate_persisted_model_route` 或等价 strict API 保留，用于：

- 判断 route-private state 是否可复用；
- native compaction exact-route admission；
- catalog/exact route cache binding；
- Doctor 严格诊断；
- 需要 exact-route contract 的测试或高级 adapter。

Interactive/application resume 不再把 strict validator 的 `SemanticDrift` 直接提升成产品终态，而是调用
`plan_session_route_resume`。

### 7.4 Decision table

| Current condition | Session opens | Same session continues | Private state reused | User action |
| --- | --- | --- | --- | --- |
| exact route | yes | yes | eligible under existing gates | none |
| credential rotated | yes | yes | eligible under existing gates | none |
| endpoint path/options changed, trust binding equal | yes | yes, automatic rebind | no | none; show notice |
| provider/protocol/origin/tenant trust changed | yes | after explicit confirmation | no | confirm/select/new |
| old epoch has no trust proof | yes | after explicit confirmation | no | confirm/select/new |
| connection missing | yes | after explicit replacement | no | repair/select/new |
| config invalid/unconfigured | yes | after setup | no | open setup/config |
| provider build/auth/network failure | yes | route remains selected | no new reuse | repair/retry/new |
| session stream corrupt/unsupported | shell yes; target isolated | no | no | choose/new/export diagnostics |

## 8. Durable route epoch and event contract

### 8.1 New control event

`sigil-kernel` 新增两个 provider-neutral event：

```rust
ControlEntry::SessionRouteRebound {
    provider_name: String,
    model_name: String,
    resolved_model_route: ResolvedModelRoute,
}

ControlEntry::SessionRouteTrustBound {
    route_semantic_fingerprint: String,
    egress_trust_binding: RouteEgressTrustBinding,
}
```

它表示当前配置仍解析出同一个 `ModelRef`，但 semantic route 已变化，session 选择在新 epoch 继续。
旧 route 可以从前一个 `SessionIdentity`、`SessionModelSelected` 或 `SessionRouteRebound` 投影得出，因此
新事件不重复保存 endpoint，也不需要保存 old fingerprint 副本。

`SessionRouteTrustBound` 只保存 opaque、secret-free trust binding，并且只在 latest route boundary 之后、
`route_semantic_fingerprint` 与 current route 精确匹配时生效。新 session 的 `SessionIdentity`、显式
`SessionModelSelected` 和 automatic `SessionRouteRebound` 都必须与 trust event 作为同一个 ordered writer
batch提交；任一 append/sync 失败都不得激活新 epoch或构建 provider。旧 session 没有该 event 时投影为
`Unproven`，只禁用 automatic rebind，不阻止 transcript 打开或 explicit confirmation。

事件要求：

- append-only；
- secret-free；
- current schema 严格 serde；
- event type 映射为 `session_route_rebound`；
- trust event type 映射为 `session_route_trust_bound`；
- route boundary + trust binding batch append/sync 成功后才可更新 in-memory current route；
- 重试时按本节 8.4 的精确比较顺序返回 `Applied`、`AlreadyApplied` 或 typed stale；
- source route 或 immutable target snapshot binding 漂移时返回 typed stale，不写事件。

### 8.2 Existing explicit selection remains

`SessionModelSelected` 继续表示用户通过 `/model`、`/config` 或其他显式产品动作选择完整 route。
自动 config-drift recovery 不复用该事件，避免审计把“自动恢复”伪装成“用户选择”。
Explicit selection 也必须在同一 writer batch 追加对应 `SessionRouteTrustBound`，否则新 epoch 的 trust 状态为
`Unproven`，后续不能 automatic rebind。

### 8.3 Projection changes

所有 current-route projection 必须按出现顺序处理：

```text
SessionIdentity
  -> SessionModelSelected | SessionRouteRebound
  -> SessionModelSelected | SessionRouteRebound
  -> ...
```

下列查询必须从 `entries_after_latest_route_boundary()` 而不是只从 latest model selection 之后读取：

- continuation states；
- response handle；
- prefix snapshot；
- provider-private background/paused handle；
- native carrier/candidate activation；
- route-private compaction proof；
- provider request cache binding。

Portable message、task、usage、mutation、verification 与 portable compaction projection继续读取完整安全
session truth，并按各自原有 cursor/epoch/stale 规则运行。

### 8.4 Rebind commit API

入口不得自行扫描 JSONL 并手写 event。`sigil-runtime` 提供共享 mutation helper，内部调用 kernel
`Session`：

```rust
pub fn apply_session_route_resume_plan(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    session: &mut Session,
    plan: SessionRouteResumePlan,
    quiescence: SessionRouteMutationPermit,
) -> Result<SessionRouteResumeOutcome, SessionRouteResumeError>;
```

`SessionRouteMutationPermit` 是 session-scoped、generation-bound、一次性消费的 quiescence proof。它只能由
application/worker authority 在持有 exclusive session writer/transition ownership、确认 foreground 和
background owner 均已 terminal，并 join/reap 旧 provider worker 后签发。`Session` 不自行推断 active run；
只有短时 append writer 或 UI `is_busy = false` 也不能签发 permit。

Apply 时必须：

1. 验证并消费 permit 的 session scope、authority generation 与 no-active-owner proof；
2. 使用 planner 同一个 immutable `ResolvedRouteConfigSnapshot` 重新解析 target `ModelRef`；
3. 确认重新解析的 target route/trust binding 等于 plan target；
4. 若 current route 等于 plan source，append/sync `SessionRouteRebound + SessionRouteTrustBound` ordered batch，
   返回 `Applied`；
5. 否则仅当 current route 等于 plan target，且 latest boundary 能证明其 predecessor/source、target、trust
   binding 与 plan 完整一致时返回 `AlreadyApplied`，不重复 append；
6. 其余 current/source/target 组合返回 typed stale；
7. batch 成功后更新 in-memory route/trust projection；
8. 返回 typed outcome，包含 `Applied | AlreadyApplied`、是否重置 private state，但不包含 secret/endpoint。

## 9. Crash, idempotency and concurrency semantics

### 9.1 Crash points

| Crash point | Required recovery |
| --- | --- |
| before rebind append | old route remains current; next startup replans |
| append before sync failure | existing JSONL tail recovery applies; no provider request |
| durable rebind after sync, before provider build | new route is current; next startup is exact and does not append duplicate event |
| provider build failure after rebind | session remains usable; config/new/session picker remain available |
| provider request fails before first chunk | ordinary run failure; route remains current; no automatic route rollback |
| failure after request bytes may have been sent | no transparent retry or automatic replay |

Rebind 不代表 provider 可用，也不承诺首次请求成功。它只提交用户当前配置对应的 route epoch。

### 9.2 Writer and transition ownership

- route rebind 与 explicit model switch 复用 session writer/transition ownership和 quiescence permit；
- 多个 surface/process 竞争同一 session 时，未取得 exclusive session owner或 permit 的 caller返回 typed busy；
- plan/apply 之间 session route、authority generation 或 immutable snapshot binding改变时返回 stale；
- 磁盘 config 在 snapshot 创建后改变不影响本次 apply；显式 reload生成新 snapshot并重新 planning；
- route apply 不与 active run、task child、approval、continuation dispatch 或 session transition 交错；
- 不通过更大的 process mutex 掩盖跨进程 writer ownership。

### 9.3 Pending input

- 未 durable 的 composer draft 始终由产品 shell 持有，worker pre-ready failure 不得清除；
- 已 durable queue item 保持原 queue identity，不因 worker restart 复制；
- 对 provider outcome uncertain 的旧 dispatch，继续遵守 existing physical-attempt/reconciliation 规则，不因
  route rebind 自动重发；
- route recovery 完成后，只 dispatch 仍被 durable projection 证明为可运行的 queue item。

### 9.4 Interactive attachment lease

`sigil-runtime` 提供 session-scoped `InteractiveSessionAttachmentLease`（实现命名可调整），作为
write-capable surface 的 cross-process exclusive authority：

- lease 使用稳定的 OS-backed sidecar coordination inode，non-blocking acquire，并与短生命周期 append writer
  lock 分离，避免同一 owner append 时自锁；
- 成功 owner 在持锁后生成 bounded opaque attachment generation，供 recovery binding 和 stale UI response 使用；
  generation metadata 不是 lease authority，只有仍存活的 OS lock handle 能证明 ownership；
- PID、surface label、started-at 等可以作为 bounded best-effort diagnostic metadata，但不得作为 ownership
  authority，也不得包含 workspace path、prompt 或 credential；
- 进程正常退出、session switch 或 panic cleanup 释放 lease；进程 crash 后由 OS 自动释放，不能靠删除 lock file
  猜 owner 已死亡；
- fresh session 必须在写 initial identity/启动 worker 前取得自己的 attachment lease；
- resume/switch 必须先取得 target lease，再提交 active-session switch，最后释放 source lease；target busy 时 source
  session、draft 和 worker 保持不变；
- attachment lease 不能替代 route mutation 所需的 generation-bound quiescence permit，也不能替代 active run、
  lifecycle delete 或 append writer 的既有校验；
- headless one-shot read-only catalog/export 不需要 attachment lease；任何会执行 run、写 queue/control event 或长期
  持有 worker 的 adapter 必须进入同一 write-capable ownership contract。

同一 session 的两个并发 explicit resume 只有一个可以取得 lease。失败方返回 typed
`SessionAlreadyActive` recovery state，不启动 worker、不 append event，也不提供 force takeover。

## 10. TUI architecture and UX

### 10.1 Default launch intent is fresh

TUI 启动先解析明确的 launch intent，不再把“无参数”解释为 latest：

```rust
pub enum TuiLaunchIntent {
    Fresh,
    ResumeLatest,
    ResumeSelector(String),
}
```

该类型属于 launcher/TUI adapter，不进入 `sigil-kernel` durable API。

| Entry | Launch intent | Required behavior |
| --- | --- | --- |
| plain `sigil` | `Fresh` | 创建并 attach 新 session；不得扫描 latest 作为 active target |
| `sigil resume <selector>` | `Resume(selector)` | 显式解析并尝试独占 attach 目标 session |
| `sigil resume` | `Resume(latest)` | 保留显式 resume latest 兼容语义，但仍执行 attachment lease 校验 |
| TUI `/resume` | `Resume(selection)` | 用户从 session browser 显式选择并尝试 attach |
| TUI `/new` | `Fresh` | 使用 10.6 的 worker-independent fresh-session path |

“最近会话”仍可用于 `/resume` picker 排序和 `sigil resume` 的显式 latest 选择，但不能影响 plain `sigil`
启动。退出提示继续打印 exact `sigil resume <session-id>`，使恢复意图可见、可复制、无歧义。

如果配置尚未完成、workspace trust 尚未确认或没有 valid default route，plain `sigil` 先进入现有 setup/trust
surface，不提前创建缺失 identity 的伪 session；条件满足后再执行 `Fresh`。

### 10.2 Single write-capable attachment

TUI 在恢复 transcript 并启动 session worker 之前取得 9.4 的 attachment lease。Lease busy 时 shell 保持可用，
目标 session 只显示为“已在另一个 Sigil 窗口中使用”，并提供 server/runtime 给出的安全动作：start new、back to
session library、retry attach。不得仅凭 PID 猜测 owner、自动 steal、启动只写一半能力的 worker，或把 busy
session 设为 composer 的 durable target。

Default-fresh 解决普通的第二窗口路径，exclusive lease 解决显式 resume、同时启动和跨 surface race；二者缺一
不可。仅改变 latest 选择策略不能证明同一 session 不会被双开。

### 10.3 Shell readiness is independent from worker readiness

TUI 顶层状态至少区分：

```rust
pub enum AgentRuntimeAvailability {
    Starting,
    Ready,
    SessionAlreadyActive(SessionOccupancyView),
    NeedsRouteConfirmation(RouteConfirmationView),
    NeedsRouteSelection(RouteRecoveryView),
    NeedsSetup(SetupReason),
    ProviderUnavailable(ProviderStartupFailureView),
    SessionUnavailable(SessionFailureView),
}
```

这不是把内部状态矩阵暴露给用户。renderer 只映射为少量产品状态：ready、会话已在另一个窗口使用、需要
配置、需要选择连接、连接暂不可用、目标 session 无法打开。

### 10.4 Startup flow

```text
load config
  -> resolve launch intent
       Fresh -> allocate session -> acquire attachment lease -> write initial identity/trust batch
       Resume -> resolve target -> acquire attachment lease -> restore safe transcript view
       lease busy -> keep shell alive with typed recovery; do not spawn worker
  -> for the attached session, plan session route
       exact/rebind -> commit if needed -> spawn worker
       needs confirmation/replacement/setup -> keep shell and attachment ownership; do not spawn broken worker
  -> render
```

Same-ModelRef semantic drift 且 egress trust binding 可证明一致时默认自动 rebind，不弹阻塞 modal。TUI
留一条 timeline notice：

> 连接配置已更新，已使用当前配置继续；服务端上下文缓存已重置。

普通文案不出现 fingerprint、epoch、continuation、native carrier 或 fork requirement。

Trust binding 变化或旧 epoch 为 `Unproven` 时，transcript 和 shell 仍直接打开，但 composer send 进入一次
route confirmation：

> 连接目标已变化。使用当前连接继续会重置服务端上下文。

用户确认后在同一 session 提交 explicit route selection/trust binding；也可以直接 new、修复配置或返回
session picker。该确认保护 egress trust boundary，不要求 fork，也不阻止其他产品操作。

### 10.5 Session control leaves the agent worker bootstrap path

以下动作必须由 launcher/application session control plane 处理，或调用不依赖 active agent worker 的共享
runtime service：

- start new session；
- switch/resume session；
- open session picker；
- apply route replacement/rebind；
- save config and retry startup；
- fork a finalized local session when source truth and writer lease are independently available；
- export/pin/delete preview 等不需要 active agent 的 local lifecycle action。

Agent worker 继续拥有：

- provider stream；
- tool/approval/run scheduler；
- active task/background owner；
- run-time session transition quiescence enforcement。

当 worker 健康时，现有 unified session transition 可以继续用于 active in-process handoff；但产品 shell 必须有
pre-ready fallback，不得为了执行 `new` 先启动当前坏 session 的 worker。Fallback 只能在 worker 尚未建立
session owner，或旧 worker 已 shutdown + join/reap 且 exclusive session authority 已取得时签发 quiescence
permit；UI 中滞后的 idle/busy flag 不能作为证明。

Session switch 必须先对 target 做 non-blocking attachment acquire；target busy 或 transcript invalid时不得关闭
source worker或释放 source attachment。Target transcript可加载且 route plan 为 ready或 typed recoverable state
后，才 shutdown/join source worker、切换 AppState并释放 source；target provider达到 `WorkerReady` 不是切换前提。

### 10.6 `/new`

`/new` 保留为现有兼容入口，不新增顶层 `sigil new` command。它必须：

1. 在 shell 确认当前无 active run owner；
2. 分配新 session path；
3. 对新 path 取得 attachment lease；
4. 使用当前 valid default route 初始化新 session；
5. 保留 workspace trust 语义；
6. shutdown/join 旧 worker，切换 AppState并释放旧 session attachment；
7. 再尝试启动 worker。

如果没有 valid default，`/new` 打开 setup/config recovery，不创建缺失 route identity 的伪 session；如果
provider build 随后失败，新的 session view、draft 和配置入口仍保留。

### 10.7 Missing connection recovery

当 persisted connection 不存在时，TUI 显示单一 recovery surface，提供：

- 修复连接配置；
- 选择当前 ready connection 继续此会话；
- 新建会话；
- 返回 session picker。

选择 replacement 或确认 changed/unproven trust route 后追加显式 `SessionModelSelected +
SessionRouteTrustBound` ordered batch，因为这是用户选择，不是 automatic semantic rebind。原 session ID、
portable history 与 task state 保持不变；route-private state 隔离。

Local fork 必须从 latest `SessionIdentity | SessionModelSelected | SessionRouteRebound` projection取得 source
route/trust epoch；不得继续只扫描 initial `SessionIdentity`。若 source route 不再 exact compatible，用户显式
选择 current route 后，fork destination 写入该 route 和新的 trust binding；source stream保持不变。

### 10.8 Startup failure is not `RunFailed`

`WorkerMessage::RunFailed(String)` 不再承载 pre-ready route/config/provider startup failure。协议应提供 typed
startup-unavailable message，或在 spawn 前由 launcher 直接持有 typed state。`RunFailed` 只表示
`WorkerReady` 后一次真实 run 的失败。

Launcher 不再通过“pre-ready 收到任意 RunFailed”推断应清空所有 pending commands。每个 pending action
必须有显式 owner、admission 和 terminal disposition。

## 11. Desktop, HTTP and headless behavior

### 11.1 Shared application semantics

`sigil-runtime` application session loader、TUI spawn 和 HTTP run preparation 必须调用同一个 route resume
planner/apply helper。不得出现：

- TUI 自动 rebind，但 Desktop 继续 hard-fail；
- HTTP 静默替换 route，而 TUI 要求用户 fork；
- renderer 根据 error string 自己决定 replacement。

### 11.2 HTTP/OpenAPI

Application run/open response 增加 bounded typed transition receipt：

```rust
pub enum SessionRouteTransitionKind {
    Exact,
    Rebound,
    ExplicitlyConfirmed,
}

pub struct SessionRouteTransitionView {
    pub kind: SessionRouteTransitionKind,
    pub connection_id: Option<String>,
    pub model_id: Option<String>,
    pub remote_context_reset: bool,
}

pub enum SessionRouteRecoveryCode {
    RouteConfirmationRequired,
    RouteSelectionRequired,
    ModelRouteNotConfigured,
    ConnectionConfigInvalid,
    ProviderUnavailable,
    SessionAlreadyActive,
    SessionWriterBusy,
    SessionStreamInvalid,
}

pub enum SessionRouteRecoveryAction {
    ConfirmCurrentRoute,
    RepairConnection,
    SelectReplacement,
    StartNewSession,
    RetryProvider,
    RetrySessionAttach,
    BackToSessionLibrary,
}

pub struct SessionRouteRecoveryView {
    pub code: SessionRouteRecoveryCode,
    pub safe_message: String,
    pub allowed_actions: Vec<SessionRouteRecoveryAction>,
    pub recovery_binding: String,
    pub retryable: bool,
}
```

Wire DTO 不包含 physical session path、raw endpoint、credential、semantic fingerprint 或 provider-private
payload。OpenAPI 与 generated Desktop TypeScript contract 同步生成并检查 drift。

Machine codes 与 allowed-action 集合是 normative wire contract，不是建议值。Route recovery 的
`recovery_binding` 必须绑定 session scope、latest route boundary cursor、authority generation 和 immutable
config snapshot identity；attachment-busy recovery 还必须绑定 exact target session identity 与 observed
attachment generation。Action apply 时 exact compare，迟到/重复/跨 session command 返回 typed stale或幂等
receipt。

稳定 machine codes：

- `session_route_confirmation_required`；
- `session_route_selection_required`；
- `model_route_not_configured`；
- `connection_config_invalid`；
- `provider_unavailable`；
- `session_already_active`；
- `session_stream_invalid`；
- `session_writer_busy`。

`session_route_rebound` 是成功 receipt/event，不是 error code。内部 strict validator 仍可使用
`session_route_drift` 做 exact admission 诊断。

Session library 和 transcript read 在内容可安全读取时始终返回成功 read view；若 runtime 不能启动，同一个
response 携带 `SessionRouteRecoveryView`。在 confirmation/selection/setup 未完成时提交 run，或尝试把已经由
其他 write-capable surface attach 的 session 激活为当前可写 conversation 时，HTTP 返回 `409 Conflict` typed
recovery envelope，复用完全相同的 code、allowed actions和 binding。Busy attachment 不阻止 library/read，
但不得创建第二个 writer/worker。Session stream invalid 导致目标 transcript 无法读取时，open 返回 typed
target-session error，但 library/new endpoint继续可用。Renderer 不从 HTTP status 或 message 自行推断
action。

### 11.3 Desktop UX

Desktop 在 transcript 可见的前提下显示 inline notice/toast：

> Connection settings changed. Continuing with the current connection; remote context was reset.

Connection/trust/attachment recovery 时只显示 server-provided allowed actions，例如
confirm/repair/select、retry attach、new、back to library。Renderer 不读取 JSONL，不解析 raw error，不持有
endpoint 或 bearer。映射链固定为：

```text
sigil-runtime typed disposition
  -> sigil-http/OpenAPI recovery envelope
  -> generated sigil-desktop client DTO
  -> Tauri allowlisted business result
  -> renderer action list
```

### 11.4 Headless invocation

一次明确的 headless run invocation 只在 egress trust binding 可证明一致时自动执行 same-ModelRef portable
rebind，并在 machine-readable output 中返回 transition receipt。Trust changed/unproven 时返回 typed
confirmation-required；caller 必须提交 exact recovery binding。Notice 不得写进 machine-readable stdout 的
非协议文本；CLI human diagnostics写 stderr。

Connection 缺失时 headless 返回 typed selection-required，不自动使用 default。未来若需要 strict exact-only
automation policy，应作为 adapter-level advanced option，不新增普通用户配置开关，也不改变 Desktop/TUI 默认
语义。

对已有 durable session 执行 write-capable one-shot run 也必须取得 shared cross-process attachment/execution
authority；session 已由 TUI/Desktop attach 时返回 typed busy，不与 interactive owner 并发 append。只读 catalog、
support projection 和 safe export 保持可用。

## 12. Continuation and compaction eligibility

### 12.1 Ordinary continuation

`ContinuationStateSaved`、`ResponseHandleTracked` 和 `PrefixSnapshotCaptured` 只从 latest route boundary 之后
读取。Rebind 后第一次 request 必须满足：

```text
previous_response_handle = None
old provider continuation states = []
old provider-native carrier = ineligible
old route-private prefix/cache proof = ineligible
portable messages/checkpoint = eligible under existing projection rules
```

### 12.2 Native continuation

Provider-native candidate/handle/artifact 必须同时绑定：

- session scope；
- route epoch boundary/cursor；
- provider route fingerprint；
- provider/model/wire profile；
- existing payload integrity/provenance。

任一不匹配都只让该 native candidate ineligible，并机械回退 portable projection；不能再次把整个 session
restore 变成 fatal error。

### 12.3 Portable compaction

Portable checkpoint 在 route rebind 后继续作为 durable truth，但必须继续通过现有 exact cursor、source
provenance、safe-persist、schema 和 context-assembly validation。Route rebind 不允许：

- 把 provider-native carrier 当成 portable checkpoint；
- 重新解释旧 provider-private reasoning block；
- 跳过 portable fallback；
- 让 route change 隐式触发一次额外 provider summary request。

若现有 portable checkpoint 本身不能跨 provider/profile 安全物化，compaction projection应返回自己的 typed
ineligible/stale 并从 portable transcript 重建；不能回退使用 old native state。

## 13. Security, privacy and egress

1. Semantic fingerprint 算法保持 secret-free；credential rotation 不改变它。
2. Kernel durable event 不新增 endpoint、header、credential ID、secret 或 options body；只保存 opaque bounded
   egress trust binding。
3. Connection ID 是配置引用，不是 trust identity。Automatic rebind 同时要求 exact `ModelRef` 可解析、source
   trust proof存在且 source/target trust binding相同。
4. Provider family、protocol、origin/tenant trust boundary变化或旧 epoch无 proof时必须显式确认；confirmation
   receipt绑定 exact recovery binding，不能由普通 notice代替。
5. Rebind startup 不发送历史或 request bytes；用户下一次 send/headless run 才产生 egress。
6. Connection 缺失时不自动选择 default，防止历史静默流向无关 destination。
7. 用户可见 notice 只显示 bounded connection label/model，不显示 query-bearing endpoint 或 raw provider body。
8. Support/telemetry 只记录 disposition、provider family/protocol 的安全分类与计数，不记录 raw endpoint、
   fingerprint、prompt 或 credential。
9. Existing provider TLS、no-redirect、CA bundle、egress disclosure 与 request-wire retry rules保持不变。
10. Attachment lock diagnostic metadata 不记录 prompt、session/workspace path、credential 或 raw terminal
    command；PID 只可用于本机提示，不能授权 steal/takeover。

## 14. Error and recovery taxonomy

| Internal condition | Product state | User remediation |
| --- | --- | --- |
| strict exact match | ready | none |
| rebind committed | ready + one notice | continue |
| egress trust changed/unproven | route confirmation required | confirm/select/new |
| connection missing | route selection required | repair/select/new |
| config invalid | setup/config required | fix highlighted config |
| provider build/auth failure | connection unavailable | repair/retry/new |
| interactive attachment lease busy | session already active | retry/start new/back to library |
| session writer busy | session busy | retry/open another session |
| session stream corrupt/current schema invalid | target session unavailable | choose/new/diagnostics |
| post-ready provider HTTP/SSE failure | run failed | retry/edit config/new |

错误类型必须跨 crate 使用 stable typed enum/DTO；raw `anyhow` context 仅用于内部定位和安全日志。TUI timeline、
Desktop renderer 与 HTTP clients 不解析字符串中的 `session_route_drift`。

## 15. Crate and module ownership

### 15.1 `sigil-kernel`

- `SessionRouteRebound` 与 `SessionRouteTrustBound` provider-neutral durable event；
- latest route boundary projection；
- session route apply 的 append-only mutation 与 in-memory update；
- continuation/prefix/native eligibility 的 epoch boundary；
- 不解析 config，不持有 endpoint，不决定 UI remediation。

### 15.2 `sigil-runtime`

- `SessionRouteResumePlan` 和 pure planner；
- secret-free `ResolvedRouteConfigSnapshot` 与 `RouteEgressTrustBinding` 计算；
- strict validator 与 portable resume planner 的职责分离；
- session execution authority、quiescence permit 与 rebind plan/apply helper；
- cross-process interactive attachment lease 与 typed busy outcome；
- application/TUI/HTTP 共享的 typed outcome；
- provider build 与 route apply 消费同一 immutable snapshot。

### 15.3 `sigil-tui`

- shell/runtime availability state；
- plain launch -> fresh session policy 与 explicit resume intent；
- attachment acquire/switch/release ownership；
- non-blocking recovery surface；
- launcher-owned pre-ready session lifecycle fallback；
- typed worker startup bridge；
- timeline notice、state transition 和 real PTY acceptance；
- 不手写 JSONL route mutation。

### 15.4 `sigil-http`

- authenticated typed route transition/recovery DTO；
- `session_already_active` recovery code、binding 与 allowed actions；
- application service outcome 到 HTTP/OpenAPI 映射；
- 不复制 route planner，不暴露 server-private path。

### 15.5 `sigil-desktop` and `apps/desktop`

- `sigil-desktop` 只消费生成的 typed HTTP contract；
- Tauri adapter 只转发 allowlisted business DTO；
- renderer 展示 server-provided recovery actions/status；
- renderer 不读取 config/session 文件，不解析 raw server error。

## 16. Implementation plan

### R61.0 Contract freeze and characterization

- 固定 endpoint 404 -> config edit -> latest restore failure reproduction；
- 固定 drifted latest -> `/new` bootstrap deadlock；
- 固定同一 workspace 两次 plain `sigil` 当前都会 attach latest session 的双开 reproduction；
- 增加 strict validator 与 current continuation-boundary characterization；
- 冻结 `Fresh | Resume(selector) | Resume(latest)` launch intent、attachment busy code、decision table、event name、
  machine codes 和 privacy budget；
- 本切片只增加 failing/characterization tests 与 RFC，不改变产品行为。

### R61.1 Shared resume planner

- runtime `ResolvedRouteConfigSnapshot`、`RouteEgressTrustBinding` 和 `SessionRouteResumePlan`；
- exact/rebind/confirmation/replacement/setup 分类；
- 保留 strict validator；
- typed errors、Rustdoc、pure unit tests；
- application run preparation 接入 planner，但 rebind mutation留到 R61.2。

### R61.2 Durable route rebind epoch and trust proof

- kernel `SessionRouteRebound`、`SessionRouteTrustBound` 与 ordered writer batch；
- current route/identity/stats/audit/event projections；
- `entries_after_latest_route_boundary`；
- continuation、response handle、prefix、native carrier/candidate eligibility；
- explicit selection/initial identity trust projection；
- `Applied | AlreadyApplied | Stale` 比较顺序与 crash/idempotency tests。

### R61.3 Execution authority and TUI recovery control plane

- session-scoped exclusive authority、generation-bound quiescence permit 和 worker join/reap proof；
- shared cross-process `InteractiveSessionAttachmentLease`、opaque generation 与 crash release；
- plain `sigil` 默认 fresh；只有 `sigil resume [selector]` 和 `/resume` 进入历史 session；
- startup route planning、trust-equal automatic rebind 与 changed/unproven confirmation；
- shell readiness 与 worker readiness 分离；
- `StartNewSession`、switch/recovery selection 的 pre-ready launcher/application path；
- typed startup unavailable state；
- 保留 draft、pending durable queue identity 和 session browser；
- local fork 改为 latest route/trust boundary projection；
- TUI state、runner、launcher tests 与真实 PTY regression。

### R61.4 HTTP and Desktop parity

- application loader/run context 使用共享 planner/apply；
- authenticated DTO、OpenAPI 和 generated TypeScript；
- Desktop inline recovery notice/actions；
- native client、Tauri allowlist、renderer interaction tests；
- 真实 `sigil serve` contract test。

### R61.5 Documentation and diagnostics

- RFC-0056 supersession pointer；
- core technical solution 更新 route restore snapshot；
- EN/ZH troubleshooting、configuration、providers、user guide；
- Doctor 输出 exact/rebindable/confirmation-required/missing/invalid typed diagnosis，但不泄漏 raw
  endpoint/secret；
- changelog 说明 route drift 不再锁死 session。

### R61.6 Acceptance and closure

- touched standard/full gate；
- kernel/runtime/TUI/HTTP/Desktop targeted tests；
- OpenAPI/generated contract drift；
- real TUI PTY；
- source-built Desktop + real `sigil serve` dogfood；
- endpoint 404/config correction canary；
- 独立 code/security/product audit；
- 同步 RFC implementation status 与剩余 external gate。

## 17. Test matrix

### 17.1 Runtime planner

- exact route -> `Exact`；
- credential rotation -> `Exact`；
- normalized endpoint trailing slash equivalent -> `Exact`；
- endpoint path/options change且 trust binding相同 -> `RebindCurrentModel`；
- provider family/protocol/origin/tenant change -> `NeedsConfirmation`；
- legacy/unproven source trust -> `NeedsConfirmation`；
- connection missing -> `NeedsReplacement`；
- invalid current connection/global config -> typed unavailable/setup；
- planner performs no write/network/credential exposure。

### 17.2 Kernel/session

- rebind preserves session scope、portable messages、title、usage、task state；
- old continuation state、response handle、prefix snapshot不可见；
- old native candidate/carrier不可激活；
- portable checkpoint保持可验证；
- duplicate same source/target rebind returns `AlreadyApplied` only when predecessor/target proof matches；
- source/target/snapshot/authority generation drift returns stale without append；
- crash after durable append resumes exact without duplicate；
- route boundary/trust event batch is secret-free and current-schema valid；
- permit缺失、旧 worker未 join 或 active owner存在时 zero mutation；
- fork source route来自 latest boundary，不回退 initial `SessionIdentity`。

### 17.3 TUI

- 同一 workspace 首次 plain `sigil` 创建 session A，第二次 plain `sigil` 创建不同的 session B；两者都能
  `WorkerReady`，第二次不得读取/attach A；
- `sigil resume <A>` 在 A 被另一 TUI attach 时进入 `SessionAlreadyActive`，zero worker、zero append；
- A owner 正常退出或 crash 后 OS lease 释放，随后 explicit resume A 成功；
- 两个并发 explicit resume A 只有一个取得 lease，失败方保持 shell/new/library可用；
- `/resume` 切换到 busy target 时 source session、worker、draft 和 attachment 均保持；
- `sigil resume` 无 selector仍明确选择 latest并执行 lease gate；退出提示包含 exact session ID；
- bound same-origin wrong Responses path -> 404 -> exit/edit path -> explicit resume -> `WorkerReady`；
- changed-origin或legacy/unproven route打开 shell并在 explicit confirmation 后 `WorkerReady`；
- session ID unchanged；automatic path exactly one rebind event，confirmation path exactly one explicit selection；
- drifted latest session does not block `/new`；
- missing connection allows config、session picker、new and explicit replacement；
- config invalid opens setup without generic run failure；
- pre-ready failure preserves composer draft；
- durable queued input neither duplicates nor disappears；
- active run still blocks route mutation；
- keyboard/mouse recovery actions and focus ownership；
- terminal size/help/notice rendering remains bounded。

### 17.4 HTTP/Desktop

- open/run returns exact/rebound/recovery-envelope typed view；
- open recovery是 successful transcript state，blocked run返回 `409` 同 binding envelope；
- busy attachment 的 library/read保持可用，write activation/run返回 `409 session_already_active`；
- retry attach 使用 exact session/attachment-generation binding，不能作用到已切换的 renderer target；
- allowed actions、recovery binding、stale/idempotent receipt跨 generated client保持；
- DTO contains no path/endpoint/fingerprint/secret；
- renderer never parses raw error；
- rebind notice and replacement actions survive reload/reconnect；
- new/open/library remain usable while provider unavailable；
- OpenAPI and generated client remain in sync；
- real serve restart reproduces same session semantics as TUI。

### 17.5 Negative and security

- plain launch never consults latest session as active target；
- busy attachment不能通过 stale PID、删除 metadata、second worker或 headless run绕过；
- crash释放 OS lease但不删除/改写 session stream；
- connection deletion never silently selects default；
- same connection ID with changed egress trust never automatic rebinds；
- route rebind never reuses old response ID/output items/native carrier；
- startup rebind performs zero provider request；
- uncertain prior request is not automatically replayed；
- malicious/invalid session cannot block shell or another session；
- writer/authority busy、stale plan或未 join worker不能产生两个冲突 boundary；
- raw endpoint/query/credential absent from session event、timeline、Desktop IPC、support bundle and telemetry。

## 18. Acceptance criteria

RFC-0061 implementation 只有在以下条件全部成立后才能标记 complete：

1. 用户复现场景无需恢复旧 endpoint 或 fork，通过 exact `sigil resume <session-id>` 或 `/resume` 即可重新进入
   并继续同一 session；trust-equal route自动 rebind，changed/unproven trust只需一次 exact-bound
   confirmation。
2. 同一 workspace 连续启动两个 plain `sigil` 必须创建不同 session；第二个 invocation 不读取或 attach
   第一个 invocation 的最近会话。
3. 历史 session 只通过显式 `sigil resume [selector]` 或 TUI `/resume` 进入；`sigil resume` 无 selector的
   latest 兼容行为仍经过 attachment lease gate。
4. 同一 session 同时最多一个 write-capable interactive attachment；TUI/TUI、TUI/Desktop 和
   interactive/headless race均不能产生第二个 worker或并发 append owner。
5. Busy target 返回 `session_already_active` 且 zero mutation；source TUI、draft 和 worker保持。原 owner退出或
   crash 后 explicit resume可以取得 OS-released lease，不需要删除 lock file或 force takeover。
6. Same-ModelRef trust-equal semantic drift只追加一个 durable route rebind/trust batch；重复 apply 返回
   `AlreadyApplied`，不重复 append。
7. Rebind 后 provider wire fixture证明旧 route-private state为零复用。
8. Portable transcript、session ID、title、task、usage 和 portable compaction truth保持。
9. Drifted/missing/invalid/busy route均不能阻止 TUI/Desktop shell、new、config 和 session library。
10. `/new` 不再依赖坏 session worker达到 ready，并在新 session attachment成功后才释放 source。
11. Connection missing、changed egress trust 或 unproven legacy route不静默发送历史；explicit
    confirmation/replacement可在同一 session继续。
12. `RunFailed` 不再承担 pre-ready route/config/provider/attachment startup taxonomy。
13. TUI、application runtime、HTTP 和 Desktop使用同一个 route resume decision 与 session ownership model。
14. Session event不新增 raw endpoint/credential/private payload；existing semantic fingerprint和新增 opaque trust
    binding只留在内部 durable state，不进入 HTTP DTO、Desktop IPC、用户文案或 telemetry。
15. Crash/stale/writer-authority-busy/attachment-busy/worker-not-joined/uncertain-request测试证明没有重复 rebind、
    跨 epoch private-state write、自动重发、lost draft 或双开 session。
16. Targeted standard/full gates、OpenAPI drift、real PTY 和 real serve Desktop contract通过。
17. EN/ZH 文档、RFC-0027/RFC-0056 supersession pointer和核心技术方案与真实实现一致。
18. 独立审计未发现剩余 P0/P1；所有认可或部分认可的 P2 finding已处理或在 RFC 中明确 deferred owner/gate。

## 19. Metrics and privacy budget

建议 metrics：

- `session_route_resume_total{disposition}`；
- `session_route_rebind_total{reason}`；
- `session_route_confirmation_total{reason,action}`；
- `session_route_recovery_total{reason,action}`；
- `session_route_private_state_reset_total{kind}`；
- `session_launch_total{intent,outcome}`；
- `session_attachment_total{surface,outcome}`；
- `worker_startup_unavailable_total{kind}`；
- `session_new_without_worker_total{outcome}`；
- `session_route_resume_ms{phase}`。

禁止 label/value：

- raw endpoint/host/path/query；
- connection label 或 credential identity；
- full ModelRef/fingerprint；
- session path、workspace path、prompt 或 provider raw body。

允许 bounded low-cardinality：disposition、provider family、protocol family、rebind reason、surface、outcome。

## 20. Alternatives considered

### 20.1 Delete semantic fingerprint

拒绝。Responses output items、Anthropic/Gemini continuation、native compaction carrier 与 cache binding 仍然需要
exact route compatibility。删除 fingerprint 会把体验问题变成跨 endpoint/provider 私有状态误复用问题。

### 20.2 Keep fail-closed restore and only fix `/new`

拒绝。它能解除 bootstrap deadlock，但用户仍必须放弃同一 session 或恢复错误配置，且 Desktop/HTTP仍会
重复相同问题。真正错误的是把 private-state incompatibility提升成 portable-session fatal error。

### 20.3 Always fork on route drift

拒绝。fork 会改变 session identity、分散标题/任务/历史，并把正常配置修复变成用户数据管理成本。
Fork 应表达 conversation branch，而不是 provider cache reset。

### 20.4 Silently use current default when connection is missing

拒绝。Default 可能属于不同 provider、账号或网络 destination。没有 exact saved connection identity 时，发送完整
历史必须来自显式 replacement selection。

### 20.5 Reuse `SessionModelSelected` for automatic rebind

拒绝。虽然现有 projection boundary 可以工作，但审计无法区分用户主动选择与配置恢复。新增中立
`SessionRouteRebound` 让 mutation cause 可证明，并避免把自动行为伪装成用户动作。

### 20.6 Mutate route metadata in place

拒绝。它破坏 append-only history，无法证明旧 continuation 属于哪个 route epoch，也无法在 crash 后区分配置
修复前后状态。

### 20.7 Add a global permissive/strict config toggle

拒绝。普通用户不应理解 route compatibility policy matrix。产品默认采用 portable rebind；exact validator
继续作为内部能力和高级 automation contract，而不是 `/config` 主路径开关。

### 20.8 Restore latest only when it appears idle

拒绝。它仍把 plain `sigil` 的“开始另一个任务”解释成隐式 resume，且“扫描时 idle”与真正 attach 之间存在
race。Default 必须是明确的 `Fresh`，并由 attachment lease独立防住 explicit resume race；不能靠 PID 文件、
mtime、最近活动时间或 best-effort worker探测决定是否双开。

## 21. Consequences

正向结果：

- 普通 endpoint 修复不再锁死 session；
- 同一 workspace 的第二个 plain TUI自然进入 fresh session，不再误 attach第一个窗口；
- explicit resume有跨进程 single-writer attachment guard，crash后由 OS 自动回收；
- session history 与 provider acceleration state 边界清晰；
- semantic fingerprint 从过宽的身份锁回到正确的 compatibility guard；
- `/new`、config 和 session library成为真正的恢复控制面；
- Desktop/TUI/HTTP共享 typed semantics；
- append-only audit仍能证明每段 provider route；
- 后续跨 provider model switch、native compaction与 route diagnostics拥有统一 epoch模型。

成本与风险：

- 新增 current session event会触及多个 projection 和 strict serde surface；
- worker/session control ownership调整涉及 TUI launcher 与 runner 高风险链路；
- native continuation/candidate必须完成 route-epoch audit，不能只修普通 response handle；
- Desktop/OpenAPI parity增加交付范围；
- 默认启动从 latest 改为 fresh 是可见行为变化，需要 CLI help、退出提示、EN/ZH 文档和 PTY fixture同步；
- 长生命周期 attachment sidecar、generation 与跨 surface acquire/release扩大 session ownership实现范围；
- resolved-config snapshot 继续承载 route authority，egress trust binding 决定是否允许自动 rebind，因此 config
  publish、snapshot binding、writer authority 与可见 notice 必须可靠；
- 需要真实 PTY/serve dogfood，单元测试不足以证明 bootstrap recovery。

## 22. Supersession and documentation policy

本 RFC accepted 后：

- RFC-0056 `10.3 Restore and drift` 中“endpoint/protocol/options drift 导致 session restore fail closed”由
  RFC-0061 替代；strict fingerprint 对 private-state reuse 继续有效；
- RFC-0056 route-drift error table改为 selection-required/rebind receipt语义；
- RFC-0057 exact-route native resume继续要求 strict route；route mismatch回退 portable truth，不阻断 session；
- RFC-0058 worker ownership补充：agent worker不拥有 pre-ready product recovery control plane；
- RFC-0027 local lifecycle补充：合法的 new/select/fork入口在 agent worker不可用时仍可调用共享 service，
  interactive attachment lease与 lifecycle/append writer lease职责分离；
- RFC-0052 Desktop continuity补充 typed route transition/recovery view。

在 RFC-0061 accepted 前不修改上述历史 RFC 的 normative text；避免 proposed 设计被误写成已实现事实。

## 23. Implementation boundary

本 RFC 创建与设计审计阶段只提交文档，不修改 Rust、TypeScript、OpenAPI、session schema 或产品行为。
实现必须按 R61.0-R61.6 切片推进，每个切片带对应测试和状态同步；不得以“先把 validator 改成永远成功”
替代完整的 route epoch、worker recovery 和跨表面 contract。

## 24. Independent design audit

2026-08-03 完成独立只读设计审计。首轮识别 2 个 P1 和 3 个 P2：egress trust identity、跨 worker
quiescence authority、幂等提交顺序、config snapshot TOCTOU 语义，以及 Desktop/HTTP recovery contract。
上述 finding 均已纳入本 RFC；最终复核未发现剩余或新增 P0/P1/P2。最终复核提出的两项 P3 稳定命名遗漏
也已关闭：`provider_unavailable` 和 `session_route_trust_bound` 已进入 normative 清单。

同日新增 default-fresh TUI launch 与 single write-capable attachment addendum。该 addendum 已完成仓库实现对照和
文档自检，但属于首轮独立审计之后新增的 normative scope；在再次独立复核前，RFC 状态保持
`startup ownership addendum re-audit pending`。
