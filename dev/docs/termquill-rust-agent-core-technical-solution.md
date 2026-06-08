# Termquill Rust Agent 核心技术方案（Draft v0）

## 1. 背景

`termquill` 计划做成一个基于 Rust 的 AI coding agent，定位是 TUI-first 的终端产品、内核复用、前端可插拔。

它要继承的不是某个具体项目的代码形态，而是那套已经被验证过的核心能力模型：

- 配置驱动的模型与工具编排
- 一个可被多前端复用的无传输耦合 controller
- 支持工具调用的 agent 主循环
- MCP 兼容的插件接入能力
- cache-first 的会话与记忆模型
- 可选的 planner / executor 双模型协作

这个项目应该复制“能力边界”和“架构契约”，而不是逐行翻译 Go 实现。源项目是 MIT 协议，参考其架构和能力设计在许可上没有问题，但 `termquill` 的实现必须保持 Rust 风格，不能把 Go 的包结构和偶然实现细节原封不动搬过来。

## 2. 目标

第一代 `termquill` 内核应该达成这些目标：

1. 用 Rust 构建一个可被 TUI、CLI、HTTP、未来桌面端共同复用的 agent kernel，其中 TUI 是第一用户表面。
2. 保持 provider、tool、plugin 都由配置和注册机制驱动，而不是写死在核心里。
3. 第一阶段优先支持 OpenAI-compatible chat completions provider，同时给未来更多 provider 留出扩展位。
4. 内置工具和 MCP 工具通过统一的工具注册表暴露给 agent。
5. 保持 cache-stable 的 session 设计，把 prefix-cache 命中率视为顶层架构约束，而不是附带优化。
6. 提供适合自动化 coding 场景的 permission layer 和 workspace confinement。
7. 给 planner / executor 双模型协作预留清晰的架构边界，但不强行塞进 MVP。

## 3. 第一阶段非目标

第一版需要明确不做这些事情，避免一开始范围失控：

- 第一阶段不做桌面壳
- 第一阶段不做 codegraph 或更重的代码智能子系统
- 第一阶段不做 npm、Homebrew、自更新这类分发包装层
- 在 OpenAI-compatible 路径稳定前，不铺太多 provider
- 在单会话内核跑稳之前，不做复杂的多 agent 编排
- 第一阶段不继续扩张用户可见命令面，不把 provider 专项能力直接暴露成产品主心智

## 4. 设计原则

1. 契约优先：先定义稳定 trait 和事件契约，再铺前端。
2. 内核优先：CLI、TUI、desktop 都只是 adapter，不能各写一套执行逻辑。
3. TUI-first 产品表面：优先把真实用户会看到的交互壳做对，再决定哪些命令需要显式暴露。
4. 配置驱动、插件驱动：模型和工具来自配置、注册和运行时接入，不靠核心里的大段 `match`。
5. 缓存优先：system prompt prefix 尽可能稳定；memory、skills 只在 session 启动时折入一次；任何会破坏 byte-stable prefix 的动态注入都必须被隔离。
6. Rust 风格优先：用清晰 ownership、显式状态机和合理 async 边界，而不是机械翻译 Go。
7. 分阶段复杂化：先保持 crate 数量少、职责清楚，压力出现后再拆。

## 5. 推荐工作区结构

第一阶段的工作区结构应该保持克制：

```text
termquill/
  Cargo.toml
  rust-toolchain.toml
  dev/
    governance/
      code-standards.md
      engineering-standards.md
    docs/
      termquill-rust-agent-core-technical-solution.md
  crates/
    termquill-kernel/
      src/
        agent/
        controller/
        event/
        memory/
        permission/
        session/
        tool/
        provider/
        config/
    termquill-provider-deepseek/
      src/
    termquill-tools-builtin/
      src/
    termquill-mcp/
      src/
    termquill-runtime/
      src/
    termquill-cli/
      src/
    termquill-tui/
      src/
```

### 为什么这样拆

- `termquill-kernel`：承载领域契约、核心状态和主循环。
- `termquill-provider-deepseek`：首个旗舰 provider，实现 DeepSeek 专项能力与 OpenAI-compatible 主链路适配。
- `termquill-tools-builtin`：隔离文件、shell、搜索等内置工具。
- `termquill-mcp`：隔离 stdio / HTTP MCP client 和工具适配逻辑。
- `termquill-runtime`：收口跨入口共享的 provider factory、tool registry 和 run options，避免 TUI / CLI 各自硬编码装配链。
- `termquill-cli`：薄启动器、调试入口和自动化入口，不承担最终产品心智。
- `termquill-tui`：第一层用户入口，负责真正的交互体验、审批流和会话可见性。

这个拆分刻意比“教科书式 Clean Architecture”更少。memory、permission、config、controller 在第一阶段都留在 `termquill-kernel` 内，因为它们会一起快速演化，过早拆包只会增加 friction。

当前落地实现中，`termquill-runtime` 是必要的轻量装配层，而不是新的领域层。它只负责把 `RootConfig` 解析成 `Box<dyn Provider>`、`ToolRegistry` 和 `AgentRunOptions`；kernel 仍然不知道 runtime 存在。

这里要特别说明：这不意味着 `termquill` 被做成 DeepSeek 专属，而是表示第一套“做深做透”的 provider 先落在 DeepSeek 上。未来仍可增加：

- `termquill-provider-openai-compat`
- `termquill-provider-anthropic`
- `termquill-provider-gemini`

但这些 provider 都应该服从同一个 `termquill-kernel` 契约，而不是反过来把内核做成某家厂商私有运行时。

## 6. 核心领域模型

### 6.1 Provider 抽象

Provider 是可聊天、可流式、可恢复、可承载长任务状态的模型后端。这里不应只抽象成传统 chat-completions 风格，而要向更高一层的 “response / item” 模型抽象靠齐。

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = anyhow::Result<ProviderChunk>> + Send
            >
        >
    >;
}
```

这里需要锁定几条规则：

- kernel 不得硬编码 `DeepSeek`、`OpenAI`、`Anthropic` 这样的厂商类型
- provider instance 必须来自解析后的配置
- 切模型是 config 和 controller 层的问题，不是编译期静态问题
- provider 抽象必须能承载 background task、response handle、reasoning artifact、续跑 cursor 这些能力位
- 不能把 provider 的长任务、推理摘要、工具流事件都压扁成“只有文本 delta”的模型
- provider 的 `stream()` 必须是真流式：HTTP/SSE body 读取、SSE frame 解码和 `ProviderChunk` 映射应边读边 yield；只允许在尚未 yield 任何 chunk 前做透明 retry

### 6.2 Tool 抽象

内置工具和 MCP 工具必须统一满足同一个运行时接口。

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn read_only(&self) -> bool { false }

    async fn execute(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> anyhow::Result<ToolResult>;

    async fn preview(
        &self,
        _args: serde_json::Value,
    ) -> anyhow::Result<Option<DiffPreview>> {
        Ok(None)
    }
}
```

这里的关键约束是：

- 每个工具都要暴露 JSON Schema 兼容的参数定义
- 工具执行失败要返回给模型，不应该直接把整个进程打死
- preview 是可选能力，只给交互式前端做审批卡片和 diff 预览用
- 文件类内置工具必须对 workspace root 做 canonicalize，并用路径组件判断 confinement；绝对路径、`..`、目标 symlink 或父目录 symlink 指向 workspace 外时都必须拒绝

### 6.3 Tool Registry

运行时注册表要统一挂这几类能力：

- enabled built-in tools
- MCP 适配后的远程工具
- 未来 skill 包装出来的工具

agent loop 只能依赖 registry，不能直接依赖具体工具集合。

### 6.4 Session 模型

Session 至少要持有这些状态：

- system prompt
- user / assistant message
- tool call / tool result message
- usage metadata
- checkpoint metadata

Session 自身应该和存储解耦。持久化层可以单独序列化为 JSONL 或其他 append-friendly 格式。

### 6.5 Cache-First 上下文分区

为了保留 Reasonix 最核心的“缓存极致利用”特性，`termquill` 不应该只停留在“尽量少改 prompt”的口号层，而要直接把上下文建模成三个区域：

```text
┌─────────────────────────────────────────┐
│ IMMUTABLE PREFIX                        │
│   system + tool_specs + memory + skills │
├─────────────────────────────────────────┤
│ APPEND-ONLY LOG                         │
│   user / assistant / tool results       │
├─────────────────────────────────────────┤
│ VOLATILE SCRATCH                        │
│   per-turn transient plan / repair      │
└─────────────────────────────────────────┘
```

