# Sigil Capability Roadmap

状态：Roadmap v1.0 / Frozen

更新日期：2026-06-25

冻结说明：本版本只承载能力目标、依赖顺序和阶段边界；后续实现细节进入 RFC，不再继续扩充 Roadmap。

审阅基线：`main` 分支 `d44b2f82a4c6fff330c3b30e878d176dbfe2dc5d` 附近的静态源码审阅。本文不代表已经完成编译、TUI 冒烟或测试验证。

当前完成度快照（2026-07-15）：本 Roadmap 仍保持 frozen，不再扩充 phase 或交付范围；实时执行状态由 repo-local RFC status board 跟踪，属于 `.repo-local-dev/` 下的工作区本地材料，不作为仓库内稳定链接提交。截至该审计，roadmap 的核心语义大多已落地；E13.11 model eval runner 已由 RFC-0028 完成。剩余工作主要是 evidence/platform/product-pressure gated：E06.6 persistent repo graph / semantic retrieval、E08.5/E16.7 projection escalation、E14.4 physical worktree、E14.8 cleanup hardening、E05.10 Windows backend 和 Docker/container PTY productization。

注意：下文第 3 节中的“当前状态”和各 phase “当前问题”保留 2026-06-25 frozen baseline，用于说明路线图设计时的缺口，不应解读为 2026-07-15 的实时完成表。

本文基于当前仓库实现、`README.md`、`dev/governance/*` 和
[`sigil-rust-agent-core-technical-solution.md`](sigil-rust-agent-core-technical-solution.md)
整理。它不是对核心技术方案的替代，而是把下一阶段能力成熟度拆成可执行路线。

## 1. 当前判断

Sigil 已经是一个真正的 TUI-first coding agent，而不是普通 tool-calling CLI 或只提供 agent framework 的库。

当前已经形成的核心能力：

- Agent loop：已具备模型、工具、观察结果、继续调用模型的闭环，并支持流式 reasoning、tool call、provider continuation、最大轮次和结构化工具错误反馈。
- Tool system：工具具备 JSON schema、读写执行分类、动态权限主体、preview、egress audit 和结构化结果。
- Safety and audit：写工具支持 diff / preview；权限支持 allow / ask / deny、工具规则、subject glob 和外部目录控制；session/control state 走 append-only 持久化。
- TUI product surface：TUI 已经是第一用户入口，承载 transcript、composer、approval、tool activity、config、resume、context compaction 和 provider/tool 状态展示。
- Multi-provider and MCP：runtime 支持 DeepSeek、OpenAI-compatible、Anthropic、Gemini；MCP 已具备 stdio server、lazy activation、trust、approval、resources/prompts 和 elicitation 边界。
- Code intelligence：已有 LSP 与 Tree-sitter fallback，提供 symbols、definition、references、diagnostics、code action 和 rename 等工具。
- Task and subagent：已有 `/task` planner/executor/subagent 角色、任务控制日志、子 agent 工具、mailbox、后台 task 和 token budget。

下一阶段的主要问题不是继续堆命令，而是补齐三个成熟度缺口：

1. 证明任务完成，而不是只接受模型 final answer。
2. 找准上下文，而不是只依赖模型主动调用 read / grep / LSP 工具。
3. 隔离执行环境，而不是只靠 permission policy 判断能否执行。

能力调研带来的路线细化是：Sigil 应吸收 Evidence、Context Archive、Checkpoint/Rewind、Session Revert、只读 DAG、sandbox enforcement、thread projection、agent graph、执行可观测性和多客户端协议的产品经验，但不能引入并行写 agent、sandbox fail-open、只靠工具权限伪装只读环境，或让插件/外部工具游离于统一 trust / egress policy 之外的风险。Sigil 的优势是 append-only control plane、权限审计、LSP code intelligence 和 egress 控制；新增能力应建立在这些控制面之上。

执行层新增一个前置判断：Verification、Checkpoint、Projection、Crash Resume 都依赖可靠事件基础。能力开发前必须先补 durable event envelope、stream sequence、兼容读取和最小 deterministic eval，否则后续阶段没有稳定回归基线。

## 2. Roadmap 原则

1. TUI 是第一用户表面。新增能力必须先考虑 TUI 状态、事件流、键位提示和恢复体验。
2. Kernel 保持 provider-neutral。DeepSeek、Anthropic、Gemini、OpenAI-compatible 的私有语义不得进入公共 kernel API。
3. Completion 必须可判定。模型输出 final text 只能表示模型认为完成，不能直接等同于用户目标已验证完成。
4. Permission 不等于 sandbox。权限负责决定能不能执行，执行后端负责限制最多能影响什么。
5. Context 要可审计、可压缩、可复用。自动召回不能变成不可解释的大段 prompt 注入。
6. Session/control state 必须 append-only、可持久化、可恢复。影响恢复正确性的状态不能只存在进程内存里。
7. JSONL 是事实来源，SQLite 或其他索引只能作为可重建投影和产品查询视图；事件 envelope、stream sequence、schema version 和 reducer 接口先于完整投影落地。
8. Read-only agent 必须由工具能力和执行后端共同约束；禁止 edit 工具不等于工作区只读。
9. 插件、自定义工具、MCP 和未来技能包必须进入同一套 trust、egress、secret 和环境变量注入审计。
10. 协议层和 app server 只在核心状态、事件和投影稳定后引入，避免过早按未来客户端形态拆 crate。
11. Durable event、live event 和 protocol event 必须分层；事实日志只保存恢复、审计和投影需要的状态变化。
12. Workspace 本地指令、插件和 MCP 配置只有在 workspace 被信任后才可提升为受信任输入。
13. Append-only 表示 session stream 内不能静默改写历史，不表示用户不能删除整个 session、workspace state、artifact 或 cross-session memory。
14. 每个阶段都要有验收标准和可执行 gate。docs-only 阶段至少确认路径、链接和命令不漂移。

## 3. 优先级总览

| 优先级 | 能力 | 当前状态 | 目标 |
| --- | --- | --- | --- |
| P0 | Durable Event Foundation + Minimal Eval | JSONL 是 append-only，但缺少统一 envelope、stream sequence、schema version、尾部损坏恢复和基础 conformance eval | 事件地基、兼容读取、reducer 接口、deterministic fake provider/tool 和最小状态机 eval |
| P0 | Verification Contract + Evidence Projection | Agent / task 可以返回 final answer；`/task` 中非 blocking tool error 可能被记录为 recovered 后完成 | 拆开 `RunStatus` 与 `VerificationVerdict`，用事件派生 evidence projection |
| P0/P1 | Checkpoint / Rewind | RFC-0024 V1 已实现：受控普通文件 exact batch restore、冲突审计、verification stale、完整 turn conversation fork 与 TUI Review 流程；不覆盖 shell/remote 副作用 | 后续只扩展可证明的 backend snapshot/unrevert 能力，不扩大 V1 承诺边界 |
| P0/P1 | Execution Sandbox | Permission、preview、workspace confinement 较强；`bash` / terminal 仍是本地进程 | 抽象执行后端、capability model、profile presets、fail-closed policy 和一个非交互 shell sandbox |
| P1 | Context Engine | Memory 主要是项目指令文件；LSP/code-intel 是模型主动调用工具 | Trust-labeled context archive、BM25、LSP inputs、secret/egress policy 和 token-budget packing |
| P1 | Task DAG + Reviewer/Verifier | `/task` 仍是 sequential orchestrator；普通子 agent 可并发但不等同于 task DAG | 只读并发、写任务隔离、显式依赖 schema、review、verify 和 bounded replanning |
| P1/P2 | Thread Projection + Agent Graph | Append-only JSONL 审计强，但缺少统一查询投影和平台化 agent graph 视图 | JSONL 作为 truth source，增加 thread/task/agent/cost/verification 可重建投影 |
| P2 | Tool Router Observability | Tool execution 有 control entries，但 dispatch、sandbox、network、token 和 tool routing trace 还不是统一产品视图 | 记录 turn/tool/sandbox/network/token/agent dispatch trace，并可供 TUI / eval / projection 使用 |
| P2 | Extension Trust Plane | MCP trust 已经较强；插件、自定义工具、技能包和 compaction hook 还需要统一信任边界 | 所有扩展注册、安装、执行、env 注入、egress 和 secret access 都有持久 trust decision |
| P2 | Structured Compaction + Task Memory | Compaction 是确定性本地文本摘要，保留角色/工具名和截断内容 | 结构化保存 objective、constraints、decisions、commands、verification、risks 和 unresolved issues |
| P2 | Crash Resume | 重启后能恢复历史并标记 interrupted；后台句柄和 mailbox 仍在进程内存 | 用 job intent、step lease、heartbeat、idempotency key 和 receipt 做 restart reconciliation |
| P2/P3 | Protocol / App Server Boundary | TUI / CLI 直接复用 runtime 和 kernel 装配；未来 IDE / daemon 还没有稳定协议层 | 在状态事件稳定后定义 `sigil-protocol`、app server 和多客户端 command/event surface |
| P2/P3 | Eval Harness | 主要靠手工任务体验和单元测试 | 用 repo-local eval 衡量 verified success、tool calls、token、approval 和 wall time |

