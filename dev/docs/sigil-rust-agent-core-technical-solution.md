# Sigil Rust Agent 核心技术方案（Implementation Snapshot v1）

## 1. 背景

`sigil` 是一个基于 Rust 的 AI coding agent：内核复用、前端可插拔，
Desktop 与 TUI 作为并列的一等产品表面共享同一套任务、审批、恢复和验证语义。

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

1. 用 Rust 构建一个可被 Desktop、TUI、CLI 和 HTTP 共同复用的 agent kernel，
   其中 Desktop 与 TUI 是并列的一等用户表面。
2. 保持 provider、tool、plugin 都由配置和注册机制驱动，而不是写死在核心里。
3. 通过独立 provider crate 支持 DeepSeek、OpenAI-compatible Chat Completions、OpenAI Responses、Anthropic 和 Gemini，同时保持内核 provider-neutral。
4. 内置工具和 MCP 工具通过统一的工具注册表暴露给 agent。
5. 保持 cache-stable 的 session 设计，把 prefix-cache 命中率视为顶层架构约束，而不是附带优化。
6. 提供适合自动化 coding 场景的 permission layer 和 workspace confinement。
7. 给 planner / executor 双模型协作预留清晰的架构边界，但不强行塞进 MVP。

## 3. 初始阶段非目标与当前演进

以下条目记录第一版的范围控制，不表示当前产品仍然缺少对应能力。Desktop 已在后续阶段作为
与 TUI 并列的一等产品表面落地；其他条目仍按当前实现和对应 RFC 判断：

- Desktop shell 不与第一版 kernel 同期交付；当前已由 `sigil-desktop` 与 `apps/desktop` 落地
- 第一阶段不做 codegraph 或更重的代码智能子系统
- 第一阶段核心 runtime 不依赖 npm、Homebrew 或自更新；首发分发包装层只作为 release 工程存在，复用 `sigil` binary、GitHub release archives、Homebrew tap formula 和 npm wrapper，不把命令包装层变成独立产品表面
- 不把 Anthropic/Gemini/DeepSeek/OpenAI-compatible 的私有 request 或 stream 语义上移进 kernel
- 在单会话内核跑稳之前，不做复杂的多 agent 编排
- 第一阶段不继续扩张用户可见命令面，不把 provider 专项能力直接暴露成产品主心智

## 4. 设计原则

1. 契约优先：先定义稳定 trait 和事件契约，再铺前端。
2. 内核优先：共享执行逻辑留在 kernel/runtime；Desktop 与 TUI 作为产品表面、CLI 与 HTTP 作为
   adapter，都不能各写一套执行逻辑。
3. 双一等产品表面：优先定义共享产品语义，再分别把 Desktop 与 TUI 的交互壳做对；
   CLI/HTTP 不反向塑造普通用户心智。
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
    sigil-provider-http/
      src/
        lib.rs
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
    sigil-provider-openai-responses/
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
    sigil-process/
      src/
        lib.rs
        tests/lib_tests.rs
    sigil-updater/
      src/
        apply.rs
        cache.rs
        channel.rs
        github.rs
        install_source.rs
        lib.rs
        tests/
          *_tests.rs
    sigil-desktop/
      src/
        client.rs
        dto.rs
        lib.rs
        launcher.rs
        manager.rs
        protocol.rs
        secret.rs
        tests/
          *_tests.rs
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
          terminal_lifecycle_bridge.rs
          worker_loop/
            active_run.rs
            agent_runtime.rs
            mcp_refresh.rs
            provider_status.rs
            queue_driver.rs
            scheduler.rs
            task_runtime.rs
            terminal_control.rs
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
- `sigil-provider-http`：所有 provider 共用的安全 HTTP client builder。默认保留 rustls 内建信任根；显式设置 `SSL_CERT_FILE` 时追加该 PEM bundle，供企业代理与私有网关使用。它不关闭证书链或主机名校验，也不承载任何 provider 协议、模型或鉴权语义。
- `sigil-provider-deepseek`：首个旗舰 provider，内部拆成 transport、endpoint、request、response、stream、mapper、reasoning、tools、pricing 等模块。DeepSeek 专项能力在这里解释和降级，不反向污染 kernel。
- `sigil-provider-openai-compat`：OpenAI-compatible Chat Completions provider，覆盖通用 streaming text、tool call、usage 和 endpoint/header 配置，不承载 DeepSeek reasoning replay、strict tools、prefix/FIM 或 beta endpoint 语义。
- `sigil-provider-openai-responses`：OpenAI Responses provider，独立处理 Responses 的 `input` / output-item / SSE 协议，并将每轮完整、原样的原生 output items 作为 provider continuation state 绑定到对应 assistant message；它不修改 Chat Completions wire contract，也不把 OpenAI 字段泄漏到 kernel。官方 route 的 cache key 使用 tenant partition 下的 HMAC 稳定分片，logical A0/A2 boundary 不触碰 active A4；`/responses/compact` 返回的 opaque window 只有在同 cursor 的 portable checkpoint 已激活后才能写成加密 native carrier，不能把候选明文写入 JSONL。
- `sigil-provider-anthropic`：Anthropic Messages provider，负责 Anthropic 版本 header、beta header、top-level system、`tool_use` / `tool_result`、incremental tool argument，以及 provider-native `web_search_20250305` server tool / citation / continuation 映射。官方 route 在 A0/A2 使用不超过四个 slot 的 `cache_control`，active A4 不写 breakpoint。公开 beta `compact-2026-01-12` 仅在 official route、精确 version/model 和已有 portable checkpoint 时，以 provider-local paused driver 生成加密 native carrier；route/model/protocol/store/retention/expiry 或 payload validation 失败时机械回退 portable checkpoint。kernel 只看到中立的 message、tool spec、hosted evidence、usage、capability 和受保护 carrier ref，不会看到 Anthropic tool version、server block 或 encrypted carrier。
- `sigil-provider-gemini`：Gemini GenerateContent provider，负责 `systemInstruction`、`functionDeclarations`、`functionCall` / `functionResponse`、block reason，以及 provider-native `google_search` / grounding metadata 映射；Gemini 的 function-response、hosted model eligibility、streaming grounding index 与 retry 细节保留在 provider crate 内。
- `sigil-tools-builtin`：隔离文件、shell、搜索等内置工具实现，统一通过 `Tool` trait、preview、permission subject 和结构化 `ToolResult` 回到 agent loop。大输出路径使用 kernel policy-safe streaming sink 形成 artifact + bounded view；`read_tool_artifact` 只接受 opaque ref 和 byte/line/literal selector，并返回 transient bounded page + body-free receipt。`lib.rs` 只保留兼容 façade；工具注册、workspace path confinement、文件工具、changeset、shell、persistent terminal 和 non-interactive execution backend 分别维护在对应子模块中，backend 内部再按 local / Seatbelt / Bubblewrap / Docker 拆分。
- `sigil-process`：只承载跨 crate 复用的进程树 lifecycle ownership、整树终止和离线 capability probe。Windows 使用 kill-on-close Job Object，Unix 提供独立process-group配置和整组终止primitive；等待、grace policy与receipt仍由调用crate拥有。它不承载shell选择、sandbox、terminal I/O、MCP framing、desktop bootstrap、receipt或TUI状态。
- `sigil-updater`：Desktop、TUI 与 CLI 共享的更新策略边界，拥有 release discovery、SemVer channel isolation、immutable/digest admission、安装源分类、24 小时 global cache 与 standalone binary replacement；它不拥有产品 UI、release publication 或 package-manager execution。GitHub digest 只作为完整性证据，standalone apply 还要求编译期分发 marker、精确 tag/target/asset 和下载后二进制元数据复核；npm、Homebrew、Cargo、source 与 unknown 只返回原安装器命令。
- `sigil-desktop`：桌面 Rust 后端的 library-only boundary。它生成并私有持有 per-launch bearer，以独立process tree启动one `sigil serve` per workspace，bounded解析startup metadata，再用no-proxy/no-redirect的鉴权`/server-info`验证同一DTO；关闭时先drop owner pipe等待graceful drain，超时才整树终止。typed client只反序列化server response，包含server-private path的DTO没有IPC serialization surface。它不依赖kernel、runtime、TUI或HTTP server crate，renderer也不能取得token/child/generic HTTP。
- `apps/desktop`：RFC-0044 的 Tauri 2 + React/TypeScript/Vite companion。Tauri backend通过`sigil-desktop`维持one process per workspace，native recent store私有持有canonical path；renderer只接收workspace id/display/server state、bounded catalog rows与process-local session summary。history pagination/search/filter/new/open/rename/confirmed-delete全部经authenticated typed HTTP client；rename追加bounded lifecycle decision，可用会话的delete复用exact preview/apply并与活动run/verification互斥，无法打开的invalid/oversized/scan-limited来源则只能在重新校验catalog fingerprint并取得maintenance + writer lease后隔离或删除；JSONL 与同名 resource tree 一起进入 quarantine/tombstone，稳定 writer-lock inode 保留，跨进程占用返回 typed busy。session catalog只读取当前 application id、user version 与 projection row schema；同 application id 的旧 rebuildable cache 在 exclusive recovery lease 下整体隔离后从当前 JSONL truth 重建，不做旧 row migration；wrong owner、未知较新 schema 与 corruption 仍 fail closed。会话库 mutation 同时刷新 library page 与 App-owned sidebar catalog。server-private path与durable scope在IPC前丢弃。capability仅允许冻结的desktop业务command，不开放generic shell/process/filesystem/HTTP。wire schema由`sigil-http` OpenAPI导出并在CI检查snapshot和generated TypeScript drift；SSE `ProtocolEvent`/`PublicRunEvent` 也属于该合约，native client以provider-neutral typed DTO消费task/plan/batch/step/integration事件，未知事件只降级且raw payload不进入renderer，renderer不直接持有loopback client或bearer。RFC-0046 进一步把桌面表现层收口到 Material-derived semantic roles 和 Sigil-owned accessible primitives；application-scope `system | light | dark` 由 bounded native store 持久化，不进入 workspace/session/OpenAPI/SQLite/runtime truth，theme/navigation/review 切换不得 remount active conversation。
- `sigil-code-intel`：隔离 LSP client 生命周期、多语言 Tree-sitter request-local fallback、RepoMapLite source map、符号/诊断缓存、warm LSP context snapshot、代码查询 tools，以及带 approval diff preview 的 LSP edit tools（code action / rename）。首批 request-local adapter 覆盖 Rust、Python、JavaScript/JSX、TypeScript/TSX 和 Go，使用编译期固定 grammar、ignore-aware bounded walker/read、deterministic caps 和 same-language unique-reference heuristic；它不建立 persistent repo graph，也不把 heuristic edge 宣传为 resolved call graph。配置结构保留在 kernel 的通用 `CodeIntelligenceConfig` / `LanguageServerConfig` 中，code-intel 可以依赖 kernel 的工具契约和配置类型，但 kernel 不反向依赖 LSP 或 Tree-sitter；动态结果以 bounded Context V1/tool result 进入 provider-visible request，不修改 stable base system prompt。`LanguageServerConfig.trust_required = true` 时，runtime 必须把当前 session 对精确 workspace 的 durable trust projection 传到 code-intel，并在 command resolution 与 process spawn 前 fail-closed；旧调用入口和 fresh headless session 默认 `Unknown`，`trust_required = false` 只显式关闭 LSP 进程启动 gate，不改变写工具权限。外部规划型写入采用 kernel-owned `ToolPreparationDraft -> PreparedToolCall` 一次性 envelope：code-intel 只负责单次 LSP plan、source/version/hash、完整 edit set 与 proposed bytes 的进程内 materialization，kernel 用 exact target subjects 求 permission，并把 args、policy、approval、preview 与 execution 绑定同一 digest；execute 只能按值消费 artifact，不能再次查询 LSP。多文件写入复用 RFC-0002 coordinator，进程内失败采用可审计的补偿回滚，crash 仍按逐文件 reconciliation 处理而不宣称原子事务。
- `sigil-mcp`：隔离 stdio 与 Streamable HTTP MCP client、OAuth 2.1 凭据生命周期和工具适配逻辑，把远端 MCP 工具包装成同一个 kernel tool registry surface。
- `sigil-runtime`：收口跨入口共享的 provider factory、tool registry、run options、Context source provider contract / hard-cap enforcement 和 request resolver，避免 TUI / CLI 各自硬编码装配链。tool surface 保留与 registry 同一个 `CodeIntelligenceService` inner；每次请求先在 35ms 内只读 query-relevant warm LSP cache，有命中时使用 explicit path + LSP rows 并跳过 RepoMap，miss/disabled/timeout 才使用 request-local multilingual RepoMap。它把这些结果转换为带 score breakdown 的 bounded Context V1 items，并把 trusted plugin hook output / caller-supplied MCP resource text 通过同一个 source-provider contract 转成 `ExtensionProvided` / `McpResource` rows；缺失或不可信输入只产生 excluded provenance，不阻塞普通 request。normal、plan、headless、queue 和 compaction preparation 共享该 resolver；已经冻结的 provider request 不在 dispatch 时重算。kernel 只看到 provider-neutral `ContextItem` 和 packer，不知道 runtime 存在。
- `sigil-http`：HTTP/SSE adapter crate。`lib.rs` 只保留兼容 façade；protocol envelope、server config、bearer auth、loopback listener framing、SSE durable/live event surface、DTO、run driver trait、session/run registry 和 OpenAPI schema 分别维护在对应子模块中。listener 只拥有 HTTP framing/auth/registry routing，不依赖 `sigil-tui`，不复制 agent loop。历史session reopen只接受catalog提供的relative ref与expected durable id，并由runtime重新验证lifecycle/JSONL truth；SQLite projection不能授权resume。artifact page route 必须 authenticated、typed、session/source-bound、hash-verified 且 endpoint cap 固定，response/error 均不包含物理路径。
- `sigil`：提供 `sigil` binary。无子命令时直接启动 TUI；`run`、`doctor`、`update`、`serve` 和隐藏 provider 调试命令保留为显式自动化/高级入口，不承担最终产品心智；`update check` 只发现更新，`update apply --yes` 只对已准入 standalone archive 执行替换，包管理器安装仅返回 owner command；`serve` 当前通过共享 runtime application service 启动 loopback-only、bearer-authenticated HTTP/SSE listener，支持 durable replay、live event、approval/cancel 与 graceful drain，不提供 remote bind 或 multi-user daemon 语义。`sigil-desktop`已按workspace监管单独的`serve`进程，通过单行版本化JSON/鉴权`server-info`完成bootstrap，并用stdin owner pipe与process-tree fallback拥有child lifecycle；诊断事实由 `sigil-runtime` 提供，避免 CLI、TUI与desktop各写一套判断。
- `scripts/build-release-archive.sh`：提供本地 release archive 构建与 built binary smoke，并为可独立替换的官方归档写入 `github-release` 分发 marker；`scripts/render-homebrew-formula.sh` 生成 `sigil-ai.rb` tap formula；`scripts/prepare-npm-packages.sh` 从 release archives 生成 scoped npm wrapper 和 platform package tarballs，npm launcher 再覆盖 install-source marker 以保留包管理器 ownership；`scripts/release-doctor.mjs` 绑定 tag、Cargo/Desktop/Tauri/Cargo.lock/changelog、remote main/tag 与 exact-SHA CI；`scripts/release-candidate.mjs` 冻结 tag commit、候选 asset inventory/size/SHA-256；macOS Desktop 使用 append-only 公证账本把 build+submit、单次 status、offline finalize 与 upload 分离，每个 attempt 绑定 tag/commit/Team/profile label/目标架构/不可变 submission SHA-256，Apple 原始响应原子落盘，缺失 ID 只能唯一 history reconciliation 或显式 orphan 后重提；`scripts/upload-desktop-macos-release.sh` 是签名双架构 Desktop 进入 draft 的唯一 maintainer 入口，并复验 finalized ledger，默认拒绝替换不同字节。`.github/workflows/release.yml` 只在 tag push 时构建一次多平台 TUI archive、生成 provenance、准备 npm tarball 和 draft Release；显式 publish 不再重编，而是按 candidate manifest 复用原 tarball，先验证双架构 macOS Desktop DMG、updater archive、checksum 与 signature，冻结 `latest.json`，再公开 immutable Release、通过 npm Trusted Publisher 按 platform-first/root-last 发布、部署 Pages updater endpoint，并由独立 job 使用仅限 `JimmyDaddy/homebrew-sigil` 的 SSH deploy key同步 tap。主 workflow 在 npm 发布后通过 `repository_dispatch` 启动有界等待的公开 npm/GitHub/Desktop/Pages/Homebrew smoke；`release.published` 另行覆盖非 `GITHUB_TOKEN` 触发的人工发布。crates.io package name 决策仍是 release-management 工作。
- `sigil-tui`：并列一等产品表面中的终端实现。`app.rs`、`runner.rs`、`ui.rs` 是 facade；状态流、worker 协议和 renderer 分别下沉到 `app/*`、`runner/*`、`ui/*`；`app/state.rs` 承载 runtime、composer、approval、session browser 以及 timeline presentation、review/checkpoint、agent panel、egress disclosure 等私有领域 bundle，根 `AppState` 只为兼容保留公开 timeline/event/scroll 字段和顶层编排状态；`runner/worker_loop.rs` 只保留 worker façade，私有 `WorkerLoopState` 统一持有 session/run/compaction/refresh/agent 状态，scheduler 通过统一 `WorkerEvent` inbox 阻塞等待 command、typed completion、durable projection 与 supervisor wake，只在存在 MCP/terminal 等真实 deadline 时使用 nearest-deadline timeout；七个 advancement function 与穷尽 public-command 到 domain-typed-command classifier/handler 分别承担确定性 safe-point 推进和路由。session scheduler 的 queue、TaskGuidance、continuation、terminal 与 usage/readiness 热查询读取 kernel active-session 增量 projection，并以 durable frontier/CAS 保持最终写入权威；switch/new-session/local-session fork/checkpoint fork 复用一个 session transition，替换 projection observer generation，并在 foreground 或 detached background run 存在时 fail-closed，同时按目标 session 重建 agent supervisor 与模型可见 agent-tool surface。终端运行时采用 alternate-screen 全屏模型，Ratatui 是当前应用帧的唯一物理输出所有者；启动不读取 cursor position，也不把 transcript 写入 terminal 原生 scrollback。异步 `EventStream` 是输入的唯一读取者，主 transcript、child transcript、composer、status、modal 与 info rail 全部在同一个应用帧和坐标系内渲染。历史浏览只由 `AppState` 的 bounded timeline render store、虚拟 scroll offset 与 logical content anchor 管理；PageUp/Ctrl-Home/滚轮、新输出、height resize、width reflow 和 info-rail 显隐都必须重投影同一锚点，不能维护第二套物理 frontier/seed/rebase 状态。每次 resize 由 fullscreen autoresize 重建 viewport；退出、普通错误和 panic 必须先清理应用帧并离开 alternate screen，再在恢复后的 primary screen 输出 resume hint 或错误。info rail 是可响应收起的普通布局区域，不拥有独立终端写入路径，也不能成为隐藏其他区域重绘错误的稳定性开关。所有 transcript、status、composer 与 info-rail 宽度统一采用 Ratatui terminal-cell 模型，并先清理控制字符和非 emoji 序列中的 default-ignorable 字符；timeline render store 在缓存和命中区计算前就把每行约束到真实 live-panel 宽度，不把 renderer 的二次 wrap 当作布局事实。interactive TUI 独占 stdout/stderr 所指向的终端字节流，进程级 tracing 不能在运行期写入 stderr 绕过 Ratatui；非交互 CLI 仍保留标准 tracing 输出。TUI `/doctor` 复用 runtime 诊断事实；`/update [check|refresh|apply]` 复用 updater policy，网络和替换在独立后台任务执行，启动自动检查只在 release packaged build 且非 CI/source 时调度，并且永不自动 apply；普通模块测试在 `src/tests/*_tests.rs`，状态流测试在 `app/tests/*_tests.rs`，runner 测试在 `runner/tests/*_tests.rs`，renderer 测试在 `ui/tests/*_tests.rs`。