这三段要分别承担不同责任：

- `Immutable Prefix`：在 session boot 时计算一次，之后默认不改写，是 prefix-cache 命中的核心区域。
- `Append-Only Log`：按发生顺序单调追加，不能重排、不能中途就地覆盖。
- `Volatile Scratch`：只服务当前回合的临时状态，例如 repair、内部计划、局部推导，不直接上游发送，也不直接写回 prefix。

这里需要锁死 6 条不变量：

1. Prefix 只在 session boot 时组装一次，默认永不改写。
2. Log 只允许 append，禁止 reorder、禁止 in-place rewrite。
3. 每轮动态生成的时间戳、随机串、无必要的 header 抖动都不能进入 prefix。
4. Scratch 中的信息只有在经过显式折叠后才能进入 log，不能直接污染 prefix。
5. 并行工具调用即使并发执行，落回历史时也必须按声明顺序写入。
6. 除非发生受控 compaction，否则前一轮请求的字节前缀在下一轮必须继续可命中。

此外，缓存利用不能只靠“感觉”，必须做成硬观测项。`termquill` 需要在 telemetry 中持续产出：

- 每轮 `cache_hit_tokens`
- 每轮 `cache_miss_tokens`
- 每轮 `cache_hit_ratio`
- 整个 session 的累计 `cache_hit_ratio`
- 因缓存带来的估算节省成本

### 6.6 核心数据结构建议

为了让实现阶段不走形，建议尽早把几类核心结构定下来。

#### CompletionRequest

```rust
pub struct CompletionRequest {
    pub provider_name: String,
    pub model_name: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSchema>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub cache_discipline: CacheDiscipline,
    pub previous_response_handle: Option<ResponseHandle>,
    pub traffic_partition_key: Option<String>,
    pub background: bool,
    pub store: bool,
    pub deterministic_materialization: bool,
}
```

关键点：

- `provider_name` 和 `model_name` 分开存，避免后续切换模型时语义混乱
- `cache_discipline` 必须显式进入 request 构造，而不是散落在调用栈的 if 分支里
- `previous_response_handle` 预留给支持 response continuation 的 provider
- `traffic_partition_key` 是跨 provider 的稳定租户分区键；DeepSeek adapter 需要把它映射到 `user_id`
- `background` 和 `store` 不是 OpenAI 特例，而应成为长任务 provider 的通用请求位
- `deterministic_materialization` 用来强制开启缓存纪律要求下的稳定序列化

#### ProviderChunk

```rust
pub enum ProviderChunk {
    TextDelta(String),
    ReasoningDelta(String),
    ReasoningSummaryDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgsDelta { id: String, delta: String },
    ToolCallComplete(ToolCall),
    Usage(UsageStats),
    BackgroundTaskAccepted(BackgroundTaskHandle),
    BackgroundTaskStatus(BackgroundTaskStatus),
    ResponseHandle(ResponseHandle),
    ReasoningArtifact(ReasoningArtifact),
    ContinuationState(ProviderContinuationState),
    Done,
}
```

关键点：

- 工具调用必须支持“开始 / 参数流式增量 / 完整结束”三段式重组
- `Usage` 必须是正式 chunk，不要依赖调用方从原始 HTTP body 里偷偷扒字段
- `ReasoningSummaryDelta` 和 `ReasoningArtifact` 必须分开，前者可展示，后者是 opaque continuation object
- `ResponseHandle` / `BackgroundTaskHandle` 需要作为正式输出，而不是只存在 provider adapter 私有状态里
- `ContinuationState` 是 provider 私有续跑状态的流式出口，kernel 只负责持久化和恢复，不解释其内部语义

#### ToolCall / ToolResult

```rust
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
    pub metadata: ToolResultMeta,
}
```

关键点：

- `args_json` 在重组完成前应保留原始字符串形态，避免过早解析把截断问题藏起来
- `ToolResultMeta` 可承载 `exit_code`、`changed_files`、`truncated`、`bytes` 等信息

#### SessionLogEntry

```rust
pub enum SessionLogEntry {
    User(ModelMessage),
    Assistant(ModelMessage),
    ToolResult(ModelMessage),
    Control(ControlEntry),
}
```

这里建议把“发给模型的消息”和“只给系统自己的控制记录”区分开：

- `User / Assistant / ToolResult` 是真正可能进入 provider request 的历史
- `ControlEntry` 只给 controller、resume、审计和 UI 使用，不进入上游 prompt

建议把 `ControlEntry` 做成 append-only 的系统控制记录，而不是临时运行时侧带：

```rust
pub enum ControlEntry {
    SessionIdentity { provider_name: String, model_name: String },
    ContinuationStateSaved(ProviderContinuationState),
    ResponseHandleTracked(ResponseHandle),
    BackgroundTaskTracked(BackgroundTaskHandle),
    PrefixSnapshotCaptured(PrefixSnapshot),
    UsageSnapshot(UsageStats),
    CompactionApplied(CompactionRecord),
    Note { kind: String, data: serde_json::Value },
}
```

建议语义：

- `ContinuationStateSaved`：保存必须跨 turn / resume / compaction 存活的 provider 私有状态
- `ResponseHandleTracked`：记录可续跑句柄
- `BackgroundTaskTracked`：记录后台任务句柄
- `PrefixSnapshotCaptured`：记录当前稳定前缀的快照
- `UsageSnapshot`：记录 usage 与 cache token 统计，供 resume 后恢复 session stats
- `CompactionApplied`：记录稳定 compaction summary 与 tail 计数，供后续 request 做 provider-visible projection
- `Note`：承接不值得升格为独立结构的控制面元数据

这样做的好处是，provider continuation、后台任务恢复、缓存诊断都会落在同一条 append-only 审计链上，而不是散在 runtime 内存和 UI 状态里。

当前实现中，`Session` 提供 `latest_response_handle`、`latest_prefix_snapshot`、`latest_compaction_record` 和 `continuation_states` 这类显式查询方法；agent run 初始化下一轮 request 时会从 durable control state 恢复最新匹配 provider 的 response handle，而不是只依赖进程内变量。

建议至少保留一类 provider 无关的 continuation 记录：

```rust
pub struct ProviderContinuationState {
    pub provider_name: String,
    pub state_kind: String,
    pub opaque_blob: serde_json::Value,
}
```

它的职责是承载“必须跨 turn、跨 resume、跨 compaction 持久化”的 provider 私有状态。
这样一来，像 DeepSeek `reasoning_content` replay 这类要求，就不会被错误地塞进 provider 进程内存，导致恢复后丢状态。

#### PrefixSnapshot

```rust
pub struct PrefixSnapshot {
    pub materialized_text: String,
    pub sha256: String,
    pub provider_name: String,
    pub model_name: String,
    pub memory_fingerprint: String,
    pub tool_schema_fingerprint: String,
    pub skill_index_fingerprint: String,
}
```

这类结构非常重要，因为“缓存极致利用”不是抽象口号，而是要能明确知道：

- 这段 prefix 到底是什么
- 它是不是被意外改了
- 当前 session 为什么还能命中，或者为什么突然掉命中

#### ResponseHandle / BackgroundTaskHandle / ReasoningArtifact

```rust
pub struct ResponseHandle {
    pub provider_name: String,
    pub response_id: String,
    pub continuation_cursor: Option<String>,
}

pub struct BackgroundTaskHandle {
    pub provider_name: String,
    pub task_id: String,
    pub resumable: bool,
}

pub struct ReasoningArtifact {
    pub provider_name: String,
    pub opaque_blob: serde_json::Value,
}
```

建议语义如下：

- `ResponseHandle`：用于 provider 级续跑、恢复流式事件、或后续增量请求
- `BackgroundTaskHandle`：用于轮询、重连、取消长任务
- `ReasoningArtifact`：用于跨请求延续 provider 私有推理工件，但不直接展示给用户
- 若某类 provider 工件还需要跨 turn / resume 持久化，应折叠进 `ProviderContinuationState`，而不是只保留在 adapter 私有状态里

#### SessionStats

```rust
pub struct SessionStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_savings: f64,
}
```

建议这个结构既支持“本轮增量”，也支持“整会话累计”。

### 6.7 确定性序列化规范

如果 `termquill` 要把缓存命中做成旗舰能力，那么 prompt materialization 不能交给默认 JSON serializer 的偶然行为。