## 4. Phase 0：Durable Event Foundation + Deterministic Eval

目标：先把 append-only control plane 的事件地基做稳，再在其上构建 verification、checkpoint、projection 和 crash resume。

当前问题：

- 当前 session JSONL 已统一使用 `StoredEvent` envelope，并具备 stream sequence、event id、schema/event version、correlation / causation 链路。
- 预发布格式边界只接受当前 V2 schema；旧顶层 `SessionLogEntry` 和旧 compaction payload 都不读取、不 upcast、不迁移。
- 后续工作应继续审计 append flush / sync、尾部恢复和跨进程单写者在新增 durable event 上是否保持一致，而不是重新引入旧格式 bridge。
- 后续阶段需要的 reducer、projection、evidence 和 crash reconciliation 目前缺少统一输入契约。
- 事件日志与文件副作用之间还缺少最小 crash-consistency 协议。
- 流式 reasoning/text delta、spinner 和短暂 tool progress 不应和 durable state change 混在同一事实日志里。
- 完整 model task eval 可以后置，但 deterministic conformance eval 必须前置。

交付物：

1. 引入 stored event envelope：

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

2. `stream_sequence` 只在同一个 session stream 内严格单调递增；`(session_id, stream_sequence)` 唯一。不要为跨 session 全局顺序引入全局锁或全局计数器。
3. 跨 session 关联依赖 `event_id`、`correlation_id`、`causation_id`、`occurred_at` 和 `parent_session_id`。
4. 区分 envelope 的 `schema_version` 与具体 payload 的 `event_version`，避免某个事件字段变化时升级整个日志 schema。
5. 明确三层事件：

```rust
enum DurableDomainEvent {
    ToolExecutionStarted,
    ToolExecutionFinished,
    WriteCommitted,
    VerificationRecorded,
    ApprovalResolved,
    TaskStatusChanged,
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

6. Durable Event Stream 只记录恢复、审计和投影需要的状态变化；流式 token、reasoning delta 和瞬时进度属于 Live Event，不进入事实日志。
7. 预发布 V2-only cutover：session JSONL 只接受 `StoredEvent`；顶层裸 `SessionLogEntry` 以结构化 compatibility error 拒绝，且不得被 tail recovery 截断、重写或当成空 session。
8. 正式发布后如需跨已发布 schema 演进，必须另立 migration RFC 定义 event upcaster 链路；当前预发布 V2 不提供 upcaster。未知事件处理策略：
   - 未知但非关键事件：保留原始 event，跳过相关 projection。
   - 影响权限、写入、验证或恢复的未知事件：fail closed。
   - 任何未知事件都不能静默丢弃。
9. `record_checksum` 覆盖除 checksum 自身外的不可变 event body，不只校验 payload。
10. checksum 使用 canonical serialization，至少覆盖：
   - `schema_version`
   - `event_type`
   - `event_version`
   - `event_id`
   - `session_id`
   - `stream_sequence`
   - `occurred_at`
   - `correlation_id`
   - `causation_id`
   - `parent_session_id`
   - `payload`
11. 定义单条 event 最大字节数、payload 最大嵌套深度和超限拒绝策略；checksum mismatch 与 JSON parse failure 使用不同错误类型。
12. 增加尾部半行或损坏最后一行恢复策略；中间损坏仍应明确报错并给出诊断。
13. Phase 0 不依赖完整 Artifact Store，先实现最小 `RecoveryQuarantineStore` 或保留 `session.jsonl.corrupt.<timestamp>`：
   - 保存原始损坏 JSONL 副本。
   - 记录原始 hash、文件权限和恢复元数据。
   - Phase 2 可再迁移到通用 Artifact Store。
14. 尾部恢复必须留下审计痕迹：
   - 取得独占锁。
   - 创建并 sync 损坏副本。
   - 截断到最后一个完整 event。
   - sync 原文件。
   - 追加 `LogTailRecovered` event。
   - 记录 original size、recovered size、discarded bytes、quarantine path 和原文件 hash。
15. 明确 append flush / sync policy 和单写者策略；跨进程写入需要 OS file lock 或显式拒绝。
16. 定义最小 crash-consistent mutation protocol，Phase 2 用于受控写工具：

```rust
struct MutationPrepared {
    operation_id: OperationId,
    before_hash: Option<String>,
    intended_after_hash: Option<String>,
    snapshot_coverage: SnapshotCoverage,
}

enum SnapshotCoverage {
    Captured(ArtifactId),
    NoPriorContent,
    SkippedSensitive,
    Unsupported,
    Unavailable,
}

struct MutationCommitted {
    operation_id: OperationId,
    observed_after_hash: Option<String>,
    workspace_revision: WorkspaceRevision,
}

struct MutationReconciled {
    operation_id: OperationId,
    observed_state: MutationObservedState,
    resolution: MutationResolution,
}
```

17. 受控写入流程的目标顺序：
   - 捕获 before state。
   - 持久化并 sync `MutationPrepared`。
   - 使用临时文件 + atomic replace 执行写入。
   - sync 文件及父目录。
   - 持久化 `MutationCommitted`。
18. 恢复时发现 `MutationPrepared` 但没有 `MutationCommitted`，必须读取当前文件 hash，判断未执行、已执行或状态未知，并追加 `MutationReconciled`。
19. Shell 无法预知副作用时，至少记录 execution start、执行结果、workspace scan 和 execution finish / interrupted。
20. 持久层可以使用 `event_type + serde_json::Value`，但 kernel 内部应保留强类型 `DomainEvent`；reducer 不应直接处理任意字符串和 JSON。
21. `record_checksum` 只用于发现意外损坏，不表示日志具备防恶意篡改能力，也不能宣传为 tamper-proof。
22. 定义 reducer 接口，后续 evidence、task、agent graph、checkpoint、projection 都基于 reducer 派生。
23. 建立 deterministic conformance tests：
   - final answer 后缺少 required check
   - check passed 后再次写入导致 verification stale
   - 子 agent 不能削弱 verification policy
   - approval denial
   - max turns
   - crash 写入半行 JSONL
   - `MutationPrepared` 后进程崩溃
   - 文件已写但 commit event 缺失
   - 合法 JSON 但 record checksum 错误
   - interrupted tool reconciliation
   - secret redaction
   - sandbox fail-closed

验收标准：

- V2 session log 能加载并产出 reducer 输入；旧顶层 JSONL 格式必须在任何写入前明确拒绝且保持字节不变。
- 尾部损坏恢复不会导致整个 session 不可读。
- 每个 durable event 都有稳定 stream sequence、event id、event type 和 event version。
- `record_checksum` 覆盖 event body，checksum 错误不会被当成普通 JSON parse failure。
- 尾部恢复后用户可以看到 `LogTailRecovered`，不会静默丢弃损坏尾部。
- 流式 token、reasoning delta 和瞬时进度不会写入 durable event stream。
- 受控写入 crash 后可以通过 prepared / committed / reconciled 事件解释文件状态。
- Deterministic fake provider / fake tool 能在 CI 中覆盖 verification 和 failure 状态机。
- 后续 projection store 可以删除后从 JSONL 重建。

建议验证：

```bash
cargo test -p sigil-kernel
cargo check
```

## 5. Phase 1：Verification Contract + Evidence Projection

目标：把“模型说完成”升级为“系统可判定完成状态”。

当前问题：

- Agent run 的终止条件主要是模型没有继续返回 tool call 后记录 final answer。
- `/task` step 里，普通 tool error 如果不是 blocking 且最终文本非空，仍可能被标记为 completed，并把错误记录为 recovered tool error。
- 当前状态混合了执行生命周期和验证结论，难以表达 cancelled、paused、stale verification 等情况。
- 当前没有统一的 evidence projection 来证明写入、命令、检查、todo 和 readiness audit 之间的因果关系。

交付物：

1. 引入 `VerificationPolicy`，至少表达：
   - `required_checks`
   - `completion_criteria`
   - `allow_unverified_completion`
2. 拆分执行生命周期和验证结论：

```rust
enum RunStatus {
    Running,
    Completed,
    Paused,
    Blocked,
    Failed,
    Cancelled,
    Interrupted,
}

