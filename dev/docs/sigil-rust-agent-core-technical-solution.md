# Sigil Rust Agent 核心技术方案（Implementation Snapshot v1）

## 1. 背景

`sigil` 计划做成一个基于 Rust 的 AI coding agent，定位是 TUI-first 的终端产品、内核复用、前端可插拔。

它要继承的不是某个具体项目的代码形态，而是那套已经被验证过的核心能力模型：

- 配置驱动的模型与工具编排
- 一个可被多前端复用的无传输耦合 agent runtime
- 支持工具调用的 agent 主循环
- MCP 兼容的插件接入能力
- cache-first 的会话与记忆模型
- 可选的 planner / executor 双模型协作

这个项目应该复制“能力边界”和“架构契约”，而不是逐行翻译 Go 实现。源项目是 MIT 协议，参考其架构和能力设计在许可上没有问题，但 `sigil` 的实现必须保持 Rust 风格，不能把 Go 的包结构和偶然实现细节原封不动搬过来。

## 2. 目标

第一代 `sigil` 内核应该达成这些目标：

1. 用 Rust 构建一个可被 TUI、CLI、HTTP、未来桌面端共同复用的 agent kernel，其中 TUI 是第一用户表面。
2. 保持 provider、tool、plugin 都由配置和注册机制驱动，而不是写死在核心里。
3. 通过独立 provider crate 支持 DeepSeek、OpenAI-compatible、Anthropic 和 Gemini，同时保持内核 provider-neutral。
4. 内置工具和 MCP 工具通过统一的工具注册表暴露给 agent。
5. 保持 cache-stable 的 session 设计，把 prefix-cache 命中率视为顶层架构约束，而不是附带优化。
6. 提供适合自动化 coding 场景的 permission layer 和 workspace confinement。
7. 给 planner / executor 双模型协作预留清晰的架构边界，但不强行塞进 MVP。

## 3. 第一阶段非目标

第一版需要明确不做这些事情，避免一开始范围失控：

- 第一阶段不做桌面壳
- 第一阶段不做 codegraph 或更重的代码智能子系统
- 第一阶段核心 runtime 不依赖 npm、Homebrew 或自更新；首发分发包装层只作为 release 工程存在，复用 `sigil` binary、GitHub release archives、Homebrew tap formula 和 npm wrapper，不改变 TUI-first 产品入口
- 不把 Anthropic/Gemini/DeepSeek/OpenAI-compatible 的私有 request 或 stream 语义上移进 kernel
- 在单会话内核跑稳之前，不做复杂的多 agent 编排
- 第一阶段不继续扩张用户可见命令面，不把 provider 专项能力直接暴露成产品主心智

## 4. 设计原则

1. 契约优先：先定义稳定 trait 和事件契约，再铺前端。
2. 内核优先：CLI、TUI、desktop 都只是 adapter，不能各写一套执行逻辑。
3. TUI-first 产品表面：优先把真实用户会看到的交互壳做对，再决定哪些命令需要显式暴露。
4. 配置驱动、插件驱动：模型和工具来自配置、注册和运行时接入，不靠核心里的大段 `match`。
5. 缓存优先：system prompt prefix 尽可能稳定；memory、skills 只在 session 启动时折入一次；任何会破坏 byte-stable prefix 的动态注入都必须被隔离。
6. Rust 风格优先：用清晰 ownership、显式状态机和合理 async 边界，而不是机械翻译 Go。
7. 分阶段复杂化：crate 数量保持克制，crate 内按已经稳定的状态流、协议和渲染域拆分。

## 5. 当前工作区结构与模块边界（重构后）

当前实现已经从早期骨架进入“crate 边界稳定、crate 内按职责拆分”的阶段。本节描述的是当前代码事实，不再只是推荐草案；后续重构必须先判断是否继续维护这些 facade 与子模块边界。

当前 provider 边界已经进一步收敛：`sigil-runtime` 统一持有 provider-specific config parsing、provider-neutral config draft DTO、provider status request DTO、provider status refresh task manager、API key env label、DeepSeek 余额/模型列表请求、隐藏 DeepSeek prefix / FIM developer debug adapter 和 provider/model context-window metadata resolver；`sigil` binary 和 `sigil-tui` 只消费这些 runtime DTO/view/result/adapter，不直接依赖 provider crate 或 HTTP client。后续新增 provider 或 provider 状态面时，先扩展 runtime 表面，再让入口层消费 provider-neutral 结果。

```text
sigil/
  Cargo.toml
  rust-toolchain.toml
  dev/
    governance/
      code-standards.md
      engineering-standards.md
    docs/
      sigil-rust-agent-core-technical-solution.md
  crates/
    sigil-kernel/
      src/
        lib.rs
        agent.rs
        approval.rs
        config.rs
        event.rs
        memory.rs
        permission.rs
        provider.rs
        session.rs
        tool.rs
        tests/
          agent_tests.rs
          config_tests.rs
          memory_tests.rs
          permission_tests.rs
          provider_tests.rs
          session_tests.rs
    sigil-provider-deepseek/
      src/
        capabilities.rs
        client.rs
        config.rs
        endpoint.rs
        errors.rs
        fim.rs
        lib.rs
        mapper.rs
        models.rs
        prefix.rs
        pricing.rs
        provider.rs
        reasoning.rs
        request.rs
        response.rs
        retry.rs
        stream.rs
        tools.rs
        tests/
          *_tests.rs
          stream_test_support.rs
    sigil-provider-openai-compat/
      src/
        capabilities.rs
        client.rs
        config.rs
        errors.rs
        lib.rs
        mapper.rs
        models.rs
        provider.rs
        request.rs
        stream.rs
        tests/
          *_tests.rs
    sigil-provider-anthropic/
      src/
        capabilities.rs
        client.rs
        config.rs
        errors.rs
        lib.rs
        mapper.rs
        models.rs
        provider.rs
        request.rs
        stream.rs
        tests/
          *_tests.rs
    sigil-provider-gemini/
      src/
        capabilities.rs
        client.rs
        config.rs
        errors.rs
        lib.rs
        mapper.rs
        models.rs
        provider.rs
        request.rs
        stream.rs
        tests/
          *_tests.rs
    sigil-tools-builtin/
      src/
        changeset_tool.rs
        constants.rs
        execution_backends/
          bubblewrap.rs
          docker.rs
          local.rs
          mod.rs
          seatbelt.rs
        file_tools.rs
        lib.rs
        path.rs
        registry.rs
        shell.rs
        support.rs
        terminal_process.rs
        terminal_tools.rs
        tests/lib_tests.rs
    sigil-code-intel/
      src/
        lib.rs
        workspace.rs
        language.rs
        lsp.rs
        service.rs
        cache.rs
        tools.rs
        error.rs
        tests/
          *_tests.rs
    sigil-mcp/
      src/
        lib.rs
        tests/lib_tests.rs
    sigil-runtime/
      src/
        lib.rs
        tests/lib_tests.rs
    sigil-http/
      src/
        auth.rs
        config.rs
        driver.rs
        dto.rs
        lib.rs
        listener.rs
        openapi.rs
        protocol.rs
        registry.rs
        sse.rs
        tests/lib_tests.rs
    sigil/
      src/
        main.rs
        tests/main_tests.rs
    sigil-tui/
      src/
        app.rs
        app/
          state.rs
          tests/
        runner.rs
        runner/
          worker_loop/
            active_run.rs
            agent_runtime.rs
            mcp_refresh.rs
            provider_status.rs
            queue_driver.rs
            scheduler.rs
            task_runtime.rs
            terminal_refresh.rs
          tests/
        ui.rs
        ui/
          tests/
        commands.rs
        setup.rs
        config_panel.rs
        context_window.rs
        provider_status.rs
        sessions.rs
        slash.rs
        timeline.rs
        view_model.rs
        tests/
```

### 当前边界说明