建议现在就规定以下稳定化规则：

- tool schema 按稳定 key 排序后输出
- JSON object 字段顺序固定
- memory import 展开顺序固定
- MCP roots 列表按 URI 排序
- provider request 里不允许混入 UI 动态状态
- prefix materialization 必须总是生成 fingerprint

这部分建议单独抽成一个 `PromptMaterializer` 组件，输入：

- prefix snapshot inputs
- append-only log slice
- cache discipline

输出：

- 发给 provider 的稳定字节序列
- 对应 fingerprint

## 7. Controller 与事件流

`termquill` 的核心应该围绕一个无传输耦合的 controller。它拥有一个会话运行时，并向上游壳层发出 typed event。

### 7.1 Controller 命令面

Controller 对外至少暴露这些命令：

- `submit(input)`
- `cancel()`
- `approve(call_id, allow_mode)`
- `set_plan_mode(enabled)`
- `compact_now()`
- `new_session()`
- `resume(session_id)`

### 7.2 Event 模型

推荐的事件类型如下：

- `TurnStarted`
- `PhaseChanged`
- `TextDelta`
- `ReasoningDelta`
- `ToolCallStarted`
- `ToolCallCompleted`
- `ApprovalRequested`
- `UsageUpdated`
- `CacheStatsUpdated`
- `ProgressUpdated`
- `BackgroundTaskUpdated`
- `SessionChanged`
- `Notice`
- `TurnCompleted`
- `TurnFailed`

CLI、TUI、HTTP streaming、未来 desktop UI 都应该消费同一套事件流，而不是各自重写 turn lifecycle。

其中 `UsageUpdated` 和 `CacheStatsUpdated` 至少要能让前端展示：

- 当前回合输入 / 输出 token
- 当前回合 cache 命中率
- 整个 session 的累计 cache 命中率
- 当前回合与整个 session 的估算成本
- 长任务当前状态与最近一次进度

## 8. Agent 主循环

单模型执行循环建议这样工作：

1. 从当前 session 和暴露给模型的 tool schema 组装 request。
2. 流式调用 provider。
3. 发出可见文本和 reasoning delta 事件。
4. 从 stream 中重组完整 tool call。
5. 如果没有 tool call，则本轮完成。
6. 如果有 tool call，则走 permission check 和可选 preview。
7. 执行工具，把结果写回 session，然后继续下一轮。
8. 当模型正常结束或达到 max-step 上限时停止。

几个关键行为约束：

- 全只读工具批次可以并行执行
- 混合读写批次必须串行执行
- 整个循环必须可取消
- 被中断的工具轮次要保留足够上下文，防止恢复后出现 tool-call pairing 损坏
- 历史写回必须保持 append-only，禁止为“清理上下文”而重排旧消息
- per-turn volatile scratch 不直接上游发送给 provider
- 并行工具即使执行完成顺序不同，写回 log 时也必须按声明顺序落盘

如果目标是最大化 prefix-cache 命中，还要额外禁止这些常见反模式：

- 每轮在 system 区域注入新的时间、会话摘要或运行时 banner
- 因 UI 方便而在 prompt 头部塞动态状态
- 在未 compaction 的情况下重写旧 tool result
- 因 provider 切换把不同模型混在同一个共享会话里

对于支持后台执行的 provider，还应补充两条路径：

- `submit_background`：把任务交给 provider 后台执行，controller 持有 `BackgroundTaskHandle`
- `resume_background`：基于 `BackgroundTaskHandle` 或 `ResponseHandle` 恢复轮询或流式追尾

这意味着 agent loop 不应该假设“一个 turn 只能是同步流式完成”，而要允许：

- 同步流式完成
- 后台排队后轮询完成
- 先流一段，再断线后基于 cursor 继续追流

## 9. Planner / Executor 协作模型

双模型协作应当属于第二阶段能力，但 kernel 现在就要留出清晰接缝。

设计选择如下：

- planner 和 executor 必须使用两个独立 session
- planner 不带工具，只生成简洁 plan
- executor 接收 handoff，再做真实执行
- 两个 session 绝不能合并成一个共享会话

这样做的原因不是形式主义，而是要保持 prefix cache 稳定，同时避免 planner 的中间话语污染 executor 上下文。

## 10. Memory 模型

Memory 必须遵循 cache-first 思路。

### 10.1 分层文档记忆

第一层 memory 建议支持：

- `TERMQUILL.md`
- `AGENTS.md`
- `CLAUDE.md`
- 本地覆盖文件，如 `TERMQUILL.local.md`
- 单独一行的 `@path` 导入

这样设计的理由：

- `TERMQUILL.md` 是项目自己的命名
- 同时兼容 `AGENTS.md` / `CLAUDE.md`，迁移成本更低

### 10.2 Prefix 稳定性

在 session boot 时：

- 先加载基础 system prompt
- 再追加语言和行为策略
- 再加载层级 memory 文档
- 再加载 skill index

在 session 运行中：

- 除非发生 compaction，否则不改写这段 prefix
- 中途新增的 memory 变化应通过 transient tail queue 注入，而不是直接篡改 prefix

### 10.3 Cache-Safe Compaction

`termquill` 必须支持 compaction，但 compaction 只能作为“受控的稀有 cache reset 点”，不能退化成普通 agent 常见的随手改写历史。

compaction 规则建议如下：

- 只有当上下文占用达到阈值时才触发
- compaction 只折叠较旧的中间段，不动 prefix
- 最近一段消息尾巴必须保持 verbatim，避免留下孤立的 tool message
- 折叠后的 summary 必须是稳定、简洁、可复用的，不引入每轮波动字段
- compaction 前的原始历史应归档，保证可追溯
- 手动 compaction 前，TUI 应先给出 provider-visible before/after preview，让用户知道会折叠掉什么，再决定是否真正提交 `/compact`

第一版可以先定义两个阈值：

- `soft threshold`：上下文窗口 50% 左右时允许手动或后台建议 compaction
- `hard threshold`：上下文窗口 80% 左右时自动执行 compaction

除此之外，还应加一条和成本直接相关的策略：

- 大型 tool result 在完成其所在回合的消费后，应允许做 turn-end compaction，只保留后续回合真正需要的摘要；如果以后还要精读，让模型重新读取文件或重新执行只读查询

### 10.4 Auto Memory

Auto memory 不必阻塞 MVP。第一版只支持文档型 memory 是可以接受的，indexed fact store 后续再补。

## 11. MCP 插件模型

MCP 是 `termquill` 的核心差异化能力之一，应该尽早落地。

MCP 设计不应只覆盖 tools/prompts/resources，还应直接对齐当前规范里已经值得落地的 client features。

### 11.1 支持的传输

建议阶段顺序：

1. 先做 `stdio`
2. 再做 streamable HTTP
3. legacy SSE 只有在现实需求出现后再考虑

### 11.2 暴露规则

每个 MCP server 可能暴露：

- tools
- prompts
- resources
- roots
- progress
- elicitation

模型可见命名建议统一为：

- `mcp__<server>__<tool>`
- `/mcp__<server>__<prompt>`
- `@<server>:<uri>`

### 11.3 Roots / Progress / Elicitation

建议第一版即在协议层保留这些能力：

- `roots/list`
- `notifications/roots/list_changed`
- `notifications/progress`
- `elicitation/create`

对 `termquill` 的具体价值分别是：

- `roots`：可和 workspace sandbox 对齐，把“允许 server 看到哪些根目录”正式协议化
- `progress`：长时工具和远程 server 可以发正式进度，而不是靠文本日志刷屏
- `elicitation`：server 能在 `tools/call` 等处理中合法地向用户要补充输入

### 11.4 信任与数据出境模型

远程 MCP server 不只是“再多一种工具来源”，它本质上是数据出境和 prompt injection 风险边界。

建议给每个 server 建立独立 trust policy：

```rust
pub enum McpTrustClass {
    Official,
    SelfHosted,
    ThirdParty,
}

pub struct McpServerTrustPolicy {
    pub trust_class: McpTrustClass,
    pub approval_default: ApprovalMode,
    pub egress_logging: bool,
    pub allow_secrets: bool,
    pub pin_version: bool,
}
```

建议默认策略：

- `Official`：可降低 friction，但仍保留敏感调用审批能力
- `SelfHosted`：可按团队策略放宽
- `ThirdParty`：默认逐次审批、默认记录出境数据、默认不透传高敏凭据

### 11.5 协议版本与能力协商