主 transcript 与 child-agent transcript 的历史浏览都保存内容锚点；新输出、文件 reload、
高度变化或宽度 reflow 只能重投影同一锚点。child tail window 滚动时还必须用稳定的 logical
entry identity 跨越 bounded cache 的前缀裁剪，不能把相对尾部偏移误当作用户正在阅读的位置。

Provider connection 配置采用 V2 复合身份。kernel 只定义中立的 `ConnectionId`、
`ModelRef` 与 durable `ResolvedModelRoute`；runtime 拥有
`ProviderConnectionConfig`、V1 -> V2 投影、credential reference、connection inventory、
provider-native catalog 及其 exact-fingerprint cache。`sigil.toml` 只保存连接、endpoint、
协议选项、默认 `ModelRef`、可选的 connection/model context-window 映射和 credential reference，
不保存新输入的密钥。显式单模型窗口优先于 provider-owned exact metadata，再回退全局
`fallback_context_window_tokens`；remote catalog 的附加 metadata 不参与该解析。凭据可来自命名环境
变量、OS credential store，或 owner-only `~/.sigil/credentials.json`；新配置默认使用 file
backend。`auto` 也是严格的 non-interactive file-only 策略，不查询或清理旧 native record；
`keyring` 是唯一允许平台认证 UI 的显式 native-store 策略。旧 native record 不自动迁移或读取。
因此这里
的安全承诺是“凭据与普通配置、session、catalog cache、日志和 support bundle 分离”，而不是
“任何凭据绝不在本机持久化”。TUI 首启与 `/config` 采用 connection-first 流程；启动只投影
secret-free offline readiness，用户主动进入配置流程后才异步验证 stored credential。native
调用在 blocking worker 中 process-global 串行执行，不以无法取消底层系统 prompt 的短 timeout
制造伪失败。TUI 与 Desktop 的首次设置和普通设置都提供可留空的单模型 context-window 入口，
没有模型目录的兼容站点仍可用手动 model ID 完成配置。Desktop 只消费不含 secret 的 exact model option 和 freshness/availability
元数据；application run context 按 connection 投影全部已配置连接的有界已知模型，Desktop 与 TUI
选择值都保持完整 `ModelRef`。设置 saved default 使用共享 typed mutation，不保存 renderer-local
Provider/model override；TUI `/config` 选择 connection/model 并保存时，会同时更新 saved default，
并在当前 idle session 追加同一 route-selection boundary 后重绑 worker，而不是创建新 session。
配置校验、credential rotation 或 atomic publish 失败时，TUI 保留 draft/dirty state，并在 header、
detail 和 footer 投影持久 save-error；可识别字段会获得焦点，后续编辑清除旧错误状态。
session identity 建立 initial exact route，后续只允许在 idle 边界追加完整、
可审计的 route-selection event；endpoint/protocol semantic fingerprint 漂移时 fail closed，
fork/restore 不得静默改写既有事件，切换 connection 或 model 后不得复用边界前 provider-private
continuation/cache material。

这个拆分仍然比“教科书式 Clean Architecture”更少：crate 边界只承载产品级职责，crate 内模块才承载局部复杂度。memory、permission、config、session 继续留在 `sigil-kernel` 内，因为它们共同定义通用执行语义；TUI 的输入、modal、session、approval、timeline、worker bridge 等状态流留在 `sigil-tui` 内，Desktop 的 renderer 与 native shell 状态留在各自 adapter 内，因为它们属于不同产品表面的交互模型。

### 重构后不变量

- `app.rs` 只保留 `AppState` façade、bootstrap、顶层 key routing 和跨状态编排；运行状态、composer、approval、session browser、timeline presentation、review/checkpoint、agent panel 和 egress disclosure 字段归入 `app/state.rs` 的领域 bundle；已有公开 `timeline`、`events`、`timeline_scroll_back`、`activity_scroll_back` 不因内部所有权调整而移动；新增状态流放入 `app/*`，状态流测试放入 `app/tests/*_tests.rs`。
- `runner.rs` 只暴露 worker protocol 和 spawn 入口；worker command/message、spawn 装配、event bridge、approval bridge、session/compaction flow 放入 `runner/*`；worker loop 的 scheduler 只协调 unified inbox、nearest deadline 与 deterministic advancement，私有 state aggregate、七类 advancement、public command 到 domain-typed command 的穷尽 dispatch、统一 session transition、active run、queue、MCP/provider refresh、agent/task runtime 和 terminal refresh 放入 `runner/worker_loop/*`。生产 worker 不使用固定 cadence 的 general poll；snapshot/progress wake 在 producer 侧按 session scope、observer generation 和 family 合并，command/completion 仍必须 deliver。新增 `WorkerCommand` 必须同时通过无通配符 classifier 转换和目标 domain handler 的编译期穷尽处理；session transition 必须对 switch/new/local fork/checkpoint fork 重建 session-scoped scheduler state、projection observer、agent supervisor 和 agent-tool surface，并拒绝仍持有 foreground/detached background run 的切换。
- `ui.rs` 只作为 renderer 模块入口和必要 re-export；shell layout、theme、geometry、text、timeline、tool card、markdown、approval、setup/config、modal 等渲染块放入对应 `ui/*`。
- 单元测试实现不再回填 inline test module；业务文件只保留测试模块声明，测试实现放入同层 `tests/<module>_tests.rs`、领域专属 `app/tests/*` / `runner/tests/*` / `ui/tests/*`，共享 fixture 使用 `common.rs` 或 `*_test_support.rs`。
- Markdown 只由 `ui/markdown.rs` 和 `MarkdownRenderOptions` 统一解析和缩进，不允许 assistant timeline、tool preview、approval modal 各自维护解析规则。
- 新增快捷键或命令时，必须同步 `commands.rs` metadata、info rail、keyboard help 和 README。