enum VerificationVerdict {
    NotEvaluated,
    NotApplicable,
    Pending,
    Passed,
    Failed,
    Missing,
    Inconclusive,
    Stale,
    Skipped,
}
```

3. 用户可见状态由 `RunStatus + VerificationVerdict` 推导，例如：
   - `Completed + Passed` -> `Verified`
   - `Completed + Missing` -> `CompletedUnverified`
   - `Completed + Stale` -> `CompletedUnverified`
   - `Completed + Skipped` -> `CompletedUnverified`
   - `Completed + NotEvaluated` -> `CompletedUnverified`
   - `Completed + NotApplicable` -> `Completed`
   - `Failed + Failed` -> `FailedVerification`
   - `Blocked + Pending` -> `NeedsUser`
4. 状态约束：
   - `Pending` 只表示检查正在运行，终态 `RunStatus` 不能搭配 `Pending`。
   - `NotEvaluated` 表示尚未判定是否需要验证或尚未运行 readiness。
   - `Passed` 必须引用适用于当前 workspace snapshot 的 receipt。
   - `Skipped` 必须引用用户或 policy 的明确决定。
   - `Stale` 必须引用使验证失效的 mutation event。
5. 引入 evidence events，并把 `EvidenceProjection` 定义为投影视图而不是第二个事实来源：
   - `WriteRecorded`
   - `CommandFinished`
   - `CheckFinished`
   - `DiagnosticRecorded`
   - `TodoChanged`
   - `ReadinessEvaluated`
   - `WorkspaceMutationDetected`
6. 每条 receipt 至少表达：
   - `receipt_id`
   - `scope`
   - `producer_tool_call`
   - `workspace_revision` 或 `changeset_id`
   - `status`
   - `artifact_refs`
   - `redaction_state`
   - `recorded_at_stream_sequence`
7. 为工具和外部变化定义副作用分类：

```rust
enum ToolEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}

enum WorkspaceKnowledge {
    Clean(WorkspaceRevision),
    Dirty(WorkspaceRevision),
    UnknownDirty,
}
```

8. 定义验证相关 workspace 范围，避免构建产物或缓存目录让验证自我失效：

```rust
struct VerificationScope {
    include: Vec<PathPattern>,
    exclude: Vec<PathPattern>,
    tracked_files_only: bool,
    generated_roots: Vec<PathBuf>,
}