MCP 规范本身是按日期版本演进的，`termquill` 需要把协议版本协商做成显式状态，而不是隐式假定“所有 server 都一样”。

建议记录：

- server 宣告的协议版本
- server capabilities
- client capabilities
- 当前启用的特性子集

### 11.6 启动策略

第一版先支持：

- 明确配置为 required 的 server 用 eager startup
- 非关键 server 用 lazy startup

background tier 可以后补，不必第一阶段就做。

## 12. Permission 与 Sandbox

Permission 和 sandbox 必须看成两层，不要混在一起。

### 12.1 Permission Policy

Permission policy 负责决定一次工具调用是：

- `allow`
- `ask`
- `deny`

规则至少支持：

- 只按 tool-name 匹配
- 按 tool-name + subject glob 匹配

默认行为建议：

- read-only tool 默认 allow
- write tool 走配置的 mode
- headless run 遇到 `ask` 默认 allow
- interactive run 遇到 `ask` 弹审批

### 12.2 Sandbox

Sandbox 是执行层强制约束，不是策略层判断。

第一版最安全、也最现实的落地点是：

- 把文件写工具限制在 workspace root 内
- 在放行前统一解析 symlink 和 `..`

shell sandboxing 更难，建议放到 phase 3 或 phase 4，因为跨平台进程隔离本来就是整套系统里最难啃的部分之一。

## 13. 运行模式设计

为了同时保住“缓存极致利用”与“未来可兼容更多后端”，`termquill` 不应该停留在粗粒度双模式，而要把缓存纪律做成 provider-specific 的正式策略。

### 13.1 Cache Discipline Profiles

建议将原本的双模式升级为：

```rust
pub enum CacheDiscipline {
    DeepSeekExactPrefix,
    AnthropicPromptCaching,
    OpaqueProviderCache,
    NoCacheDiscipline,
}
```

它们的语义分别是：

- `DeepSeekExactPrefix`：以字节稳定前缀和命中 token 指标为核心
- `AnthropicPromptCaching`：以显式 cache-control 边界和 TTL 模型为核心
- `OpaqueProviderCache`：provider 有缓存，但机制不透明，只做保守适配
- `NoCacheDiscipline`：不依赖 provider cache，只保留 append-only 和审计约束

### 13.2 DeepSeek 极致缓存模式

这个模式是给真正追求 prefix-cache 命中率的场景准备的，原则上应该作为 `termquill` 的旗舰模式。

在这个模式下，必须强制以下规则：

- session 使用 `Immutable Prefix + Append-Only Log + Volatile Scratch`
- prefix 默认只在 boot 时生成一次
- 禁止把动态状态注入 system 区域
- 禁止在未 compaction 时重写旧历史
- planner / executor 必须分 session
- provider 切换必须新开 session
- 需要暴露完整缓存指标和节省成本指标

适用前提：

- provider 支持稳定的 prefix-cache 机制
- provider 能返回缓存 token 指标，或至少能被我们可靠推导
- 用户愿意接受比通用 agent 更强的行为约束

### 13.3 Anthropic / Opaque / NoCache 模式

这些模式不是旗舰模式，但值得在架构层预留：

- `AnthropicPromptCaching`
  - 使用 provider 显式缓存边界
  - 把 cache TTL 和 cache block placement 当成 provider policy 的一部分
- `OpaqueProviderCache`
  - 不强求可解释的命中单元
  - 只输出保守 telemetry
- `NoCacheDiscipline`
  - 不围绕 provider cache 调参
  - 但仍保持 append-only log、resume、tool integrity、permission 和审计能力

但仍然必须保留这些底线：

- controller、tool registry、session lifecycle 不变
- 工具调用修复和历史一致性不变
- permission / sandbox / resume / 审计能力不变

### 13.4 运行模式与缓存纪律分离

`runtime_mode` 和 `cache_discipline` 不应该是同一个概念。

建议配置层拆成：

```toml
[agent]
runtime_mode = "auto"      # auto | strict | flexible
cache_mode = "auto"        # auto | deepseek_exact_prefix | anthropic_prompt_caching | opaque | none
```

其中：

- `runtime_mode`：决定系统总体行为风格
- `cache_mode`：决定 prompt materialization 与 provider cache 适配策略

这样后面就不会因为想切到更严格的审批模式，顺手把缓存语义也一起改掉。

### 13.5 模式选择策略

建议配置上支持：

规则建议如下：

- `runtime_mode=auto`：根据 provider profile 和工作负载自动决定整体行为
- `cache_mode=auto`：根据 provider capabilities 自动映射到合适的 `CacheDiscipline`
- 显式指定 `deepseek_exact_prefix` 时，如果 provider 不满足条件则直接报错

同时建议 provider metadata 暴露这些能力位：

```rust
pub struct ProviderCapabilities {
    pub exact_prefix_cache: bool,
    pub reports_cache_tokens: bool,
    pub supports_reasoning_stream: bool,
    pub supports_tool_stream: bool,
    pub supports_background_tasks: bool,
    pub supports_response_handles: bool,
    pub supports_reasoning_artifacts: bool,
    pub supports_structured_output: bool,
    pub supports_assistant_prefix_seed: bool,
    pub supports_schema_constrained_tools: bool,
    pub supports_infill_completion: bool,
    pub supports_system_fingerprint: bool,
}
```

这样模式判定就不是拍脑袋，而是可程序化决策。

同时要明确一条边界：

- `ProviderCapabilities` 只承载跨 provider 可复用的通用能力位
- 像 `reasoning_content replay`、`thinking mode 忽略采样参数`、`beta 端点切换` 这种厂商特有规则，应留在 provider profile 或 provider-specific feature/quirk 层

### 13.6 为什么要做 provider-specific discipline 而不是一个兼容模式

如果只有一个模糊兼容模式，项目很容易发生这类架构漂移：

- 为了兼容更多 provider，不断放松 prefix 稳定性
- 为了 UI 方便，向 prompt 头部塞动态状态
- 为了省事，把 provider 切换做成同 session 内热切换

这种漂移短期看起来更“通用”，长期会直接毁掉 `termquill` 最有辨识度的能力。所以 provider-specific discipline 不是为了增加复杂度，而是为了保护旗舰模式不被兼容性需求慢慢侵蚀。

## 14. 推荐 Rust 技术栈

基础依赖建议如下：

- `tokio`：async runtime
- `futures` / `tokio-stream`：stream 组合
- `serde`、`serde_json`、`toml`：配置和协议序列化
- `reqwest`：HTTP model / MCP client
- `async-trait`：第一阶段先解决 object-safe async trait 问题
- `thiserror` + `anyhow`：错误分层
- `tracing` + `tracing-subscriber`：结构化日志
- `uuid`：id 生成
- `globset`：permission rule 匹配
- `ignore` + `walkdir`：文件扫描
- `similar` 或 `dissimilar`：diff preview
- `ratatui` + `crossterm`：第一代 TUI 交互壳

后续可能会加：

- `portable-pty`：更完整的 shell / PTY 支持
- `tauri`：桌面壳
- `nix` 或平台相关 crate：更强的 confinement

## 15. 交付阶段

在进入 phase 划分之前，需要先明确产品表面原则：

- `termquill-tui` 是第一用户入口，也是后续能力设计的基准面
- `termquill-cli` 保留为启动器、自动化入口和调试通道，不承载最终产品心智
- `strict tools`、`prefix completion`、`FIM` 这类 provider 专项能力，默认应被 TUI 内部流程吸收，而不是直接变成普通用户必须理解的顶层命令
- 如果某个能力只能靠新增命令解释自己，应该先反问：它是否其实应该是 TUI 内部动作、审批卡片或编辑模式

### Phase 0：脚手架

交付物：

- workspace 和 crate skeleton
- 共享 config loader
- event types
- logging setup
- session serialization format 选择
- `termquill-tui` crate skeleton 与最小 app state

退出条件：

- `cargo check`、`cargo test`、`cargo fmt`、`cargo clippy` 全部打通

### Phase 1：最小内核 + TUI 骨架

交付物：

- `termquill-provider-deepseek`（复用 OpenAI-compatible 主链路）
- tool trait 和 registry
- built-in tools：`read_file`、`write_file`、`edit_file`、`ls`、`glob`、`grep`、`bash`
- 单模型 agent loop
- `termquill-tui` 最小交互壳：消息区、输入区、状态区、事件流渲染
- 薄 `CLI run` 调试入口
- stdio MCP support