- `sigil-kernel`：承载 provider、tool、session、event、approval、permission、memory、config 和 agent loop 等通用契约。当前采用 flat public module 文件，测试统一收纳在 `src/tests/*_tests.rs`；这里不出现 DeepSeek 专有字段，也不持有 TUI 状态。
- `sigil-provider-deepseek`：首个旗舰 provider，内部拆成 transport、endpoint、request、response、stream、mapper、reasoning、tools、pricing 等模块。DeepSeek 专项能力在这里解释和降级，不反向污染 kernel。
- `sigil-provider-openai-compat`：OpenAI-compatible Chat Completions provider，覆盖通用 streaming text、tool call、usage 和 endpoint/header 配置，不承载 DeepSeek reasoning replay、strict tools、prefix/FIM 或 beta endpoint 语义。
- `sigil-provider-anthropic`：Anthropic Messages provider，负责 Anthropic 版本 header、beta header、top-level system、`tool_use` / `tool_result` 和 incremental tool argument 映射；kernel 只看到中立的 message、tool spec、usage 和 `ProviderChunk`。
- `sigil-provider-gemini`：Gemini GenerateContent provider，负责 `systemInstruction`、`functionDeclarations`、`functionCall` / `functionResponse` 和 block reason 映射；Gemini 的 function-response 配对细节保留在 provider crate 内。
- `sigil-tools-builtin`：隔离文件、shell、搜索等内置工具实现，统一通过 `Tool` trait、preview、permission subject 和结构化 `ToolResult` 回到 agent loop。`lib.rs` 只保留兼容 façade；工具注册、workspace path confinement、文件工具、changeset、shell、persistent terminal 和 non-interactive execution backend 分别维护在对应子模块中，backend 内部再按 local / Seatbelt / Bubblewrap / Docker 拆分。
- `sigil-code-intel`：隔离 LSP client 生命周期、Rust Tree-sitter fallback、RepoMapLite request-local source map、符号/诊断缓存、warm LSP context snapshot、只读 code intelligence tools，以及带 approval diff preview 的 LSP edit tools（code action / rename）。配置结构保留在 kernel 的通用 `CodeIntelligenceConfig` / `LanguageServerConfig` 中，code-intel 可以依赖 kernel 的工具契约和配置类型，但 kernel 不反向依赖 LSP 或 Tree-sitter；动态代码智能结果、warm LSP snapshot 和 RepoMapLite 候选只通过 bounded context/tool result 进入 provider-visible history，不注入 system prompt，也不落成 persistent repo graph。
- `sigil-mcp`：隔离 stdio MCP client 与工具适配逻辑，把远端 MCP 工具包装成同一个 kernel tool registry surface。
- `sigil-runtime`：收口跨入口共享的 provider factory、tool registry、run options 和 Context V0 source provider contract / hard-cap enforcement，避免 TUI / CLI 各自硬编码装配链。它负责把 RepoMapLite 候选转换为带 score breakdown 的 bounded Context V0 items，把 caller-supplied warm LSP snapshot 转成 bounded `LspSymbol` / `LspDiagnostic` / `LspReference` rows，并把 trusted plugin hook output / caller-supplied MCP resource text 通过同一个 source-provider contract 转成 `ExtensionProvided` / `McpResource` rows；snapshot 缺失、plugin 未信任或 MCP resource 缺少 egress decision 时只产生 excluded provenance，不阻塞普通 request。kernel 只看到 provider-neutral `ContextItem` 和 packer，不知道 runtime 存在。
- `sigil-http`：HTTP/SSE adapter crate。`lib.rs` 只保留兼容 façade；protocol envelope、server config、bearer auth、loopback listener framing、SSE durable/live event surface、DTO、run driver trait、session/run registry 和 OpenAPI schema 分别维护在对应子模块中。listener 只拥有 HTTP framing/auth/registry routing，不依赖 `sigil-tui`，不复制 agent loop。
- `sigil`：提供 `sigil` binary。无子命令时直接启动 TUI；`run`、`doctor`、`serve` HTTP/SSE adapter preflight 和隐藏 provider 调试命令保留为显式子命令，不承担最终产品心智；`serve` 当前只验证 localhost/token defaults 并输出 routing pending 状态，不启动 HTTP listener；诊断事实由 `sigil-runtime` 提供，避免 CLI 与 TUI 后续各写一套判断。
- `scripts/build-release-archive.sh`：提供本地 release archive 构建与 built binary smoke；`scripts/render-homebrew-formula.sh` 生成 `sigil-ai.rb` tap formula；`scripts/prepare-npm-packages.sh` 从 release archives 生成 scoped npm wrapper 和 platform package tarballs；`.github/workflows/release.yml` 在 tag 发布时构建多平台 archive、生成 provenance attestation、渲染 Homebrew formula asset、准备 npm tarballs 并创建 GitHub release。独立 tap 同步、npm registry 发布、crates.io package name 决策和自更新仍是 release-management 工作。
- `sigil-tui`：第一用户入口的 TUI 实现。`app.rs`、`runner.rs`、`ui.rs` 是 facade；状态流、worker 协议和 renderer 分别下沉到 `app/*`、`runner/*`、`ui/*`；`app/state.rs` 承载 `RuntimeStatusState`、`ComposerState`、`ApprovalState` 和 `SessionBrowserState`，避免继续向根 `AppState` 追加散落字段；`runner/worker_loop.rs` 只保留 worker façade，scheduler、active run、queue、MCP refresh、provider status、agent runtime、task runtime 和 terminal refresh 维护在 `runner/worker_loop/*`；TUI `/doctor` 复用 runtime 诊断事实；普通模块测试在 `src/tests/*_tests.rs`，状态流测试在 `app/tests/*_tests.rs`，runner 测试在 `runner/tests/*_tests.rs`，renderer 测试在 `ui/tests/*_tests.rs`。

这个拆分仍然比“教科书式 Clean Architecture”更少：crate 边界只承载产品级职责，crate 内模块才承载局部复杂度。memory、permission、config、session 继续留在 `sigil-kernel` 内，因为它们共同定义通用执行语义；TUI 的输入、modal、session、approval、timeline、worker bridge 等状态流则留在 `sigil-tui` 内，因为它们属于第一用户表面的交互模型。

### 重构后不变量

- `app.rs` 只保留 `AppState` façade、bootstrap、顶层 key routing 和跨状态编排；运行状态、composer、approval 和 session browser 字段归入 `app/state.rs` 的领域 bundle；新增状态流放入 `app/*`，状态流测试放入 `app/tests/*_tests.rs`。
- `runner.rs` 只暴露 worker protocol 和 spawn 入口；worker command/message、spawn 装配、event bridge、approval bridge、session/compaction flow 放入 `runner/*`；worker loop 的 scheduler、active run、queue、MCP/provider refresh、agent/task runtime 和 terminal refresh 放入 `runner/worker_loop/*`。
- `ui.rs` 只作为 renderer 模块入口和必要 re-export；shell layout、theme、geometry、text、timeline、tool card、markdown、approval、setup/config、modal 等渲染块放入对应 `ui/*`。
- 单元测试实现不再回填 inline test module；业务文件只保留测试模块声明，测试实现放入同层 `tests/<module>_tests.rs`、领域专属 `app/tests/*` / `runner/tests/*` / `ui/tests/*`，共享 fixture 使用 `common.rs` 或 `*_test_support.rs`。
- Markdown 只由 `ui/markdown.rs` 和 `MarkdownRenderOptions` 统一解析和缩进，不允许 assistant timeline、tool preview、approval modal 各自维护解析规则。
- 新增快捷键或命令时，必须同步 `commands.rs` metadata、info rail、keyboard help 和 README。

这里要特别说明：这不意味着 `sigil` 被做成 DeepSeek 专属，而是表示第一套“做深做透”的 provider 先落在 DeepSeek 上；OpenAI-compatible、Anthropic 和 Gemini 也必须服从同一个 `sigil-kernel` 契约，而不是反过来把内核做成某家厂商私有运行时。TUI `/config` 和 `doctor` 只消费 `ProviderCapabilities` 派生出的中立 capability view，不展示 provider 私有字段作为产品主心智。

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
- 切模型是 config 和 runtime entrypoint 的问题，不是编译期静态问题
- provider 抽象必须能承载 background task、response handle、reasoning artifact、续跑 cursor 这些能力位
- 不能把 provider 的长任务、推理摘要、工具流事件都压扁成“只有文本 delta”的模型
- provider 的 `stream()` 必须是真流式：HTTP/SSE body 读取、SSE frame 解码和 `ProviderChunk` 映射应边读边 yield；只允许在尚未 yield 任何 chunk 前做透明 retry

### 6.2 Tool 抽象