这里要特别说明：这不意味着 `sigil` 被做成 DeepSeek 专属，而是表示第一套“做深做透”的 provider 先落在 DeepSeek 上；OpenAI-compatible Chat Completions、OpenAI Responses、Anthropic 和 Gemini 也必须服从同一个 `sigil-kernel` 契约，而不是反过来把内核做成某家厂商私有运行时。TUI `/config` 和 `doctor` 只消费 `ProviderCapabilities` 派生出的中立 capability view，不展示 provider 私有字段作为产品主心智。

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

    fn permission_plan(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolPermissionPlanDraft>;

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
- `ToolSpec` 必须保持 provider-neutral，表达 `name / description / input_schema / category / access / network_effect / preview`，不携带 DeepSeek、MCP 或 TUI 私有状态；其中 `access` 只描述本地 Read/Write/Execute，`network_effect` 独立描述可选的 Read/Mutate/Unknown 网络效果
- 工具执行失败要返回给模型，不应该直接把整个进程打死
- preview 是可选能力，只给交互式前端做审批卡片和 diff 预览用，返回统一的 `ToolPreview`
- `permission_plan` 是每次具体调用唯一的权限事实入口；registry 只解析一次参数，产出 immutable `ToolPermissionPlanV2`，并让策略、审批、执行、审计和会话授权复用同一 `plan_hash`
- plan 同时包含 access、operation、effects、subjects、analysis 完整度、containment 请求、semantic scope、工具默认 mode 与安全摘要；不允许 UI、策略或执行层分别重新解析同一调用
- 文件类工具必须从结构化参数中导出精确 subject；Shell 必须从完整 POSIX 语法树聚合每个子命令、重定向、wrapper 与动态结构；MCP annotation 仅是不可信提示，不完整、冲突或来自第三方时保守降级为 `Unknown/Ask`
- 通用默认 planner 只适用于静态声明足以完整描述调用的工具；Shell、MCP、Skill、Agent、Web、LSP mutation 等动态工具必须显式声明 V2 plan
- analysis 不完整、参数/策略/backend/profile/environment 在审批后漂移，或执行时缺少 exact prepared plan，一律 fail closed；risk 只用于呈现，最终 allow/ask/deny 由 effect、策略、containment 和 hard safety 共同决定
- `egress_audit` 用于工具域内安全出境审计摘要，返回值会进入 durable control state；实现必须先脱敏并限制大小，不能包含原始 secret、文件内容或大 payload
- `execute` 必须接收 provider 侧的 `call_id` 并原样写回 `ToolResult.call_id`，保证 tool call / result 配对可恢复
- 文件类内置工具必须对 workspace root 做 canonicalize，并用路径组件判断 confinement；绝对路径、`..`、目标 symlink 或父目录 symlink 指向 workspace 外时必须生成 `External` subject，再由 `permission.external_directory` gate 决定 deny / ask / allow
- 临时 shell scratch 文件使用运行时注入的 `$SIGIL_SCRATCH_DIR`，实际目录位于 Sigil 用户态 cache root，对模型显示为 `cache/tmp`。scratch 是 session-scoped：每个 session 通过其稳定 scope id 推导独立命名空间（`<cache root>/workspaces/<workspace>/tmp/sessions/<session scope id>`），同一 session 的连续 tool call 复用同一目录，resume 后路径不变；bash、terminal_start、TUI 与 Desktop/application runtime 共用同一推导规则。命名空间在写入前设置为 owner-only（Windows 使用受保护的 owner-only DACL），并受 per-session 容量配额与 workspace 硬上限约束；达到配额时工具返回结构化 `scratch_quota_exceeded` 错误，绝不静默转用系统 `/tmp`。过期命名空间由 TTL GC 回收，且 GC 永远不会删除仍被 active tool 或 terminal task lease 占用的命名空间。系统 temp 目录不作为内置例外：`/tmp`、macOS `/private/tmp`、Windows `%TEMP%` 等仍属于 workspace 外路径，必须走 `permission.external_directory`。

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

当前 JSONL active-session path 由同 canonical path 共享的 process-local coordinator 统一持有 linear
writer、durable frontier、bounded scheduler projection 与 observer registry。启动/recovery 仍以
完整 JSONL 为唯一 truth；成功 append 后先增量 apply 已验证 event delta，再在 writer/projection lock
外发布 typed changed-family notice。projection reducer 失败不能回滚 durable append，只会使 cache
fail closed 并在下一次 authority read 重建。projection 只保留 active queue/revision、task/accepted
plan、bounded compaction summary、pending continuation、active terminal 与 usage/readiness，不复制
transcript、tool output 或 checkpoint body。Idle automatic compaction 先用纯内存 usage/capability/
scheduler state做 cheap preflight；只有 eligible 才从当前 `Session` 捕获 entry-count 与 durable
frontier一致的 immutable snapshot，background 复用同一 coordinator，不再从 path reload一份竞争
session；projection 不复制 transcript、artifact body 或 checkpoint body，只保留 bounded
scheduler/tool-pressure facts；最终 activation仍由 writer-side source cursor/CAS拒绝 stale
candidate。session JSONL、writer lease、lifecycle journal 及其 lease 创建和 writer-open 修复均为
owner-only；Doctor 对最近 session/lease 与 lifecycle journal 做同一不变量检查，custom
`session.log_dir` 不依赖父目录权限间接保密。

Session continuity 与 provider-private acceleration compatibility 分开处理。Portable transcript、任务、
usage、标题和可恢复 control state 始终可读；semantic route fingerprint只决定旧 response handle、native
carrier、cache proof 等私有状态能否复用。同一 durable egress trust binding 内的配置修正由 runtime 在
持有 writer authority、quiescence permit 和 immutable config snapshot 时追加
`SessionRouteRebound + SessionRouteTrustBound` 原子边界；origin/tenant变化、connection缺失或 legacy
trust无法证明时，Desktop/TUI/HTTP都投影 bounded typed recovery，未经 exact-bound确认不得发送历史。
直接运行 TUI 默认创建 fresh session，历史只通过显式 `sigil resume [selector]` 或 `/resume` attach；
每个 session 另有 OS-backed、crash-released的跨进程 write-capable attachment lease，目标 busy 时不释放
source、不启动第二个 worker。该 attachment lease 与 durable writer/lifecycle lease职责独立。

V2 tool result 的 policy-safe bytes 写入 session JSONL sibling resource tree：
`<session-stem>/artifacts/{staging,refs,blobs,trash}`。对外 ref 是随机、session-scoped
`ta1_*` capability，不可反推物理路径；descriptor 记录 observed/policy-projected/persisted bytes、
SHA-256、completeness、sensitivity、retention 和 retrieval policy。artifact 先 staging + fsync +
immutable publish，descriptor 后 append；反向顺序禁止。fork 复制 bytes 并重新签发 ref，export 显式
声明 artifact completeness，delete 与 manifest-only GC 使用 tombstone + grace，active read lease 和
pin/verification/review hold 均是 mark root。

active projection 增量维护 body-free tool-output pressure item：opaque ref/hash、bounded facts/model
excerpt、pair 状态、retention class、token upper bound、active epoch 和 GC reachability。append 后只发送
`ToolOutputPressure` changed-family wake；TUI worker coalesce 后做纯内存 preflight，fit-required aging
先于 semantic compaction，artifact GC 作为最低优先级单飞 blocking task。steady state 不固定轮询，
不完整重放 JSONL，也不持有 data-file lock 等待 I/O。session reload 若 canonical frontier 与当前
Ready projection 完全一致，seed 必须是 no-op，不能伪造 all-family wake 再次触发 GC。artifact manifest
的 identity/session 完整性与 retrieval policy 分层校验：合法 complete 的零字节 artifact 仍是可 inventory、
可回收且读取结果为 EOF 的标准空文件；policy-unavailable artifact 继续禁止 resolve/read。相同 maintenance
失败在一个 session 内只显示一次，成功或 session transition 才重置该 notice latch。

首次 model view 同时受 tool-specific 与 root-run 两级预算：`read_file`、list/glob/grep/search 类
高流量工具最多 8 KiB，普通工具最多 16 KiB，一个 root agent run 的 preview 正文总计最多 64 KiB。
aggregate budget 只按实际写入 `initial_model_view.preview` 的 UTF-8 bytes 扣减，不按工具最大额度
预扣。预算耗尽后 durable result 仍保留 bounded facts、opaque ref、token upper bound 和
`line_page/search_literal` retrieval hint；后续正文只通过 typed retrieval 进入当前 request。

上述 root-run cumulative counter 是 RFC-0059 V2 的 current baseline。RFC-0062 的 proposed V3 clean
cutover 将其替换为四个独立 plane：harness-owned policy-safe artifact capture；per-result 8/16 KiB +
per-assistant-tool-batch 64 KiB actual-byte initial projection；RFC-0059 current/recent/high-signal protection 与
oldest-eligible next-epoch token aging；`read_tool_artifact` 独立 retrieval budget。新的 assistant batch 不继承
历史 batch 的 remaining bytes；历史压力不能把 current safe non-empty result 投影为零。同一 oversized batch
先为每个 safe non-empty result 分配 deterministic minimum preview，再按 tool-call declaration order 分配
剩余 bytes。该段在 RFC-0062 标记 implemented 前只描述 target，不改变 V2 session schema 事实。

持久化边界执行统一的 `SafePersist` 投影：user message、running-input queue、plan/task、agent mailbox/result、tool/provider stream 与 external URL 在首次写 session/control/history 前先做 secret/query/signed-carrier redaction、大小/行数限制和安全摘要。Exact prompt 只在当前进程内交给 provider、`Up/Down` history 或 queue dispatch；durable entry 不保存可反推 verifier，恢复后对 exact-only continuation fail closed 为 stale/interrupted。Query-bearing 或 signed URL 只由 session-local `WebUrlCapabilityStore` 持有，并同时绑定 session id、TTL、LRU 与 restart policy，durable projection 只保存不可反推的安全标识。

外部数据使用 provider-neutral `ExternalSourceRecord` 与 `CitationSupport`：source content 始终带 `ExternalUntrusted`，manual/automatic compaction 与 recovery 不得提升信任；provider/MCP 原始 source id 在进入 session 前改写为 session-local id。Source evidence 与 claim citation 分离，只有存在真实 UTF-8 byte range 时才生成 citation，并绑定最终 safe assistant text digest，避免把未验证来源或 raw carrier 变成 durable 权威事实。

所有后续 Web/remote MCP transport 还必须通过 provider-neutral durable egress barrier。`HostedToolAuthorization`、`McpTransportAuthorization`、`WebFetchTransportAuthorization`、`EgressDisclosurePresented`、`QueryEgressStarted`、`QueryEgressOutcome` 与 `HostedToolOutcome` 是独立 recovery-critical typed event，统一使用 linear `DurableAuditWriter` 的 exact receipt；无 durable store、append/sync 失败或 receipt identity 不匹配都不能得到 effect permit。Transport 顺序固定为 authorization → durable authorization → presenter write/flush receipt → durable disclosure → DNS/dial；Query 顺序固定为 safe query/budget preflight → presenter receipt → durable disclosure → durable Started → body first byte。`PreEgressDisclosure` 和一次性 `DisclosurePresentationReceipt` 绑定 kind、correlation、route/profile fingerprint、无 query/userinfo 的 safe destination、content digest 与 sink fingerprint；ACK 只证明指定 sink 完成呈现/发布，不是 permission grant，也不声称人类已阅读。

E21.16 将该 contract 具体化为三个互不混淆的 presenter：TUI worker 经一次性回执把 disclosure 排入 active card，且只有 `Terminal::draw` 成功返回后才 ACK；同一 safe logical destination 的 handshake、query 与 tool-call disclosure 在 TUI 聚合为一张连续 operation card，卡片可以累计 route/data category/count，并占用 live panel 顶部独立 pinned strip，不覆盖 transcript、底部 live status 或 composer。每条底层消息仍必须分别经过覆盖其 destination、route 与 data category 的成功 frame 才能取得独立 receipt，对应 tool/activation 结束或 run 终止才移除，不能退化为一帧闪现或一次 receipt 覆盖多条出站消息。CLI 只向 stderr 写入安全字段并完成 flush，绝不污染机器可读 stdout；HTTP 最初只写 dedicated structured event 到 synthetic server-side replay buffer，明确不代表生产 listener、live subscriber 或人类已阅读。会话 Audit 视图显示有界的安全 source/citation 摘要，不创建链接或浏览器动作；Doctor 对 Web V1 只做离线 capability/binding/profile 诊断，不触发 socket。E21.17 在同一 public cutover 中接入 TUI/CLI presenter、配置、route 与用户文档；RFC-0026 P26.4B 后续新增 path-bound、crash-safe bounded production disclosure journal/presenter，只有原子持久化完成才返回 receipt。P26.4C 已把该 journal 接入 loopback bearer listener 的 authenticated replay route，并让 `sigil serve` 使用同一 production presenter；synthetic presenter 继续只用于测试。

`QueryEgressStarted` 之后只能追加一个 Completed/Failed/RateLimited/Cancelled/Interrupted outcome，不能自动重发到同一或不同 destination。Hosted authorization 同样只有一个 terminal outcome。Session load 会为悬空 query/hosted authorization 幂等补 Interrupted，重复 recovery 不重复追加，也不重建 query 或重新扣 budget。Runtime ordering coordinator 在 presenter await 前后、durable barrier 之间及返回 wire permit 前重验 admission；revoke/cancel 在 Started 后胜出时立即写 Cancelled，budget hard-cap 在 post-start 胜出时立即写 Failed/BudgetExhausted。

`WebTaskTreeBudget` 只在 top-level run 创建一次 `Arc` owner，并固定 root run id 与 hard limits；main、Planner、SubagentRead/explore、child 和 provider/MCP attempt 只 clone 同一 handle。Provisional reservation 精确绑定 correlation、initial attempt、route lease 与 route fingerprint；pre-wire failure 可 refund，socket attempt 或 body first byte commit 后的 attempt/logical/hosted 计数永不 refund，redirect/reconnect 使用唯一 attempt id 分别记账。Wire/decoded/model bytes逐 chunk 原子 charge，超限触发 root-owned cooperative cancellation hook；per-host/total attempts和concurrency也 hard-cap。Concurrency permit 只有调用方显式证明 operation 完全 quiescent 后才能释放；仅 drop/abort 会保守标记 cleanup incomplete 并保持容量占用，避免把仍可能存活的网络工作误算为已释放。

Provider-hosted search 通过 `CompletionRequest.hosted_tools`、`HostedToolKind` 与按 exact model 查询的 `HostedWebSearchCapability` 表达，不伪装成本地 `ToolSpec`。Hosted provider 的 started/evidence/failed sidecar 使用不可序列化的 secret carrier 承载 raw URL、title、query 与 remote source id；hosted-enabled turn 的 text、reasoning、summary 和 evidence 全部先进入 kernel hard-capped `HostedTurnBuffer`。只有 runtime 注入的 `HostedEvidenceProcessor` 完成 URL validation、session-local source id 重写、安全 persistence projection 和 UTF-8 citation offset mapping 后，kernel 才能向 session、`RunEvent` 或产品 handler 发布 finalized safe text。缺 processor、buffer cap+1、finalizer error、取消或 provider hosted failure 都在 raw delta 可见前 fail closed。Provider adapter 使用显式 `HostedRequestWireState` 区分 zero-wire materialization/connect failure 与 request-bytes-started；后者永不透明 retry。Exact hosted query 只作为 post-hoc observation 留在 transient evidence 中，不伪造本地可审计 preflight。

Gemini hosted search adapter 按 Google GenerateContent 协议把当前 `google_search` 作为独立 built-in tool object 写入 request，而不是伪装成 `functionDeclarations` 或使用旧 `google_search_retrieval`。Model eligibility 使用 provider-owned exact matrix；unknown/旧模型保守 unsupported。根据 Google 当前 capability 表，Gemini 3 exact models允许与 custom function declarations 组合，Gemini 2.x 组合 fail closed；这一组合边界通过 provider-neutral `HostedCustomToolCompatibility` 上报，runtime 的 `auto` route 对不兼容模型保留普通 custom tools 并回退到稳定 client websearch，显式 `provider_hosted` 则在 provider wire 前报错。`max_uses` 与 domain filter 未被当前 `google_search` wire contract支持，因此 capability声明为 unsupported enforcement，带这些limit的request在0 wire bytes前拒绝。Streaming mapper按响应顺序累积 `webSearchQueries` 与增量 `groundingChunks`，并按 Google 协议把跨响应的 `groundingChunkIndices` 映射回source；`Segment.startIndex/endIndex` 按Part内UTF-8 byte offset（end-exclusive）处理。越界、非char boundary、part不连续、segment text不匹配或未知source index只丢弃claim citation，已验证source仍保留。Provider仅把raw query/URI/title包装成redacted、无serde的hosted evidence；runtime finalizer成功前handler与durable session保持0 raw emission。Hosted request只对可证明的connect-time zero-wire failure复用同一prepared attempt；response/status/stream已表明request bytes开始后，即使0 chunk也不透明retry。该adapter仍不注册public web route、不新增配置或TUI入口；Google字段与exact model表的依据是[Grounding with Google Search](https://ai.google.dev/gemini-api/docs/google-search)和[GenerateContent GroundingMetadata/Segment API](https://ai.google.dev/api/generate-content)。

Anthropic hosted search adapter 固定基础 GA `web_search_20250305`，不自动升级到带 dynamic filtering / response-inclusion 的后续版本；后续版本会额外引入 code-execution caller 与数据保留语义，必须由独立切片重新评审。Exact model matrix由provider crate维护，unknown和已退役model保守unsupported。平台eligibility同样由provider实例私有判定：当前仅原生`https://api.anthropic.com` Messages endpoint声明hosted support，自定义Anthropic-compatible `base_url`不假设其代理server tool能力并在0 wire bytes前fail closed。`HostedToolRequest`的authorization、kind、max uses与domain filters进入canonical content-bound fingerprint；domain pattern在kernel先执行count/bytes/ASCII/canonical grammar约束，Anthropic request builder再映射`max_uses`和互斥的`allowed_domains` / `blocked_domains`。Server `max_uses`与domain filter均声明hard enforcement。

Anthropic stream mapper把`server_tool_use`和`web_search_tool_result`按`tool_use_id`精确关联，不生成本地`ToolCall`；同一响应里的client `tool_use`仍走本地registry，hosted/client mixed turn只有client call会被执行。Result error与合法empty result分离；provider aggregate `usage.server_tool_use.web_search_requests`不伪装成单次invocation usage。Citation只在`citations_delta`能唯一映射到一个已验证source URL且能绑定紧邻此前text delta的UTF-8 byte span时生成；provider返回的source `cited_text`只属于source excerpt，不用于匹配assistant输出。重复URL、未知URL或边界不明只保留source并丢弃claim citation。

`encrypted_content`、`encrypted_index`、raw query和完整server blocks只保存在有硬上限的provider进程内continuation store。Durable `ProviderContinuationState`只保存随机handle、`InterruptOnRestart`语义与安全reason，不保存raw carrier或可离线反推digest；live continuation按assistant message id原样回放完整blocks，进程重启或eviction后只使用已经finalize的safe assistant text与normalized source sidecar重新物化，不发送缺字段或伪造partial server block。`pause_turn`保留exact blocks和相同hosted tool definition；mixed turn的pending server call与client result顺序也由同一carrier恢复。Hosted request收到HTTP status、stream error、unsafe EOF或request bytes可能已发送后均0透明retry；新尝试必须由上层重新授权并生成新request。协议依据是Anthropic官方[Web search tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool)与[Server tools](https://platform.claude.com/docs/en/agents-and-tools/tool-use/server-tools)。该adapter仍不注册public web route、不新增配置或TUI入口。

内置 WebFetch 在 E21.17 注册为 public `webfetch`，只接受当前 session `WebUrlCapabilityStore` 可解析的 `source_id`，不接受 novel raw URL。runtime 先生成不含 query/userinfo 的 route preview，经 `WebFetchTransportAuthorization`、presenter receipt 与 durable disclosure barrier 后，才允许 shared destination guard 执行 DNS。Direct/NO_PROXY 路由每一跳重新解析全部 A/AAAA，拒绝 mixed/private/permanent address set，并把 reqwest dial pin 到本次完整校验集合；private exception 必须 exact host 命中且每个地址都落在显式 CIDR，metadata、loopback、link-local、unspecified、multicast与platform-reserved永不可覆盖。Environment proxy 只对 logical destination 做可证明的 guard，安全投影明确标记 `proxy_remote`，proxy credential不进入 audit/Debug；无完整 resolved-set receipt 时不能使用 private exception。

WebFetch transport 禁用自动 redirect、自动 retry、Referer 和 reqwest 自动解压，显式流式处理 gzip/br/zstd/deflate，并分别执行 wire/decoded/model hard cap。Same-origin redirect 每跳重新经过 durable barrier、budget attempt 与 destination guard；cross-origin或 HTTPS downgrade只返回并持久化新的 session capability，不在原调用内继续。HTML/charset 正文经过 bounded decode、active-content剔除、terminal-control清理与 kernel persistence sanitizer，最终输出 `FetchedPage` provenance、安全 URL registration、transport security、network guard与 truncation metadata。Tool result 与 hosted/search source 的 exact capability 在 agent pre-persistence boundary stage/commit，raw URL 在 RunEvent 前被消费；同一 tool result 的 provider-visible message、`WebUrlCapabilityDescriptor` 和 `ExternalProvenance` 必须在一次有序 writer batch 中完成持久化，随后才向 TUI 发布 control 事件，避免 UI 的 session reader 在条目之间抢占 JSONL 锁。运行中的 TUI control 更新只重算内存投影并保留最近一次 durable review cache，完整 durable review 在 session 同步/run 边界刷新。main、provider turns 与 agent child 共享 root-owned `WebTaskTreeBudget` handle。

Active-run facts 是下一次 provider request 的 advisory context，不是 finalization request，也不是对已经完成的 assistant stream 做事后否决：agent loop 必须在构建 post-tool request 前以 `active_run_facts` 注入当前 run 的实质性 facts，只按本轮 `tool_call_ids` 投影 command、approval 与 changed-file evidence，并明确要求模型从当前状态继续。该 context 在一个 root run 内是单一、可替换的 transient snapshot；facts 变化时原位替换，不能把 v1、v2、v3 的完整累计快照连续追加到后续 request。agent facts 只纳入 durable invocation grant 绑定到当前 root logical run 且仍 running/unread/unsettled 的 child；历史 root、closed 或已完整交付的 child 不得持续触发新一轮生成。该 transient context 禁止预投影 `RunStatus::Completed`、`pending_final_answer` 或 final readiness；真正的 readiness 只在 final answer 已产生后计算。runtime 必须先用内存投影确认存在 material facts，普通 policy-allowed 网络只读记录仍保留审计，但单独存在时不能触发额外生成或 JSONL 读取。final answer 写入后，`RunStatusChanged`、`RunFinalized` 与 `ReadinessEvaluated` 必须在同一个 ordered writer batch 中完成一次 durable sync；worker 直接接纳 run task 返回的 authoritative in-memory session，并把 Session detached 期间由 worker 已成功持久化的 control delta 合并回内存投影，不在 `RunFinished` 前重读完整 JSONL。TUI 在 run 边界只读取一次 durable records，并用一次 checkpoint/readiness reducer 从同一 snapshot 同时更新顺序状态与 review sidebar。完全相同的 `PrefixSnapshot` 复用最近一次 durable snapshot，不重复把同一 materialized prefix 写入 session。

上述 facts 从 Some 收敛到 None 时必须移除旧 transient snapshot；final-answer blocker 只读取同一
current-root child 集合，不再嵌入全 session command/file facts。Blocker prompt 也只有一个可替换实例，
并有独立于 `max_turns` 的有限 retry budget；稳定 blocker 不能在默认无 turn 上限时形成 provider 热循环，
超限必须以 blocked terminal 收口，且 child/Task 映射不得把该 terminal 误记为 completed。

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
    /// 仅用于进程内 bounded inline adapter；不是 durable raw body contract。
    pub content: String,
    pub status: ToolResultStatus,
    pub metadata: ToolResultMeta,
}

pub struct ToolResultRecordedV2 {
    pub schema_version: u16,
    pub message_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub artifact: ToolArtifactBindingV1,
    pub facts: ToolResultFactsV1,
    pub initial_model_view: ToolModelViewV1,
    pub initial_model_view_sha256: String,
    pub capture_telemetry: ToolResultCaptureTelemetryV1,
    pub recorded_at_ms: u64,
}

pub enum ToolResultStatus {
    Ok,
    Error(ToolError),
}
```

关键点：

- `args_json` 在重组完成前应保留原始字符串形态，避免过早解析把截断问题藏起来
- 错误分类只放在 `ToolResultStatus::Error(ToolError)` 中，不通过 metadata 或文本约定判断
- 可能产生大输出的工具通过 `ToolContext::create_policy_safe_tool_output_sink()` 边执行边写 session-scoped immutable artifact；publish 成功后才能 append V2 descriptor
- provider-visible tool message 来自 `ToolModelViewV1` 的 bounded canonical JSON；JSONL 保存 descriptor、facts 和 initial model view，不保存 artifact body；普通 initial preview 同时受 tool-specific per-result cap 与 root-run 64 KiB aggregate cap，aggregate budget 只按实际 preview bytes 扣减；`read_tool_artifact` 使用自身 16 KiB per-call、8 次/64 KiB per-turn retrieval budget，不消耗也不受 initial-preview aggregate cap 阻断；独立 `ToolDisplayViewV1` 只作为 bounded 产品面 DTO 返回
- RFC-0062 V3 target 不再使用 root-run cumulative preview counter：current assistant batch 使用独立 64 KiB actual-byte cap 和 per-result minimum preview；历史 provider context 由 token pressure、protected classes、oldest-eligible deterministic aging 与 next-epoch activation 管理；当前实现仍是上一条 V2 contract
- `ToolResultMeta` 可承载 `exit_code`、`changed_files`、`truncated`、`bytes` 等非错误分类信息；V2 capture 将其投影为有大小上限的 `ToolResultFactsV1`
- model/TUI/Desktop/HTTP 只接收 `ta1_*` opaque ref。正文按 byte/line/literal selector 通过 `read_tool_artifact` 或 authenticated typed page endpoint 读取，并受 per-call/per-run budget、session scope 与 full SHA-256 校验约束

#### SessionLogEntry

```rust
pub enum SessionLogEntry {
    User(ModelMessage),
    Assistant(ModelMessage),
    ToolResultV2(ToolResultRecordedV2),
    Control(ControlEntry),
}
```

这里建议把“发给模型的消息”和“只给系统自己的控制记录”区分开：

- `User / Assistant / ToolResultV2.initial_model_view` 是真正可能进入 provider request 的历史
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
- `ToolPermissionPlannedV2` / `ToolPermissionDecisionV2`：记录安全、有界、无原始命令或 secret 的 plan/decision 事实，包含 exact plan hash、effect、subject、analysis、策略来源、containment 和可用会话授权
- `ToolApproval`：记录与 approval request id、plan hash 和 execution binding hash 绑定的请求、accepted/resolved/expired/stale/cancelled 终态及 preview 事实；控制路由 accepted receipt 到达后 Desktop/TUI 必须立即退出 waiting 状态
- `ToolExecution`：记录工具执行 started/completed/failed/interrupted，包含同一 plan/binding identity、duration、subjects、changed files、有界 metadata、structured error 与 provider-visible result hash
- compaction 不再作为 `ControlEntry` 记录：它使用 `CompactionStarted`、`TaskMemoryRecordedV1`、`CompactionAppliedV2` 和 terminal direct-JSON durable event；只有经 lifecycle/sidecar resolver 验证的 V2 boundary 才能影响 request context
- `Note`：承接不值得升格为独立结构的控制面元数据

这样做的好处是，provider continuation、后台任务恢复、缓存诊断都会落在同一条 append-only 审计链上，而不是散在 runtime 内存和 UI 状态里。

当前实现中，`Session` 提供 `latest_response_handle`、`latest_prefix_snapshot`、`continuation_states` 和 store-backed V2 context projection 等显式查询方法；agent run 初始化下一轮 request 时会从 durable control state 恢复最新匹配 provider 的 response handle，而不是只依赖进程内变量。

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
pub struct PrefixSnapshotMaterialization {
    pub schema_version: u16,
    pub byte_len: usize,
    pub message_count: usize,
    pub tool_schema_count: usize,
    pub runtime_context: Option<PrefixRuntimeContextSummary>,
}

pub struct PrefixSnapshot {
    pub materialization: PrefixSnapshotMaterialization,
    pub sha256: String,
    pub provider_name: String,
    pub model_name: String,
    pub memory_fingerprint: String,
    pub tool_schema_fingerprint: String,
    pub skill_index_fingerprint: String,
}
```

`sha256` 对完整 `messages_json + "\n" + tools_json` 计算，`materialization` 只保存固定上界的
字节/消息/工具计数和最多 8 条无正文 Context provenance；完整历史不会复制进 control event。
因此 `PrefixSnapshotCaptured` 的大小不随 session 历史增长，但仍能回答：

- 完整 prefix 的稳定 identity、字节量和组成是什么
- 它是不是被意外改了
- 当前 session 为什么还能命中，或者为什么突然掉命中

该 V2 shape 是当前唯一支持的 session format：缺少 `materialization`、仍依赖
`materialized_text` 的非当前日志直接拒绝，用户可删除该 session；实现不通过提高 1 MiB event
limit 掩盖无界 control payload。

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
- HTTP 与 Desktop 已复用 kernel event stream，并分别拥有收窄的 transport / IPC protocol

### 7.1 TUI worker 命令面

当前 TUI worker protocol 的主要命令面包括；完整枚举以
`crates/sigil-tui/src/runner/protocol.rs` 为准：

- `SubmitPrompt { prompt, reasoning_effort }`
- `SubmitPlanPrompt { prompt, reasoning_effort }`
- `InvokeInlineSkill { skill_id, arguments, reasoning_effort }`
- `InvokeChildSessionSkill { skill_id, arguments }`
- `InvokeAgentProfile { profile_id, prompt, parent_prompt }`
- `SubmitTask { prompt }`
- `ContinueTask { task_id: Option<String>, guidance: Option<String> }`
- `ApprovalDecision { call_id, approved }`
- `CancelRun`
- `CancelTerminalTask { task_id }`
- `CloseAgent { thread_id, reason }`
- `CancelAgent { thread_id, reason }`
- `MessageAgent { thread_id, prompt }`
- `CompactNow`
- `CheckChangedFilesDiagnostics`
- `CleanMutationArtifacts { target }`
- `DeleteMutationArtifact { artifact_id }`
- `StartNewSession { session_log_path }`
- `SwitchSession { session_log_path }`
- `Shutdown`

对应主要消息包括；完整枚举以
`crates/sigil-tui/src/runner/protocol.rs` 为准：

- `Event(Box<RunEvent>)`
- `Notice`
- `RunStarted`
- `SkillRunStarted`
- `PlanRunStarted`
- `AgentRunStarted`
- `AgentResultContinuationStarted`
- `RunFinished`
- `PlanRunFinished`
- `AgentRunFinished`
- `TaskRunStarted`
- `TaskRunFinished`
- `RunCancelled`
- `AgentThreadEvent`
- `AgentThreadStatusLive`
- `AgentThreadClosed`
- `AgentThreadCancelled`
- `TerminalTaskUpdated`
- `ConversationQueueUpdated`
- `ConversationQueueDispatchStarted`
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

Desktop、TUI、CLI 与 HTTP streaming 都消费同一套事件语义，而不是各自重写 turn lifecycle。

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
- 整个循环使用 kernel-owned `RunCancellationOwner` / `RunCancellationHandle` 协作式取消。provider turn、tool、process、MCP socket 与 child work 在最后责任边界取得 forward effect permit；取消请求原子关闭新 permit，cleanup/rollback/reap 仍可取得 cleanup permit。
- TUI 只投影 `Cancel requested -> Cancelling -> Cancelled | Interrupted`。唯一 owner 等待 owned task 与 effect permit 有界收敛后才可写 `Cancelled`；deadline 超时、process-tree/MCP cleanup 未确认或 hard abort 都写 `Interrupted`/cleanup-incomplete，不把本地 future drop 伪装成远端确认。
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

Planner / executor / subagent 协作已经作为跨表面的共享 task flow 落地。Durable task 在 TUI 中的显式入口是 `/task <任务>`；`/plan` 只表示一次性 Plan mode / read-only planning prompt，不创建 durable task state。TUI 中 `task.routing_policy = "auto"` 时，普通 chat 先进入独立 routing-only microturn；模型只能在 `request_task_planning` 与 `continue_without_task_planning` 之间给出一个 typed semantic decision，host 不扫描 prompt 关键词。正向 decision 进入同一 durable task flow；负向 decision 后才在下一 turn 恢复普通工具面。free text 或无效 decision 只重试一次，仍无效则 blocked，不能把 routing 文本当作用户回答。默认 `auto` 时普通输入走三路自动路由（Chat / PlanReview / Task），显式 `manual` 保持 chat-first。Production HTTP driver 与 Desktop-owned `sigil serve` child 已附加共享 foreground task executor，并完成 Task control/recovery parity：typed continuation 可携带 task-targeted guidance；integration review/accept 绑定 exact task/plan/preview digest、promotion authority 与 parent verification；Pause 复用 TUI 的 exact `TaskPauseRequest`、root cancellation scope 和 Task stop transition。请求前绑定 task/plan/scope，只有 root execution、child/effect permit 全部 quiescent 后才通过单一有序 writer batch 追加 active step/child terminal，并最后追加 Task `Paused` / `Cancelled`；cleanup、join 或最终 binding 不确定时只能追加 `Interrupted`。普通 run cancel 只有在 durable cancellation scope 真实绑定 Task 时才修改该 Task，不能误伤旧任务。autonomous planner 的 typed participant schema 只接受 read/write/review；可信 verification policy/check 由 host 绑定到 mutation step，并在 participant 结束后执行，不创建缺少 verification tool 的模型 `verify` participant。Task participant 在 mutation 后只有有界 read-only 收敛尾部；超过额度或即将耗尽轮次时，host 注入 route-fingerprinted finalization contract，并移除 client/hosted tools，只允许一次 bounded result 收口，且不影响普通 chat。HTTP schema v9 的 authenticated typed routes、幂等 command receipt 与 production supervisor 复用相同 authority；Desktop schema v9 handshake、native typed client、Tauri allowlist commands、Task card 与 integration inspector 消费相同 contract。canonical conversation display 还在固定 durable frontier 投影最多 128 个 step/lane 的 `task_control` 和显式 truncation，应用重启后即使没有 process-local live event 也能恢复 Continue/guidance/integration controls，同时不暴露 objective、prompt/transcript、private workspace/ref 或 mutation authority。release 默认值为 `auto`（review-first 基线），只有 qualified real-model evidence 与 rollout manifest 精确匹配才允许 `DirectTask`。恢复只补本地 handoff/TaskRun admission crash gap，不重放原 conversation provider request；只有能证明尚未发生 planner/participant dispatch 的 task 才自动接管，stale Running step/lease 会先记为 Interrupted/Paused，再由 `/task continue` 显式继续。`/plan continue` 不再作为 alias。普通 chat 明确要求 subagent / 子 agent delegation 时，可通过 agent-thread tools 直接创建 child agent，不需要进入 durable task。

当 durable focus 中只有一个 exact current、resumable、accepted-plan Task 时，routing surface
才额外曝光 `continue_existing_task`；task id、task status、plan version/status、source turn 与
route fingerprint 都由 host 冻结，模型参数不能选择身份。selection receipt 以 append-only
control entry 落盘，adapter 在 dispatch 前再次执行 exact CAS；source turn 的 exact prompt
只保留在进程内并作为 guidance 进入 planner review，由 planner 决定补充未开始步骤或接受
下一 plan version。planner 的 result/terminal 与 apply materialization，或新 plan、carried
completed steps 与 result/terminal，分别以单个 crash-safe writer batch 落盘；安全 guidance
可在 reload 后由所有 continuation 入口恢复到原 target steps，敏感 guidance 只保留 safe
projection，必须重新输入匹配原 hash 的 exact 文本，且不得扩大到其他 pending steps。
所有 continuation 入口在创建新 authority 前，必须先解析同一 Task 的 unfinished
selection/promotion 或 materialization：同一 receipt 复用，不同 receipt fail closed，不能再次
启动 planner。若崩溃发生在 guidance planner 的 durable Started 与 settlement 之间，startup
不自动重放不确定的 provider generation；下一次显式 continue 先把 selection-owned 旧 attempt
标为 Interrupted（active Task 同批转 Paused），再以新 ordinal 重试。Completed 但未 settlement
的旧 attempt 仍 fail closed。
普通 Chat / PlanReview 会清除 current Task focus，迟到的旧 Task progress 只能更新历史投影，
不能重新抢占 TUI 或共享 conversation display。显式 `/task continue` 与 application Continue
在 provider I/O 前写入绑定 exact cancellation scope、task status 与 plan version/status 的
`TaskRunTargetSelected`；无 accepted plan 的恢复也走同一 shared continuation runtime。
显式 `/plan` 即使没有 `PlanReviewAttempt::Started`，其 durable `PlanDraftCreated` 也会清除旧
current Task focus；旧 paused Task 仅保留为 resumable history，不能与新 plan preview 同时作为
当前控制面展示。

自动 routing 的 negative decision 还必须通过 route-fingerprinted direct-execution
continuation contract 进入 ordinary turn：恢复 ordinary tools 后执行原始请求，不能只复述
routing decision 或宣布将要行动。routing-only microturn 的 text/reasoning/assistant narrative
不进入 live UI，也不持久化为 transcript；同批经过 preview/approval 的 memory 工具 lifecycle
仍必须可见。该过渡由 typed decision 驱动，不扫描用户 prompt 关键词。

RFC-0063 把 conversation 语义路由扩展为 Chat / PlanReview / Task 三路。每次 durable route
decision 都绑定 route contract fingerprint（routing contract、exact tool surface、capability
与 host route facts），自动 capability 由 host 证据决定，模型不能自封：无精确 rollout
qualification 时只能得到 ReviewFirst，只有 qualified orchestration manifest 精确匹配 provider、
model、endpoint、build 与 task defaults 才允许 DirectTask；kill switch 或 provider 不支持
streaming tool calls 时整体 Unsupported。PlanReview 是完整 durable lifecycle（Started →
DraftReady / CompletedWithoutDraft / Failed / Interrupted / Cancelled）：模型通过 typed
`submit_plan_draft`（schema v2）提交 plan draft，host strict-validate 后 append
`PlanDraftCreated`，只读 research 与 draft 提交都在 child session 内；`/plan` 与自动 review
共享同一个 PlanReviewCoordinator。plan decision（Run / Save / Revise / Reject）绑定 exact plan
id/hash，Run 直接走 RFC-0018 的 `TaskCreatedFromPlan` 前缀，不重放 planner；Revise 启动新一轮
attempt。RFC-0063 §13.9 后，Revise 先通过 RFC-0064 收集 durable guidance；base plan 与 revision
substate 同时投影，candidate 成功前不覆盖 base plan，所有 terminal failure 都恢复 base actions。
公开 conversation 投影只暴露 bounded plan review summary（active plan、revision、status、counts、
risk、allowed actions、source、stale），完整 plan document 通过绑定 plan id/hash 的 authenticated
detail contract 读取；detail 包含 structured step/detail/dependency/path/check/risk/lineage，但不暴露
prompt、private ref 或 authority。HTTP
`POST /sessions/{id}/plan-decision` 与 Desktop `desktop_plan_decision` IPC 复用同一 typed
command receipt 的幂等语义；React/TUI compact card 只作为入口，DraftReady 后由 dedicated Plan Review
workbench 提供完整审阅。research 与 submit-only finalizer 使用隔离 child context；finalizer 不继承
research tool history/continuation，只消费 bounded evidence bundle，非 `submit_plan_draft` 调用在 dispatch
前以 typed protocol violation 终止。

RFC-0064 定义普通 agent、PlanReview 与 interactive planner 的 durable user-input protocol。模型只能通过
bounded `request_user_input` typed tool 创建问题；host 先 append `UserInputRequestedV1`，再以
`AgentRunDisposition::AwaitingUserInput` 结束 physical worker并保留 suspended logical ownership。
answer command 绑定 session/run/request/generation/hash/command identity，durable accepted 后由 supervisor
CAS claim 并恰好一次启动带 exact synthetic tool result 的 continuation，不 replay 原 provider turn。
pending request 无 wall-clock timeout，restart/switch/reconnect 从同一 projection 恢复；background agent
question 进入 root attention queue。approval、MCP elicitation 与 secret input 保持各自 authority：MCP 只
共享 bounded form renderer，断线后不 replay answer；secret 明文不进入 session 或模型 tool result。

三路 eval corpus（`dev/evals/model-fixtures/orchestration-v1`，rfc-0063-orchestration-v1）冻结为
20 Chat / 15 PlanReview / 15 DirectTask，report schema v2 的 gate 按类推导
Chat→Task FP（<=5%）、PlanReview→Task premature（=0）、DirectTask miss（<=10%）与
ReviewFirst baseline 的 over-route / miss（<=10%）；durable route decision 只从
`ConversationRouteDecisionRecorded` 计数，assistant prose 不参与 gate 证据。route-local
hard-invariant kill switch 把 DirectTask 降级到 ReviewFirst（保留可审阅的 plan review
handoff），只有 plan lifecycle 自身 invariant 才降级到 Unsupported/Manual。

2026-07-23 的 O5a 实现后，ordinary-chat natural-language delegation 仍未稳定绑定
`AgentDelegationRequirement`（不得用关键词推断），因此 ordinary ingress delegation hard gate
仍不能描述为已完成。Task scheduler 选出的多个 shared-read-only ready steps 已由 runtime
prepare/execute/commit launcher 真实并发执行；participant future 不接收或修改 parent Session，
terminal/result 仍按稳定 plan order 由 parent single writer 提交。自动 task routing、typed handoff、
planner/participant transcript isolation、唯一 parent final synthesis、Plan V2 direct promotion、
crash-gap reconciliation、O5a read concurrency、O5b2a whole-batch admission、O5b2b
supervisor-lifetime provider route cooldown、O5b2c shared-read-only durable bounded retry、
O5b2d1 adaptive provider route concurrency window 和 O5b2d2 Planner/Synthesis durable
retry、O5b2d3a 实时 route attribution 以及 O5b2d3b completion-arrival/request-order 双序
进度和 O5b2 显式 batch action/envelope boundary 已完成。同步 prepare 只在返回前借用
parent `Session`，detached child future 不捕获 parent；全部 terminal envelope 收口后，kernel
才把 parent 交给 one-shot commit envelope 并按 request sequence 单写 durable commit。到达顺序
只保存在 `AgentSupervisor` 生命周期内的 live snapshot；TUI task strip/info rail 显示
`arrival #N → commit #M`，不会把运行态 arrival 顺序写入 session 或恢复授权。
`TaskRunProjection.active_steps` 和 TUI 多 active-step
展示已在 O5b1 完成。目标协议和分阶段改进计划见
[RFC-0053](rfcs/0053-autonomous-task-routing-and-parallel-agent-orchestration-v1.md)。

当前实现选择如下：

- RFC-0051 Intent Stack 的 R51.0-R51.7 已完整落地在 provider-neutral
  `sigil-kernel::intent` / `intent_admission` / `intent_lineage` / `intent_layer` /
  `intent_operation`：model/provider proposal 使用无 runtime
  authority 的 alias schema，accepted plan、独立 acceptance event、Task/Chat provenance、
  artifact/layer/operation 与 bounded public DTO 使用严格 V1 schema；canonical JSON digest
  与 exact byte digest 分型，layer core -> artifact manifest -> final layer manifest 使用无循环
  单向 digest 图。host-only acceptance authority 将 `UserDeclaredRoot` 绑定原始 user turn，并将
  `SuggestedDecomposition` 绑定 exact proposal digest 的显式确认；runtime 重新分配 retry-stable
  intent/criterion id。admission 与 execution/ChangeSet/verification 共六个 intent durable
  event 已注册，Task-bound admission 把 accepted
  `TaskPlan` 作为 mixed writer batch 的最后一条记录，append-only projection 只有在紧邻且
  identity/version 匹配时才激活；crash prefix 与旧 session 分别投影为 incomplete 和
  `history_unavailable`。TaskPlan write step 持久化 runtime-resolved stable `intent_refs`；
  Task/Chat execution 绑定 exact attempt，Chat direct file mutation 可从 RFC-0002 evidence
  生成 bounded ChangeSet，Task 只接受 `WorkspaceApply` parent mutation lineage，GitRef-only
  与缺失/stale evidence 降级 read-only。`SystemVerified` 还必须匹配显式 criterion-scoped
  CheckSpec、policy、receipt、ChangeSet 和当前 parent snapshot。R51.3 只从 exact terminal、
  applied ChangeSet 与 RFC-0002 prepare/commit materialize content-addressed forward/reverse
  patch、file hunk 和 canonical manifest；同文件跨 active intent 一律 shared，后续 formatter、
  codegen/unknown-dirty 或 artifact lifecycle 缺口降级 read-only，exclusive/available layer
  artifacts 进入 retention protected set。R51.4 以只含 stable id/version/digest 的 request 和
  host-only approval authority 生成 exact drop preview，在 workspace lease 内重投影并通过
  全文件 prepare-before-commit 的 RFC-0002 CAS batch 应用 reverse artifact；operation id
  绑定 preview 所在 durable stream frontier，使拒绝后的合法重试获得新 identity、并发旧
  preview 在 append 前失效。partial/crash 只按 durable evidence 收口，不重放文件写。
  Committed drop 使旧 verification stale、public intent 进入 Dropped、checkpoint restore
  返回 intent-state conflict，并让该 layer 退出 retention protected set。R51.5 提供
  dependency closure、read-only revise/replace impact、immutable supersession 与 exact
  fork/workspace adoption；R51.6 提供 Desktop/TUI 共享 review 语义、responsive/mouse、retention/conflict
  与 exact Drop confirmation；R51.7 让 TUI、typed HTTP、Desktop 和 CLI automation 复用同一
  application command/projection，并把 Plan V2 Intent proposal、Task child grant、worktree
  ChangeSet、integration promotion、`ChangeSetApplied` 与 materialized layer 串成 exact
  lineage。Canonical worker-loop dogfood 已覆盖三个 Intent 的并行隔离执行、parent promotion、
  durable reload、automatic compaction 存续和精确 leaf Drop。
- `sigil-kernel::TaskStateProjection` 从 append-only control log 重建 task run、plan、step、child session 和 route 摘要状态。
- `sigil-runtime::ConversationCoordinator` 将 TUI direct/queued source turn 绑定到 typed run purpose；TUI 在 typed handoff 后于同一 cancellation/approval root 内继续 task。Application source 使用相同 conversation purpose、foreground Task executor、typed control 与 restart recovery contract。
- Planner 通过 internal model-visible `task_plan_update` tool 写入 durable plan；该 tool 由 agent loop 拦截并写 `ToolExecution` audit，不作为普通 workspace tool 执行。
- Planner、Executor、Subagent 和 Synthesis 都使用 retry-stable child session；parent 只记录 attempt/result、bounded summary、child final ref 和 task control state，不持久化内部 prompt/transcript。
- Executor step 使用 transient request context 接收 objective / plan / step，不把每个 step prompt 写成 parent session 的普通 user message。
- Continue guidance 使用同一 transient request context 注入当前 executor/subagent step，不作为新的普通 user history 写入。
- Subagent read/write step 使用 child session；parent session 只记录 child-session link、状态和 summary hash。
- Plan mode prompt 使用普通 agent loop，但用户 prompt 和 plan-mode 指令都只作为本轮 transient context 注入，不追加为 parent `User` entry；工具面使用 planner scoped registry，同时保留 agent-thread tools 以支持显式只读 delegation。
- Plan mode 的 fenced `sigil-plan-v2` 与 `TaskPlanEntry` 共用 role/dependency/mode/isolation 语义。只有当前 V2 draft 且 base/current workspace snapshot 均存在并完全相等时才直接成为 accepted TaskPlan；非当前 schema 直接拒绝，字段不完整或 stale draft 不能 promotion，正常 `/task` 请求仍可通过隔离 Planner 生成新的当前计划。Task 终态由隔离 Synthesis 生成，host 校验 result hash/plan version 后向 parent 追加唯一 FinalAnswer 和 `TaskFinalAnswerCommitted`；恢复可幂等修补部分提交前缀。participant result 自带 terminal status；若 step result 已落盘但 readiness/step terminal 未落盘，恢复会 Blocked/Pause 并释放 lease，禁止重跑潜在副作用。
- Kernel 已提供独立于 durable task 的 `PlanApproved` control entry 和 `PlanApprovalProjection`，记录 plan version/hash、批准时间、`ask` 或 `workspace_edits` 权限、scope、过期策略和是否清理 planning context；`workspace_edits` 只覆盖带 required preview 的 workspace file write tool，不放宽 shell/execute、network、MCP 或 Agent spawn。TUI plan prompt 完成后会在 live band 展示 approval surface，并通过 worker `ApprovePlan` 追加 `PlanApproved` 后同步回 TUI；`ApprovePlan` 会从 plan 文本中保守提取 workspace path 写入 `PlanApprovalScope.workspace_paths`，plan 未包含路径时 scope 为空并保留既有全 workspace 行为；执行阶段已按 active `PlanApproved(workspace_edits)` 将 scope 内 workspace file write 的 `Ask` 降级为 `Allow`，显式 `Deny`、external directory、空 subject、scope 外路径和非文件写工具仍按原 permission policy 处理。模型语义偏离 approved plan 时要求重新批准仍是后续项。
- Child agent result 默认只把 bounded summary 和 result ref 带回 parent context。对 root-run 内、整批均为 runtime-owned `spawn_agent(join_before_final)` 且 contract-safe read-only 的调用，host 会在 tool batch 后并发驱动全部 child、先持久化 terminal/result，再按原调用顺序注入 bounded `agent_join_results` transient context，模型无需调用 `wait_agent`；自定义 Agent 类工具和 background spawn 不获得该并发资格。`spawn_agents` compatibility surface 则把 2-4 个 read-only participant 作为一个 host-owned join batch 注册，完成后注入 typed `agent_batch_results`。`AgentResultContinuation::Completed` 只在携带对应 context 的下一 provider turn 成功返回后写入，发送前取消、max-turn 和 provider 失败不能伪造交付。joined child 不作为可脱离 parent 的 task，settle drop 会同步取消 unfinished future 并释放 supervisor slot；delegate 返回后、settle 前的 tool-result 持久化/事件失败会显式 abort dependency，settle 后、provider dispatch 前的 root cancel 会把 Started context 转为 Cancelled；root cancellation 和单成员 parent commit 失败仍会收口全部 sibling。手动 detached/background 状态检查仍可使用 `wait_agent`，但它只返回轻量状态，不返回 child final answer 正文。完整 final answer 保留在 child session，需要更多细节时通过独立的 `read_agent_result` tool 显式分页读取；分页正文只作为当前 request 的 transient context 提供给模型，durable parent tool result 只记录 offset、长度、截断状态和 result ref，不能通过无限调大 summary、反复 wait 重复 summary、恢复后重复回放分页正文或回灌完整 child transcript 解决长报告场景。
- O4b1 已把 joined participant 的完成收集抽为 runtime-owned `AgentCompletionHub`：完整 batch 在 poll child future 前按稳定 attempt identity 拒绝重复 registration；每个 accepted participant 只产生一个 `AgentTerminalEnvelope`，failure 同样作为 terminal envelope 收口，不会提前丢弃 sibling。envelope 同时保留 completion arrival index 和原始 request sequence，因此 parent 可以按真实完成顺序接收、继续由单写者追加 durable control state，并在构造 provider transient context 前恢复稳定 request order。
- O4b2 已加入 `spawn_agents`、stable `request_key`、whole-batch preflight 和 supervisor 原子 slot reservation。容量、profile、delegation、permission/tool-safety、provider/session materialization 或 join registration 任一失败时，不 poll participant provider future；成功 join 成员经 completion hub 并发收口，按 request key 稳定排序后注入 `agent_batch_results`，模型 polling turn 为 0。
- O4b3a 已为隔离 Task planner 加入一次性的 planner-only `request_task_discovery`：默认最多 3 个 Explore probe、硬上限 4，`multi_agent_mode=none` 或配置为 0 时不暴露。Probe 固定绑定 trusted built-in Explore、`SubagentRead`、`SharedReadOnly` 与继承的 root cancellation/web budget；duplicate id/objective、非 workspace-relative path、结构重叠、unsafe tool registry 或容量不足会在任何 provider dispatch 前拒绝整批。成功批次原子预留 slot，经 completion hub 真实并发，parent 单写提交 terminal/result 后按 `probe_id` 稳定注入 `task_discovery_results` 并自动恢复 planner，模型 polling turn 为 0。Planner discovery 仍沿用自己的 completion hub；Task DAG shared-read-only participant 的 prepare/execute/commit launcher 已在 O5a 独立接入。
- O4b3b durable/TUI projection slice 已把 provider-neutral `AgentBatchId` 和稳定 member key
  加入 append-only `AgentThreadStarted`。普通 `spawn_agents` 与 planner discovery 都在 Started
  entry 中持久化 batch identity；非 batch thread 的两个字段都为空，不完整 identity fail closed，
  重复 member key 或 parent mismatch 会标记 projection degraded。TUI 从 durable projection重建
  batch header 和缩进成员，compact rail 保留 group context，header 不可选且不改变 `/agent`
  可选序号。
- O4b3b parent logical-run identity slice 已在 provider-neutral `AgentToolDelegate` 上绑定当前
  root logical-run id。runtime 在 child admission 前使用 `root logical-run id + outer tool call
  id` 派生 opaque `AgentBatchId`；缺失或空 identity 时整批拒绝，同一 root run replay 保持稳定，
  不同 root run 互相隔离，且不再依赖 session 文件路径。detached background batch 已复用该
  durable identity。
- O4b3b detached background batch slice 已支持 `completion_mode=background`。它复用整批
  preflight、session materialization 和 supervisor 原子 slot reservation；每个 member 获得独立
  cancellation owner 与 mailbox。runtime 先创建 gated tasks，再把全部 detached handles 原子写入
  共享 owner，成功后才放行 provider dispatch；注册失败会 abort gated tasks 并为已 Started
  members 写失败终态，因此不会出现半批 provider 启动。成功 tool result 立即返回
  `backgrounded=true`；空闲 collector 后续单写 terminal/result，TUI 通过既有 non-blocking
  result-ready continuation 展示完成且不抢占 queued follow-up。
- O4b3b restart reconciliation 已接入 session writer restore。无终态 attempt 先追加
  `AgentRunInterrupted`；ThreadStarted/Running 已 durable、但 AttemptStarted 尚未 durable 的
  crash window 再追加 thread-level Interrupted，因此 batch member 全部 terminal 后 active batch
  自动归零。`AgentResultContinuation::Started` 因 provider delivery outcome 不确定而恢复为
  Failed；Pending 只有绑定 durable child result 时保留，否则 Failed。恢复结果 append-only、
  幂等且不创建 provider request，稳定 batch identity 只用于审计与投影，不能作为自动重放授权。
- Parent agent 在发起 Agent 类 tool call 的同一 model turn 中产生的 pre-tool assistant 文本只作为 live stream 展示，不作为持久 parent session history 重放，并在 TUI 中按 Thinking 样式渲染；这避免“先自己补做，再等待子 agent”的内容污染父上下文。最终面向用户的回答必须发生在 child result/status 已回到 parent 后的后续 turn。
- Kernel 已提供 `AgentDelegationRequirement`：绑定该 requirement 的 run 会拒绝接受未产生 terminal 或 result-bearing Agent 类工具结果的 final answer，并通过 transient retry prompt 要求模型调用 agent-thread tool；无效输入、tool execution error 或仍处于 running 状态的 agent tool result 不会解除 hard gate。当前 TUI ordinary-chat 生产路径尚未稳定绑定该 requirement，接线与 typed delegation authority 属于 RFC-0053 O1。
- Task DAG 的 shared-read-only ready batch 已在 O5a 接入真实并发。Coordinator 先按 plan order 追加所有 Running/attempt，runtime prepare 完成 child thread/session admission 后，让不捕获 parent Session 的 child futures 并发 execute，再按 request order单写 commit。`[task].max_parallel_read_steps` 默认 `4` 并已由 TUI task runtime 接线；supervisor budget 继续限制有效 active children。独立 member failure 不提前取消 sibling，依赖步骤在全部 batch terminal 提交后阻断；逆序 completion 不改变 durable parent 顺序。O5b1 进一步让 `TaskRunProjection.active_steps` 从 append-only step 状态重建全部 active identity；`current_step` 仅作为恰好一个 active step 时的派生简写，TUI task strip/info rail 会同时标记 active 行，Task cancel/interruption 也会收口全部 active step 与 started child。多个 active child 并存时，缺少 source identity 的 MCP elicitation 直接 fail closed，不猜测 latest child。O5b2a 已把并发 read child 改成 whole-batch admission：runtime 先完成全部 member 的 shared-read-only/agent/session preflight，再由 supervisor 原子预留全部 active-child slot；容量或任一 preflight 失败时整批返回失败且 provider dispatch 为零，全部 child 成功领取 reservation 并持久化 Started 后才放行并发 execute。启动提交中途失败会为已 Started member 写失败终态并释放 reservation，仍不触发 provider。O5b2b 进一步让五个 canonical provider 在 429 时用 kernel `ProviderRateLimitError` 保留 `Retry-After` delta-seconds/HTTP-date；Task role provider 共享 `AgentSupervisor` 生命周期内的 provider+model route-pressure registry，重建 task runner 不会丢失 cooldown，每个 model turn 和 read batch preflight 都检查 cooldown。缺 header 时使用有界指数退避与 route+strike-derived deterministic jitter，单次上限 120 秒；cooling read batch 通过 typed `ProviderRouteCooldownError` 为所有 member 保留 retry-after/route metadata，同时保持零 provider dispatch/零 child Started；不同 route 互不阻塞，已在途请求不取消且 stale success 不能清掉更新的 429。O5b2c 为 shared-read-only Task step 增加 durable bounded retry：真实 429 只有在 child physical-attempt projection 证明 `ConfirmedNoModelConsumption + RateLimited`、zero-output/zero-tool/zero-effect 时才获授权，cooldown preflight 使用零派发 proof；上一 attempt Failed、`TaskParticipantRetryScheduled` 与 step Pending 由 parent 原子追加。schedule 绑定 route、retry-stable input hash、前后 attempt id、`not_before` 和 proof，默认最多 2 次、累计等待最多 120 秒；replacement 使用新 child session/logical run，重启只消费未 Started schedule 一次，输入漂移在 provider dispatch 前失败。已有输出、tool/effect、transport uncertain 或 write step 不自动 retry。O5b2d1 在同一 registry 上加入每个 `provider + model` 独立的 adaptive route window：TUI `[task].max_parallel_read_steps` 是窗口上限，请求在 dispatch 前领取覆盖完整 response stream 的 lease；429 将窗口减半且最小为 1，cooldown 后的成功 completion 按当前窗口大小累计并加 1 恢复。route 饱和只暂停该 route 的新 dispatch，不取消在途 sibling 或阻断其他 route；stream Done、error 和 drop 都释放 lease。窗口/in-flight/恢复进度是 supervisor 生命周期内的运行态，不进入 kernel 公共协议，也不构成 restart authority。O5b2d2 把同一 durable retry authority 扩展到隔离 Planner/Synthesis：physical attempt 必须证明 `ConfirmedNoModelConsumption + RateLimited`，child session 还必须零 assistant/tool/TaskPlan/changeset；已经调用 discovery/task-plan tool 或已经产生文本时 fail closed。failed attempt 和 schedule 原子追加，replacement 使用新 attempt/session/logical run，Planner 与每个 plan version 的 Synthesis 各自最多 retry 2 次、累计等待最多 120 秒，重启只消费未 Started schedule。O5b2d3a 在相同 supervisor 生命周期内投影 provider route/role/in-flight/waiting/cooldown/adaptive window diagnostics；O5b2d3b 让 Task read batch 复用 `AgentCompletionHub`，按 completion arrival 更新 process-local snapshot，parent 在全部到达后继续按 stable request sequence 单写 durable commit。TUI 从 live snapshot 显示 arrival/commit 双序，task boundary 和 task identity filter 防止上一批次污染新运行；两类 snapshot 均不进入 kernel 公共协议或 restart authority。O5b2 最后以同步 `prepare_child_session_batch`、不借用 parent 的 detached future 和 consuming `TaskChildSessionBatchCommitEnvelope` 收窄 trait 边界：child await 完成后 kernel 才重新提供 parent session 执行 one-shot commit；不支持 batch trait 的 runner 走顺序执行。并发审批决策当前用共享 mutex 串行化，child 的实际 approval/tool audit 保存在 child session，parent route summary 到 commit 时再稳定追加。
- O6a 已把相互独立的 `SubagentWrite + ChangesetOnly` ready step 接入第二条有界并发路径；`[task].max_parallel_changeset_steps` 默认 `2`，TUI task runtime 会把 read/changeset 上限的较大值交给 provider route window，同时两类 batch 保持 homogeneous，不能混跑。Coordinator 在整批 admission 前冻结一份共享 immutable base snapshot，并绑定到每个 child request；runtime 复用 prepare / detached future / consuming commit envelope，使 provider request 真并发且不跨 await 借用 parent Session。child 工具面仍由 changeset-only registry 收窄，只能返回结构化 proposal，不能直接修改 parent workspace；parent 在稳定 request order 提交前重新校验 snapshot，drift 时 fail closed，成功时才追加 `ChangeSetProposed`、`IsolatedChangeSetProduced` 和 `MergeReviewRequested`。shared-workspace direct write 继续串行独占；worktree materialization、path confinement、conflict graph、integration refs 与最终 promotion CAS 留给 O6b 以后。
- O6b1 已增加 runtime-private physical Git worktree materializer，但尚未接入 Task 产品路径。它只接受 clean repository root、无 submodule 且 exact parent snapshot 的 workspace；destination 固定落在 canonical Git common directory 下的 owned root，并由 path-safe opaque id 唯一派生。Checkout 后以 manifest 内容证明 child 初始树等于 parent base，同时为 child 生成独立 snapshot id；因此 child verification 不会跨 workspace 继承。Materialization receipt 不可 clone，cleanup 按值消费并只调用 Git 删除 exact owned worktree，不对任意路径执行递归删除。同一 Git common directory 的 `worktree add/remove/prune` 通过 runtime-private cross-process lease 串行管理，防止并行 lane 竞争共享 `.git/worktrees` inventory；lane apply、verification、commit 与 private-ref CAS 继续并行。Append-only ownership、restart inventory、Task child workspace binding、artifact extraction 与 cleanup outcome durability 属于 O6b2。
- O6b2a 已补齐 isolated workspace append-only lifecycle 与 restart inventory。`IsolatedWorkspacePrepared` 必须在 physical materialization 前冻结 workspace/parent/owner/mode/base/backend binding，`IsolatedWorkspaceCreated` 继续表示 ready，`IsolatedWorkspaceCleanupRecorded` 以 removed/already-missing/retained/failed 记录 cleanup outcome。Projection 会把 prepared-only crash window、created workspace、retained/failed cleanup 保留在 cleanup inventory，只有 terminal removed/already-missing 移出；prepared/created duplicate binding 不一致时标记 inconsistent，不能静默覆盖 ownership。该 slice 只建立事实层，Task child binding 与 cleanup 执行由后续 O6b2b 接线。
- O6b2b 已把 physical worktree 接入 Task child 产品路径。Planner schema 接受 `SubagentWrite + Worktree`；kernel 冻结 parent base snapshot，runtime 按 `Prepared -> physical materialization -> Created -> child start` 顺序建立 durable binding。Supervisor 在 provider dispatch 前复核 exact owner、Git backend、active lifecycle、owned-root confinement 和 worktree inventory，tool/permission workspace 都绑定 child root。Child terminal 后从 frozen base commit 提取有界 text diff、file hash 与独立 child snapshot，ref drift、symlink/special file、binary/non-UTF8、unsafe path 或 artifact budget 溢出均 fail closed；parent snapshot 必须保持不变，proposal 再复用既有 merge review。success、failure、cancellation 都消费 cleanup receipt；TUI 启动与 session transition 会从 append-only inventory 重试 crash-window cleanup并记录 removed/already-missing/failed。当前仍要求 clean、无 submodule 的 Git repository root；parallel Worktree batch、conflict graph、integration lane 和 final promotion CAS 属于 O6 后续。
- O6e integration lane 已形成可恢复边界：homogeneous Worktree whole-batch 先冻结同一 clean commit 或 dirty-overlay base，完成全部 owned worktree materialization、Created 与 child Started 后才统一放行 provider。Kernel 以 content-bound effect facts 构建 deterministic conflict graph；runtime 对 clean base 使用 expected-old/new-object CAS 的 managed ref，对 dirty/untracked base 使用 expected snapshot/revision CAS 的 snapshot workspace，不同 lane 的 apply 与 RFC-0003 scoped check 可并发，同 lane 保持 accepted plan 顺序，parent workspace 不在 lane 阶段被修改。`IntegrationLanePrepared/MemberApplied/VerificationLinked/Terminal/CleanupRecorded` 均在后续 effect 前 durable acknowledgement，verification receipt 绑定 exact check spec、scope、backend、network 和 candidate；active/retained overlay artifact 由 isolated-workspace lifecycle retention-pin，恢复只重建 inventory，不重放 apply/check。lane candidate 与最终 promotion target 均为 tagged union；promotion authority、parent mutation/final verification 与 O6g 产品面继续按 RFC-0053 O6f/O6g 实现。
- O6f promotion protocol、物理 substrate、authoritative parent-check gate 与 crash reconciliation 已建立独立安全检查点：`TaskPromotionPreview` 只从全部 terminal-ready、已有 scoped receipt 且 cleanup disposition 明确的 lanes 生成，并绑定 ordered candidates、aggregate diff、单一 target、verification invalidation、intent/policy 与 digest；host-owned `TaskPromotionAuthorityConsumed` 再绑定 expiry 和 single-use nonce，普通 TaskPlan、planner 文本或 tool approval 不能替代。当前唯一启用来源是 exact user integration review；RFC-0005 E05.17 deferred 时 controlled-auto source fail closed。Runtime 会从 exact frozen base 重建一个 aggregate candidate；`WorkspaceApply` 经 RFC-0002 full preflight/mutation batch 修改 parent 且不更新 ref，`GitRefAdvance` 只对 clean、未 checkout、expected-old 匹配的目标执行单次 CAS 且不改用户 worktree。authority consumed 与 Prepared 都需 durable ack，parent/ref drift、checked-out ref、digest/file preflight mismatch 或 ack 拒绝都在首个目标 effect 前 fail closed 并清理 private candidate。promotion 与 parent checks 使用同一份 preview-bound policy scope；GitRef 的 runtime-owned clean checkout 保留到 checks terminal 后再清理。启动和 session switch 对 Prepared-only attempt 执行 zero-forward-effect reconciliation：只用 exact ref、完整 RFC-0002 mutation batch、冻结 policy 与当前 snapshot 补齐可唯一推导的终态，歧义进入 needs-review，绝不重放 merge、check 或 provider。task runner 要求当前 plan version 的所有 integration plans 都产生 `synthesis_ready_attempt` 才启动 Synthesis。
- O6g integration review 产品闭环已完成：changeset-only 与 worktree child 的 exact diff 在离开 child runner 前进入 RFC-0002 内容寻址 artifact lifecycle；全部 physical lanes terminal-ready 后，runtime 从冻结 base 重放 aggregate candidate、持久化 aggregate diff，并返回绑定 task/plan version、ordered lane candidates、单一 promotion target、verification policy/digest 的 `TaskPromotionPreview`，由 kernel single-writer 追加 recovery-critical preview entry。Kernel 只投影当前 task/plan version 的未消费 exact review；TUI detail 从 durable projection 展示 aggregate diff、脱敏 lane provenance、冲突原因与 child/lane/parent verification 分层，review/load/accept 都绑定 request id、task id、plan version 和 preview digest。用户接受后 runtime 会重新构建并校验 candidate、消费单次 authority、执行 promotion 与 authoritative parent verification；只有 `Passed` / `NotApplicable` 才按 exact task id 自动续跑 Synthesis。stale/superseded/late response 无法作用于新状态，private worktree path/ref 不进入产品面。
- O8a 的 verification rerun action 已采用 exact binding：产品投影只从当前最新 plan 生成 action，request id 对 task id、plan version、step、check spec/hash、policy hash 与 workspace snapshot 做内容绑定；kernel 在追加任何 queued lifecycle 前重算 identity，并确认 plan 仍是最新、未 supersede 且仍包含目标 step。TUI、HTTP、Desktop IPC 与 generated contract 传递同一组字段，因此旧 modal、迟到回包或跨 plan 的 rerun 无法作用于新状态。
- O8a 的 TUI product completion 已完成：task、live progress 与 pending follow-up 的 viewport/render/hit-area 复用唯一 live-band 高度计算，已发送 follow-up 不常驻 pending list；versioned session view cache 以 250-step / 752-entry fixture 证明 unchanged frame 不扫描 JSONL 或重放 reducer。`Alt-P` 只从 running task 的最新 accepted plan 生成内容绑定的 `TaskPauseRequest(request_id, task_id, plan_version)`；worker 同时校验 durable task/plan 与 active cancellation scope，auto handoff 继承 root scope 时会收窄为 exact task target。quiescence 成功后 physical run 诚实记录 cancellation，而 durable task 写 `Paused`、active step/child 写 `Interrupted`，后续 `/task continue` 可恢复；planning 无 accepted plan、scope 漂移、stale plan 或迟到 action 均在停止 run 前 fail closed。自动 handoff -> Pause -> Continue -> Completed、长 task cache、三区布局和 worker-to-app session restore 均有回归测试。
- O8c 的 production-path harness 已接入 `ApplicationRunServices`：committed generated corpus 固定为 20 negative / 10 positive，route contract 与 runtime-derived provider/model/config/corpus facts 共同形成 exact route identity，V1 report 独立计算 false-positive、positive miss、majority misroute 与 zero-tolerance invariants。`scripts/run-evals.sh` 会传递 bounded route contract、验证两层 report artifact，并在 deterministic mode 检查 corpus drift 和关键 orchestration gates；CI 另跑真实 TUI PTY fixture campaign。2026-07-25 对 `6432fc5728a6` 的 DeepSeek V4 Flash campaign 完成 90 次 provider admission，但只有 55 次完成；exact route positive miss 为 `77.8%`，并有 33 次 TLS handshake EOF。该证据不合格且已驱动 routing-only typed microturn 修复，修复后的 prompt/tool digest 与旧报告不同。2026-07-26 已补齐 confirmed-pre-dispatch connect retry：DeepSeek 只把 typed reqwest connect-phase error 映射为 provider-neutral rejection，kernel 在前一 physical attempt 已同步确认 zero-generation/zero-output/zero-effect 后，最多用同一冻结 request 新建两个独立 attempt；HTTP status、timeout、stream error 和 transport uncertain 均不重试。queue/task recovery 只接受同 provider/model/purpose/request fingerprint 的有序安全 predecessor chain。该历史失败只保留为诊断证据；每个 release route 仍必须由最终 exact-build 的 `auto + proactive` 30×3 report 独立 qualification。
- O8d 使用 release-owned sidecar 激活新安装默认，而不是修改 Rust schema 默认：exact candidate binary 从完整 qualified report 生成 path-free `sigil-orchestration-rollout-v1.json`，并重新验证 commit/build、route identity、task-config digest、30×3 repetition、阈值与零容忍不变量。Quick Setup 只对缺少配置且 exact provider/model/官方 endpoint/build/digest 匹配的 route 写入 `auto + proactive`；其他 route、custom endpoint、sidecar 缺失/损坏/过期以及所有已有配置保持 `manual + explicit_request_only`。route-local kill switch 在 typed handoff 与 exact spawn admission 前从 durable facts 触发，并同时降级 routing 与 proactive spawn；accepted TaskPlan recovery 和 Task history 保留。Doctor 投影 release qualification 与 session report handle；archive/npm/Homebrew 只把由同一 binary 验证生成的 sidecar 安装在 binary 旁。
- 主会话 running-input queue 是内部 durable control plane，使用 `ConversationInputQueued`、`ConversationInputEdited`、`ConversationInputReordered`、`ConversationInputStatusChanged`、`ConversationInputQueueControl` 和 recovery-critical `ConversationInputPromoted` append-only entry 持久化；TUI 产品层把它呈现为 visible follow-up，而不是暴露为隐藏队列。普通 chat 在 active run busy 时会显示为 follow-up，不提前写入 provider-visible user history；busy 状态下的 agent mention 不会静默降级成 main-thread chat，而是保留输入并提示用户等待或使用专门的 agent message 入口。普通输入位于 child agent view 时同样 fail-closed：即使 child projection 暂时缺失也不得 fallback 为 main-thread queue，agent 消息必须走已有的 typed `MessageAgent` 路径。worker 在当前 turn 结束后先冻结 exact request；可证明压力时先应用 portable compaction，再以 queue-revision CAS promotion、safe user/capability commit 和 provider physical-attempt Started 为唯一 send barrier，并把 promotion 的 `dispatch_run_id` 原样作为该首次 physical attempt 的 logical run id。没有本地 admission 时仍可发送同一冻结请求，但不猜测 provider token limit 或启用未证明 compaction。恢复或 run error 只根据同一 logical run 的 durable physical-attempt evidence 分类：已完成、已有输出或副作用后的终态写 `Delivered`；已确认未消费写 `Rejected` 并 pause queue；缺 terminal、传输结果不确定、interrupted、缺失或多个匹配 attempt 一律写 `Stale`，不自动重放远端请求。`/queue next` 只调整顺序等待下一 turn；`/queue interrupt` 先走 cancel/interrupted audit，再 dispatch 选中 item；`now` 与 `send-now` 不再解析。
- TUI follow-up 行与四个 action 都有真实鼠标命中区；键盘 `Tab -> Enter` 必须经 launcher 到达 worker command channel。queue 首项默认就是 next dispatchable，并在 optimistic entry 创建时记录 deferred promote intent；用户在 durable id 返回前再次选择 `Run next` 也保留同一 intent 和明确反馈，而不是以“已经排在第一位”为由吞掉 action。durable id 返回后补发 promote，重排或恢复 paused queue 后同时重新唤醒 conversation queue 与 TaskGuidance advancement；recovery task handoff 不得抢在已经 next-dispatchable 的 main follow-up 前启动。queue item 离开 `Queued` 后清理对应 edit buffer；active target 变化导致当前 queue strip 不再可见时同步清除 queue focus/selection，让键盘立即回到 composer。高度不超过 14 行的终端使用三行 composer，使 live strip 收缩时立即把空间归还 transcript。普通配置面板的单模型上下文窗口使用 `automatic / 64K / 128K / 256K / 1M` 预设循环；已有 custom value 在用户主动切换前保持原值，精确自定义继续由配置文件承载。
- TUI live status band 的估高、render 与 transcript viewport 共享实际 frame/inner width，不再使用固定 80 列或重复扣减 padding。queue、progress、plan、task 行先按 display width 截断并禁用二次自动换行；plan approval 最多占 12 行并按宽度缓存 Markdown 行数投影；短窗口先保留实际接管 Enter 的 focused plan/queue/verification 决策行，再分配 progress、task 与详情，截断时显示明确提示。egress disclosure 的五行也进入同一 shell 容量预算：空间足够时披露与当前 action 同时可见，空间不足时不得把未渲染的披露标记为已展示。PTY acceptance 必须确认首帧前进入 `?1049h` alternate screen、启动期间零 CPR、`tcsetwinsize + SIGWINCH` 真实 resize 后整帧仍可重排、输出中不存在单行 DECSTBM fast path 且 application transcript 不进入 native history；还要用 Ctrl-Home/Ctrl-End 验证应用内历史锚点，并确认 `?1049l` 发生在 resume hint 或 fatal error 输出之前。测试 VT emulator 必须区分 primary/alternate buffer，不能用 inline scrollback 假设自证。
- Background child result completion 与 follow-up / internal queue 有明确优先级：`join_before_final` / blocking child 完成后优先触发 parent continuation；普通 non-blocking background child 完成只写 `AgentResultContinuation(Pending)` ready 状态。当主会话已经有 pending follow-up 时，non-blocking result 不抢占 queued input，只以 bounded transient system notice 提醒模型可按需 `wait_agent` / `read_agent_result`。
- `/agent close <child-id|current>` 不再由 TUI 直接追加 `AgentThreadClosed`；TUI 只解析目标并发送 worker `CloseAgent`，worker 通过 runtime `close_agent_thread` 复用 model-visible `close_agent` 的 terminal 校验和 control entry 生成，再把同步后的 session entries 返回给 TUI。`/agent cancel <child-id|current>` 解析 running target 后发送 worker `CancelAgent`，background child 的独立 cancellation owner 会先 durable 记录 request，再 cancel+join；quiescence 成功写 `AgentThreadStatusChanged(Cancelled)`，超时写 `Interrupted`/cleanup-incomplete，并统一追加 `AgentRunInterrupted`。
- `list_agents`、`message_agent`、`cancel_agent` 和 `close_agent` 已作为 agent coordination tools 注册。`list_agents` 返回所有 agent thread 的 status、objective、result ref、messageable/closable/cancelable 与 approval_pending；`message_agent` 用于给 active background child mailbox 投递 follow-up，记录 `AgentThreadMessageRouted` requested -> resolved/rejected 审计，tool result 明确返回 `delivered_to_mailbox`、`will_apply_after_current_turn`、`interrupt_requested=false` 和 `interrupts_in_flight_provider_stream=false`，语义是 next safe point steering，不承诺 mid-token 或正在执行 tool 时实时中断；`cancel_agent` 只取消仍有 live handle 的 running background child；terminal child、无 mailbox 或无效目标会返回 rejected/unsupported，且不改变 child lifecycle terminal status。
- Subagent tool approval 与 MCP elicitation 会在 parent session 记录 route summary；真实工具审批、工具执行和 elicitation 决策仍按原有 control entry 机制审计。
- O7 approval routing 会为每次 Ask 计算包含原始参数、安全 preview、tool access/network facet 与完整 permission policy 的签名，并把 task、exact parallel batch、thread、participant attempt、agent attempt、tool call、workspace 和 isolation 写入 parent route。当前交互式工具审批使用 no-expiry sentinel，不以 300 秒 wall-clock deadline 终结；只由显式决定、取消、presenter/通道失败或 run/session shutdown 收口。并行 participant 只有在这些 facet 完全一致时共用一次 presenter decision，每个 child 仍独立记录 requested/resolved 证据；presenter 失败或取消会释放所有 follower。没有交互 owner 的 background child 进入 `Blocked` 并保留 exact route，restart 不复用旧 decision，后续新 attempt 必须重新生成 preview。
- 普通 tool error 是 agent loop 的可恢复输入；如果 step 最终产出回答，task orchestrator 继续后续步骤，并把恢复过的错误写入 step reason。审批拒绝、权限类错误、interrupted tool call 和 max turns 仍会阻断 task。
- Role-specific provider、reasoning effort 和 tool scope 由 `sigil-runtime` 装配；planner 与 subagent-read 默认只读，executor 默认完整工具面，subagent-write 受 `[task].allow_write_subagents` 控制。
- `sigil-runtime::AgentProfileRegistry` 已把内置 role 投影为 profile，并通过 `AgentInvocationPolicy`（`manual_only` / `model_allowed` / `system_only`）和 `AgentResultPolicy`（`summary_only` / `summary_with_page_ref` / `artifact_only` / `foreground_merge_required`）表达调用与结果返回语义。当前 durable profile 必须同时携带 `invocation_policy`、`user_invocable` 与 `model_invocable`；缺字段的 profile/session 直接拒绝，不反推；model-visible agent index 只暴露 trusted、enabled、scope-contained 且 `model_allowed` 的 profile，并把 `result_policy` 纳入 fingerprint 和 `spawn_agent` 描述。内置 `worker` 现在是 `ModelAllowed` 的 `SubagentWrite` profile，但只通过 changeset-only foreground 隔离运行：foreground / join-before-final worker 必须返回结构化 changeset proposal，parent workspace 被 child 直接修改会失败，成功时追加 `ChangeSetProposed`、`IsolatedChangeSetProduced` 和 `MergeReviewRequested`；background worker spawn 仍返回 `unsupported_write_background_without_isolation`。runtime worker 使用 workspace-aware registry，已支持从固定 workspace `.sigil/agents` 发现 Sigil-native workspace profiles：`.sigil/agents/<id>/agent.toml` 或 `.sigil/agents/<id>/AGENT.md`。Native profiles 默认 enabled、manual-only、needs-review、read-only，只有显式 trusted 且 model_allowed 后才进入 model-visible index；`AgentProfileTrustDecision` append-only control entry 会通过 `AgentProfileTrustProjection` 覆盖非 system profile 的 trust 状态，TUI worker 的 agent tools 注册面和 runtime supervisor 都使用 session-aware registry，因此 source/profile hash 变化后旧 trust decision 会失效并回到 `needs_review`，默认退出 model-visible index；duplicate built-in/profile id 会 warning 并跳过，alias/slash name 冲突会 deterministic warning 并禁用冲突别名，symlink escape 会 warning 并跳过。Skill discovery 同时支持固定 workspace `.sigil/commands/*.md`，默认发现为 user-invocable inline command，并通过 `/command-id` 走 trusted inline skill invocation。默认兼容发现还会读取 `.agents/skills`、Codex `.codex/agents/*.toml`、OpenCode `.opencode/{skills,commands,agents}` 和 Claude Code `.claude/{skills,commands,agents}`；只有显式设置 `compatibility_auto_discover = false` 才关闭默认集合，Reasonix 仍需显式加入。优先级为 Sigil-native / plugin、Codex、OpenCode、Claude、Reasonix，跨工具同名条目按稳定 ID 去重。兼容资源属于工作区内容并继承工作区信任，不再经过逐条 trust projection；外来 agent 不导入 provider、sandbox 或 permission 私有语义，默认映射为 trusted、manual-only、read-only profile，仍受工作区信任、runtime tool scope 和 permission policy 约束。兼容 command 在 application composer 中以 `/名称` 暴露，agent 以 `@名称` 暴露；child-session agent 只进入 agent catalog，不重复出现在 skill catalog。已知外来 tool 名只映射到 Sigil 等价工具，未知名称丢弃。同一 registry 会把 skill discovery 中 `run_as=child_session` 的兼容条目投影为 subagent profiles；`disable-model-invocation` / `disableModelInvocation` 会映射为 manual-only，`allowed-tools` / `allowedTools` 只能收窄工具面，包含 `disallowed-tools` / `disallowedTools` 的条目因 subtractive scope 不能安全表达为 profile 会 warning 并跳过。受信任 plugin manifest 可通过 `[[agents]]` 贡献 agent profile；未 trust plugin 只在 config 中展示 capability，不注册 runtime profile，已 trust 且 hash 匹配时才生成 `AgentProfileSource::Plugin` profile，并用 namespaced id 避免与 workspace/native profile 裸 id 冲突。spawn 时 profile tool scope 会与 role registry scope 取交集，profile 不能扩大角色原本的工具面；profile description/instructions 会作为 transient child system prompt 注入子会话，不持久化进 parent history。
- Native workspace agent profile 支持 OpenCode-style `permission`：TOML `agent.toml` 支持 `[permission]` shorthand / per-key table 和 `[permission.commands] allow/ask/deny`，Markdown `AGENT.md` frontmatter 支持 nested `permission:` map。Runtime 会先复制 root `PermissionConfig`，再追加 agent permission rules；kernel permission evaluation 对 tool/path rule 保留最后匹配生效，对 command group 采用 `deny > ask > allow`，command `allow` 只放宽默认 shell ask，不覆盖显式 tool/rule ask 或 deny。`tool_scope` / `allowed_tools` 只收窄工具可见性，不授予权限；root `read-only` mode、protected path、destructive operation、external-directory gate 和 foreground changeset-only write isolation 仍是硬安全边界。
- `AgentProfilePolicyDecision` append-only control entry 已用于非 system profile 的 effective policy overlay，覆盖 `enabled` / `user_invocable` / `model_invocable`。policy replay 需要 profile id、source、source hash、profile hash 全部匹配当前 snapshot；hash 变化后旧 policy 失效。runtime `model_visible_index`、`AgentToolRuntime::resolve_spawn_profile` 和 `AgentSupervisor::begin_chat_child_thread` 使用 effective policy 过滤，但 overlay 不修改源 `AgentProfile`，因此不会污染 snapshot hash。
- TUI `/config` 的 `Agents` section 已改用 workspace-aware `AgentProfileRegistry`，展示 built-in、native、compatibility profiles 的 source/kind/trust/effective enabled/user/model、provider/model、tool scope 和 nickname candidates；footer trust/block/enable/user/model actions 会追加 `AgentProfileCaptured` 与对应 trust/policy decision 到当前 session JSONL。普通 inline/reusable skill 留在 `Skills` section，并继续通过 footer load/invoke 生成受 runtime `load_skill` policy 约束的请求；slash selector 的 skill fallback 同步限定为 trusted inline skills，`run_as=child_session` 兼容资源不再作为普通 skill slash row 展示或通过 `/skill-id` 解析启动。Composer 起始 `@` 会打开 agent mention selector，候选只来自 enabled、trusted、user-invocable 的 session-aware profiles；提交 `@profile <prompt>` 会走 TUI worker `InvokeAgentProfile` 和 runtime `AgentToolRuntime::invoke_agent_profile`，以 `AgentInvocationSource::Mention` 启动 foreground child thread，并按 user-invocable policy 校验，而不是把 mention 当普通 chat prompt 交给 delegation hard-gate。
- `/config` agent detail 会展示 permission 摘要和 write policy 摘要，包括 mode、command pattern count、tool override count、rule count、external-directory 状态以及 write-capable profile 是否只能通过 changeset-only foreground merge 写入；复杂 rule 编辑仍留给配置文件，不进入默认 footer action。

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

`sigil` 必须支持 compaction，但 compaction 只能作为“受控的稀有 cache epoch rotation”，不能退化成普通 agent 常见的随手改写历史。当前且唯一策略是
`compaction.strategy = "cache_aware_v3"`。`soft_threshold_ratio`、`hard_threshold_ratio`、
`tail_messages` 和其他非当前字段会使配置校验失败，不做翻译、默认值补齐或回滚读取。

V3 request projection 固定为 `ProviderStatic -> SessionAnchor -> ContinuityCheckpoint ->
VerbatimTail -> DynamicOverlay`。正常 turn 只追加 active tail；epoch rotation 才创建新的稳定前缀：

- `SessionAnchorV1` 只投影 accepted Intent、Task 与 user-control authority，保留 active exact source
  spans，不无条件 pin 第一条用户消息；
- `ConversationContinuityV2` 每轮从 durable truth、上一个 active ledger 与本次 delta 重建，所有
  decision/progress/pending/artifact/verification/risk 都必须有 durable source ref；
- 正常 semantic rotation 使用当前同一 provider/model route 额外发起一次 bounded LLM 摘要请求：
  旧 epoch request 保持为 cache-stable 前缀，只追加 strict JSON instruction 和 closed source index；
  不创建子 agent/session，不执行 client tool，不开放 hosted tool。输出只能成为
  `ModelGeneratedUnverified` narrative，objective/constraint/authorization/completion/verification
  继续由 deterministic durable projection 决定；
- tail 按 token target 选择完整 turn，user/assistant/tool pair、approval、queued input 与 active turn
  保持原子；大型 tool output 先形成可恢复 shrink candidate，当前 epoch 的旧 bytes 不原地改写；
- automatic trigger 是 `fit required OR trusted expected cost wins`。forecast 扣除 output、tool growth、
  provider state 与 safety error，同时核算 cache read/write/miss、首轮 reset 与 break-even turns；
- 无可信价格或 forecast confidence 不足时，cost-only automatic fail closed；manual `/compact`
  直接完成零 provider I/O 的 local prepare，然后生成、校验并原子激活一次 full semantic
  compaction；命令本身就是用户授权，不再出现额外确认。相同 cursor/layout 不重复尝试，连续失败和
  post-rotation emergency 由 durable circuit breaker 截止；
- 首版在摘要调用前的 exact upper-bound economics gate 闭合前，idle cost-only automatic 一律
  fail closed，避免为了判断是否省钱而先产生一次确定账单；fit-required/pre-turn/overflow 不受此限制；
- 只有具备 exact portable proof profile 且 adapter 在受信 official route 上声明有效 cache capability
  时才启用 automatic V3；custom/compatible/未知 route 没有足够证明时直接不可用。

TUI、serve 与 Desktop 消费同一个两阶段 typed preview。`prepared` 表示 local plan 已完成且没有
provider consumption；`ready` 表示摘要调用已经发生、actual usage 与 exact target admission 已通过但
尚未 activation。两阶段共同展示 strategy、pressure phase、forecast confidence、admission reason、
预计 savings/break-even 与 native carrier availability；只有 `ready`
展示摘要调用实际 cache-read/uncached/output/cost。standalone tool-output shrink 只消费
`prepared` plan，追加独立 context epoch sidecar，不创建 semantic checkpoint。
原始 JSONL 永远不覆盖；manual/automatic activation 都通过同一 portable lifecycle、stale-frontier CAS
和 continuity validation。

摘要请求以 `ProviderPhysicalAttemptPurpose::SemanticCompaction` 单独审计，usage 计入 session 总成本，
但不覆盖最近正常 conversation generation 的 cache 观测。manual 与 idle/cost-only 路径摘要失败时
不静默应用空 narrative；只有 pre-turn fit-required 或 overflow emergency 可以追加明确的
`semantic_compaction_deterministic_emergency_fallback` 后使用确定性 continuity floor。

provider-native compaction 只是可丢弃加速层。OpenAI Responses/Anthropic carrier 的 schema、
加密 materialization、exact-route validation 与 portable fallback 已由 provider/kernel 拥有，但当前
产品路径保持 fail-closed：`compaction.native_carrier_enabled` 默认关闭，且即使显式设为 `true`，
在同 cursor carrier 能被下一次请求实际消费的 resume contract 落地前也不会发起额外 provider 请求。
未来启用时必须先完成 portable activation，再绑定 connection fingerprint、model snapshot、protocol、
source cursor、store/retention/expiry 与 protected payload；carrier 丢失、过期、route/model 切换或
policy 不兼容时追加失效审计并直接从 portable checkpoint 组装请求，不调用模型修复记忆。native
失败只能降级为 notice，不能回滚或污染 portable truth。

发生 context-window 拒绝也不能仅凭 HTTP status 或错误文本自动重试。唯一启用的 overflow recovery 是官方 OpenAI Responses `https://api.openai.com/v1` 上固定 `gpt-4.1-2025-04-14` snapshot：同一 foreground logical run 必须恰好一条 `context_window_exceeded + ConfirmedNoModelConsumption` durable terminal 且没有 output/side-effect refs；随后对同一冻结 post-compaction target 调用官方 `/responses/input_tokens`，并以该计数、显式 32K output reservation 与 8K safety buffer 完成完整 fit proof。计数本身先写同步、non-generating 的 `InputTokenMeasurement` start/terminal，成功后才允许新的 portable lifecycle；TUI 先刷新 lifecycle，再把仅存在进程内的一个冻结 target 交给一次新的 conversation attempt。alias、兼容 endpoint、普通错误、计数失败、profile drift、多个 physical attempt、任何 crash/restart 均 fail closed，不重发计数、不 apply、不 replay conversation；恢复后的 run 也不具备递归恢复资格。DeepSeek V4 Flash 仍没有本项 provider rejection contract，因此不进入 overflow path。

模型切换不是 mid-turn recovery：busy 状态下 `/model` 直接拒绝，不改变 route 或运行中的请求；idle
切换在同一个 durable session 中追加完整 `connection_id/model_id` 与 secret-free
`ResolvedModelRoute` 的 `SessionModelSelected` 控制事件，从下一次运行生效。该事件是
provider-native continuation/cache 的隔离边界，边界前 material 不得被新 route 复用；Desktop 与 TUI
重建 provider worker 时保持原 session id、对话历史和任务状态。每次实际 provider attempt 继续记录精确
provider/model。`/model` 不隐式修改 saved default；`/config` 的 provider 保存操作则把用户选中的
route 同时用作 saved default 与当前 session 的后续 route，减少设置页中的双重确认。

除此之外，还应加一条和成本直接相关的策略：

- 大型 V2 tool result 在 current/recent/high-signal 保留窗口之外，先由 deterministic batch aging
  切换下一个 context epoch，只保留 status/facts、hash、bounded preview 和 opaque artifact ref；
  如果以后还要精读，让模型使用 `read_tool_artifact` 的 bounded typed selector
- cost-only aging 只在 cache-reset economics admission 通过后激活；fit-required aging 在
  semantic compaction 前尝试。manual “只整理工具输出”同样使用 active incremental pressure
  projection 与 exact-frontier CAS，不读取整份 JSONL，也不生成 fake transcript artifact ref
- repeated artifact page 在同一 run 内返回 `unchanged` receipt，不把相同正文再次永久写入 JSONL 或
  无限复制到 model context
- 新 session 只追加 `ToolResultRecordedV2`；pre-V2 `tool_result_recorded` 仅作为 parser sentinel
  返回 `Unavailable` compatibility diagnostic，不能进入 provider/TUI projection，也不能
  生成 `DurableTranscriptEvent` fake artifact ref

### 10.4 Auto Memory

文档型 memory 仍由 `[memory].enabled` 控制并进入稳定 prefix。另有一条显式、默认启用且可退出的可写
memory vertical slice，由 `[memory].writable` 控制：

- `remember_user_preference` 保存跨 workspace 的稳定交互或工作流偏好；
- `remember_project_fact` 保存 canonical current workspace 范围内的 user-asserted 项目事实或约定；
- 模型根据用户语义和工具描述自行判断是否需要写入，kernel/runtime 不以关键词匹配 prompt；
- automatic routing microturn 在 writable memory 可用时冻结同一份 canonical route + remember tool
  surface；一个 response 必须恰好有一个 route decision，但可以额外携带语义明确的 remember call。
  Remember 仍走普通 permission plan、preview、approval、execution audit 与 durable receipt 管线，并在
  plan/task handoff 发生前 settle；即使 provider 先声明 route call，host 也必须先执行并持久化所有合法
  remember call，再记录 route decision，同时仍按 provider 原始 declaration order 写回 tool result batch；
  dynamic、frozen、queued 与 route fingerprint 必须消费同一 surface；
- 两类写入都需要 preview/approval，只有 sidecar 原子发布且 ref-only journal durable sync 完成后才返回
  包含 scope、memory id 和 version 的 durable receipt；没有成功回执就不能声称已经长期记住；
- `[memory].writable = false` 时 system contract 明确要求模型只能承诺当前会话保留；
- active memory 通过 Context V1 dynamic suffix 召回，不进入稳定 prefix；user preference 为
  user-private、project fact 为 repository sensitivity，疑似 secret/credential 在落盘前拒绝；
- `inspect_memory` 提供 active entry 与 admission provenance，`forget_memory` 先 tombstone，再物理删除
  Sigil-controlled sidecar；V1 不把 pre-pack retrieval candidate 误记成 provider injection，也不声称能够
  撤回历史 provider egress 或独立审计证据。

该 vertical slice 只覆盖显式 `user_asserted` V1，不等同于 RFC-0010 P10 的自动候选提炼、
evidence-backed promotion、supersede/invalidate、branch/snapshot validity 和完整 TUI lineage 管理。

## 11. MCP 插件模型

MCP 是 `sigil` 的核心差异化能力之一，应该尽早落地。

MCP 设计不应只覆盖 tools/prompts/resources，还应直接对齐当前规范里已经值得落地的 client features。

### 11.1 支持的传输

建议阶段顺序：

1. 先做 `stdio`
2. 再做 streamable HTTP
3. SSE transport 只有在现实需求出现后再考虑

E21.17 已把 Streamable HTTP 协议核心接入用户根 flat tagged MCP config、lazy/explicit activation 与 TUI/CLI eager startup。每个HTTP message都重新消费runtime提供的durable authorization/disclosure/budget attempt，再经过共享destination guard，使用fresh no-pool/no-retry client；session只在initialize response与独立initialized 202空body均验证后commit。header/env/TLS预检通过不可序列化`PreparedMcpStreamableHttpHeaders`在authorization/DNS前完成，resolved secret HMAC与profile/config/proxy fingerprint逐attempt绑定。JSON/SSE、pagination、schema、CallToolResult、canonical workspace roots、form-only elicitation、typed status与cancel均维持bounded/fail-closed边界。

[RFC-0040](rfcs/0040-mcp-production-reliability-oauth-v1.md) 已单独解锁 user-root Streamable HTTP 的 Authorization Code + PKCE：先修复 remote lazy activation/refresh 与 stdio process ownership，再实现 RFC 9728 / RFC 8414 discovery、RFC 8707 resource binding、keyring-only credential lifecycle 和显式 TUI auth modal。OAuth metadata/token/revoke 的每个 destination 仍必须独立消费 durable egress authorization、budget 与 shared destination guard；401 或已发送 request 的歧义不允许透明 refresh/retry。stdio/plugin/SSE transport OAuth、URL elicitation、sampling 与 tasks 继续排除。

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

当前 MCP 实现同时支持 stdio 与用户根配置声明的 Streamable HTTP transport。两者都覆盖 `initialize`、`tools/list`、`tools/call`、provider-visible 名称清洗/截断/hash 去重、read-only `resources/list` / `resources/read`、read-only `prompts/list` / `prompts/get`；stdio transport 还会在等待响应时处理 server 发来的反向请求：

- `roots/list` 返回入口层已解析的 workspace root，runtime 必须把 TUI / CLI 的 effective workspace root 传入 MCP 注册流程
- `notifications/progress` 映射到 TUI live panel，不写重复 timeline，避免远端 server 用 progress 刷爆用户界面
- `notifications/tools|resources|prompts/list_changed` 标记 server stale，并在 worker 空闲边界刷新该 server 的 provider-visible tool surface
- `elicitation/create` 已由可插拔 client handler 承载：TUI runtime 声明 `elicitation` capability，并通过 modal 让用户 accept / decline / cancel flat primitive object 字段；非交互默认 handler 返回明确 unsupported JSON-RPC error，不伪造用户输入，也不让请求挂死。TUI elicitation decision 会写入 append-only `ControlEntry::McpElicitation`，只记录 server、请求 message/schema hash、字段名和 action，不保存用户输入值。
- MCP tool/resource/prompt 输出必须先本地脱敏，再按 32 KiB/2,000 lines 限额截断，并在 `ToolResultMeta` 中保留 truncation 与 MCP server/tool/trust/operation metadata；截断只使用结构化 metadata，不向文本插入可能命中 secret carrier 的 marker。已经通过 `resources/read` 取得的 bounded text 只有在调用侧显式交给 runtime MCP resource context adapter 后才会成为 `McpResource` Context V1 candidate；adapter 会再次执行 MIME filter、size cap、egress decision 和 packer 校验，不能绕过 permission / egress 直接改写 request。

stdio transport 按声明的 MCP `2025-06-18` 使用 newline-delimited JSON，不再接受未声明的 LSP `Content-Length` framing。每个 inbound envelope 必须是单个 JSON object，batch array 直接拒绝；server request/notification 的 `id` 仅接受 string/integer、`params` 仅接受 object，success response 的 `result` 必须是 object。单个 inbound/outbound frame 硬限制为 4 MiB，单次 operation 最多处理 256 个 inbound message、累计 8 MiB；outbound frame 在完整 bounded encode 成功前不会写入 pipe。CRLF compatibility 只允许硬上限后的唯一候选 `\r`，任何其他 cap+1 byte 都立即返回 `frame_too_large`，不能继续占用 deadline。`initialize`、`initialized` 与首次 `tools/list` 共用一个 startup absolute deadline，tool/resource/prompt 则从等待串行 connection lock 前开始消费 `ToolContext.timeout_secs`，覆盖 write、flush、frame read 与 inbound handler；零值使用有限的 30 秒安全默认，不表示无限等待，非零值最高限制为 24 小时，极端 `u64` 输入不能造成 deadline overflow/panic。frame/message/cumulative/stderr limit 统一投影为带 limit 与 observed lower bound 的结构化 `resource_limit`。

timeout、framing/EOF、unexpected response id、message budget 或 MCP stderr 8 MiB hard limit 会由同一个 first-winner `Ready -> Closing -> Closed` owner 原子发布 reason/typed cause，关闭 stdin、尝试终止 process group/tree，并回收直接 child；loser 不能覆盖首因，清理不完整时必须明确报告。Windows healthy teardown 由 kill-on-close Job Object 持有并终止完整 stdio MCP process tree；若 leader 在 teardown 前已消失则保持 tree cleanup unconfirmed 的真实证据。旧 `Arc<McpClient>` 随后在写任何 bytes 前 fail-fast；恢复必须通过既有 activation/refresh 流程重新执行 process binding、environment binding、pin 和 lifecycle scan，创建新的 process generation，不能在旧 stream 上自动重放请求。tool lifecycle owner 使用 provider-neutral 的 exact raw server scope + unique generation id，refresh 不依赖 sanitize/truncate/hash 后的 provider name；显式 activation/refresh 对 optional server 同样 strict，replacement 失败时恢复旧 generation，旧 generation shutdown 失败时只按 replacement exact owner 回滚新 generation。多 server registration 失败会回滚本次已注册 generation，duplicate exact server name 在 launch 前 fail-fast，generation shutdown 会尝试所有 distinct owner 后再聚合错误。stderr 只保留 64 KiB head/tail 与 total/truncation evidence，hard-limit/reader-failure 以 typed cause 投影，raw full stderr 不进入错误或持久状态。

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
    pub transport_fingerprint: String,
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
}
```

当前默认策略：

- `Official`：可降低 friction，但仍保留敏感调用审批能力
- `SelfHosted`：默认 `approval_default = Ask`、`egress_logging = true`、`allow_secrets = false`
- `ThirdParty`：建议逐次审批、记录出境数据、默认不透传高敏凭据

当前 `approval_default` 已参与逐调用 permission decision；`egress_logging` 已写入安全出境摘要；`allow_secrets = false` 已阻断 MCP tool/resource/prompt args、`roots/list` payload 和 elicitation response 中的已解析 secret。`pin_version = true` 采用两阶段校验：带 `inherit_env` 的 server 在 spawn 前绑定 command/args 文本、grant name、canonical execution base，以及隔离 baseline `PATH` 解析出的 executable canonical path/content digest；缺少 pinned identity 或静态 fingerprint 不匹配时直接失败且不向子进程注入 grant。静态部分匹配后才 initialize，并校验 protocol version、server name 和 server version。该 binding 不解释 interpreter args，也不证明 args 所引用脚本的内容；path-based spawn 继续遵循 RFC-0005 的可信本机/无恶意同用户并发前提，不宣称为 handle-bound host attestation。未启用 `inherit_env` 的既有 command fingerprint 保持兼容。lazy activation 会把获批的 exact process subject 带到 launcher，审批后 binding 漂移会在 spawn 前拒绝；每个 MCP client 会把 resolved grant value 合入自己的 redactor，且以 `ExtensionProcessLifecycleRecorded` 独立记录 clean/pre-spawn/post-spawn 结果而不污染 workspace dirty verdict。Secret redactor 使用缓存的 multi-pattern longest-match 与 immutable input mask，Bearer/assignment 不重扫 replacement；carrier count/总字节和输出增长都有硬预算，预算或安全 marker 无法满足时整体 fail-closed，truncated head/tail overlap 使用受 carrier 总预算约束的线性匹配。resources/prompts 协议入口复用同一 secret egress gate，且不会自动注入 system prompt；MCP resource context 进入 Context V1 时仍必须携带 egress decision，否则只写 `ExcludedEgressDenied` provenance，不渲染 snippet。

MCP 配置在 runtime 注册前还会提升为不可序列化的
`ResolvedMcpServerDeclaration`。它同时保留 declared/effective name、
`UserRoot | PluginManifest | BuiltinReleaseProfile` origin、execution-base kind 与 plugin
manifest attestation；collision namespacing 只改变 effective name。Plugin declaration 在
process subject/static pin 校验前以及真正 spawn 前各重读一次 canonical manifest，并复核
manifest hash、version、capability digest 和最新 trust decision。Plugin launcher 不缓存
discovery-time trust snapshot：两次校验都通过 read-through trust source 重放当前 append-only
session projection，因此在 approval/lazy activation 与 spawn 之间 disable 或重新 review 会
fail closed 且保持 zero-spawn。用户根 stdio 固定使用
canonical workspace root，plugin stdio 固定使用 canonical plugin root；bare command 走隔离
baseline `PATH`，带路径分隔符的 relative command 必须在 execution base 内 canonicalize 且拒绝
symlink escape，absolute command 保留既有 trust/pin 路径，args 始终原样交给 child。只有
`McpExecutionBase::None` 禁止 stdio，Builtin origin 本身不等价于 `None`。process approval
binding 使用进程随机 key 的 HMAC 绑定 exact origin/base/path/command/args/executable；可持久化的
declaration stable pin 只绑定 safe projection fingerprint 与 executable content digest，不把
canonical path 或低熵 command/args 放进普通 SHA-256。post-start tool/resource/prompt permission
subject 优先使用该 exact authorization HMAC，session grant 比较也使用完整 binding，不能只按稳定
trust-class normalized name 复用。durable lifecycle、Doctor 与产品面
只保存 declared/effective name、origin kind、base kind 和 safe projection fingerprint，不保存
canonical path、command/args 或其可离线猜测的普通 digest；pre-spawn trust/attestation 拒绝即使
没有 process receipt，也会从 declaration carrier 写入同一组 safe lifecycle identity。

该 carrier 与 fallible registry API 的完成不代表 plugin MCP 已自动并入 active product
startup/refresh。RFC-0002/RFC-0003 仍明确要求 plugin-owned 长生命周期进程先接入同一
external-process unknown-dirty recorder 与对应产品 lifecycle gate；在该 gate 原子完成前，
常规 TUI/CLI startup/refresh 继续只消费 user-root MCP。E21.13 的 plugin 路径通过显式
declaration registry E2E 验证 origin/attestation/base/receipt 边界，供后续切片安全接线，
不会借本切片静默扩大自动执行面。

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
- TUI `/config` 的 MCP section 提供 Theme 风格的 `Server` 选择行；`Enter` 循环切换当前查看对象且不修改配置，`Down` 进入 footer 后由 `activate` action 对当前项执行 activate/refresh，`PageUp/PageDown` 保留为选择兼容别名；worker 空闲时可对已保存的 lazy server 执行 activation，并把真实工具加入当前 agent registry，运行中 activation 会被拒绝；模型也可通过 `mcp_activate_server` 工具按需启动指定 lazy server，成功后下一轮 request 会看到真实 MCP tools；eager MCP 启动失败或超时时只更新对应 server 的 `failed` lifecycle，不阻断普通 chat、`/plan` 或内置工具；lifecycle summary 会展示 `deferred`、`activating`、`ready` 或 `failed` 运行态
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
- `ToolAccess` 只表达本地 `Read / Write / Execute`；网络单独使用 `NetworkEffect::Read / Mutate / Unknown` 与 `NetworkPolicy::Allow / Ask / Deny`
- 本地 `Write / Execute` 由 `permission.mode` 决定：`manual` 默认 ask，`auto-edit` 只自动允许 workspace 文件编辑，`read-only` 拒绝本地写入/执行。`danger-full-access` 是明确的非交互模式：动态执行、高风险/破坏性 effect floor 与 deterministic analysis incomplete 仍提高 risk、保留 snapshot/confirmation metadata 和审计 reason，但最终所有 local/network/source/external-directory `Ask` facet 都归一化为 `Allow`，不会再产生 approval 请求；credential/protected target hard deny、managed/explicit Deny、disabled external-directory Deny 与 circuit breaker 仍然生效
- `NetworkEffect::Read` 始终服从独立 NetworkPolicy；`read-only` 下的 `Mutate / Unknown` 直接拒绝，Manual/AutoEdit 与 network/source policy 按 `Deny > Ask > Allow` 求交。`NetworkEndpoint` 不得因 `ToolSubjectScope::External` 被误送入只属于 `Path` 的 external-directory gate。`danger-full-access` 将 network/source Ask 视为用户已在运行模式层明确授权，但不能覆盖任何 Deny；交互用户显式创建的 durable session grant 只可把同一 tool 的只读 `NetworkRequest` Ask facet 降为 Allow，不能覆盖 source Ask、任何 Deny、`Mutate / Unknown` 或不同 tool
- `ToolApprovalSessionGrant` 以 append-only control entry 持久化获批 facet（local/network）与匹配 scope，reload 同一 session 后继续参与决策；缺少任一当前字段的日志直接拒绝。可识别的 workspace validation grant 使用规范化策略 scope 和 executable-core argument binding，忽略 `tail/head/grep` 等纯输出管道，但每次执行仍以完整 AST/参数生成 exact permission plan hash 并在 effect 前重验；不同 validation 参数、family、effect、risk、workspace 或 containment 不复用。只读 network endpoint 使用 `network_read_tool` scope，允许同一 tool 在 session 内跨 URL 复用，但每个 URL 仍必须重新通过 capability binding、destination guard、durable egress barrier、逐消息 disclosure 与 budget；exact-subject grant 不得在策略 facet 或 scope 漂移后扩大权限。命中 exact network facet 的 grant 必须作为当前执行的 network authorization 传播到 tool context，不能只把 policy decision 改为 `Allow` 后丢失执行授权
- `ToolSpec` 只保存静态声明；每个动态 adapter 通过单一 `permission_plan` 为最终参数生成完整 `ToolPermissionPlanV2` draft，一次性绑定 access、operation、effects、subjects、containment、semantic scope、默认策略和安全摘要；registry 与 scoped registry 只消费并透传这一个 immutable plan contract。通用 MCP tool 投影为本地 `Read + NetworkEffect::Unknown`，本地 stdio/plugin extension process 启动投影为本地 `Execute + NetworkEffect::Unknown`
- extension process 的网络启动授权使用不可序列化的 run-scoped admission：`Ask` 只有在真实交互审批证据存在时才可越过 spawn boundary；`Deny` 只有 exact backend plan 同时证明 network isolation、process-tree isolation 和 denied network receipt 时才可启动，并在执行后复核 receipt。model-triggered activation 作为本地 `Execute + NetworkEffect::Unknown` 工具走完整 local/network/source permission；配置声明的 eager、direct activation 与 refresh 属于既有 lifecycle management path，不把 `approval_default` 重新解释为启动审批，但仍必须显式携带当前 NetworkPolicy，不能退回隐式 default Allow
- `ToolAccess` 只接受当前 `read` / `write` / `execute` 值；`access = "network"` 与其他非当前值直接拒绝，不安装容器级转换器
- `bash` 静态是 `Shell / Execute`，但它覆盖单一 `permission_plan`，由结构化 Shell analyzer 把简单只读命令分类为 `Read`，并为重定向、wrapper、pipeline、subshell、dynamic expansion、测试/包管理和写操作生成逐节点 effects 后取最严格 aggregate；parser 不完整或执行边界无法证明时 fail closed
- Shell analyzer 只可把语法与数据边界都能完整证明的受限循环降级为 `Read`；例如静态字面量列表上的文件存在性检查，循环变量只能出现在固定前缀的只读 path 中，分支只能输出结果。动态列表、glob、command substitution、未知变量、路径前缀漂移或任何 mutation 都必须继续按 `Execute` / incomplete 处理，不能为了减少误拒绝而放宽 protected target hard deny
- 受限循环若包含 Git 查询，`Read` 判定还必须绑定实际执行的 root-owned、不可写 shell/Git identity；执行前重算 binding，并使用隔离环境把 `PATH` 收窄到该 Git，同时禁用 pager、signature helper、lazy fetch、optional locks 与交互提示。PATH 中更早命中的工作区/用户可写 Git、identity 漂移或 Git 配置副作用无法被约束时一律 fail closed，`danger-full-access` 也不能越过 protected target hard deny
- `[permission.commands] allow/ask/deny` 在归一化命令文本上做显式匹配，优先级固定为 `deny > ask > allow`；它可以减少已信任命令的重复确认，但不替代 shell classifier、path subject、external-directory gate 或 protected path overlay
- 命中的 command permission 必须作为结构化 `CommandPermissionMatch` 保留在 permission decision、approval request 和 `ToolApproval` audit entry 中，TUI 审批卡片展示对应 `permission.commands.<group>`、pattern 与命令文本
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
- `reqwest`：provider HTTP client、runtime provider status client，以及 Streamable HTTP MCP/OAuth client
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
- `tauri`：桌面壳（已由 `apps/desktop` 锁定为 Tauri 2；版本与供应链以 manifest/lockfile 台账为准）
- `nix` 或平台相关 crate：更强的 confinement

## 15. 交付阶段

在进入 phase 划分之前，需要先明确产品表面原则：

- Desktop 与 TUI 是并列的一等产品表面；`sigil` 无子命令启动 TUI
  只是终端 binary 的默认行为
- `sigil run`、`sigil doctor` 和隐藏 provider 调试命令保留为自动化入口和调试通道，不承载最终产品心智
- `strict tools`、`prefix completion`、`FIM` 这类 provider 专项能力，默认应被 Desktop/TUI 交互流程吸收，而不是直接变成普通用户必须理解的顶层命令
- 如果某个能力只能靠新增命令解释自己，应该先反问：它是否其实应该是共享应用动作，并由 Desktop/TUI 以审批卡片、编辑模式或其他表面原生交互呈现

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

2026-08-15 起，交互式 TUI 统一使用 alternate-screen 全屏帧。此前 inline viewport 同时依赖
terminal 原生 scrollback、单行 DECSTBM scrolling region、Ratatui 差量 buffer 与 cursor-position
探测；这些能力在 Terminal.app、iTerm2、xterm.js、Zellij/tmux 等环境中的语义并不一致，任何一次
物理滚屏与内存 buffer 脱同步都会让后续卡片、composer 或 info rail 重叠。全屏迁移后，primary
screen 只负责保留用户原来的 shell 内容，应用运行期间的所有可见状态和历史都由同一个帧树拥有；
右侧 info rail 因此保留为普通响应式布局区域，而不是通过删除产品信息来掩盖输出 ownership 问题。
进入 alternate screen 后、首帧前必须用 write-only `ED 3`、`ED 2` 和 cursor home 清空当前物理
显示与 terminal 暴露的 scrollback；不能假定 alternate-screen 切换一定返回空 buffer，也不能为清屏
调用任何会触发 CPR 的 cursor-position 读取。

- 主消息历史：在 alternate-screen 内使用应用拥有的 bounded render store 与虚拟滚动，不把消息行写入
  terminal 原生 scrollback。PageUp/Ctrl-Home/滚轮只改变应用内 scroll offset；End/下滚回到 live tail。
  历史视图以 entry 内 logical content offset 锚定可见顶行，stream delta、timeline append、height resize、
  width reflow 与 info rail 显隐都不得把用户正在阅读的位置拖向 live tail
- Bash tool card 的标题与展开内容必须来自已经过 SafePersist/secret redaction 的 ToolCall 展示上下文：折叠态显示有界单行命令，`Ctrl-O` 展开时显示完整安全命令和输出；session restore 使用同一 durable ToolCall 投影，不能只在 live 内存中可见。运行中 progress 与最终 result 必须先按当前活动 invocation 的 `call_id` 关联，并以其精确 timeline occurrence 作为替换 identity；`execution_id` 只可作为该活动 occurrence 内的 transient correlation，不能全局扫描并合并历史，从而既不能留下同一次调用的重复卡片，也不能把跨 turn 复用的 `call_id` / `execution_id` 误合并
- 主内容区：在同一全屏帧内显示历史窗口、当前流式尾部与可操作卡片，不要求用户在 chat 区和 composer 之间切焦点
- 底部输入区：支持多行输入、发送、取消、清空
- 右侧信息区 + composer 下状态行：展示写权限、subagent 状态、cache 命中、上下文压力、花费与余额；右栏启动可见性由 `[appearance].info_rail` 控制，运行中用 `F2` 显示/隐藏、`Shift-F2` 切换精简/详情，窄终端仍按布局能力自动收起
- 模态或侧栏审批区：展示工具预览、写操作 diff、允许/拒绝动作
- 会话控制区：新会话、恢复会话、切换 workspace、查看错误详情
- setup 模式：当没有可用配置时，TUI 内部直接承载首启配置流，而不是把用户赶回命令行手写配置；Provider 作为第一项可直接切换，最终的 `Trust folder, save and start` 同时表达当前启动目录的显式信任与配置保存，不再使用独立且默认关闭的信任开关阻塞启动
- 用户配置的 setup、`/config` 和 `sigil mcp add/remove` 共用 kernel-owned 独占 sidecar lease 与同目录原子替换；MCP 读改写从读取、校验到发布全程持锁，进程崩溃不会留下半截 TOML，并发入口不能静默覆盖彼此。显式配置符号链接会保持链接身份并更新其 canonical regular-file target。
- provider 视图：不仅展示当前 provider-visible context，还应承载 compaction preview 这类“提交前先解释后果”的上下文操作

这意味着 `kernel` 事件流在 phase 1 就要按 TUI 消费习惯设计，而不是先按“stdout 打印一堆日志”来塑形。

当前实现还需要保持代码结构服务这个信息架构：`AppState` 作为 façade 收敛 bootstrap、顶层 key routing 和跨状态编排；运行状态、composer、approval、session browser、timeline presentation、review/checkpoint、agent panel 和 egress disclosure 字段归入 `crates/sigil-tui/src/app/state.rs`，已有公开 timeline/event/scroll 字段继续留在根 façade；输入焦点、slash selector、modal、setup/config、session/resume、timeline/history、tool card interaction/focus、approval、worker bridge、command dispatch 分别维护在 `crates/sigil-tui/src/app/*`；状态流测试维护在 `crates/sigil-tui/src/app/tests/*_tests.rs`，共享 fixture 只放 `app/tests/common.rs`。setup/config、commands、view model 等 TUI 普通模块的测试维护在 `crates/sigil-tui/src/tests/*_tests.rs`；provider config/status/context-window 这类入口共享 helper 的测试维护在 `crates/sigil-runtime/src/tests/*_tests.rs`；worker runner 通过 `runner.rs` façade 暴露协议和启动入口，worker protocol、spawn 装配、event/approval bridge、session/compaction flow 与 runner 状态机测试维护在 `crates/sigil-tui/src/runner/*`，worker loop 由私有 state aggregate、薄 scheduler、七类 advancement、public command 到 domain-typed command 的穷尽 handler、覆盖四种 scope-changing path 的统一 session transition，以及 active run、queue、MCP/provider refresh、agent/task runtime、terminal refresh 共同维护在 `runner/worker_loop/*`；renderer 通过 ViewModel 或 render options 读取 UI 数据；`ui.rs` 只作为 `ui/*` 模块入口和必要 re-export，顶层 shell layout、theme/geometry/text 底座、timeline、tool card、markdown、approval、setup/config、modal 等渲染块分别维护在对应 `ui/*` 模块，renderer 测试维护在 `ui/tests/*_tests.rs`。用户交互面优先使用 TUI 焦点和快捷键：tool card 选择/展开走 `Ctrl-G`、`Alt-J/K`、`Ctrl-O` 与 `Esc`，不依赖 hidden slash command；新增快捷键和命令通过 `commands.rs` metadata 同步 info rail、keyboard help 和 README。Markdown 展示由 `ui/markdown.rs` 和 `MarkdownRenderOptions` 统一约束，assistant timeline、tool preview、approval modal 不各自维护解析规则。

主题与右侧信息栏启动可见性作为 TUI appearance 能力落在 `AppearanceConfig`，而不是拆成独立 crate。`sigil-kernel` 只承载可序列化的 `AppearanceConfig`、`ThemeId`、`info_rail` 和 `[appearance.colors]` 原始字符串；`sigil-tui` 将主题配置解析为 `ThemePalette`，并把 `info_rail` 作为启动默认值投影到当前运行的可见状态。内置主题包括 `sigil_dark`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`nord` 和 `high_contrast_dark`。颜色 override 只允许稳定语义 token 和 `#RRGGBB`，用于 TUI 外观，不进入 session/control state、approval 审计、tool payload 或 provider-visible context。`/config` 里的 Appearance draft 会优先供 renderer 解析，让用户在保存前即时预览完整 config palette，包括背景、边框、标题 chip、正文、弱化文字、选中行、状态和提示 token；保存后运行时 config snapshot 更新并重建 timeline render cache，避免旧消息缓存保留旧主题色。Info rail 的 `F2` 运行时覆盖只改变进程内布局状态，不写回配置，也不进入会话审计。

### 15.2 Desktop/TUI 双表面下的能力暴露规则

为了避免把产品越做越像命令集合，需要提前规定：

1. 普通用户可以进入 Desktop 或 TUI；终端里的 `sigil` 默认进入 TUI。
2. `run` 这类命令保留给自动化、CI、脚本或最小 smoke test。
3. `prefix completion` 不应该成为普通用户必须理解的单独概念，而应在交互表面中表现为“继续补全”或“沿当前前缀续写”。
4. `FIM` 不应该成为普通用户必须手动切换的独立模式，而应在编辑/补洞场景中由应用内部自动选择。
5. `strict tools` 是 provider/tool discipline，不是用户命令；用户看到的应该是“工具调用更严格/可审批/更可预测”。

## 16. 关键风险

1. Async trait 设计：Rust 在安全性上比 Go 更强，但第一版 object-safe streaming API 需要选得足够稳。
2. Shell 执行可移植性：进程控制、PTY、跨平台 confinement 都很容易踩坑。
3. 文件编辑工具正确性：编码保留、partial replace、diff preview、中断恢复都不简单。
4. MCP 协议边角：streamable HTTP、notifications、server lifecycle 这些细节容易被低估。
5. 过早拆太多 crate：行为还没稳定前，包过多只会拖慢迭代。
6. 如果先把 provider 专项能力做成越来越多的 CLI 子命令，再补 Desktop/TUI 交互，最终很容易得到“命令集合”而不是“交互产品”。

## 17. 已锁定的关键决策

当前实现已经锁定并落地这些工程决策：

1. 项目配置文件名为 `sigil.toml`
2. Desktop 与 TUI 是并列的一等用户壳；`sigil` 默认启动 TUI，子命令只保留 debug / automation，不做命令产品化
3. kernel 是 event-driven、agent-runtime-centered
4. provider crate 保持 provider-specific 协议细节内聚，kernel 只承载中立契约
5. DeepSeek、OpenAI-compatible、Anthropic、Gemini 共用 runtime 装配与 capability view
6. MCP `stdio` 与 Streamable HTTP/OAuth 进入 runtime/TUI 生命周期，server lifecycle、凭据和 trust policy 保持可配置
7. planner / executor / subagent 在 base loop 之上以可审计的 task state 继续演进

## 18. 立即下一步

当前阶段最正确的下一步不是继续扩张命令表面，也不是把 provider 专项能力做成更多 CLI 入口，而是：

1. 补齐 `/doctor` 与 `/config` 的 provider capability 明细，确保用户能核验具体能力差异
2. 扩充 Anthropic/Gemini provider 的真实协议 fixture，覆盖 tool result、thought signature、finish reason 和 safety 边界
3. 继续收紧 provider canonical naming、model routing、auth resolution 在 Desktop/TUI/runtime/doctor 之间的一致性
4. 为 provider-specific continuation state 保持 durable、append-only 的恢复测试
5. 让 provider setup assistant 在 Desktop 与 TUI 复用同一配置语义，而不是把配置体验继续摊到 README 或隐藏命令里
6. 继续补齐 Desktop 的签名分发与更新渠道，同时保持 TUI 安装渠道稳定

这样做能让 `sigil` 站在两件最重要的东西上：一个可复用、可扩展、契约稳定的 agent 内核，以及两个共享产品语义、分别适配桌面与终端的信息架构，而不是一个子命令越来越多的命令集合。

## 19. DeepSeek 专项优化设计

结合 DeepSeek 官方 API 文档，`sigil` 如果要把 DeepSeek 当作旗舰后端，不应该只停留在“兼容 OpenAI SDK 调用”的层面，而应该显式做一套 `DeepSeekProviderProfile`。

### 19.1 Canonical Model Policy

根据 DeepSeek 官方文档，当前推荐模型名是：

- `deepseek-v4-flash`
- `deepseek-v4-pro`

而 `deepseek-chat` 与 `deepseek-reasoner` 是兼容别名，并计划于 **2026 年 7 月 24 日** 弃用。

因此建议：

- 所有新配置、日志、遥测、会话元数据一律使用 canonical model id
- provider 当前仍公开的 alias 只按 provider API 的实时模型标识处理，不改写配置
- 非当前配置中的 alias 不读取、不映射，也不触发迁移流程

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

已经落地的 `sigil-provider-anthropic`、`sigil-provider-gemini`、`sigil-provider-openai-compat` 和 `sigil-provider-openai-responses` 也沿用同样模式：

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

O5b2d1 已在 task role runtime 落地 provider-neutral 的第一层本地背压：

1. 每个 `provider + model` route 拥有独立 adaptive concurrency window
2. `[task].max_parallel_read_steps` 是 route window 上限，429 触发乘法下降，成功 completion
   按窗口大小做加法恢复
3. route lease 覆盖完整 response stream；Done、error 或 drop 都释放容量
4. 饱和 route 的新请求在真正 provider dispatch 前等待，不取消已在途请求，也不阻塞其他 route

这层状态属于 `AgentSupervisor` 生命周期内的 runtime policy，没有公开成 kernel type，也不作为
重启后重放 provider request 的授权。稳定 `user_id` / partition key 的更细粒度信号量，以及
前台主会话与 planner/reviewer/compactor 的优先级配额，仍应留在 provider/runtime 私有策略中。

对于 `sigil` 这种 agent 内核，好的体验不是“理论峰值最高”，而是“在 DeepSeek 并发纪律下仍稳定收敛，不制造 429 风暴”。

### 19.19 模型发现与别名治理

虽然 `deepseek-v4-flash` / `deepseek-v4-pro` 已经是当前 canonical model id，但 `sigil` 仍建议在 provider 启动阶段做一次轻量模型发现与校验。

建议行为：

- 初始化时可选调用模型列表接口，补充推荐与诊断；站点不提供列表或刷新失败时仍允许保存明确模型 ID
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