退出条件：

- 能从 TUI 发起一轮真实对话并看到流式输出
- 能在 TUI 中完成一个端到端 coding task
- MCP tool roundtrip 正常
- 工具失败时能结构化回传，不把进程直接打死

### Phase 2：交互控制层

交付物：

- 带审批与取消能力的 controller
- TUI chat shell 完整化
- session persistence + resume
- slash commands
- MCP prompts / resources
- provider 专项能力的交互收口：
  - `strict tools` 显示为工具模式与审批状态
  - `prefix completion` 显示为“继续补全/沿前缀续写”动作
  - `FIM` 显示为编辑器内补洞能力，而不是独立产品模式

退出条件：

- 交互式会话可以审批或拒绝写操作
- cancel 不会破坏 session history
- resume 之后能继续工作

### Phase 3：增强型 Agent 能力

交付物：

- planner / executor 双模型模式
- context compaction
- 层级文档 memory
- subagent / task tool
- workspace write confinement

退出条件：

- planner 和 executor 保持独立且 cache-stable
- 长会话 compaction 后不会打坏 tool-call consistency
- 文件写入无法通过 symlink 或 `..` 逃出 workspace root

### Phase 4：产品化

交付物：

- HTTP streaming 壳或 desktop 壳
- 更成熟的 shell execution control
- updater 和 packaging
- metrics 与 cost accounting
- 如果还有必要，再补 richer code intelligence

### 15.1 第一代 TUI 信息架构

第一代 `termquill-tui` 不需要一开始就做得像 IDE，但必须先把用户真正需要的几个面做好：

- 主消息历史：优先直接写入 terminal 原生 scrollback，避免把长对话性能和滚动体验绑死在内部 widget 上
- 底部 live strip：只保留当前流式尾部、composer 与紧凑状态，不要求用户在 chat 区和 composer 之间切焦点
- 底部输入区：支持多行输入、发送、取消、清空
- 右侧信息区 + composer 下状态行：展示写权限、subagent 状态、cache 命中、上下文压力、花费与余额
- 模态或侧栏审批区：展示工具预览、写操作 diff、允许/拒绝动作
- 会话控制区：新会话、恢复会话、切换 workspace、查看错误详情
- setup 模式：当没有可用配置时，TUI 内部直接承载首启配置流，而不是把用户赶回命令行手写配置
- provider 视图：不仅展示当前 provider-visible context，还应承载 compaction preview 这类“提交前先解释后果”的上下文操作

这意味着 `kernel` 事件流在 phase 1 就要按 TUI 消费习惯设计，而不是先按“stdout 打印一堆日志”来塑形。

当前实现还需要保持代码结构服务这个信息架构：`AppState` 作为 façade 收敛行为编排，输入焦点、approval state、session history、slash selector、timeline state、provider status 等纯状态放在独立模块；setup/config 状态模型维护在 `setup.rs` / `config_panel.rs`；renderer 通过 ViewModel 或 render options 读取 UI 数据；`ui.rs` 只作为 `ui/*` 模块入口和必要 re-export，顶层 shell layout、theme/geometry/text 底座、timeline、tool card、markdown、approval、setup/config、modal 等渲染块分别维护在对应 `ui/*` 模块。用户交互面优先使用 TUI 焦点和快捷键：tool card 选择/展开走 `Ctrl-G`、`Alt-J/K`、`Ctrl-O` 与 `Esc`，不依赖 hidden slash command；新增快捷键和命令通过 `commands.rs` metadata 同步 info rail、keyboard help 和 README。Markdown 展示由 `ui/markdown.rs` 和 `MarkdownRenderOptions` 统一约束，assistant timeline、tool preview、approval modal 不各自维护解析规则。

### 15.2 TUI-first 下的能力暴露规则

为了避免把产品越做越像命令集合，需要提前规定：

1. 普通用户主入口默认只有进入 TUI 这一件事。
2. `run` 这类命令保留给自动化、CI、脚本或最小 smoke test。
3. `prefix completion` 不应该成为普通用户必须理解的单独概念，而应在 TUI 中表现为“继续补全”或“沿当前前缀续写”。
4. `FIM` 不应该成为普通用户必须手动切换的独立模式，而应在编辑/补洞场景中被 TUI 内部自动选择。
5. `strict tools` 是 provider/tool discipline，不是用户命令；用户看到的应该是“工具调用更严格/可审批/更可预测”。

## 16. 关键风险

1. Async trait 设计：Rust 在安全性上比 Go 更强，但第一版 object-safe streaming API 需要选得足够稳。
2. Shell 执行可移植性：进程控制、PTY、跨平台 confinement 都很容易踩坑。
3. 文件编辑工具正确性：编码保留、partial replace、diff preview、中断恢复都不简单。
4. MCP 协议边角：streamable HTTP、notifications、server lifecycle 这些细节容易被低估。
5. 过早拆太多 crate：行为还没稳定前，包过多只会拖慢迭代。
6. 如果先把 provider 专项能力做成越来越多的 CLI 子命令，再补 TUI，最终很容易得到“命令集合”而不是“交互产品”。

## 17. 建议现在就锁定的决策

在正式编码前，建议先锁这几件事：

1. 项目配置文件名定为 `termquill.toml`
2. 第一层用户壳先做 `termquill-tui`；`termquill-cli` 只保留 bootstrap / debug / automation，不做命令产品化
3. kernel 是 event-driven、controller-centered
4. 第一个 provider backend 先做 `termquill-provider-deepseek`，并复用 OpenAI-compatible 主链路
5. MCP `stdio` 是 phase 1 必做，streamable HTTP 放 phase 2
6. 先做层级文档 memory，再做 indexed auto memory
7. planner / executor 在 base loop 稳定前保持可选

## 18. 立即下一步

现在最正确的下一步不是继续扩张命令表面，也不是分发包装，而是：

1. 搭 workspace
2. 定 kernel contracts 和 event types
3. 搭最小 `termquill-tui` 壳和 app state
4. 实现 `termquill-provider-deepseek`
5. 实现最小 tool registry 和三个文件工具
6. 先跑通一个端到端 TUI 工作流，再继续扩范围

这样做能让 `termquill` 一开始就站在两件最重要的东西上：一个可复用、可扩展、契约稳定的 agent 内核，以及一个真实面向用户的终端交互产品，而不是一个子命令越来越多的命令集合。

## 19. DeepSeek 专项优化设计

结合 DeepSeek 官方 API 文档，`termquill` 如果要把 DeepSeek 当作旗舰后端，不应该只停留在“兼容 OpenAI SDK 调用”的层面，而应该显式做一套 `DeepSeekProviderProfile`。

### 19.1 Canonical Model Policy

根据 DeepSeek 官方文档，当前推荐模型名是：

- `deepseek-v4-flash`
- `deepseek-v4-pro`

而 `deepseek-chat` 与 `deepseek-reasoner` 是兼容别名，并计划于 **2026 年 7 月 24 日** 弃用。

因此建议：

- 所有新配置、日志、遥测、会话元数据一律使用 canonical model id
- alias 只在读取旧配置时做向后兼容映射
- 一旦发现 alias，应在 UI 或日志里给出迁移提醒

### 19.2 DeepSeek Provider Profile

建议在 provider 层增加一个明确的 profile：

```rust
pub struct DeepSeekProviderProfile {
    pub primary_base_url: String,   // https://api.deepseek.com
    pub beta_base_url: String,      // https://api.deepseek.com/beta
    pub anthropic_base_url: String, // https://api.deepseek.com/anthropic
    pub default_model: String,      // deepseek-v4-flash
    pub default_thinking: bool,
    pub default_reasoning_effort: ReasoningEffort,
    pub quirks: DeepSeekProviderQuirkProfile,
}

pub struct DeepSeekProviderQuirkProfile {
    pub requires_reasoning_replay_after_tool_call: bool,
    pub ignores_sampling_params_in_thinking_mode: bool,
    pub strict_tools_requires_beta_endpoint: bool,
    pub prefix_completion_requires_beta_endpoint: bool,
    pub fim_requires_non_thinking_mode: bool,
    pub keep_alive_uses_blank_lines: bool,
    pub streaming_keep_alive_uses_sse_comments: bool,
}
```

建议默认值：

- `default_model = deepseek-v4-flash`
- `default_thinking = true`
- `default_reasoning_effort = max` for coding / agent workloads
- `quirks` 按 DeepSeek 官方行为预填