内置工具和 MCP 工具必须统一满足同一个运行时接口。

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<Vec<ToolSubject>> {
        Ok(Vec::new())
    }

    fn permission_access(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<ToolAccess> {
        Ok(self.spec().access)
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<Option<ApprovalMode>> {
        Ok(None)
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<Option<ToolEgressAudit>> {
        Ok(None)
    }

    async fn preview(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
    ) -> anyhow::Result<Option<ToolPreview>> {
        Ok(None)
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> anyhow::Result<ToolResult>;
}
```

这里的关键约束是：

- 每个工具都要暴露 JSON Schema 兼容的参数定义
- `ToolSpec` 必须保持 provider-neutral，表达 `name / description / input_schema / category / access / preview`，不携带 DeepSeek、MCP 或 TUI 私有状态
- 工具执行失败要返回给模型，不应该直接把整个进程打死
- preview 是可选能力，只给交互式前端做审批卡片和 diff 预览用，返回统一的 `ToolPreview`
- `permission_subjects` 是审批与 permission layer 的稳定资源键，文件类工具必须从结构化参数中导出，shell / MCP 等工具可返回多 subject，而不是让 UI 猜字符串
- `permission_access` 默认使用 `ToolSpec.access`；少数工具可以按本次参数保守调整 access，例如 `bash` 只对简单只读 allowlist 命令降为 `Read`，未知或复杂语法仍为 `Execute`
- `permission_default_mode` 用于工具域内更具体的默认审批策略，例如 MCP server trust policy；它只改变默认基线，显式 permission tool/rule override 仍然优先
- `egress_audit` 用于工具域内安全出境审计摘要，返回值会进入 durable control state；实现必须先脱敏并限制大小，不能包含原始 secret、文件内容或大 payload
- `execute` 必须接收 provider 侧的 `call_id` 并原样写回 `ToolResult.call_id`，保证 tool call / result 配对可恢复
- 文件类内置工具必须对 workspace root 做 canonicalize，并用路径组件判断 confinement；绝对路径、`..`、目标 symlink 或父目录 symlink 指向 workspace 外时必须生成 `External` subject，再由 `permission.external_directory` gate 决定 deny / ask / allow
- 临时 shell scratch 文件使用运行时注入的 `$SIGIL_SCRATCH_DIR`，实际目录位于 Sigil 用户态 cache root，对模型显示为 `cache/tmp`。系统 temp 目录不作为内置例外：`/tmp`、macOS `/private/tmp`、Windows `%TEMP%` 等仍属于 workspace 外路径，必须走 `permission.external_directory`。

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

为了保留 Reasonix 最核心的“缓存极致利用”特性，`sigil` 不应该只停留在“尽量少改 prompt”的口号层，而要直接把上下文建模成三个区域：

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

此外，缓存利用不能只靠“感觉”，必须做成硬观测项。`sigil` 需要在 telemetry 中持续产出：

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
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub previous_response_handle: Option<ResponseHandle>,
    pub continuation_states: Vec<ProviderContinuationState>,
    pub traffic_partition_key: Option<String>,
    pub background: bool,
    pub store: bool,
    pub deterministic_materialization: bool,
}
```

关键点：

- `provider_name` 和 `model_name` 分开存，避免后续切换模型时语义混乱
- 缓存纪律不再作为单独 request 字段存在；稳定前缀、append-only log、control state 和 `deterministic_materialization` 共同约束 request materialization
- `previous_response_handle` 预留给支持 response continuation 的 provider
- `continuation_states` 承载 provider 私有且必须跨 turn / resume / compaction 存活的 opaque state
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
    pub status: ToolResultStatus,
    pub metadata: ToolResultMeta,
}

pub enum ToolResultStatus {
    Ok,
    Error(ToolError),
}
```

关键点：

- `args_json` 在重组完成前应保留原始字符串形态，避免过早解析把截断问题藏起来
- 错误分类只放在 `ToolResultStatus::Error(ToolError)` 中，不通过 metadata 或文本约定判断
- provider-visible tool message 使用 `ToolResult::to_model_content()` 生成稳定 compact JSON envelope，顶层包含 `status`、`content`、`error`、`meta`
- `ToolResultMeta` 可承载 `exit_code`、`changed_files`、`truncated`、`bytes` 等非错误分类信息；当前还会在 `meta.details.call` 写入安全的调用上下文摘要，例如 `command`、`path`、`pattern`、`subjects`，供 TUI tool card 在标题行显示“这次到底调用了什么”

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
- `ControlEntry` 只给 agent runtime、resume、审计和 UI 使用，不进入上游 prompt

建议把 `ControlEntry` 做成 append-only 的系统控制记录，而不是临时运行时侧带：

```rust
pub enum ControlEntry {
    SessionIdentity { provider_name: String, model_name: String },
    ContinuationStateSaved(ProviderContinuationState),
    ResponseHandleTracked(ResponseHandle),
    BackgroundTaskTracked(BackgroundTaskHandle),
    PrefixSnapshotCaptured(PrefixSnapshot),
    MemorySnapshotCaptured(MemorySnapshot),
    UsageSnapshot(UsageStats),
    ToolApproval(ToolApprovalEntry),
    ToolExecution(ToolExecutionEntry),
    CompactionApplied(CompactionRecord),
    Note { kind: String, data: serde_json::Value },
}
```

建议语义：

- `ContinuationStateSaved`：保存必须跨 turn / resume / compaction 存活的 provider 私有状态
- `ResponseHandleTracked`：记录可续跑句柄
- `BackgroundTaskTracked`：记录后台任务句柄
- `PrefixSnapshotCaptured`：记录当前稳定前缀的快照
- `MemorySnapshotCaptured`：记录 request 使用的 memory/system 消息；后续 request 在 fingerprint 未变时复用该快照，fingerprint 变化时追加新快照，避免静默忽略 `AGENTS.md` 等文件更新
- `UsageSnapshot`：记录 usage、cost 与 cache token 统计，供 resume 后恢复 session 生命周期累计 stats
- `ToolApproval`：记录权限策略评估、审批请求、审批结果和 preview 失败，包含 call id、tool name、access、subjects、policy/user decision、reason 与 preview hash
- `ToolExecution`：记录工具执行 started/completed/failed/interrupted，包含 duration、subjects、changed files、metadata、structured error 与 provider-visible result hash
- `CompactionApplied`：记录稳定 compaction summary 与 tail 计数，供后续 request 做 provider-visible projection
- `Note`：承接不值得升格为独立结构的控制面元数据

这样做的好处是，provider continuation、后台任务恢复、缓存诊断都会落在同一条 append-only 审计链上，而不是散在 runtime 内存和 UI 状态里。

当前实现中，`Session` 提供 `latest_response_handle`、`latest_prefix_snapshot`、`latest_compaction_record` 和 `continuation_states` 这类显式查询方法；agent run 初始化下一轮 request 时会从 durable control state 恢复最新匹配 provider 的 response handle，而不是只依赖进程内变量。

工具恢复规则是：只有 `Started` 没有 `Completed / Failed / Cancelled / Interrupted` 终态的 execution，在 `Session::load_from_store` 时追加 `Interrupted` 控制记录；provider-visible history 若仍等待 tool result，则投影一个结构化 `ToolErrorKind::Interrupted` tool result，不自动重放工具。

`agent.max_turns` 默认不限制，用户可在配置里显式设置数字作为保险丝。它是防止模型无限循环请求工具的运行保护，而不是工具执行错误分类。当前 agent 达到该阈值时会发出 notice 并以可恢复方式结束本轮 run，保留已经追加的 assistant tool calls 和 tool results；下一条用户消息可以继续基于这些历史推进，不把这类停止伪装成 bash/read_file 等工具失败。

建议至少保留一类 provider 无关的 continuation 记录：

```rust
pub struct ProviderContinuationState {
    pub provider_name: String,
    pub state_kind: String,
    pub message_id: Option<String>,
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

当前实现中，`UsageSnapshot` 是持久化事实源，`SessionStats` 可以从 append-only control log 重建整会话累计 usage。TUI 额外维护一个非持久化的 `session_delta_stats`，表示本次打开、恢复或切换到当前 session 后新增的 usage/cost；它在新 session 或 session switch 时清零，在每个 `RunEvent::Usage` 到达时与整会话 `stats` 同步累加。

用户侧费用展示必须区分两个口径，UI label 使用自然短语而不是内部字段名：

- `total spent`：从 session 创建至当前的生命周期累计扣费，resume 后由 `UsageSnapshot` 重建
- `spent since opening`：本次 TUI 打开或恢复该 session 后新增的扣费，不写入 session log

cost 字段当前仍以 provider 计价逻辑输出的 USD 金额作为内部源。TUI 展示时根据 provider balance 的 currency 选择显示货币；DeepSeek balance 返回 `CNY` 时，`total spent`、`spent since opening` 与 `cache save` 统一显示为 `CNY`，避免余额与扣费单位混用。

### 6.7 确定性序列化规范

如果 `sigil` 要把缓存命中做成旗舰能力，那么 prompt materialization 不能交给默认 JSON serializer 的偶然行为。

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

## 7. Agent runtime、runner 与事件流

当前实现没有单独的 `controller/` 模块。通用执行入口由 `sigil-kernel::Agent`、`AgentRunOptions`、`RunEvent` 和 `EventHandler` 承担；TUI 的交互控制由 `sigil-tui/src/runner/*` 的 worker protocol、spawn、event bridge、approval bridge 与 session flow 承担。

这个拆法的边界是：

- kernel 只描述 agent run、session、approval、tool、provider 和事件契约，不知道 TUI worker 存在
- TUI runner 把用户交互转成 `WorkerCommand`，把 kernel 事件和运行结果转成 `WorkerMessage`
- CLI 可以直接使用 runtime 装配和 kernel agent loop，不需要引入 TUI runner
- 未来 HTTP / desktop shell 可以复用 kernel event stream，但各自拥有自己的 transport protocol

### 7.1 TUI worker 命令面

当前 TUI worker protocol 的命令包括：

- `SubmitPrompt { prompt, reasoning_effort }`
- `SubmitTask { prompt }`
- `ContinueTask { task_id: Option<String>, guidance: Option<String> }`
- `ApprovalDecision { call_id, approved }`
- `CancelRun`
- `CompactNow`
- `StartNewSession { session_log_path }`
- `SwitchSession { session_log_path }`
- `Shutdown`

对应消息包括：

- `Event(Box<RunEvent>)`
- `Notice`
- `RunStarted`
- `RunFinished`
- `TaskRunStarted`
- `TaskRunFinished`
- `RunCancelled`
- `NewSessionStarted`
- `SessionSwitched`
- `SessionCompacted`
- `RunFailed`

### 7.2 Kernel event 模型

当前 kernel `RunEvent` 包括：

- `TextDelta`
- `ReasoningDelta`
- `ToolCallStarted`
- `ToolCallArgsDelta`
- `ToolCallCompleted`
- `ToolApprovalRequested`
- `ToolApprovalResolved`
- `ToolResult`
- `Usage`
- `ContinuationState`
- `Control`
- `AssistantMessage`
- `Notice`

CLI、TUI、HTTP streaming、未来 desktop UI 都应该消费同一套事件流，而不是各自重写 turn lifecycle。

其中 `Usage`、`Control` 和 session stats 至少要能让前端展示：

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

- `submit_background`：把任务交给 provider 后台执行，session control state 或外层 runner 持有 `BackgroundTaskHandle`
- `resume_background`：基于 `BackgroundTaskHandle` 或 `ResponseHandle` 恢复轮询或流式追尾

这意味着 agent loop 不应该假设“一个 turn 只能是同步流式完成”，而要允许：

- 同步流式完成
- 后台排队后轮询完成
- 先流一段，再断线后基于 cursor 继续追流

## 9. Planner / Executor / Subagent 协作模型

Planner / executor / subagent 协作已经作为 TUI-first task flow 落地。Durable task 入口是 TUI `/task <任务>`；`/plan` 只表示一次性 Plan mode / read-only planning prompt，不创建 durable task state。恢复后不会自动重放未完成任务。composer 普通输入始终保持 chat-first，不会因为当前 session 有未完成 task context 自动触发 `ContinueTask`。`/task continue` 是显式继续入口；`/plan continue` 不再作为 alias。普通 chat 明确要求 subagent / 子 agent delegation 时，可通过 agent-thread tools 直接创建 child agent，不需要进入 durable task。

当前实现选择如下：

- `sigil-kernel::TaskStateProjection` 从 append-only control log 重建 task run、plan、step、child session 和 route 摘要状态。
- Planner 通过 internal model-visible `task_plan_update` tool 写入 durable plan；该 tool 由 agent loop 拦截并写 `ToolExecution` audit，不作为普通 workspace tool 执行。
- Executor step 使用 transient request context 接收 objective / plan / step，不把每个 step prompt 写成 parent session 的普通 user message。
- Continue guidance 使用同一 transient request context 注入当前 executor/subagent step，不作为新的普通 user history 写入。
- Subagent read/write step 使用 child session；parent session 只记录 child-session link、状态和 summary hash。
- Plan mode prompt 使用普通 agent loop，但用户 prompt 和 plan-mode 指令都只作为本轮 transient context 注入，不追加为 parent `User` entry；工具面使用 planner scoped registry，同时保留 agent-thread tools 以支持显式只读 delegation。
- Kernel 已提供独立于 durable task 的 `PlanApproved` control entry 和 `PlanApprovalProjection`，记录 plan version/hash、批准时间、`ask` 或 `workspace_edits` 权限、scope、过期策略和是否清理 planning context；`workspace_edits` 只覆盖带 required preview 的 workspace file write tool，不放宽 shell/execute、network、MCP 或 Agent spawn。TUI plan prompt 完成后会在 live band 展示 approval surface，并通过 worker `ApprovePlan` 追加 `PlanApproved` 后同步回 TUI；`ApprovePlan` 会从 plan 文本中保守提取 workspace path 写入 `PlanApprovalScope.workspace_paths`，plan 未包含路径时 scope 为空并保留既有全 workspace 行为；执行阶段已按 active `PlanApproved(workspace_edits)` 将 scope 内 workspace file write 的 `Ask` 降级为 `Allow`，显式 `Deny`、external directory、空 subject、scope 外路径和非文件写工具仍按原 permission policy 处理。模型语义偏离 approved plan 时要求重新批准仍是后续项。
- Child agent result 默认只把 bounded summary 和 result ref 带回 parent context；`wait_agent` 只负责轻量状态同步，不返回 child final answer 正文。完整 final answer 保留在 child session，需要更多细节时通过独立的 `read_agent_result` tool 显式分页读取；分页正文只作为当前 request 的 transient context 提供给模型，durable parent tool result 只记录 offset、长度、截断状态和 result ref，不能通过无限调大 summary、反复 wait 重复 summary、恢复后重复回放分页正文或回灌完整 child transcript 解决长报告场景。
- Parent agent 在发起 Agent 类 tool call 的同一 model turn 中产生的 pre-tool assistant 文本只作为 live stream 展示，不作为持久 parent session history 重放，并在 TUI 中按 Thinking 样式渲染；这避免“先自己补做，再等待子 agent”的内容污染父上下文。最终面向用户的回答必须发生在 child result/status 已回到 parent 后的后续 turn。
- 普通 chat 如果用户明确要求 subagent / 子 agent delegation，TUI 会把该意图映射为 `AgentDelegationRequirement`；agent loop 在可用 Agent 类工具面存在时会拒绝接受未产生 terminal 或 result-bearing Agent 类工具结果的 final answer，先用 transient retry prompt 要求模型调用 `spawn_agent` 等 agent-thread tool，重试后仍未满足时不写入该 final answer；无效输入、tool execution error 或仍处于 running 状态的 agent tool result 不会解除 hard gate。
- 主会话 running-input queue 是内部 durable control plane，使用 `ConversationInputQueued`、`ConversationInputEdited`、`ConversationInputReordered`、`ConversationInputStatusChanged` 和 `ConversationInputQueueControl` append-only control entry 持久化；TUI 产品层把它呈现为 visible follow-up，而不是暴露为隐藏队列。普通 chat 在 active run busy 时会显示为 follow-up，不提前写入 provider-visible user history；busy 状态下的 agent mention 不会静默降级成 main-thread chat，而是保留输入并提示用户等待或使用专门的 agent message 入口。worker 在当前 turn 结束后 FIFO dispatch，成功写 `Delivered`，失败写 `Rejected` 并 pause queue。`/queue next` 只调整顺序等待下一 turn；`/queue interrupt`（兼容旧别名 `now`）先走 cancel/interrupted audit，再 dispatch 选中 item。
- Background child result completion 与 follow-up / internal queue 有明确优先级：`join_before_final` / blocking child 完成后优先触发 parent continuation；普通 non-blocking background child 完成只写 `AgentResultContinuation(Pending)` ready 状态。当主会话已经有 pending follow-up 时，non-blocking result 不抢占 queued input，只以 bounded transient system notice 提醒模型可按需 `wait_agent` / `read_agent_result`。
- `/agent close <child-id|current>` 不再由 TUI 直接追加 `AgentThreadClosed`；TUI 只解析目标并发送 worker `CloseAgent`，worker 通过 runtime `close_agent_thread` 复用 model-visible `close_agent` 的 terminal 校验和 control entry 生成，再把同步后的 session entries 返回给 TUI。running thread 仍通过后续 cancel path 处理。
- `message_agent` 已作为 agent coordination tool 注册，用于给 active background child mailbox 投递 follow-up。它记录 `AgentThreadMessageRouted` requested -> resolved/rejected 审计，tool result 明确返回 `delivered_to_mailbox`、`will_apply_after_current_turn`、`interrupt_requested=false` 和 `interrupts_in_flight_provider_stream=false`；语义是 next safe point steering，不承诺 mid-token 或正在执行 tool 时实时中断。terminal child、无 mailbox 或无效目标会返回 rejected，且不改变 child lifecycle terminal status。
- Subagent tool approval 与 MCP elicitation 会在 parent session 记录 route summary；真实工具审批、工具执行和 elicitation 决策仍按原有 control entry 机制审计。
- 普通 tool error 是 agent loop 的可恢复输入；如果 step 最终产出回答，task orchestrator 继续后续步骤，并把恢复过的错误写入 step reason。审批拒绝、权限类错误、interrupted tool call 和 max turns 仍会阻断 task。
- Role-specific provider、reasoning effort 和 tool scope 由 `sigil-runtime` 装配；planner 与 subagent-read 默认只读，executor 默认完整工具面，subagent-write 受 `[task].allow_write_subagents` 控制。
- `sigil-runtime::AgentProfileRegistry` 已把内置 role 投影为 profile，并通过 `AgentInvocationPolicy`（`manual_only` / `model_allowed` / `system_only`）和 `AgentResultPolicy`（`summary_only` / `summary_with_page_ref` / `artifact_only` / `foreground_merge_required`）表达调用与结果返回语义。旧 session/profile JSON 中只有 `user_invocable` / `model_invocable` 时会反推 invocation policy；model-visible agent index 只暴露 trusted、enabled、scope-contained 且 `model_allowed` 的 profile，并把 `result_policy` 纳入 fingerprint 和 `spawn_agent` 描述。runtime worker 使用 workspace-aware registry，已支持从 `[skills].workspace_agents_dir`（默认 `.sigil/agents`）发现 Sigil-native workspace profiles：`.sigil/agents/<id>/agent.toml` 或 `.sigil/agents/<id>/AGENT.md`。Native profiles 默认 enabled、manual-only、needs-review、read-only，只有显式 trusted 且 model_allowed 后才进入 model-visible index；`AgentProfileTrustDecision` append-only control entry 会通过 `AgentProfileTrustProjection` 覆盖非 system profile 的 trust 状态，TUI worker 的 agent tools 注册面和 runtime supervisor 都使用 session-aware registry，因此 source/profile hash 变化后旧 trust decision 会失效并回到 `needs_review`，默认退出 model-visible index；duplicate built-in/profile id 会 warning 并跳过，alias/slash name 冲突会 deterministic warning 并禁用冲突别名，symlink escape 会 warning 并跳过。同一 registry 还会把 skill discovery 中 `run_as=child_session` 的 trusted compatibility entries 投影为 subagent profiles：`.sigil/agents/*.md`，以及显式配置 `[skills].compatibility_sources = ["claude", "reasonix"]` 后的 `.claude/agents/*.md` 和 `.reasonix/agents/*.md`；`disable-model-invocation` / `disableModelInvocation` 会映射为 manual-only，`allowed-tools` / `allowedTools` 只能收窄工具面，包含 `disallowed-tools` / `disallowedTools` 的条目因 subtractive scope 不能安全表达为 profile 会 warning 并跳过。受信任 plugin manifest 可通过 `[[agents]]` 贡献 agent profile；未 trust plugin 只在 config 中展示 capability，不注册 runtime profile，已 trust 且 hash 匹配时才生成 `AgentProfileSource::Plugin` profile，并用 namespaced id 避免与 workspace/native profile 裸 id 冲突。spawn 时 profile tool scope 会与 role registry scope 取交集，profile 不能扩大角色原本的工具面；profile description/instructions 会作为 transient child system prompt 注入子会话，不持久化进 parent history。
- `AgentProfilePolicyDecision` append-only control entry 已用于非 system profile 的 effective policy overlay，覆盖 `enabled` / `user_invocable` / `model_invocable`。policy replay 需要 profile id、source、source hash、profile hash 全部匹配当前 snapshot；hash 变化后旧 policy 失效。runtime `model_visible_index`、`AgentToolRuntime::resolve_spawn_profile` 和 `AgentSupervisor::begin_chat_child_thread` 使用 effective policy 过滤，但 overlay 不修改源 `AgentProfile`，因此不会污染 snapshot hash。
- TUI `/config` 的 `Agents` section 已改用 workspace-aware `AgentProfileRegistry`，展示 built-in、native、compatibility profiles 的 source/kind/trust/effective enabled/user/model、provider/model、tool scope 和 nickname candidates；footer trust/block/enable/user/model actions 会追加 `AgentProfileCaptured` 与对应 trust/policy decision 到当前 session JSONL。普通 inline/reusable skill 留在 `Skills` section，并继续通过 footer load/invoke 生成受 runtime `load_skill` policy 约束的请求；slash selector 的 skill fallback 同步限定为 trusted inline skills，`run_as=child_session` 兼容资源不再作为普通 skill slash row 展示或通过 `/skill-id` 解析启动。Composer 起始 `@` 会打开 agent mention selector，候选只来自 enabled、trusted、user-invocable 的 session-aware profiles；提交 `@profile <prompt>` 会走 TUI worker `InvokeAgentProfile` 和 runtime `AgentToolRuntime::invoke_agent_profile`，以 `AgentInvocationSource::Mention` 启动 foreground child thread，并按 user-invocable policy 校验，而不是把 mention 当普通 chat prompt 交给 delegation hard-gate。

这个模型的重点不是把所有角色塞进一个 provider-visible transcript，而是把 task coordination 写入 control plane：Plan 是 durable control data，executor/subagent 只看到 bounded context，用户界面从 projection 展示可恢复状态。

## 10. Memory 模型

Memory 必须遵循 cache-first 思路。

### 10.1 分层文档记忆

第一层 memory 建议支持：

- `SIGIL.md`
- `AGENTS.md`
- `CLAUDE.md`
- 本地覆盖文件，如 `SIGIL.local.md`
- 单独一行的 `@path` 导入

这样设计的理由：

- `SIGIL.md` 是项目自己的命名
- 同时兼容 `AGENTS.md` / `CLAUDE.md`，迁移成本更低

### 10.2 Prefix 稳定性

在 session boot 时：

- 先加载基础 system prompt
- 再追加语言和行为策略
- 再加载层级 memory 文档
- 再加载 skill index

在 session 运行中：

- 已加载的 memory/system 消息通过 `MemorySnapshotCaptured` 进入 append-only control log；后续 request 与 resume 在 fingerprint 未变时复用最新快照
- 当 `AGENTS.md`、`SIGIL.md` 或导入的 memory 文档变化导致 fingerprint 改变时，下一轮 request 会追加新的 `MemorySnapshotCaptured` 并使用新内容；这会形成受控 cache reset 点，但不会让 AI 继续执行旧指令
- 单轮用户临时要求应作为普通 user message 进入 tail history，不应改写已持久化的旧 memory snapshot

### 10.3 Cache-Safe Compaction

`sigil` 必须支持 compaction，但 compaction 只能作为“受控的稀有 cache reset 点”，不能退化成普通 agent 常见的随手改写历史。

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

MCP 是 `sigil` 的核心差异化能力之一，应该尽早落地。

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

对 `sigil` 的具体价值分别是：

- `roots`：可和 workspace sandbox 对齐，把“允许 server 看到哪些根目录”正式协议化
- `progress`：长时工具和远程 server 可以发正式进度，而不是靠文本日志刷屏
- `elicitation`：server 能在 `tools/call` 等处理中合法地向用户要补充输入

当前 stdio MCP 实现已经支持 `initialize`、`tools/list`、`tools/call`、provider-visible 名称清洗/截断/hash 去重、read-only `resources/list` / `resources/read`、read-only `prompts/list` / `prompts/get`，并在等待响应时处理 server 发来的反向请求：

- `roots/list` 返回入口层已解析的 workspace root，runtime 必须把 TUI / CLI 的 effective workspace root 传入 MCP 注册流程
- `notifications/progress` 映射到 TUI live panel，不写重复 timeline，避免远端 server 用 progress 刷爆用户界面
- `notifications/tools|resources|prompts/list_changed` 标记 server stale，并在 worker 空闲边界刷新该 server 的 provider-visible tool surface
- `elicitation/create` 已由可插拔 client handler 承载：TUI runtime 声明 `elicitation` capability，并通过 modal 让用户 accept / decline / cancel flat primitive object 字段；非交互默认 handler 返回明确 unsupported JSON-RPC error，不伪造用户输入，也不让请求挂死。TUI elicitation decision 会写入 append-only `ControlEntry::McpElicitation`，只记录 server、请求 message/schema hash、字段名和 action，不保存用户输入值。
- MCP tool/resource/prompt 输出必须先本地脱敏，再按默认 byte/line 限额截断，并在 `ToolResultMeta` 中保留 truncation 与 MCP server/tool/trust/operation metadata。已经通过 `resources/read` 取得的 bounded text 只有在调用侧显式交给 runtime MCP resource context adapter 后才会成为 `McpResource` Context V0 candidate；adapter 会再次执行 MIME filter、size cap、egress decision 和 packer 校验，不能绕过 permission / egress 直接改写 request。

### 11.4 信任与数据出境模型

远程 MCP server 不只是“再多一种工具来源”，它本质上是数据出境和 prompt injection 风险边界。

当前配置层已经给每个 server 建立独立 trust policy：

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
    pub pinned: Option<McpServerPinnedIdentity>,
}

pub struct McpServerPinnedIdentity {
    pub command_fingerprint: String,
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
}
```

当前默认策略：

- `Official`：可降低 friction，但仍保留敏感调用审批能力
- `SelfHosted`：默认 `approval_default = Ask`、`egress_logging = true`、`allow_secrets = false`
- `ThirdParty`：建议逐次审批、记录出境数据、默认不透传高敏凭据

当前 `approval_default` 已参与逐调用 permission decision；`egress_logging` 已写入安全出境摘要；`allow_secrets = false` 已阻断 MCP tool/resource/prompt args、`roots/list` payload 和 elicitation response 中的已解析 secret。`pin_version = true` 会校验 `trust.pinned` 中的 command fingerprint、protocol version、server name 和 server version；缺少 pinned identity 时会明确失败并输出 observed pin 供用户写入配置。resources/prompts 协议入口复用同一 secret egress gate，且不会自动注入 system prompt；MCP resource context 进入 Context V0 时仍必须携带 egress decision，否则只写 `ExcludedEgressDenied` provenance，不渲染 snippet。

### 11.5 协议版本与能力协商

MCP 规范本身是按日期版本演进的，`sigil` 需要把协议版本协商做成显式状态，而不是隐式假定“所有 server 都一样”。

建议记录：

- server 宣告的协议版本
- server capabilities
- client capabilities
- 当前启用的特性子集

### 11.6 启动策略

当前已支持：

- MCP server 默认 `required = true`、`startup = "eager"`；严格 registry 构建保持“配置即必须可用”的行为，TUI worker 则先启动内置工具/code-intel 基础 registry，并把 eager MCP 放到后台激活
- `required = false` 的 eager server 启动或 `tools/list` 失败时记录 warning 并跳过，不阻断其它 server
- `startup = "lazy"` 的 server 在普通 registry 构建时不启动、不注册工具；显式 activation API 会启动 lazy server、执行 `tools/list`，成功后把真实工具加入 registry，失败按 required / optional 策略处理
- TUI `/config` 的 MCP section 提供 `activate` action；worker 空闲时可对已保存的 lazy server 执行 activation，并把真实工具加入当前 agent registry，运行中 activation 会被拒绝；模型也可通过 `mcp_activate_server` 工具按需启动指定 lazy server，成功后下一轮 request 会看到真实 MCP tools；eager MCP 启动失败或超时时只更新对应 server 的 `failed` lifecycle，不阻断普通 chat、`/plan` 或内置工具；lifecycle summary 会展示 `deferred`、`activating`、`ready` 或 `failed` 运行态
- lazy server 在 activation 成功前不向模型暴露 provider-visible 工具，避免不可调用伪工具污染 tool list

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

- `ToolAccess::Read` 默认 allow
- `Write / Execute / Network` 继承 `permission.default_mode`
- `bash` 静态是 `Shell / Execute`，但简单只读 allowlist 命令可通过动态 `permission_access` 走 `Read`；重定向、变量展开、管道、subshell、glob、未知命令、测试/包管理/写操作仍按 `Execute`
- headless run 遇到最终 `ask` 返回结构化 `approval_required` tool error，不静默执行
- interactive run 遇到 `ask` 弹审批

### 12.2 Sandbox

Sandbox 是执行层强制约束，不是策略层判断。

第一版最安全、也最现实的落地点是：

- 把文件写工具限制在 workspace root 内
- 在放行前统一解析 symlink 和 `..`

shell sandboxing 更难，建议放到 phase 3 或 phase 4，因为跨平台进程隔离本来就是整套系统里最难啃的部分之一。

## 13. 运行模式设计

为了同时保住“缓存极致利用”与“未来可兼容更多后端”，`sigil` 不应该停留在粗粒度双模式，而要把缓存纪律做成 provider-specific 的正式策略。

### 13.1 Cache Discipline Profiles

当前实现没有独立的 `CacheDiscipline` public enum，也不把缓存策略作为 `CompletionRequest` 字段传递。缓存纪律由这些实现面共同保证：

- `Session` 的 immutable prefix materialization
- append-only `SessionLogEntry`
- `ControlEntry::PrefixSnapshotCaptured`
- provider continuation / response handle control state
- `CompletionRequest::deterministic_materialization`
- provider capabilities 中的 cache token 报告能力

后续如果需要同时支持多 provider 缓存策略，profile 应作为 config/runtime 层的策略名，而不是塞进 provider-agnostic request。建议保留这些 profile 语义：

它们的语义分别是：

- `DeepSeekExactPrefix`：以字节稳定前缀和命中 token 指标为核心
- `AnthropicPromptCaching`：以显式 cache-control 边界和 TTL 模型为核心
- `OpaqueProviderCache`：provider 有缓存，但机制不透明，只做保守适配
- `NoCacheDiscipline`：不依赖 provider cache，只保留 append-only 和审计约束

### 13.2 DeepSeek 极致缓存模式

这个模式是给真正追求 prefix-cache 命中率的场景准备的，原则上应该作为 `sigil` 的旗舰模式。

在这个模式下，必须强制以下规则：

- session 使用 `Immutable Prefix + Append-Only Log + Volatile Scratch`
- prefix 默认只在 boot 时生成一次
- 禁止把动态状态注入 system 区域
- 禁止在未 compaction 时重写旧历史
- planner plan 必须落到 durable control plane；executor step context 必须 transient，subagent step 必须使用 child session
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

- agent runtime、tool registry、session lifecycle 不变
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
- `cache_mode=auto`：根据 provider capabilities 自动映射到合适的 cache profile
- 显式指定 `deepseek_exact_prefix` 时，如果 provider 不满足条件则直接报错

同时建议 provider metadata 暴露这些能力位：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningStreamSupport {
    Unsupported,
    Passthrough,
    Native,
}

pub struct ProviderCapabilities {
    pub exact_prefix_cache: bool,
    pub reports_cache_tokens: bool,
    pub reasoning_stream: ReasoningStreamSupport,
    pub supports_reasoning_effort: bool,
    pub supports_tool_stream: bool,
    pub supports_background_tasks: bool,
    pub supports_response_handles: bool,
    pub supports_reasoning_artifacts: bool,
    pub supports_structured_output: bool,
    pub supports_assistant_prefix_seed: bool,
    pub supports_schema_constrained_tools: bool,
    pub supports_infill_completion: bool,
    pub supports_system_fingerprint: bool,
    pub tool_name_max_chars: usize,
}
```

这样模式判定就不是拍脑袋，而是可程序化决策。

同时要明确一条边界：

- `ProviderCapabilities` 只承载跨 provider 可复用的通用能力位
- `reasoning_stream = Native` 表示 provider 原生承诺推理流；`Passthrough` 只表示兼容层可以透传并展示服务端返回的 reasoning delta，不承诺该能力是 OpenAI-compatible 标准行为
- `supports_reasoning_effort` 只表示 provider 接受通用 `ReasoningEffort` 请求参数；兼容层即使能透传 reasoning stream，也不因此默认声明支持 reasoning effort
- `tool_name_max_chars` 用于约束 provider-visible 工具名，例如 MCP 工具名 `mcp__<server>__<tool>` 的清洗、截断和 hash 后缀
- 像 `reasoning_content replay`、`thinking mode 忽略采样参数`、`beta 端点切换` 这种厂商特有规则，应留在 provider profile 或 provider-specific feature/quirk 层

### 13.6 为什么要做 provider-specific discipline 而不是一个兼容模式

如果只有一个模糊兼容模式，项目很容易发生这类架构漂移：

- 为了兼容更多 provider，不断放松 prefix 稳定性
- 为了 UI 方便，向 prompt 头部塞动态状态
- 为了省事，把 provider 切换做成同 session 内热切换

这种漂移短期看起来更“通用”，长期会直接毁掉 `sigil` 最有辨识度的能力。所以 provider-specific discipline 不是为了增加复杂度，而是为了保护旗舰模式不被兼容性需求慢慢侵蚀。

## 14. 推荐 Rust 技术栈

基础依赖建议如下：

- `tokio`：async runtime
- `futures` / `tokio-stream`：stream 组合
- `serde`、`serde_json`、`toml`：配置和协议序列化
- `reqwest`：provider HTTP client / runtime provider status client；MCP 当前走 stdio client
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

- `sigil` 无子命令启动 TUI；TUI 是第一用户表面，也是后续能力设计的基准面
- `sigil run`、`sigil doctor` 和隐藏 provider 调试命令保留为自动化入口和调试通道，不承载最终产品心智
- `strict tools`、`prefix completion`、`FIM` 这类 provider 专项能力，默认应被 TUI 内部流程吸收，而不是直接变成普通用户必须理解的顶层命令
- 如果某个能力只能靠新增命令解释自己，应该先反问：它是否其实应该是 TUI 内部动作、审批卡片或编辑模式

### Phase 0：脚手架

交付物：

- workspace 和 crate skeleton
- 共享 config loader
- event types
- logging setup
- session serialization format 选择
- `sigil-tui` crate skeleton 与最小 app state

退出条件：

- `cargo check`、`cargo test`、`cargo fmt`、`cargo clippy` 全部打通

### Phase 1：最小内核 + TUI 骨架

交付物：

- `sigil-provider-deepseek`（复用 OpenAI-compatible 主链路）
- tool trait 和 registry
- built-in tools：`read_file`、`write_file`、`edit_file`、`ls`、`glob`、`grep`、`bash`
- 单模型 agent loop
- `sigil-tui` 最小交互壳：消息区、输入区、状态区、事件流渲染
- 薄 `CLI run` 调试入口
- stdio MCP support

退出条件：

- 能从 TUI 发起一轮真实对话并看到流式输出
- 能在 TUI 中完成一个端到端 coding task
- MCP tool roundtrip 正常
- 工具失败时能结构化回传，不把进程直接打死

### Phase 2：交互控制层

交付物：

- 带审批与取消能力的 runner / approval bridge
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

第一代 `sigil-tui` 不需要一开始就做得像 IDE，但必须先把用户真正需要的几个面做好：

- 主消息历史：优先直接写入 terminal 原生 scrollback，避免把长对话性能和滚动体验绑死在内部 widget 上
- 底部 live strip：只保留当前流式尾部、composer 与紧凑状态，不要求用户在 chat 区和 composer 之间切焦点
- 底部输入区：支持多行输入、发送、取消、清空
- 右侧信息区 + composer 下状态行：展示写权限、subagent 状态、cache 命中、上下文压力、花费与余额
- 模态或侧栏审批区：展示工具预览、写操作 diff、允许/拒绝动作
- 会话控制区：新会话、恢复会话、切换 workspace、查看错误详情
- setup 模式：当没有可用配置时，TUI 内部直接承载首启配置流，而不是把用户赶回命令行手写配置
- provider 视图：不仅展示当前 provider-visible context，还应承载 compaction preview 这类“提交前先解释后果”的上下文操作

这意味着 `kernel` 事件流在 phase 1 就要按 TUI 消费习惯设计，而不是先按“stdout 打印一堆日志”来塑形。

当前实现还需要保持代码结构服务这个信息架构：`AppState` 作为 façade 收敛 bootstrap、顶层 key routing 和跨状态编排；运行状态、composer、approval 和 session browser 字段归入 `crates/sigil-tui/src/app/state.rs`；输入焦点、slash selector、modal、setup/config、session/resume、timeline/scrollback、tool card interaction/focus、approval、worker bridge、command dispatch 分别维护在 `crates/sigil-tui/src/app/*`；状态流测试维护在 `crates/sigil-tui/src/app/tests/*_tests.rs`，共享 fixture 只放 `app/tests/common.rs`。setup/config、commands、view model 等 TUI 普通模块的测试维护在 `crates/sigil-tui/src/tests/*_tests.rs`；provider config/status/context-window 这类入口共享 helper 的测试维护在 `crates/sigil-runtime/src/tests/*_tests.rs`；worker runner 通过 `runner.rs` façade 暴露协议和启动入口，worker protocol、spawn 装配、event/approval bridge、session/compaction flow 与 runner 状态机测试维护在 `crates/sigil-tui/src/runner/*`，worker loop 的 scheduler、active run、queue、MCP/provider refresh、agent/task runtime 和 terminal refresh 维护在 `runner/worker_loop/*`；renderer 通过 ViewModel 或 render options 读取 UI 数据；`ui.rs` 只作为 `ui/*` 模块入口和必要 re-export，顶层 shell layout、theme/geometry/text 底座、timeline、tool card、markdown、approval、setup/config、modal 等渲染块分别维护在对应 `ui/*` 模块，renderer 测试维护在 `ui/tests/*_tests.rs`。用户交互面优先使用 TUI 焦点和快捷键：tool card 选择/展开走 `Ctrl-G`、`Alt-J/K`、`Ctrl-O` 与 `Esc`，不依赖 hidden slash command；新增快捷键和命令通过 `commands.rs` metadata 同步 info rail、keyboard help 和 README。Markdown 展示由 `ui/markdown.rs` 和 `MarkdownRenderOptions` 统一约束，assistant timeline、tool preview、approval modal 不各自维护解析规则。

主题切换作为 TUI appearance 能力落在 `crates/sigil-tui/src/ui/theme/`，而不是拆成独立 crate。`sigil-kernel` 只承载可序列化的 `AppearanceConfig`、`ThemeId` 和 `[appearance.colors]` 原始字符串；`sigil-tui` 将其解析为 `ThemePalette`，再由 renderer 消费语义 token。内置主题包括 `sigil_dark`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`nord` 和 `high_contrast_dark`。颜色 override 只允许稳定语义 token 和 `#RRGGBB`，用于 TUI 外观，不进入 session/control state、approval 审计、tool payload 或 provider-visible context。`/config` 里的 Appearance draft 会优先供 renderer 解析，让用户在保存前即时预览完整 config palette，包括背景、边框、标题 chip、正文、弱化文字、选中行、状态和提示 token；保存后运行时 config snapshot 更新并重建 timeline render cache，避免旧消息缓存保留旧主题色。

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

## 17. 已锁定的关键决策

当前实现已经锁定并落地这些工程决策：

1. 项目配置文件名为 `sigil.toml`
2. 第一层用户壳由 `sigil` 默认启动 TUI；子命令只保留 debug / automation，不做命令产品化
3. kernel 是 event-driven、agent-runtime-centered
4. provider crate 保持 provider-specific 协议细节内聚，kernel 只承载中立契约
5. DeepSeek、OpenAI-compatible、Anthropic、Gemini 共用 runtime 装配与 capability view
6. MCP `stdio` 进入 runtime/TUI 生命周期，server lifecycle 和 trust policy 保持可配置
7. planner / executor / subagent 在 base loop 之上以可审计的 task state 继续演进

## 18. 立即下一步

当前阶段最正确的下一步不是继续扩张命令表面，也不是把 provider 专项能力做成更多 CLI 入口，而是：

1. 补齐 `/doctor` 与 `/config` 的 provider capability 明细，确保用户能核验具体能力差异
2. 扩充 Anthropic/Gemini provider 的真实协议 fixture，覆盖 tool result、thought signature、finish reason 和 safety 边界
3. 继续收紧 provider canonical naming、model routing、auth resolution 在 TUI/runtime/doctor 之间的一致性
4. 为 provider-specific continuation state 保持 durable、append-only 的恢复测试
5. 让 provider setup assistant 进入 TUI，而不是把配置体验继续摊到 README 或隐藏命令里
6. 在不破坏 TUI-first 心智的前提下，再评估 packaging 和分发包装

这样做能让 `sigil` 一开始就站在两件最重要的东西上：一个可复用、可扩展、契约稳定的 agent 内核，以及一个真实面向用户的终端交互产品，而不是一个子命令越来越多的命令集合。

## 19. DeepSeek 专项优化设计

结合 DeepSeek 官方 API 文档，`sigil` 如果要把 DeepSeek 当作旗舰后端，不应该只停留在“兼容 OpenAI SDK 调用”的层面，而应该显式做一套 `DeepSeekProviderProfile`。

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

所以 `sigil` 直接以 `max` 作为编码 agent 的默认推理强度更符合官方行为，而不是再从 `medium` 之类伪档位兜一层兼容

这里也建议把 provider-specific 奇异行为统一收进 `quirks`，而不是散落在：

- `ProviderCapabilities`
- agent runtime 特判
- request builder 的隐式 if/else

已经落地的 `sigil-provider-anthropic`、`sigil-provider-gemini` 和 `sigil-provider-openai-compat` 也沿用同样模式：

- 通用能力进 `ProviderCapabilities`
- 厂商怪异行为进 provider-specific quirk profile

### 19.3 Thinking Mode 参数纪律

DeepSeek 官方明确说明：思考模式下，`temperature`、`top_p`、`presence_penalty`、`frequency_penalty` 不生效。

这意味着 `sigil` 在 DeepSeek thinking mode 下不应只是“把这些参数传上去然后假装支持”，而应该：

1. provider adapter 主动剔除这些参数
2. telemetry 或 debug log 明确记录“参数已忽略”
3. 前端在 DeepSeek thinking mode 下默认不暴露这些调节项

这样可以避免用户以为自己改了采样，实际请求行为却没有变化。

### 19.4 `reasoning_content` 回传策略

这是 DeepSeek 设计里最关键、也最容易踩坑的点之一。

官方规则是：

- 如果两条 `user` 消息之间没有发生 tool call，则中间 `assistant` 的 `reasoning_content` 在后续轮次中会被忽略
- 如果发生了 tool call，则该轮的 `reasoning_content` 在后续所有用户交互轮次中都必须完整回传，否则 API 会返回 400

因此 `sigil` 的 session log 不能粗暴地把所有 `reasoning_content` 一视同仁，而且相关 replay 状态不能只放在 provider 进程内存里。

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

这意味着 `sigil` 应针对 DeepSeek 做额外的 prompt shaping：

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

这对 `sigil` 非常有价值，因为它能显著降低 tool arguments 结构漂移。

建议策略：

- 默认尝试 `strict tool mode`
- 若 schema 超出 DeepSeek strict 子集，则自动回退到普通 tool mode
- 回退动作必须可观测，并写入 debug / event log

当前实现由 `sigil-provider-deepseek::tools::prepare_tools` 在 provider request 组装阶段完成 schema normalize 与 strict fallback；不要把 DeepSeek strict schema 分类上移到通用 tool registry。

当前实现还会为 `StrictToolsMode::Auto` 的整轮 fallback 产出 `ToolSchemaDiagnostic`，provider 通过 tracing debug 记录；`StrictToolsMode::Always` 则把带 tool name 和 schema path 的错误直接作为 request materialization error 返回。schema normalize 支持 nested object、array、enum、anyOf，optional 字段用 `anyOf` 包含 `null`，object 默认补 `additionalProperties=false`。

这样 provider 在组装请求时就能知道：

- 哪些工具可以走严格模式
- 哪些工具只能走普通模式

### 19.7 DeepSeek JSON Output 策略

DeepSeek 官方支持 `response_format = { "type": "json_object" }`。

对于 `sigil`，这不应该只作为“用户工具箱里的一个可选功能”，而应直接用于几个内核子流程：

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

这样 planner / summarizer / memory reducer 可以在 DeepSeek 上优先走 JSON object，再由 `sigil` 自己稳定化渲染为 cache-friendly 文本。

### 19.8 DeepSeek Prefix Completion 策略

DeepSeek 提供对话前缀续写（Beta）：

- 最后一条消息必须是 `assistant`
- 需要设置 `prefix = true`
- 需要使用 `https://api.deepseek.com/beta`

这项能力非常适合 `sigil` 做“输出形状控制”，尤其是在这些场景：

- 强制代码块起手，例如 ```` ```rust\n ```` 或 ```` ```diff\n ````
- 强制补丁模板、提交信息模板、JSON 前缀
- 减少模型在目标格式前面加解释性废话

建议：

- 不要把它当成通用对话默认路径
- 只在“输出格式非常强约束”的子流程启用
- 与 `stop` 配合使用，缩短无意义尾部

### 19.9 DeepSeek FIM 策略

DeepSeek 提供 FIM Completion（Beta），但官方说明它只支持非思考模式。

这意味着 `sigil` 可以把 FIM 设计成主 agent 之外的一条“局部补全旁路”：

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

结合 DeepSeek 当前模型能力与价格，建议 `sigil` 的默认路由偏向：

- `deepseek-v4-flash`：默认执行器
- `deepseek-v4-pro`：高价值规划、复杂审查、困难收敛回合

原因：

- `flash` 已支持 thinking、tool calls、json output、prefix completion，且上下文 1M、最大输出 384K
- `pro` 仍更贵，应该有选择地使用，而不是默认全程挂上

建议把 planner / reviewer / compactor / summarizer 的模型选择都显式化，而不是只留一个笼统的 `default_model`。

### 19.11 `system_fingerprint` 遥测

DeepSeek 在响应和流式 chunk 中返回 `system_fingerprint`。

建议 `sigil` 把它纳入 telemetry：

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

因此 `sigil` 不应把 beta 能力做成“请求前临时拼接 URL 的字符串开关”，而应在 provider 初始化时建立清晰的 transport 分流：

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

这对 `sigil` 很关键，因为如果多租户或多工作区场景下 `user_id` 设计不稳，会直接伤害 prefix-cache 命中率，甚至带来跨用户隔离问题。

建议：

1. kernel 只暴露通用的 `traffic_partition_key`
2. DeepSeek adapter 将其稳定映射为 `user_id`
3. 默认策略使用“稳定的终端用户级键”，不要每次请求生成随机值
4. 同一真实用户在同一工作区内应尽量复用同一个键，以保留缓存收益
5. 不要直接上传原始邮箱、用户名等 PII，应先做稳定哈希或内部映射

当前 runtime 默认从 canonical workspace root 派生 `workspace-{sha256}` 形式的 `traffic_partition_key`，避免固定的 `local-user` 跨工作区复用，也避免把原始本地路径直接上传给 provider。DeepSeek adapter 仍只消费通用的 `traffic_partition_key`，并按 `user_id_strategy` 映射为 `user_id`。

建议配置增加：

```toml
[providers.deepseek.routing]
user_id_strategy = "stable_per_end_user" # stable_per_end_user | stable_per_workspace | disabled
```

如果未来 `sigil` 支持团队共享代理，这一条会直接决定缓存收益和隔离边界是否同时成立。

### 19.15 SSE / Keep-Alive 解析纪律

DeepSeek 官方文档明确说明：

- 非流式请求期间会返回空白行作为 keep-alive
- 流式请求期间会返回 SSE comment 作为 keep-alive
- 若请求在 10 分钟内仍未开始处理，连接会被关闭

这意味着 `sigil` 的 HTTP / SSE 解析器必须足够宽容，不能把这些行为误判为协议错误。

建议实现约束：

1. SSE parser 显式忽略 comment frame 与空白 keep-alive
2. “一段时间没 token”不应直接判定 provider 死亡，而要结合连接状态与 keep-alive 判断
3. agent runtime / runner 需要把“连接存活但尚未出 token”和“真正超时失败”区分成不同事件
4. 超过 10 分钟仍未开始处理的请求，应归类为 provider-side start timeout，而不是普通 read timeout

如果不做这层纪律，后面在长推理、长工具回合、网络波动时很容易把可恢复事件当作失败处理。

### 19.16 错误分类与重试策略

DeepSeek 官方错误码与通用 OpenAI-compatible 语义接近，但 `sigil` 不应只做“429 重试、其他全报错”这种过粗处理。

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

这意味着 `sigil` 不应把 429 只当作“临时打满了，睡一下再试”的网络噪声，而要把 DeepSeek 的并发模型内建进 scheduler。

建议：

1. provider 维护独立的 `flash` / `pro` 并发信号量
2. 若启用了稳定 `user_id`，可选增加“每个 partition key 的局部信号量”
3. planner / reviewer / compactor 等后台子任务不能无限挤占前台主会话并发
4. 遇到 429 时优先做本地背压，而不是所有请求同时指数退避再一起重冲

后续如果落并发预算，应先作为 `sigil-provider-deepseek` 内部 scheduler 策略，不提前公开成 kernel public type。

对于 `sigil` 这种 agent 内核，好的体验不是“理论峰值最高”，而是“在 DeepSeek 并发纪律下仍稳定收敛，不制造 429 风暴”。

### 19.19 模型发现与别名治理

虽然 `deepseek-v4-flash` / `deepseek-v4-pro` 已经是当前 canonical model id，但 `sigil` 仍建议在 provider 启动阶段做一次轻量模型发现与校验。

建议行为：

- 初始化时可选调用模型列表接口，验证配置模型是否真实可用
- 若用户配置了 `deepseek-chat` 或 `deepseek-reasoner`，启动时立刻归一化并告警
- 将“模型名归一化前后结果”写入诊断日志，方便后续排查历史配置

这样做的价值在于：

- 避免运行时才发现模型名失效
- 让 alias 弃用迁移变成启动期显式事件，而不是线上隐性行为变化
- 后续若 DeepSeek 再扩新模型，provider 能更平滑接入

## 20. `sigil-provider-deepseek` crate 骨架设计

这一节的目标不是直接写实现代码，而是把 crate 边界先收紧，回答一个关键问题：

`sigil` 如何在“DeepSeek-first”落地的同时，不把 kernel 做成 DeepSeek 专属？

答案是：把通用 session / agent / tool / event / permission 契约全部留在 `sigil-kernel`，而把 DeepSeek 的协议映射、端点分流、thinking 纪律、reasoning replay、strict tools 与 beta 能力下沉到独立 provider crate。

### 20.1 crate 定位

`sigil-provider-deepseek` 的职责应当是：

- 实现 `sigil-kernel::provider::Provider`
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
- 不定义通用 agent 事件协议

### 20.2 当前目录结构

```text
crates/
  sigil-provider-deepseek/
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
      pricing.rs
      reasoning.rs
      tools.rs
      prefix.rs
      fim.rs
      errors.rs
      tests/
        config_tests.rs
        pricing_tests.rs
        provider_tests.rs
        request_tests.rs
        stream_test_support.rs
        stream_tests.rs
        tools_tests.rs
```

当前每个模块责任如下：

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
- `pricing.rs`：上下文窗口、token 用量和成本估算相关策略
- `reasoning.rs`：thinking mode 与 `reasoning_content` replay 策略
- `tools.rs`：strict tools 可用性判断与 schema 分类
- `prefix.rs`：prefix completion 组装
- `fim.rs`：FIM sidecar 相关逻辑
- `errors.rs`：provider 内部错误枚举与标准化
- `tests/*_tests.rs`：按模块分组的 request、stream、provider、pricing、tools 和 config 测试

### 20.3 `kernel` 与 `provider-deepseek` 的边界

应当明确哪些类型属于 `kernel`，哪些只能属于 `provider-deepseek`。

保留在 `sigil-kernel`：

- `Provider` trait
- `ProviderCapabilities`
- `CompletionRequest`
- `ProviderChunk`
- `ReasoningEffort`
- `ToolSpec`
- `UsageStats`
- `ProviderContinuationState`
- provider 无关的错误分类入口

只放在 `sigil-provider-deepseek`：

- `DeepSeekProviderConfig`
- `DeepSeekProviderProfile`
- `DeepSeekProviderQuirkProfile`
- `DeepSeekEndpointClass`
- `DeepSeekReasoningReplayPayload`
- `StrictToolsMode`
- `DeepSeekRequestBody`
- `DeepSeekStreamEvent`
- `DeepSeekErrorBody`

边界判断原则很简单：

- 其他 provider 也会复用的概念，留在 `kernel`
- 只有 DeepSeek 文档才定义的概念，留在 `provider-deepseek`

### 20.4 `lib.rs` 对外暴露面

当前 `lib.rs` 仍然只把内部模块作为私有实现细节，公开面集中在 provider 构造、配置、专项 request 入口和少量诊断 helper：

```rust
mod capabilities;
mod client;
mod config;
mod endpoint;
mod errors;
mod fim;
mod mapper;
mod models;
mod prefix;
mod pricing;
mod provider;
mod reasoning;
mod request;
mod response;
mod retry;
mod stream;
mod tools;

pub use config::{
    DeepSeekProviderConfig, DeepSeekProviderProfile, DeepSeekProviderQuirkProfile, StrictToolsMode,
};
pub use fim::DeepSeekFimCompletionRequest;
pub use prefix::DeepSeekPrefixCompletionRequest;
pub use pricing::context_window_tokens as deepseek_context_window_tokens;
pub use provider::DeepSeekProvider;
```

`DeepSeekProvider` 提供稳定构造器：

```rust
impl DeepSeekProvider {
    pub fn new(config: DeepSeekProviderConfig) -> anyhow::Result<Self>;
}
```

不要把 request/response DTO、stream decoder、mapper、retry 或 endpoint selector 直接公开导出。对外应该只让上层知道：

- 这个 crate 可以被构造
- 它实现了 `Provider`
- 它需要什么配置
- 它额外提供 prefix completion、FIM 和 context window 查询这些 DeepSeek 专项入口

### 20.5 provider 内部主对象

当前核心对象形态是：

```rust
pub struct DeepSeekProvider {
    profile: DeepSeekProviderProfile,
    config: DeepSeekProviderConfig,
    capabilities: ProviderCapabilities,
    client: reqwest::Client,
}
```

这样做的好处是：

- 端点分流通过 profile 和 `DeepSeekEndpointClass` 显式完成，不是请求时现拼字符串
- capabilities 可在启动期固定下来
- 共享 HTTP client 统一处理 transport 配置，retry/error 分类留在 provider 内部模块

### 20.6 request 组装链路

一次请求在 provider 内部走这条链：

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

当前 `reasoning.rs` 的最小持久化载体是：

```rust
pub struct DeepSeekReasoningReplayPayload {
    pub reasoning_content: String,
}
```

这里的目标不是把 session state 搬到 provider 里，而是在单次 request materialization 时拥有足够的 DeepSeek 规则判断能力。

其中要特别注意：

- `DeepSeekReasoningReplayPayload` 是 provider-specific 的序列化结构
- 真正持久化到会话里的容器是 kernel 的 `ProviderContinuationState`
- replay state 使用 `state_kind = "deepseek.reasoning_replay"` 标识，opaque blob 中保存 `reasoning_content`
- provider 重启、session resume、context compaction 后，仍应能从该 opaque state 恢复 replay 语义

### 20.8 tools 子系统最小骨架

`tools.rs` 的重点不是执行工具，而是做“DeepSeek 能不能严格接这个 schema”的预判。

当前入口是：

```rust
pub struct PreparedTools {
    pub payload: Option<Vec<serde_json::Value>>,
    pub strict_mode_enabled: bool,
    pub diagnostics: Vec<ToolSchemaDiagnostic>,
}

pub fn prepare_tools(
    specs: &[ToolSpec],
    mode: StrictToolsMode,
) -> anyhow::Result<PreparedTools>;
```

这样 kernel 与 TUI runner 仍然只看到统一的工具接口，但 provider 能在请求组装前决定：

- 全量 strict
- strict schema 失败后整轮退回普通 tool mode
- `StrictToolsMode::Always` 下把不兼容作为 provider request materialization error 暴露出来

### 20.9 当前测试骨架

这个 crate 已经按模块拆出测试文件：

1. `config_tests.rs`
2. `pricing_tests.rs`
3. `provider_tests.rs`
4. `request_tests.rs`
5. `stream_test_support.rs`
6. `stream_tests.rs`
7. `tools_tests.rs`

`stream_test_support.rs` 是 stream 测试专用 helper；后续补 fixture 时仍应覆盖：

- 普通文本流
- reasoning + text 混合流
- tool call 增量参数流
- keep-alive / comment frame
- strict schema 不兼容错误
- `reasoning_content` 缺失导致的 400

### 20.10 对通用 provider 的保护

为了避免 `sigil-provider-deepseek`、`sigil-provider-anthropic`、`sigil-provider-gemini` 或兼容层反向污染 `kernel`，保持两条红线：

1. `kernel` 中不出现 `reasoning_content`、`beta_base_url`、`user_id`、`tool_use`、`systemInstruction`、`functionDeclarations` 这类 provider 专有字段名
2. provider-specific repair 逻辑只存在于对应 crate，不写进通用 agent loop

只要守住这两条，新增或增强 provider 都是在扩展同一个通用内核，而不是不断为某家 provider 特判打洞。

### 20.11 后续实现顺序

当前 provider 主链路已经落地，后续增强建议按风险顺序推进：

1. 补齐 provider request/stream fixture，覆盖 reasoning、tool args delta、keep-alive 和错误体
2. 强化 `pricing.rs` 与 usage/cache token 的一致性断言
3. 把 prefix completion 和 FIM 的专项入口继续留在 provider crate，不上移到 kernel
4. 如果要做并发预算，先作为 provider 内部 scheduler 设计，不提前公开公共并发预算类型
5. 如果要做 JSON mode，优先在 `request.rs` 里作为 DeepSeek request shaping，而不是新建公共 kernel 能力

这个顺序的好处是，先把主链路打通，再加 DeepSeek 专项增强，不会一开始就把 Beta 能力和 repair 分支缠成一团。