struct VerificationBinding {
    workspace_id: WorkspaceId,
    workspace_snapshot_id: WorkspaceSnapshotId,
    check_spec_hash: String,
    environment_fingerprint: String,
    sandbox_profile_hash: String,
}
```

9. `WorkspaceRevision` 只在明确的 workspace / worktree / snapshot stream 内有序，不能当成跨 session、跨 worktree 的全局顺序；验证最终以内容绑定的 `WorkspaceSnapshotId` 为准。
10. 默认 snapshot 范围可按以下规则生成：
   - Git tracked files。
   - 未忽略的新增源码文件。
   - 用户显式 include。
   - 排除 `.git`、Sigil 状态目录、常见 build/cache 目录、依赖缓存和生成产物根目录。
11. 会修改验证范围内文件的 check 只能产生 mutation evidence，不能作为最终 `Passed` evidence；修改后必须重新运行非写入检查。
12. 受控文件工具成功时产生精确 `WriteRecorded` 并推进 workspace revision。
13. 可能写入的 shell、MCP、插件或外部进程执行前后比较 verification scope 内的 workspace manifest / diff；检测到变化时产生 `WorkspaceMutationDetected`。
14. 如果无法覆盖变化范围或检测不可信，则设置 `WorkspaceKnowledge::UnknownDirty`，旧 verification 全部 `Stale`，不能标记为 `Passed`。
15. Readiness check 前至少重新计算关键 workspace fingerprint，以捕获用户或外部进程在 agent 运行期间的手工修改。
16. Verification contract 支持检查适用范围：
   - 纯问答 -> `NotApplicable`
   - 代码修改 -> build / test / lint
   - 文档修改 -> link / docs check
   - 配置修改 -> schema / parse check
17. Policy 继承规则：workspace、父任务、step、子 agent 逐层只能收紧：
   - `required_checks` = 父子集合并集
   - `allow_unverified_completion` = 父 && 子
   - `timeout` = min(父, 子)
18. 模型可以建议检查命令，但 readiness 由系统根据 evidence projection 计算，不能由模型自报。
19. Phase 1 先落最小 workspace trust gate；完整 Context Engine trust 仍在 Phase 4：
   - 未信任 workspace 只能发现候选检查，不能自动执行仓库配置、CI、Makefile、package scripts 或本地脚本。
   - 未信任 workspace 的检查执行必须经过显式审批，或运行在满足 policy 的 sandbox profile 中。
   - Context Engine 降级路径也不能绕过 workspace trust。
20. Repo gate 发现顺序：
   - `.sigil/verification.toml`
   - 用户显式配置
   - CI 配置
   - package scripts / Cargo / Makefile
   - 模型建议
   - 用户确认
21. 第一版可保守规定：verification scope 内任何写入或 `UnknownDirty` 都会使旧 verification stale；后续再按文件覆盖范围细化。
22. `/task` step 不再因为有 final text 就直接进入 verified completion。
23. tool error、approval denial、max turns、test failure、用户取消和进程中断进入明确状态语义。
24. 模型试图结束时可做 bounded final readiness check；拒绝 final answer 的次数、原因和补救要求必须写入 control state。
25. TUI 展示独立 badges，例如 `Run: Completed` 和 `Verification: Stale`，不要只显示一个合成状态。

验收标准：

- 用户能看出一次任务的 run status 与 verification verdict，且二者不会互相覆盖。
- 验证失败或 stale 不会被模型 final answer 覆盖成成功。
- 任务结束时能解释最后状态来自哪个 check、tool error、approval decision、cancel 或 interrupt。
- `NotEvaluated`、`Pending`、`Missing`、`Skipped` 和 `Stale` 的状态边界清楚，终态不会显示仍在 pending 的验证。
- `cargo test`、package install 或 coverage 产生的构建/缓存产物不会让刚完成的验证自我失效。
- 修改源码的 formatter、fixer、snapshot update 或 codegen 不能直接产出最终 `Passed` evidence。
- 未信任 workspace 中发现到的验证命令不会被自动执行。
- 受控代码写入有对应 `WriteRecorded`；未知或未覆盖写入会产生 `WorkspaceMutationDetected` 或 `UnknownDirty`，并使旧验证 stale。
- 写型子 agent 的最终状态会汇总到父任务 evidence projection，而不是只给一段自然语言结果。
- Verification 状态写入 append-only event stream，可在 resume 后恢复。

建议验证：

```bash
cargo test -p sigil-kernel
cargo test -p sigil-tui
cargo check
```

## 6. Phase 2：Checkpoint / Rewind

目标：为受控文件写入提供可审计、可冲突检测的 restore 能力，并通过 append-only
conversation fork 分离会话分支；unrevert / Bash snapshot 仅在 backend 能证明时后置扩展。

当前实现（RFC-0024 V1）：

- 已从现有 mutation evidence 派生 user-turn checkpoint，不新增重复 snapshot store。
- 已实现 exact id/digest、全量 preflight、batch restore、conflict evidence 和 verification stale。
- 已在 TUI Session Review 提供 preview/confirm、evidence inspect 与 complete-turn conversation
  fork；父 session 不改写，external provenance 会重绑到新 session scope。
- shell、MCP、network、database、remote、directory、rename 与 symlink 副作用仍明确不在
  restore 承诺内。

当前问题：

- 受控普通文件已有统一 checkpoint / rewind 用户能力；未受控副作用仍需 Git、backend
  snapshot 或手工恢复。
- Bash、formatter、migration script 这类命令的副作用不能仅靠编辑工具 preview 推断。
- Restore 本身也是新的 workspace mutation，不能复用 restore 前的 verification verdict。

交付物：

1. MVP 新增 append-only control entries：
   - `CheckpointCreated`
   - `FileSnapshotCaptured`
   - `MutationPrepared`
   - `MutationCommitted`
   - `MutationReconciled`
   - `CheckpointRestored`
   - `CheckpointRestoreConflict`
2. 受控 `write_file` / `edit_file` / `delete_file` 执行前后捕获最小必要 snapshot metadata：
   - path
   - before hash
   - after hash
   - previous content artifact id
   - sha256
   - size
   - redaction metadata
   - file existed / missing 状态
   - executable bit / basic permissions
   - symlink / external path 判定摘要
3. Snapshot 内容不直接写入 JSONL；JSONL 只保存 artifact id、hash、size 和 redaction metadata。
4. Artifact store 不放在 workspace 内，使用用户状态目录：
   - Linux: `$XDG_STATE_HOME/sigil/artifacts/<workspace-id>/<session-id>/`
   - macOS: `~/Library/Application Support/Sigil/artifacts/...`
   - Windows: `%LOCALAPPDATA%\\Sigil\\artifacts\\...`
5. Artifact metadata 至少表达：

```rust
struct ArtifactMetadata {
    id: ArtifactId,
    content_hash: String,
    size: u64,
    media_type: Option<String>,
    sensitivity: Sensitivity,
    encrypted: bool,
    created_by_event: EventId,
    retention_class: RetentionClass,
}
```

6. 最低安全要求：
   - secret-like 文件第一版默认不 snapshot；真正支持加密前必须先定义密钥管理、密钥丢失、导出/迁移和 headless 行为。
   - artifact 文件权限仅当前用户可读。
   - artifact 有 session / workspace 配额。
   - 支持自动过期和显式清理。
   - artifact 不自动进入模型上下文。
   - workspace 中最多保存不含敏感内容的 workspace id 或引用。
7. Artifact 清理也必须可审计：
   - `ArtifactExpired`
   - `ArtifactDeleted`
   - `ArtifactUnavailable`
   - 历史 event 仍保留 artifact metadata 和 content hash，但明确内容已不存在。
8. 正常受控写入使用 compare-and-swap：
   - 捕获 `before_hash`。
   - 准备新内容和 `intended_after_hash`。
   - commit 前确认当前 hash 仍等于 `before_hash`。
   - 通过临时文件 + atomic replace 写入。
   - sync 文件及父目录。
   - 追加 `MutationCommitted`。
9. Restore 时检查当前 hash：
   - 当前 hash 匹配写后 hash -> 自动恢复
   - 当前 hash 已变化 -> 记录 conflict 并要求用户确认
10. Restore 成功后必须追加 `CheckpointRestored` 和新的 `WriteRecorded` / `WorkspaceMutationDetected`，推进 workspace revision，并使旧 verification stale。
11. MVP 显式支持普通文件 create / update / delete；rename、hardlink、复杂 symlink、二进制大文件和权限完整恢复作为后续扩展。
12. TUI 提供 checkpoint 列表、影响文件、恢复预览和冲突确认入口。
13. 对 Bash 副作用明确分层：
   - LocalBackend 下不承诺 Bash rewind。
   - 只有具备 workspace snapshot / overlay / worktree 的 backend 才能声明 Bash side-effect rewind。
14. 后续扩展再考虑：
   - `StepSnapshotStarted`
   - `StepSnapshotFinished`
   - `PatchPartCaptured`
   - `ConversationForked`
   - `ConversationRewound`
   - `UnrevertApplied`
   - message / step / tool part 定位 restore target
15. Snapshot retention 和 artifact quota 可配置，避免把大文件或 secret-like 内容长期落盘。

验收标准：

- 非 Git workspace 中也能回退文件编辑工具造成的受控写入。
- Restore 本身进入 append-only control state，可审计、可恢复。
- Restore 不改写旧日志，projection 可以解释当前文件状态来自哪些补偿事件。
- Restore 后会推进 workspace revision，并使 restore 前的 verification stale。
- 受控写入崩溃后可以通过 `MutationPrepared` / `MutationCommitted` / `MutationReconciled` 判断文件是否已写入。
- 当前内容已变化时不会静默覆盖用户或其他 agent 的后续修改。
- Snapshot 与写入之间发生外部修改时，compare-and-swap 会阻止静默覆盖。
- Artifact 内容被 retention 删除后，历史事件仍可解释 artifact 曾经存在、hash 是什么、为什么不可用。
- Bash 副作用如果无法完整覆盖，TUI 和文档必须明确标注不可回退范围。
- Snapshot 不绕过 secret redaction、external directory policy 或 workspace confinement；artifact 不被 Context Engine 自动召回。

建议验证：

```bash
cargo test -p sigil-kernel
cargo test -p sigil-tools-builtin
cargo test -p sigil-tui
```

## 7. Phase 3：Execution Sandbox

目标：把 permission policy 和执行隔离拆开，先完成 non-interactive shell 的真实隔离边界。

当前问题：

- 文件工具、审批、preview 和 workspace confinement 已经较强。
- `bash` 和 persistent terminal 仍是在 workspace root 下启动的本地进程；persistent terminal 涉及 PTY、长进程、resize、kill 和恢复，应独立于第一版 sandbox。
- Permission policy 只能决定是否执行，不能强制限制执行后的文件、网络、PID、CPU、内存或 secret 访问边界。
- Sandbox 不能 fail open。隔离 backend 不可用时，默认不应静默降级为无隔离执行。

交付物：

1. 抽象执行后端：

```rust
trait ExecutionBackend {
    async fn execute(...);
    async fn start_terminal(...);
    async fn kill(...);
    async fn collect_artifacts(...);
}
```

2. 保留 `LocalBackend`，保持现有兼容行为。
3. 增加 backend capability model：

```rust
struct BackendCapabilities {
    filesystem_isolation: bool,
    network_isolation: bool,
    process_isolation: bool,
    resource_limits: bool,
    persistent_pty: bool,
    workspace_snapshot: bool,
}
```

4. 增加至少一个非交互 shell 隔离 backend，优先顺序：
   - macOS Seatbelt backend
   - Docker / Podman backend
   - Linux Bubblewrap backend
   - Windows restricted backend
5. Backend 负责按 capability 强制执行：
   - workspace mount
   - network policy
   - environment / secret injection
   - CPU / memory / wall-clock limit
   - PID/process cleanup
   - scratch/artifact collection
6. 增加 sandbox profile，而不是只给裸 read roots：

```toml
[sandbox]
profile = "build_offline"
fallback = "deny" # deny | prompt | unconfined
```

7. 内置 profiles：
   - `read_only_repo`
   - `workspace_write`
   - `build_offline`
   - `build_networked`
   - `unconfined`
8. 依赖缓存、工具链、SDK、系统动态库和临时目录通过只读 mount profile 显式建模；不要把 `read_roots = ["workspace"]` 写成通用默认。
9. Read-only agent 的执行后端必须以只读 workspace mount 或等价策略 enforcing read-only；shell 重定向、脚本、formatter 不能绕过 agent capability。
10. 第一版覆盖 non-interactive `bash`；persistent terminal sandbox 后续单独阶段处理。
11. 未来 background process、插件和需要本地进程的 MCP stdio path 统一通过 backend 装配。
12. 远端 MCP 工具或不受本地 backend 控制的外部服务，必须明确标记为 outside local sandbox，并继续走 trust、egress、secret 和 network 审计。
13. 网络访问从 sandbox policy 中单独形成 approval / receipt，不能只隐含在 shell 命令审批里。

验收标准：

- LocalBackend 行为保持兼容，默认迁移不破坏现有 TUI 使用。
- 隔离 backend 能证明 workspace mount、network policy、env redaction 和超时限制生效；不支持的 capability 不能被宣传为已生效。
- 如果配置要求 sandbox，而 backend 不可用，默认结果是 deny 或 prompt，不是静默 unconfined。
- TUI approval 和 tool card 能显示执行后端及关键隔离策略。
- MCP、插件或远端工具的 tool card 能显示它是否受本地 sandbox 控制，不能把 shell sandbox 文案套到所有工具上。
- Read-only agent 即使获准执行 shell，也不能写入 workspace 或 external directory。
- 文档不再把 permission 描述成 sandbox。

建议验证：

```bash
cargo test -p sigil-tools-builtin
cargo test -p sigil-runtime
cargo test -p sigil-kernel
```

## 8. Phase 4：Context Engine

目标：从“模型主动查代码”升级为“系统主动组装高质量上下文”。

当前问题：

- Memory 主要读取 `SIGIL.md`、`AGENTS.md`、`CLAUDE.md`、`SIGIL.local.md` 这类项目指令文件。
- Request 构建基本由 workspace memory、projected conversation 和 transient messages 组成。
- Code intelligence 已有 LSP / Tree-sitter 工具，但更像模型主动调用的精确查询工具，不是每轮自动工作的 context assembly。
- 当前 compaction 还没有形成 recent-turn retention、old tool output pruning、summary reuse 和 hook 输入边界的完整策略。
- 自动上下文如果不区分 trust level，检索到的源码、README 或工具输出可能被误当成高优先级指令。
- 自动召回 credential、private key、`.env` 或外部目录内容，本质也是模型数据出口，必须经过 secret / egress 策略。
- 新克隆或未知 workspace 中的项目指令文件也可能包含提示注入，不能无条件升级为受信任指令。
- 第一阶段不需要直接上 embedding 或完整 code graph；更可控的路径是先把 session archive、BM25 和 token budget packer 做稳。

交付物：

1. 建立 context archive：
   - compacted messages archive
   - tool observation archive
   - evidence receipt archive
   - decision / constraint archive
   - patch / revert archive
2. 建立本地 BM25 retrieval：
   - session history retrieval
   - memory retrieval
   - repo file retrieval
   - symbol/name retrieval
3. 建立 repo index：
   - file summary
   - symbol index
   - import/reference hints
   - recent changes
   - diagnostics snapshot
   - lightweight dependency hints
4. 自动相关文件召回：
   - lexical search
   - BM25 archive retrieval
   - symbol / LSP graph
   - definition / references
   - diagnostics relevance
   - git diff and changed files
   - current task hints
5. Context packing：
   - trust label
   - sensitivity label
   - egress decision
   - repo revision
   - recent turns retention
   - old tool output pruning
   - summary reuse
   - `ContextDigestV0`
   - token budget
   - priority ranking
   - stale context eviction
   - deterministic ordering
   - inclusion/exclusion reason
6. 跨 session repo knowledge：
   - decisions
   - recurring errors
   - verified commands
   - project-specific conventions
   - success / failure patterns
7. 定义 workspace trust：

```rust
enum WorkspaceTrust {
    Unknown,
    Trusted,
    Restricted,
    Denied,
}
```

8. 只有用户信任 workspace 后，`SIGIL.md` / `AGENTS.md` / `SIGIL.local.md` 才能作为 workspace instruction；否则它们只是 untrusted repository data。
9. Workspace trust 同时控制：
   - 项目本地指令。
   - 项目本地插件。
   - 本地 MCP 配置。
   - Extension manifest。
   - 自动执行脚本。
   - Secret / 环境变量暴露。
10. 区分 context trust levels：
   - workspace instruction：仅在 `WorkspaceTrust::Trusted` 后作为受信任指令
   - retrieved source code：不受信任数据
   - tool output：不受信任观察结果
   - extension context：按扩展 trust decision 判定
11. 明确稳定前缀与动态后缀排序：可信项目指令和稳定 system context 尽量保持 prefix-cache 友好，自动召回材料进入动态后缀。
12. Phase 4 MVP 只允许内置 Context Source；extension context hook 和 compaction hook 延后到 Phase 7 的 trust plane 完成后接入。
13. 保留 code-intel 作为精确查询工具，同时增加每轮可解释的 automatic context assembly。
14. `ContextDigestV0` 只服务 context packing，是确定性、最小摘要：

```rust
struct ContextDigestV0 {
    objective: Option<String>,
    active_files: Vec<PathBuf>,
    recent_commands: Vec<CommandReceiptId>,
    verification_state: VerificationVerdict,
    unresolved: Vec<String>,
}
```

验收标准：

- Agent request 能解释自动带入了哪些文件、symbol、diff 或历史决策。
- BM25 检索结果有 source、score、snippet、token cost 和 truncation metadata。
- 自动上下文不会无界增长，也不会绕过 workspace confinement。
- 自动召回的每个 context source 都有 source、trust_level、sensitivity、egress_decision、repo_revision、token_cost 和 inclusion_reason。
- 检索到的源码、README、依赖文件或工具输出不能提升为 system instruction。
- 未信任 workspace 中的 `SIGIL.md`、`AGENTS.md`、README 和源码注释不能以受信任指令身份进入 prompt。
- Workspace trust 能阻止项目本地插件、MCP 配置、自动脚本和 secret/env 暴露被静默启用。
- Secret-like 内容和外部目录内容进入 provider context 前必须有显式 egress decision。
- TUI 可展示 context sources、token budget 和主要排除原因。
- LSP symbols、references、diagnostics 和 code action hints 能作为 Context Engine 输入，而不是只作为模型主动调用工具。
- Tool output pruning 不会删除 audit log，只影响 provider context materialization。
- Phase 4 不接入 extension hook；后续 hook 必须先满足 Phase 7 trust plane。
- Context engine 失败时降级为当前 memory + tool 模型，不阻塞普通 chat。
- `ContextDigestV0` 不能创造新的 evidence；它只能引用 event id、receipt id 或 artifact id。
- Embedding、semantic retrieval、call graph 和 impact graph 作为后续增强，不作为 MVP 前置条件。

建议验证：

```bash
cargo test -p sigil-kernel
cargo test -p sigil-code-intel
cargo test -p sigil-runtime
```

## 9. Phase 5：Task DAG + Reviewer / Verifier

目标：把 `/task` 从顺序执行器升级为可审计的任务编排器。

当前问题：

- 当前 `/task` 以 `SequentialTaskOrchestrator` 为核心，按 pending steps 顺序执行。
- 一个 step 非 completed 后任务停止，等待显式 continue。
- 普通聊天中的后台子 agent 可以并发，但 `/task` 计划执行本身还不是 DAG scheduler。
- 配置中存在 replanning 相关字段，但 task orchestrator 还没有形成真正的 bounded replanning。
- 并行写 agent 如果没有 worktree、changeset 或 write lease 隔离，会产生文件覆盖和验证污染风险。

交付物：

1. Task plan 支持 DAG：
   - step dependencies
   - ready queue
   - blocked reason
   - durable step attempts
   - parent / child agent relation
2. 模型可见 task schema 必须完整暴露依赖字段，例如 `depends_on`；代码支持的调度语义不能藏在未声明字段里。
3. Task mode 显式区分：
   - `read`
   - `write`
   - `review`
   - `verify`
4. Read-only steps 可并发执行。
5. Write steps 默认不并发；需要并发时必须使用：
   - independent worktree
   - changeset isolation
   - write lease
   - merge review
6. 提供 canonical pipeline templates，而不是强制所有 task 都经历四个阶段：
   - 代码修改：Explore -> Implement -> Review -> Verify
   - 纯研究：Explore -> Review
   - 文档任务：Explore -> Implement -> Verify
   - 简单配置修改：Implement -> Verify
7. Reviewer 可以是模型 agent；Verifier 必须以系统 Verification Contract 为最终依据，不能让自然语言回答覆盖系统检查结果。
8. `max_replans` 接入 orchestrator，失败后支持 bounded replanning。
9. DAG 调度必须包含：
   - cycle detection
   - stable step id
   - superseded step status after replan
   - max concurrency
   - total token / cost budget
   - dependent step cancellation
   - merge conflict status
10. 子 agent 在独立 worktree 或 changeset 中的 verification 只绑定 child workspace snapshot；合并到 parent 后会产生新的 parent workspace revision，必须重新运行 parent required checks。
11. TUI 展示 DAG 状态、等待原因、并发子任务、review finding 和 verification result。
12. 为后续投影保留 agent graph 数据：
   - agent id
   - parent id
   - role
   - status
   - context mode
   - budget
   - workspace / changeset isolation mode
   - terminal verdict

验收标准：

- 只读步骤可以并发，但写入步骤不能互相踩 workspace。
- 写型并行任务必须先产生 isolated changeset，再由 merge agent 或主任务审核合并。
- Task tool schema、planner prompt、TUI 展示和 runtime 调度语义保持一致。
- Review / Verify 阶段有独立状态，不被 implementation final answer 合并掉。
- Child verification 不会被错误继承为 merge 后 parent workspace 的 `Passed` evidence。
- Replan 次数、原因、旧 plan 和新 plan 都进入 append-only control state。
- Replan 后旧 step 会进入 superseded，而不是被覆盖或删除。
- Agent graph 可以从 control log 重建，不依赖进程内 supervisor 状态。
- Resume 后能重建 task graph 的 terminal / non-terminal 状态。

建议验证：

```bash
cargo test -p sigil-kernel
cargo test -p sigil-runtime
cargo test -p sigil-tui
```

## 10. Phase 6：Thread Projection + Agent Graph Observability

目标：保留 append-only JSONL 作为事实来源，同时提供可查询、可重建的产品视图和执行 trace。

当前问题：

- Session/control JSONL 的审计语义很强，但产品层查询、线程列表、agent graph、token/cost 统计和 verification 状态查询仍主要依赖运行时投影或局部扫描。
- 多 agent、task、sandbox、network approval 和 tool dispatch 的事件已经分散存在，但还没有统一的 materialized view。
- 未来 IDE、daemon 或桌面端需要稳定的线程/任务/agent 查询面，不能直接读取 kernel 内部对象。

交付物：

1. 保持 JSONL 为 truth source，不改成以 SQLite 为事实来源。
2. 增加可重建投影：
   - thread index
   - task projection
   - agent graph projection
   - verification projection
   - cost / token projection
   - checkpoint projection
   - context source projection
3. 增加 tool / agent dispatch trace：
   - turn id
   - model request id
   - tool call id
   - sandbox decision
   - network approval
   - egress receipt
   - observation size / truncation
   - token usage
   - completion verdict
4. Projection schema 必须可版本化，并能从 V2 JSONL 重建。
5. 明确 live state 与 projection 边界：
   - 活跃 turn、tool、approval 使用 live event bus / runtime state。
   - 历史 session、task、cost、agent graph 使用 projection。
   - Resume 后通过 projection + reducer 重建 live state。
6. 每个 projection 保存：
   - `session_id`
   - `last_applied_stream_sequence`
   - `projection_schema_version`
7. Projection 幂等应用规则：
   - `sequence <= last_applied` -> 忽略
   - `sequence == last_applied + 1` -> 应用
   - `sequence > last_applied + 1` -> 报告 gap 并停止推进
8. Event apply 和 cursor 更新必须在同一数据库事务中完成。
9. Eval harness 和 crash resume 共享同一套 projection，不各自重建局部索引。

验收标准：

- 删除 projection store 后，可以从 JSONL 完整重建。
- Projection lag、rebuild failure 和 schema version mismatch 有明确诊断。
- Projection 重启或重复消费不会重复累计 token、重复插入 agent 或重复显示 checkpoint。
- Projection gap 会 fail closed，而不是跳过缺失 event 后继续推进。
- Agent graph、verification state、token/cost 和 checkpoint 列表可以稳定查询。
- Dispatch trace 能把一次 turn 中的 model request、tool routing、sandbox/network decision、observation 和 final verdict 串起来。
- 活跃审批和工具执行不依赖可能滞后的 projection 作为唯一事实来源。
- Projection 不保存未经 redaction 的 secret 或外部工具大输出。

建议验证：

```bash
cargo test -p sigil-kernel session
cargo test -p sigil-runtime projection
cargo test -p sigil-tui
```

## 11. Phase 7：Extension Trust Plane

目标：把插件、自定义工具、技能包、compaction hook 和外部服务集成都纳入统一安全与审计模型。

当前问题：

- MCP trust、egress 和 elicitation 已经有较明确的控制面。
- 未来插件、自定义工具、技能包和 context / compaction hook 会扩大本机代码执行、环境变量读取和外部服务调用边界。
- 如果扩展只作为“配置加载的代码”运行，会绕过现有 tool approval、secret-egress 和 durable audit 的产品心智。

交付物：

1. 定义扩展能力模型：
   - custom tool registration
   - event hook
   - context hook
   - compaction hook
   - env injection
   - network access
   - filesystem access
2. 定义 `ExtensionTrustDecision`：
   - source
   - version / digest
   - install scope
   - allowed capabilities
   - secret access
   - network policy
   - approval default
3. 扩展 trust 必须发生在加载可执行代码之前：
   - 先读取无需执行代码的静态 manifest。
   - 校验 version、content digest、source 和声明 capabilities。
   - 用户确认 trust decision。
   - 再启动隔离插件进程或注册工具。
4. Manifest 至少包含：
   - version pin
   - content digest
   - install receipt
   - capability manifest
   - update trust invalidation rule
5. 扩展内容、版本或 digest 变化后，旧 trust decision 失效或需要重新确认。
6. 扩展注册、加载、执行和卸载都写入 append-only control state。
7. 扩展工具统一走 `ToolSpec`、permission、ExecutionBackend、egress 和 secret policy。
8. npm / JS / TS / 本地脚本类扩展不得默认自动安装并执行；首次运行需要 trust decision。
9. TUI 展示 extension 来源、能力、最近执行、egress 和 secret access 摘要。

验收标准：

- 扩展不能在未授权情况下读取 secret、注入 env 或发起网络访问。
- 扩展提供的工具和内置/MCP 工具在 approval、egress 和 audit 上使用同一套控制面。
- Compaction hook 和 context hook 的输出有 source attribution、size limit 和 redaction。
- 不执行扩展代码也能完成 manifest 读取、digest 校验和 trust prompt。
- 删除扩展后，历史 trust decision 和 execution receipt 仍可审计。

建议验证：

```bash
cargo test -p sigil-kernel permission
cargo test -p sigil-runtime extension
cargo test -p sigil-tui config
```

## 12. Phase 8：Structured Compaction + Task Memory

目标：让长任务压缩后仍保留关键决策、失败尝试和验证证据；这里承载长期 `TaskMemoryV1`，不与 Phase 4 的 `ContextDigestV0` 混用。

当前问题：

- 当前 compaction 是确定性本地摘要，优点是便宜、稳定、可复现。
- 摘要主要保留角色、工具名和每条消息的一小段文本。
- 长任务中容易丢失决策原因、失败尝试、关键约束和跨文件关系。

交付物：

1. 引入长期结构化记忆：

```rust
struct TaskMemoryV1 {
    branch_id: Option<BranchId>,
    valid_for_snapshot: WorkspaceSnapshotId,
    supersedes: Option<TaskMemoryId>,
    source_event_ids: Vec<EventId>,
    objective: String,
    constraints: Vec<SourcedFact>,
    decisions: Vec<SourcedDecision>,
    files_changed: Vec<FileChangeRef>,
    commands_run: Vec<CommandReceiptId>,
    verification_results: Vec<VerificationReceiptId>,
    failed_attempts: Vec<AttemptRef>,
    risks: Vec<SourcedFact>,
    unresolved_issues: Vec<SourcedFact>,
}
```

2. Compaction record 支持 typed summary 和人类可读 summary。
3. Agent request builder 能把结构化摘要参与 context packing。
4. 支持 old tool output pruning 的结构化摘要，保留工具名、状态、关键 metadata、truncation 和 retrieval handle。
5. 可选生成 planner-oriented IR，记录策略、约束、风险、证据和下一步候选动作。
6. TUI 可查看 compacted task memory，不把所有细节重新刷进 transcript。
7. Summary 更新保持 append-only，避免不可审计的覆盖式改写。
8. Compaction 或模型摘要不能创造新的 evidence，只能引用 event id、receipt id 和 artifact id。
9. 模型生成或推断内容必须标记为 `inferred`、`model_generated` 或 `unverified`，不能产生 `VerificationVerdict::Passed`。
10. Revert、fork、worktree 或 branch switch 后，旧 memory 不能默认适用于新 snapshot；必要时追加：
   - `MemoryInvalidated`
   - `MemorySuperseded`

验收标准：

- Compaction 后，agent 仍能知道任务目标、硬约束、已改文件、已跑命令和未解决问题。
- 验证结果和失败尝试不会被截断文本吞掉。
- Evidence receipt 和关键 tool observation 可以被 Context Engine 召回。
- Pruned tool output 可以通过 archive/retrieval handle 定位原始审计记录。
- Task memory 与 `ContextDigestV0` 字段边界清楚，不出现两个不兼容 summary 类型。
- Task memory 绑定 branch / snapshot；不同分支或 restore 后的事实不会静默混用。
- 模型摘要不能覆盖旧摘要，只能追加新 memory 或 supersede 旧 memory。
- 预发布 cutover 不读取、upcast 或迁移旧 compaction payload：顶层裸 `SessionLogEntry` 与携带旧 `CompactionRecord` 的 V2 envelope 都必须在任何恢复或追加前 fail closed，且源文件保持不变。

建议验证：

```bash
cargo test -p sigil-kernel
cargo test -p sigil-tui
```

## 13. Phase 9：Crash Resume

目标：从“可继续历史”升级为“可恢复任务控制面”。

当前问题：

- Session load 会把没有终态的 tool execution 和 agent attempt 标记为 interrupted。
- Supervisor 的 active threads、mailbox、background handles 仍在进程内存。
- 进程终止后，系统恢复的是历史和状态，不是让原后台 agent 从原执行点透明继续。

交付物：

1. 持久化 job intent log，记录用户目标、task id、agent profile、tool policy 和 expected side effects。
2. 持久化 step lease，包含 owner、deadline、heartbeat 和可恢复性。
3. 为 tool execution 增加 idempotency key 和 tool receipt，用于重启后 reconcile 已开始、已完成、未知状态。
4. 后台 agent attempt 状态可重建。
5. Mailbox 事件进入 append-only control state。
6. 进程重启后区分：
   - `resumable`
   - `interrupted_needs_user`
   - `abandoned`
7. Terminal/process 类任务默认不透明恢复，但必须给出明确恢复动作。
8. TUI 启动时展示可恢复任务列表、恢复风险和建议动作。
9. Restart reconciliation 不能自动重放写工具或 shell 命令，除非 tool receipt 证明幂等且用户/策略允许。

验收标准：

- 进程重启后不会误报后台任务仍在运行。
- 能恢复的 task 有明确 resume action；不能恢复的 task 有明确 interrupted reason。
- Step lease 过期、heartbeat 丢失和 tool receipt 缺失会产生不同的恢复状态。
- Mailbox 中未处理的用户消息不会静默丢失。
- Crash resume 不会自动重放写工具或 shell 命令。

建议验证：

```bash
cargo test -p sigil-kernel session
cargo test -p sigil-runtime
cargo test -p sigil-tui
```

## 14. Phase 10：Protocol / App Server Boundary

目标：在核心状态、事件和投影稳定后，为 TUI、CLI、未来 IDE / daemon / desktop 提供共享协议层。

当前问题：

- TUI / CLI 复用 runtime 和 kernel 装配，但还没有一个稳定的 command/event protocol。
- 如果过早拆分 app server，容易为了未来客户端形态引入不必要 crate 和协议负担。
- 如果长期不抽协议，IDE、daemon 和远程控制面会直接耦合 kernel/runtime 内部结构。

交付物：

1. 定义 `sigil-protocol` 的最小 command surface：
   - `StartTurn`
   - `ApproveTool`
   - `CancelTurn`
   - `SpawnAgent`
   - `ContinueTask`
   - `RestoreCheckpoint`
   - `RevertSession`
   - `UnrevertSession`
2. 定义最小 event surface：
   - `ReasoningDelta`
   - `ToolStarted`
   - `ToolCompleted`
   - `VerificationUpdated`
   - `AgentStatusChanged`
   - `ContextSourcesUpdated`
   - `SandboxDecisionRecorded`
3. Protocol event 可以来自 durable event view 或 transient live event view；durable events 支持 cursor 重放，transient events 不保证重放。
4. 定义 command envelope：

```rust
struct CommandEnvelope<T> {
    protocol_version: u16,
    command_id: CommandId,
    client_id: ClientId,
    session_id: SessionId,
    expected_stream_sequence: Option<u64>,
    correlation_id: Option<EventId>,
    payload: T,
}
```

5. `command_id` 用于客户端重试去重，`expected_stream_sequence` 用于防止旧客户端覆盖新状态，`client_id` 用于审计操作来源，`correlation_id` 串联 command 与 durable events。
6. Approval command 必须携带：
   - `approval_request_id`
   - `tool_call_hash`
   - `policy_version`
   - `expires_at`
7. Tool call 参数变化、approval 过期或 policy version 不匹配时，旧审批必须拒绝，不能批准新参数。
8. SSE 支持 `Last-Event-ID` / event cursor；客户端断线后可以补齐 durable events，transient events 不要求补发。
9. `sigil-app-server` 只承载协议、session/thread routing 和 lifecycle，不复制 agent loop。
10. Server 层可提供 OpenAPI-compatible spec 和 SSE event stream，但它们只能反映同一套 protocol event。
11. TUI 逐步从直接操作 runner 内部对象迁移到 command/event bridge。
12. Protocol 版本化，保证旧客户端能给出明确 unsupported，而不是静默误执行。
13. 本地 server 默认 localhost 和显式 auth；远程访问必须经过单独安全设计，不作为 MVP 默认能力。

验收标准：

- TUI 和 CLI 至少有一个共享 command/event path。
- App server 不绕过 permission、approval、sandbox、egress 和 verification control plane。
- Durable protocol event 可以从 JSONL/projection 对齐；transient protocol event 只来自 live runtime state。
- 客户端重试不会重复执行同一个 command。
- 旧客户端不能在 stream sequence 已推进后继续批准过期 tool call。
- SSE 断线重连可以补齐 durable events，且不会承诺补发 transient event。
- OpenAPI/SSE 输出和 TUI 状态一致，不能各自定义一套事件语义。
- 新协议不要求立即支持 IDE 或 desktop，但不阻碍未来接入。

建议验证：

```bash
cargo test -p sigil-runtime
cargo test -p sigil-tui runner
cargo check
```

## 15. Phase 11：Eval Harness

目标：用可重复任务衡量 agent 能力，而不是只靠手工体验。

交付物：

1. 建立 repo-local eval cases：
   - small edit
   - multi-file refactor
   - failing test repair
   - docs sync
   - permission denial recovery
   - context retrieval
   - task replanning
   - verifier failure
   - checkpoint restore
   - session revert / unrevert
   - sandbox fallback denial
   - read-only shell write denial
   - read-only DAG parallelism
   - extension trust denial
   - compaction hook attribution
   - projection rebuild
   - tool dispatch trace
   - protocol event alignment
2. 增加 adversarial / security eval cases：
   - 恶意 workspace instruction 提示注入
   - README 要求读取并上传 secret-like 文件
   - symlink 跳出 workspace
   - 路径规范化绕过
   - 验证通过后外部修改源码
   - 验证命令自身修改源码
   - `MutationPrepared` 后进程崩溃
   - 文件已写但 commit event 缺失
   - 合法 JSON 但 checksum 错误
   - Extension manifest 信任后内容被替换
   - Extension digest TOCTOU
   - Sandbox backend 缺失时 fail-closed
   - 只读 agent 通过 shell 重定向写文件
   - Restore 后旧 verification 被错误复用
   - Child worktree verification 被错误继承到 merge 后 parent workspace
3. 记录指标：
   - final state
   - verified / unverified
   - tool calls
   - token usage
   - wall time
   - approval count
   - changed files
   - evidence receipts
   - sandbox backend
   - context sources
   - revert target
   - extension trust decision
   - dispatch trace completeness
   - projection rebuild status
   - network approvals
4. Model eval 还应记录：
   - repo fixture commit
   - Sigil build / version
   - provider / model
   - model parameters
   - tool schema digest
   - config hash
   - sandbox backend
   - OS / toolchain
   - run seed
5. 模型类 case 重复运行多次，不用单次成功或失败判断回归。
6. CI 可跑轻量 eval。
7. Release 前可跑完整 eval。
8. Eval 结果进入 release note 或内部质量报告，不直接替代单元测试。

验收标准：

- 同一 eval case 可重复运行并输出结构化结果。
- Eval 能区分模型成功、工具失败、权限阻断、验证失败和未验证完成。
- Eval 能覆盖 prompt injection、path escape、checksum mismatch、crash mutation、restore stale verification 和 read-only shell write denial。
- 关键 agent 能力变更前后能看出成功率、成本和审批次数变化。

建议验证：

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## 16. 推荐执行顺序

1. Durable Event Foundation
2. Deterministic conformance eval
3. Verification Contract
4. Evidence projection across foreground chat, `/task` and subagents
5. Controlled-write checkpoint / restore MVP
6. ExecutionBackend abstraction and LocalBackend migration
7. Sandbox fail-closed policy and one non-interactive shell backend MVP
8. Context Archive + typed summary + BM25 + token packer MVP
9. Sequential `/task` verifier integration
10. Read-only Task DAG + Reviewer / Verifier
11. Thread Projection + Agent Graph Observability
12. Extension Trust Plane
13. Structured Compaction + Task Memory
14. Crash Resume
15. Protocol / App Server Boundary
16. Model Task Eval Harness

Context Engine 和 Execution Sandbox 的先后可以按产品阶段调整：

- 如果近期重点是提升任务成功率，优先 Context Engine MVP。
- 如果近期重点是扩大真实用户使用面或运行更高风险命令，优先 Sandbox backend。

Durable Event Foundation、deterministic eval、Verification Contract 和 Evidence Projection 不建议后移。它们是后续 checkpoint、sandbox、task DAG、projection、crash resume 和 eval harness 的共同状态基础。

第一批 RFC 建议：

1. RFC-0001 Durable Event Stream and Event Taxonomy
2. RFC-0002 Crash-consistent Mutation Protocol
3. RFC-0003 Verification Contract and Workspace Snapshot
4. RFC-0004 Controlled-write Checkpoint
5. RFC-0005 Execution Backend
6. RFC-0006 Workspace Trust and Context Engine

## 17. 三个月目标

第一个月：

- 完成 Durable Event envelope、schema version、stream sequence、event id 和 V2-only session format gate。
- 完成 JSONL 尾部损坏恢复、append flush / sync policy 和单写者策略。
- 完成 deterministic conformance eval，覆盖 verification stale、policy 继承、approval denial、max turns、interrupted reconciliation、secret redaction 和 sandbox fail-closed。
- 完成 Verification Contract 和 Evidence Projection MVP。
- TUI / CLI 明确区分 run status 与 verification verdict。
- 修正 `/task` step completion 语义，不再由 final text 单独决定。

第二个月：

- 完成 controlled-write checkpoint / restore MVP。
- 完成 artifact store、hash conflict check 和 restore conflict confirmation。
- 落地 `ExecutionBackend` 抽象。
- 完成 LocalBackend 迁移。
- 完成 fail-closed sandbox policy。
- 尽力完成一个操作系统上的非交互 shell sandbox backend MVP。
- 完成 sandbox conformance tests。

第三个月：

- 完成 recent turn retention、old tool output pruning 和最小 `ContextDigestV0`。
- 完成 BM25 session / repo retrieval。
- 完成 token budget packer。
- 完成 context provenance UI，展示 source、trust_level、sensitivity、egress_decision、token_cost 和 inclusion_reason。
- 将 sequential `/task` 接入系统 verifier。
- Read-only DAG 作为 stretch goal；Write DAG、完整 Session Fork / Unrevert、Persistent Terminal Sandbox、SQLite Projection、Extension Ecosystem、Crash Resume leases 和 App Server 移出三个月承诺。

周切断点：

| 周 | 必须交付 |
| --- | --- |
| 1 | Event envelope、V2-only format gate、stream sequence |
| 2 | Tail recovery、reducer、fake provider / fake tool |
| 3 | `RunStatus`、`VerificationVerdict`、evidence events |
| 4 | `/task` 与 TUI verification 状态接入 |
| 5 | Artifact store、受控写 checkpoint |
| 6 | Restore、hash conflict、TUI preview |
| 7 | `ExecutionBackend`、LocalBackend 迁移 |
| 8 | Sandbox conformance、一个操作系统上的 non-interactive backend spike / MVP |
| 9 | Context archive、tool output pruning |
| 10 | BM25 session / repo retrieval、token packer、context provenance、feature freeze |
| 11 | Migration、regression、security / adversarial eval |
| 12 | Hardening、文档同步、release candidate review |

季度砍线：

- 第一个月至少完成 Event Foundation + Verification Kernel；TUI 美化可以后移。
- 第二个月 checkpoint 必须完成；sandbox backend 不稳定时只交付 ExecutionBackend + fail-closed contract。
- 第三个月 context provenance 和 verifier 必须完成；Read-only DAG 继续保持 stretch goal。
- 第 10 周 feature freeze；第 11-12 周不再接新功能，只做 hardening、migration、regression、安全测试和文档。
- 必须完成：Event Foundation、Verification、Checkpoint、ExecutionBackend、Context provenance。
- 尽力完成：一个操作系统上的 sandbox backend。
- Stretch：BM25 repo retrieval 完整优化、Read-only DAG。

## 18. 每阶段通用完成定义

每个阶段完成前至少确认：

1. 影响 crate 已列清楚。
2. 是否影响 TUI、CLI、provider、tool、session、approval 或 persistence 已说明。
3. 新增公共类型有明确职责和边界。
4. Session/control state 仍保持 append-only、可恢复、可审计。
5. TUI 状态、事件流、键位提示和文档同步完成。
6. 至少跑过相关 gate；如果未跑，必须在交付说明中写明。
7. README、`dev/governance/*` 或架构方案没有继续宣传尚未完成的能力。

数据生命周期原则：

- Append-only 表示单个 session stream 内不能静默改写历史；删除整个 session、workspace index、cross-session memory、artifact 或 projection cache 是单独的数据生命周期能力。
- 普通删除优先使用 logical tombstone + projection rebuild；artifact 内容、secret-like 内容和用户要求清除的数据必须支持物理删除。
- Artifact retention、projection cleanup、extension uninstall 后的数据、workspace trust 撤销后的索引和 memory 都必须有可解释状态。
- 历史 event 引用已删除 artifact 时，应显示 `ArtifactDeleted` / `ArtifactExpired` / `ArtifactUnavailable`，不能假装内容仍可恢复。
- Secret 泄漏后的紧急清除可以真正删除原始内容；append-only 审计只保留最小元数据、hash 或不可逆摘要。

## 19. 暂不建议做的事

- 不建议继续新增 command-only 主入口来包装已有能力。
- 不建议在 Verification Contract 前开放任意深度 nested agents。
- 不建议在没有 worktree、changeset 或 write lease 的情况下复制并行写 agent。
- 不建议把“禁止 edit 工具”当成真正只读，除非执行后端也 enforce read-only filesystem。
- 不建议把 provider 私有 completion / reasoning / tool-call quirk 上移到 kernel。
- 不建议在没有 ExecutionBackend 前宣传 OS-level sandbox。
- 不建议 sandbox backend 不可用时默认 fail open。
- 不建议宣称本地 shell sandbox 会自动保护所有 MCP、插件或远端工具。
- 不建议把 Context Engine 做成不可解释的长 prompt 拼接器。
- 不建议把 checkpoint / rewind 宣传成能覆盖任意 Bash 副作用，除非 backend 提供 workspace snapshot。
- 不建议自动安装或执行插件、自定义工具、compaction hook，除非已有显式 trust decision。
- 不建议在 thread/task/agent 状态稳定前过早拆出庞大的 app server 和多客户端协议。
- 不建议让 crash resume 自动重放写工具或 shell 命令。
- 不建议用 eval harness 替代单元测试、状态机测试或审批恢复测试。