原因：

- DeepSeek 官方说明思考模式默认开启
- 普通请求默认 `high`
- 对 Claude Code、OpenCode 这类复杂 agent 请求，官方文档说明会自动拉到 `max`

所以 `termquill` 直接以 `max` 作为编码 agent 的默认推理强度更符合官方行为，而不是再从 `medium` 之类伪档位兜一层兼容

这里也建议把 provider-specific 奇异行为统一收进 `quirks`，而不是散落在：

- `ProviderCapabilities`
- controller 特判
- request builder 的隐式 if/else

这样未来如果加 `termquill-provider-anthropic` 或 `termquill-provider-openai-compat`，也能沿用同样模式：

- 通用能力进 `ProviderCapabilities`
- 厂商怪异行为进 provider-specific quirk profile

### 19.3 Thinking Mode 参数纪律

DeepSeek 官方明确说明：思考模式下，`temperature`、`top_p`、`presence_penalty`、`frequency_penalty` 不生效。

这意味着 `termquill` 在 DeepSeek thinking mode 下不应只是“把这些参数传上去然后假装支持”，而应该：

1. provider adapter 主动剔除这些参数
2. telemetry 或 debug log 明确记录“参数已忽略”
3. 前端在 DeepSeek thinking mode 下默认不暴露这些调节项

这样可以避免用户以为自己改了采样，实际请求行为却没有变化。

### 19.4 `reasoning_content` 回传策略

这是 DeepSeek 设计里最关键、也最容易踩坑的点之一。

官方规则是：

- 如果两条 `user` 消息之间没有发生 tool call，则中间 `assistant` 的 `reasoning_content` 在后续轮次中会被忽略
- 如果发生了 tool call，则该轮的 `reasoning_content` 在后续所有用户交互轮次中都必须完整回传，否则 API 会返回 400

因此 `termquill` 的 session log 不能粗暴地把所有 `reasoning_content` 一视同仁，而且相关 replay 状态不能只放在 provider 进程内存里。

建议增加：

```rust
pub enum ReasoningReplayPolicy {
    OmitAfterPlainAnswer,
    MustReplayAfterToolCall,
}
```

并在 DeepSeek provider 中固定使用：

- 无工具轮次：`OmitAfterPlainAnswer`
- 有工具轮次：`MustReplayAfterToolCall`

但这里要补一条更重要的实现约束：

- replay policy 属于 DeepSeek provider 的解释规则
- replay payload 的持久化必须进入 kernel 可保存的 `ProviderContinuationState`

也就是说，provider 负责“怎么理解和生成 replay 语义”，kernel 负责“把需要跨轮次保存的 opaque state 安全存下来”。

这会带来两个直接收益：

- 避免把无意义的 reasoning_content 长期拖入上下文，降低 cache 污染
- 确保有工具轮次不会因为漏回传 reasoning_content 而触发 400

### 19.5 DeepSeek Cache Shaping

DeepSeek 上下文缓存不是简单“字符串前缀相同就命中”，而是围绕“已持久化的完整 cache prefix unit”工作。官方还说明：

- 每次请求边界会形成缓存单元
- 系统会检测多次请求的公共前缀并持久化
- 长输入或长输出会按固定 token 间隔切分缓存单元
- 缓存构建需要几秒

这意味着 `termquill` 应针对 DeepSeek 做额外的 prompt shaping：

1. 大块稳定上下文必须尽量连续放置，不要和波动文本交错
2. 对同一仓库的大型静态背景，应优先保持“稳定大前缀 + 小问题尾部”结构
3. 多轮围绕同一文档或同一仓库追问时，不要改写前半段 framing
4. 对特别大的只读背景，可以考虑一次“预热轮次”后再进入密集提问

第 4 点是基于官方缓存持久化规则做的工程推断：由于公共前缀和请求边界都会形成缓存单元，预热一轮对后续高频问题有潜在收益。

### 19.6 DeepSeek Tool Mode 策略

DeepSeek 官方提供 `strict` tool mode（Beta）：

- 需要走 `https://api.deepseek.com/beta`
- 每个 function 需显式设置 `strict = true`
- 服务端会校验 JSON Schema，不支持的类型会直接报错

这对 `termquill` 非常有价值，因为它能显著降低 tool arguments 结构漂移。

建议策略：

- 默认尝试 `strict tool mode`
- 若 schema 超出 DeepSeek strict 子集，则自动回退到普通 tool mode
- 回退动作必须可观测，并写入 debug / event log

建议在 tool registry 层做一次 schema 分类：

```rust
pub enum ToolSchemaMode {
    DeepSeekStrictCompatible,
    GenericJsonSchemaOnly,
}
```

这样 provider 在组装请求时就能知道：

- 哪些工具可以走严格模式
- 哪些工具只能走普通模式

### 19.7 DeepSeek JSON Output 策略

DeepSeek 官方支持 `response_format = { "type": "json_object" }`。

对于 `termquill`，这不应该只作为“用户工具箱里的一个可选功能”，而应直接用于几个内核子流程：

- planner 输出结构化 plan
- approval summary 输出结构化变更摘要
- memory 提炼输出结构化对象
- compaction summary 输出结构化摘要对象，再 materialize 成稳定文本

建议做一层统一 helper：

```rust
pub enum StructuredOutputMode {
    JsonObject,
    PlainText,
}
```

这样 planner / summarizer / memory reducer 可以在 DeepSeek 上优先走 JSON object，再由 `termquill` 自己稳定化渲染为 cache-friendly 文本。

### 19.8 DeepSeek Prefix Completion 策略

DeepSeek 提供对话前缀续写（Beta）：

- 最后一条消息必须是 `assistant`
- 需要设置 `prefix = true`
- 需要使用 `https://api.deepseek.com/beta`

这项能力非常适合 `termquill` 做“输出形状控制”，尤其是在这些场景：

- 强制代码块起手，例如 ```` ```rust\n ```` 或 ```` ```diff\n ````
- 强制补丁模板、提交信息模板、JSON 前缀
- 减少模型在目标格式前面加解释性废话

建议：

- 不要把它当成通用对话默认路径
- 只在“输出格式非常强约束”的子流程启用
- 与 `stop` 配合使用，缩短无意义尾部

### 19.9 DeepSeek FIM 策略

DeepSeek 提供 FIM Completion（Beta），但官方说明它只支持非思考模式。

这意味着 `termquill` 可以把 FIM 设计成主 agent 之外的一条“局部补全旁路”：

- 主 agent：继续走 chat / tool / reasoning loop
- FIM sidecar：用于小范围代码补全、局部 splice、模板中间填充

这样能把 FIM 用在最擅长的地方，而不是硬塞进主循环。

建议后续增加：

```rust
pub enum EditEngine {
    AgentPatch,
    SearchReplace,
    FimSplice,
}
```

其中：

- `AgentPatch`：复杂改动
- `SearchReplace`：确定性强替换
- `FimSplice`：局部生成型补洞

### 19.10 Model Routing 策略

结合 DeepSeek 当前模型能力与价格，建议 `termquill` 的默认路由偏向：

- `deepseek-v4-flash`：默认执行器
- `deepseek-v4-pro`：高价值规划、复杂审查、困难收敛回合

原因：

- `flash` 已支持 thinking、tool calls、json output、prefix completion，且上下文 1M、最大输出 384K
- `pro` 仍更贵，应该有选择地使用，而不是默认全程挂上

建议把 planner / reviewer / compactor / summarizer 的模型选择都显式化，而不是只留一个笼统的 `default_model`。

### 19.11 `system_fingerprint` 遥测

DeepSeek 在响应和流式 chunk 中返回 `system_fingerprint`。

建议 `termquill` 把它纳入 telemetry：

- 每轮记录 `system_fingerprint`
- 若 fingerprint 变化，打一个低级别 notice
- 在分析缓存命中率突然下降、行为漂移、工具调用形状变化时，把它作为排查维度之一

这不是决定性字段，但它对生产调试很有价值。

### 19.12 DeepSeek 专项配置建议

建议为 DeepSeek 单独设计一组 provider 级配置：

```toml
[providers.deepseek]
base_url = "https://api.deepseek.com"
beta_base_url = "https://api.deepseek.com/beta"
model = "deepseek-v4-flash"
thinking = "enabled"
reasoning_effort = "max"
cache_mode = "deepseek_exact_prefix"
strict_tools = "auto"
json_output = true
prefix_completion = "opt_in"
fim_sidecar = true
```

这个配置的含义应该是：

- 主循环默认使用 DeepSeek thinking mode
- 工具严格模式自动尝试
- cache discipline 固定走 DeepSeek 精确前缀策略
- prefix completion 只在特定子流程启用，并优先被 TUI 吸收为“继续补全/沿前缀续写”动作
- FIM 不介入主循环，只作为旁路编辑引擎或 TUI 内部补洞能力

这里还要明确一个产品约束：

- `prefix completion` 和 `FIM` 可以存在调试入口
- 但它们不应长期占据普通用户的顶层命令心智
- 当 TUI 成形后，这些能力应尽量通过编辑动作、审批动作或上下文菜单触发

### 19.13 DeepSeek 传输层与端点分流

DeepSeek 不是“一个 base URL 打天下”的接法。官方文档里至少存在三类入口：

- 标准 OpenAI-compatible：`https://api.deepseek.com`
- Beta 能力入口：`https://api.deepseek.com/beta`
- Anthropic-compatible：`https://api.deepseek.com/anthropic`

因此 `termquill` 不应把 beta 能力做成“请求前临时拼接 URL 的字符串开关”，而应在 provider 初始化时建立清晰的 transport 分流：

```rust
pub enum DeepSeekEndpointClass {
    Primary,
    Beta,
    AnthropicCompat,
}
```

建议规则：

- 主对话、普通 tool call、普通 JSON 输出走 `Primary`
- `strict tools`、`prefix completion`、`FIM` 这类 Beta 能力走 `Beta`
- 只有在为了兼容外部 Anthropic/Claude 风格客户端时才走 `AnthropicCompat`
- endpoint class 必须进入 telemetry，避免线上出现“能力失效却不知道是不是打错入口”

这能避免两类常见问题：

- 某些 Beta 能力在标准入口无效，却被误判为模型行为不稳定
- 为兼容某个前端而把 Anthropic 兼容层误当主链路，导致能力集和事件形状漂移

### 19.14 `user_id` 与缓存隔离策略

DeepSeek 官方文档对 `user_id` 的说明不是装饰字段，而是会影响安全归因、请求隔离与缓存复用边界。

这对 `termquill` 很关键，因为如果多租户或多工作区场景下 `user_id` 设计不稳，会直接伤害 prefix-cache 命中率，甚至带来跨用户隔离问题。

建议：

1. kernel 只暴露通用的 `traffic_partition_key`
2. DeepSeek adapter 将其稳定映射为 `user_id`
3. 默认策略使用“稳定的终端用户级键”，不要每次请求生成随机值
4. 同一真实用户在同一工作区内应尽量复用同一个键，以保留缓存收益
5. 不要直接上传原始邮箱、用户名等 PII，应先做稳定哈希或内部映射

建议配置增加：

```toml
[providers.deepseek.routing]
user_id_strategy = "stable_per_end_user" # stable_per_end_user | stable_per_workspace | disabled
```

如果未来 `termquill` 支持团队共享代理，这一条会直接决定缓存收益和隔离边界是否同时成立。

### 19.15 SSE / Keep-Alive 解析纪律

DeepSeek 官方文档明确说明：

- 非流式请求期间会返回空白行作为 keep-alive
- 流式请求期间会返回 SSE comment 作为 keep-alive
- 若请求在 10 分钟内仍未开始处理，连接会被关闭

这意味着 `termquill` 的 HTTP / SSE 解析器必须足够宽容，不能把这些行为误判为协议错误。

建议实现约束：

1. SSE parser 显式忽略 comment frame 与空白 keep-alive
2. “一段时间没 token”不应直接判定 provider 死亡，而要结合连接状态与 keep-alive 判断
3. controller 需要把“连接存活但尚未出 token”和“真正超时失败”区分成不同事件
4. 超过 10 分钟仍未开始处理的请求，应归类为 provider-side start timeout，而不是普通 read timeout

如果不做这层纪律，后面在长推理、长工具回合、网络波动时很容易把可恢复事件当作失败处理。

### 19.16 错误分类与重试策略

DeepSeek 官方错误码与通用 OpenAI-compatible 语义接近，但 `termquill` 不应只做“429 重试、其他全报错”这种过粗处理。

建议最少分成这几类：

- `401/403`：认证或权限失败，立即失败，不自动重试
- `402`：余额或计费失败，立即失败，并在 UI 中明确提示
- `400/422`：请求构造错误，默认不重试；若识别为 `reasoning_content` 缺失、strict schema 不兼容，可走一次定向修复后重试
- `429`：限流，指数退避并结合 provider 并发闸门
- `500/502/503/504`：可重试的服务端或网关错误，使用短窗口重试

其中最值得单独做 repair 分支的是两类：

- `reasoning_content` 回传缺失
- strict tool schema 超出 DeepSeek 支持子集

这两类不是“模型随机失败”，而是可识别、可自动修复的请求构造问题。

### 19.17 基于能力表的路由阈值

现有方案已经区分 `flash` 与 `pro`，但如果要更贴合 DeepSeek 官方能力表，建议再把“什么时候升级到 `pro`”写成明确阈值，而不是口头经验。

建议至少以这些信号做路由决策：

- 是否需要长链规划而不只是执行
- 是否需要高风险代码审查或复杂收敛
- 是否连续两轮工具调用后仍未收敛
- 是否出现大上下文、多文件、高歧义修复

建议策略：

```text
默认：deepseek-v4-flash
升级到 pro：
- 首轮任务被 classifier 判定为复杂规划/高风险审查
- flash 连续 2 轮未收敛
- 需要 reviewer / planner 生成高价值结构化结果
回落到 flash：
- 进入执行型子任务
- 进入普通工具回合
- 进入格式化、补丁、摘要等中低风险步骤
```

这能把 `pro` 的投入聚焦在最值钱的回合，而不是把整条 agent loop 都拉到高成本档位。

### 19.18 并发调度与背压

DeepSeek 官方文档对账号级并发限制写得很明确：`deepseek-v4-flash` 与 `deepseek-v4-pro` 的并发上限不同，而且在提升并发配额的场景下，`user_id` 还会形成更细粒度的并发隔离。

这意味着 `termquill` 不应把 429 只当作“临时打满了，睡一下再试”的网络噪声，而要把 DeepSeek 的并发模型内建进 scheduler。

建议：

1. provider 维护独立的 `flash` / `pro` 并发信号量
2. 若启用了稳定 `user_id`，可选增加“每个 partition key 的局部信号量”
3. planner / reviewer / compactor 等后台子任务不能无限挤占前台主会话并发
4. 遇到 429 时优先做本地背压，而不是所有请求同时指数退避再一起重冲

建议抽象：

```rust
pub struct ProviderConcurrencyBudget {
    pub flash_global_limit: usize,
    pub pro_global_limit: usize,
    pub per_partition_limit: Option<usize>,
}
```

对于 `termquill` 这种 agent 内核，好的体验不是“理论峰值最高”，而是“在 DeepSeek 并发纪律下仍稳定收敛，不制造 429 风暴”。

### 19.19 模型发现与别名治理

虽然 `deepseek-v4-flash` / `deepseek-v4-pro` 已经是当前 canonical model id，但 `termquill` 仍建议在 provider 启动阶段做一次轻量模型发现与校验。

建议行为：

- 初始化时可选调用模型列表接口，验证配置模型是否真实可用
- 若用户配置了 `deepseek-chat` 或 `deepseek-reasoner`，启动时立刻归一化并告警
- 将“模型名归一化前后结果”写入诊断日志，方便后续排查历史配置

这样做的价值在于：

- 避免运行时才发现模型名失效
- 让 alias 弃用迁移变成启动期显式事件，而不是线上隐性行为变化
- 后续若 DeepSeek 再扩新模型，provider 能更平滑接入

## 20. `termquill-provider-deepseek` crate 骨架设计

这一节的目标不是直接写实现代码，而是把 crate 边界先收紧，回答一个关键问题：

`termquill` 如何在“DeepSeek-first”落地的同时，不把 kernel 做成 DeepSeek 专属？

答案是：把通用 session / controller / tool / event / permission 契约全部留在 `termquill-kernel`，而把 DeepSeek 的协议映射、端点分流、thinking 纪律、reasoning replay、strict tools 与 beta 能力下沉到独立 provider crate。

### 20.1 crate 定位

`termquill-provider-deepseek` 的职责应当是：

- 实现 `termquill-kernel::provider::Provider`
- 承接 DeepSeek 官方 API 的请求/响应映射
- 输出统一的 `ProviderChunk`
- 构造适用于 kernel 的通用 `ProviderCapabilities`
- 在 provider profile / quirk profile 中维护 DeepSeek 专项 feature / quirk
- 封装主入口、beta 入口、Anthropic 兼容入口的 transport 分流

它不应承担这些职责：

- 不管理 session log
- 不决定工具审批策略
- 不持有 workspace / sandbox 逻辑
- 不直接编辑文件
- 不定义 controller 事件协议

### 20.2 推荐目录结构

```text
crates/
  termquill-provider-deepseek/
    Cargo.toml
    src/
      lib.rs
      provider.rs
      config.rs
      client.rs
      endpoint.rs
      models.rs
      request.rs
      response.rs
      stream.rs
      mapper.rs
      capabilities.rs
      retry.rs
      concurrency.rs
      reasoning.rs
      tools.rs
      json_mode.rs
      prefix.rs
      fim.rs
      errors.rs
      tests/
        fixtures/
```

建议每个模块责任如下：

- `config.rs`：DeepSeek provider 配置结构与默认值
- `provider.rs`：`DeepSeekProvider` 主对象与 `Provider` trait 实现入口
- `client.rs`：底层 HTTP client 包装、鉴权头、公共请求发送
- `endpoint.rs`：`Primary / Beta / AnthropicCompat` 分流
- `models.rs`：DeepSeek API 侧的原始请求/响应 DTO
- `request.rs`：从 kernel `CompletionRequest` 到 DeepSeek 请求体的组装
- `response.rs`：普通响应与流式片段的解码模型
- `stream.rs`：SSE / keep-alive / comment frame 解析
- `mapper.rs`：把 DeepSeek 响应统一映射成 `ProviderChunk`
- `capabilities.rs`：构造 `ProviderCapabilities`
- `retry.rs`：错误分类、退避与可修复重试
- `concurrency.rs`：并发预算与背压
- `reasoning.rs`：thinking mode 与 `reasoning_content` replay 策略
- `tools.rs`：strict tools 可用性判断与 schema 分类
- `json_mode.rs`：`json_object` 输出路径
- `prefix.rs`：prefix completion 组装
- `fim.rs`：FIM sidecar 相关逻辑
- `errors.rs`：provider 内部错误枚举与标准化

### 20.3 `kernel` 与 `provider-deepseek` 的边界

应当明确哪些类型属于 `kernel`，哪些只能属于 `provider-deepseek`。

保留在 `termquill-kernel`：

- `Provider` trait
- `ProviderCapabilities`
- `CompletionRequest`
- `ProviderChunk`
- `CacheDiscipline`
- `ReasoningEffort`
- `ToolSchema`
- `UsageStats`
- `ProviderContinuationState`
- provider 无关的错误分类入口

只放在 `termquill-provider-deepseek`：

- `DeepSeekProviderConfig`
- `DeepSeekProviderProfile`
- `DeepSeekProviderQuirkProfile`
- `DeepSeekEndpointClass`
- `DeepSeekReasoningReplayPayload`
- `DeepSeekStrictSchemaClassifier`
- `DeepSeekRequestBody`
- `DeepSeekStreamEvent`
- `DeepSeekErrorBody`

边界判断原则很简单：

- 其他 provider 也会复用的概念，留在 `kernel`
- 只有 DeepSeek 文档才定义的概念，留在 `provider-deepseek`

### 20.4 `lib.rs` 对外暴露面

建议 `lib.rs` 只暴露少量稳定入口：

```rust
mod provider;
pub mod config;

pub use config::DeepSeekProviderConfig;
pub use provider::DeepSeekProvider;
```

`DeepSeekProvider` 建议提供类似构造器：

```rust
impl DeepSeekProvider {
    pub fn new(config: DeepSeekProviderConfig) -> anyhow::Result<Self>;
}
```

不要一开始就把大量内部模块公开导出。对外应该只让上层知道：

- 这个 crate 可以被构造
- 它实现了 `Provider`
- 它需要什么配置

### 20.5 provider 内部主对象

建议核心对象形态类似：

```rust
pub struct DeepSeekProvider {
    profile: DeepSeekProviderProfile,
    capabilities: ProviderCapabilities,
    primary_client: DeepSeekHttpClient,
    beta_client: DeepSeekHttpClient,
    anthropic_client: Option<DeepSeekHttpClient>,
    retry_policy: DeepSeekRetryPolicy,
    concurrency_budget: ProviderConcurrencyBudget,
}
```

这样做的好处是：

- 端点分流是显式状态，不是请求时现拼字符串
- capabilities 可在启动期固定下来
- 重试和并发预算不会散落在调用栈

### 20.6 request 组装链路

建议一次请求在 provider 内部走这条链：

```text
CompletionRequest
  -> endpoint selector
  -> DeepSeekRequestBuilder
  -> transport send
  -> stream/parser
  -> chunk mapper
  -> ProviderChunk stream
```

其中几条重要规则：

- endpoint selector 根据是否启用 `strict tools`、`prefix completion`、`FIM` 决定主入口还是 beta
- request builder 负责剔除 thinking mode 下无效的采样参数
- reasoning builder 负责判断是否需要补回 `reasoning_content`
- tool builder 负责 strict schema 兼容性降级

### 20.7 reasoning 子系统最小骨架

DeepSeek 是当前方案里最需要单独拆出 `reasoning.rs` 的 provider，因为这里不是单纯“多一个字段”，而是有明确状态机。

建议最少建这几个类型：

```rust
pub struct DeepSeekReasoningReplayPayload {
    pub pending_replay: Option<String>,
    pub must_replay_after_tool_call: bool,
}

pub enum DeepSeekThinkingMode {
    Enabled,
    Disabled,
}
```

这里的目标不是把 session state 搬到 provider 里，而是在单次 request materialization 时拥有足够的 DeepSeek 规则判断能力。

其中要特别注意：

- `DeepSeekReasoningReplayPayload` 可以是 provider-specific 的序列化结构
- 真正持久化到会话里的容器应是 kernel 的 `ProviderContinuationState`
- provider 重启、session resume、context compaction 后，仍应能从该 opaque state 恢复 replay 语义

### 20.8 tools 子系统最小骨架

`tools.rs` 的重点不是执行工具，而是做“DeepSeek 能不能严格接这个 schema”的预判。

建议抽象：

```rust
pub enum DeepSeekToolMode {
    Strict,
    NonStrict,
}

pub enum DeepSeekStrictCompatibility {
    Compatible,
    Incompatible { reason: String },
}
```

这样 controller 仍然只看到统一的工具接口，但 provider 能在请求组装前决定：

- 全量 strict
- 局部降级
- 整轮退回普通 tool mode

### 20.9 测试骨架

这个 crate 一开始就该有 4 层测试，而不是等行为复杂后再补：

1. DTO 反序列化测试
2. request 映射测试
3. stream / SSE 解析测试
4. capability 与降级路径测试

推荐 fixture：

- 普通文本流
- reasoning + text 混合流
- tool call 增量参数流
- keep-alive / comment frame
- strict schema 不兼容错误
- `reasoning_content` 缺失导致的 400

### 20.10 对未来通用 provider 的保护

为了避免 `termquill-provider-deepseek` 反向污染 `kernel`，建议提前定两条红线：

1. `kernel` 中不出现 `reasoning_content`、`beta_base_url`、`user_id` 这类 DeepSeek 专有字段名
2. provider-specific repair 逻辑只存在于对应 crate，不写进通用 controller

只要守住这两条，后面再加：

- `termquill-provider-openai-compat`
- `termquill-provider-anthropic`
- `termquill-provider-gemini`

都还是在扩展同一个通用内核，而不是不断为 DeepSeek 特判打洞。

### 20.11 下一步实现顺序

如果后面开始真正搭代码，我建议 `termquill-provider-deepseek` 按这个顺序落：

1. `config.rs` + `capabilities.rs`
2. `models.rs` + `request.rs` + `response.rs`
3. `client.rs` + `endpoint.rs` + `stream.rs`
4. `mapper.rs`
5. `reasoning.rs` + `tools.rs`
6. `retry.rs` + `concurrency.rs`
7. `json_mode.rs` + `prefix.rs` + `fim.rs`

这个顺序的好处是，先把主链路打通，再加 DeepSeek 专项增强，不会一开始就把 Beta 能力和 repair 分支缠成一团。
