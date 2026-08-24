# Sigil 工程规范

本文档定义 `sigil` 的工程协作、变更流程和交付约束。

## 1. 文档分工

仓库里有两套开发文档面，职责不要混淆：

- `dev/governance/*`：直接约束当前工程协作的规范文档
- `dev/docs/*`：技术方案、架构草案、设计演进文档

简单说：

- “应该怎么写代码、怎么做工程” 看 `dev/governance`
- “为什么这样设计、未来怎么扩” 看 `dev/docs`

## 2. 仓库分层职责

### 2.1 产品层

- `apps/desktop` 与 `crates/sigil-tui` 是并列的一等产品表面，分别承载桌面与终端的信息架构
- `crates/sigil` 提供 `sigil` binary；无子命令启动 TUI，显式子命令承担自动化和调试入口
- `apps/desktop` 通过 `sigil-desktop` 和本地 HTTP/SSE 复用同一 runtime；
  Desktop 与 TUI 共享任务、审批、恢复和验证语义，不复制 agent loop

### 2.2 内核层

- `crates/sigil-kernel` 是领域核心
- 任何跨入口共享的能力，优先先沉到 kernel

### 2.3 基础能力层

- `crates/sigil-provider-http`：provider 共用的安全 HTTP client builder；支持通过 `SSL_CERT_FILE` 追加私有 CA，保留内建信任根、证书链与主机名校验，不承载 provider 协议语义
- `crates/sigil-provider-deepseek`：DeepSeek provider
- `crates/sigil-provider-openai-compat`：OpenAI-compatible provider
- `crates/sigil-tools-builtin`：内置工具
- `crates/sigil-process`：跨 crate 的最小进程树 lifecycle ownership；不承载 shell、sandbox、MCP framing 或 TUI 状态
- `crates/sigil-desktop`：桌面 Rust 后端的 child/token/bootstrap/client ownership；不依赖 kernel/runtime/TUI/HTTP server internals，不向 renderer 暴露 bearer 或 generic process/network API
- `crates/sigil-mcp`：MCP 接入
- `crates/sigil-runtime`：跨 Desktop / TUI / CLI 的 provider、tool registry、run options 装配，并提供 provider-neutral 的配置草稿、状态请求/刷新任务、provider/model metadata、agent-message route 和 session-control append helper；入口层不直接依赖 provider crate

## 3. 变更流程

### 3.1 开工前

先回答这几个问题：

- 这是产品表面变化、内核变化，还是 provider / tool 变化
- 是否会影响 Desktop 或 TUI 用户流程
- 是否会影响 session 持久化、恢复或审批安全边界
- 是否需要同步文档
- 是否把内部机制、低频调试项或高级策略暴露成了普通用户主流程；如果是，优先改成粗粒度模式、当前任务 action、doctor 建议、配置文件或高级流程

### 3.2 实施时

- 优先做最小闭环，不做“大而全”半成品
- 优先保护现有运行链路，不要为新能力破坏已有 Desktop 或 TUI 体验
- 跨 crate 变更时，优先先定边界，再改代码
- 改 TUI / `/config` / slash command 时，默认先减少用户需要理解的概念数量，再考虑暴露更多开关；不要为了覆盖所有实现能力而增加日常操作成本

### 3.3 收尾时

- 跑必要 gate；日常提交优先使用分层门禁，不默认跑完整 workspace gate
- 更新必要文档
- 在说明里明确哪些能力已完成，哪些只是扩展点
- 新增直接依赖时同步维护 [`dependency-supply-chain.md`](dependency-supply-chain.md)，记录 owner、版本/feature、许可、安全理由和发布前剩余 gate
- desktop 变更同时运行 `pnpm --dir apps/desktop check`；涉及 native shell 时至少编译 `sigil-desktop-app`，涉及 runtime route 时补真实 `sigil serve` contract test

## 4. 文档同步规范

以下变更默认要同步文档：

- 新入口、新 crate、新配置块
- Desktop 或 TUI 主流程变化
- tool 审批 / diff / session 行为变化
- provider 能力边界变化
- 代码约束、工程约束变化

同步目标通常包括：

- `README.md`
- `AGENTS.md`
- `dev/governance/*`
- `dev/docs/*` 中被影响的设计方案

## 5. 验证规范

### 5.1 分层质量门

日常本地提交不要默认跑完整 workspace gate。先按变更范围选择最小能证明当前改动的门禁：

```bash
# 日常提交：格式、workspace 编译检查、touched crate tests
./scripts/check-touched.sh --tier quick

# 中高风险提交：quick + touched crate clippy
./scripts/check-touched.sh --tier standard

# 发布前、大批量合并前或核心语义大改后：完整 workspace test + clippy
./scripts/check-touched.sh --tier full
```

分层规则：

- `quick`：适合普通代码改动、局部 TUI 状态/渲染、文档加少量测试；执行 `git diff --check`、prompt semantic gate 自测与扫描、`cargo fmt --all --check`、`cargo check` 和 touched crate 的 `cargo test -p <crate>`。
- `standard`：适合 session/event/mutation/verification/permission/tool/TUI runner 等高风险路径；在 `quick` 基础上追加 touched crate 的 `cargo clippy -p <crate> --all-targets -- -D warnings`。
- `full`：适合发布前、跨多个核心 crate 的语义大改或需要合并长期分支时；执行 workspace `cargo test` 和 `cargo clippy --all-targets -- -D warnings`。

仍可手动运行完整门禁：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/coverage.sh
```

覆盖率检查默认生成 workspace 单测覆盖率报告，由 `cargo-llvm-cov` 执行；CI 和本地必须使用同一个脚本入口，避免统计范围漂移。日常 CI 不使用全仓固定行覆盖率阻塞；需要发布级阈值时显式运行 `COVERAGE_MIN_LINES=96 ./scripts/coverage.sh`。默认统计范围排除少量 orchestration loop / adapter 文件，具体列表以 `scripts/coverage.sh` 为准；这些文件只能承载入口调度、raw terminal/worker 启动、provider I/O 桥接和启动失败出口，不应成为新增业务逻辑的覆盖率逃逸区。

本地提交 hook 使用仓库内 `.githooks/pre-commit`。启用方式：

```bash
git config core.hooksPath .githooks
```

hook 会先运行 `scripts/test-check-no-prompt-phrase-routing.py` 与 `scripts/check-no-prompt-phrase-routing.py`，阻止 production code 用固定自然语言短语替代模型 typed semantic decision；随后调用 `scripts/check-staged-coverage.py`，检查 staged 的 Rust 业务代码新增可执行行是否伴随同 crate 的测试文件改动。该检查只针对业务代码，不把测试文件纳入新增业务代码统计；也不把 staged source 里可识别的声明、导入和类型形状当作可执行业务行。如果业务文件同时有 staged 与 unstaged 修改，必须先整理 staging 后再提交。

为缩短本地提交耗时，staged gate 不再为每次提交运行 `cargo llvm-cov`。它只证明“业务代码改动有同 crate 测试证据”，不要把它当成完整 workspace 覆盖率替代品。完整 workspace 覆盖率通过显式 `./scripts/coverage.sh` 和 CI 报告观察趋势；发布级阈值应由 release/hardening 任务显式启用。RFC/session/mutation/verification 等核心语义变更优先补可复现的语义测试和 conformance case；不要为了满足本地 staged gate 而补无效断言或 pass-only 测试。

调整 staged diff 分类、同 crate 测试证据判断或覆盖率辅助解析时，必须同步更新 `scripts/test_check_staged_coverage.py` 的纯函数测试。

### 5.2 何时需要更强验证

以下情况建议至少做一次针对性人工冒烟：

- TUI 交互流程变更
- 审批逻辑变更
- session 恢复变更
- provider streaming / retry / reasoning replay 变更
- MCP 工具调用变更

### 5.3 docs-only 变更

如果只改文档，可以不跑全量 Rust gate；但要确认：

- 所有引用路径真实存在
- 示例命令可在当前仓库语境下成立
- 文档没有继续宣传已经隐藏或不再推荐的产品心智

## 6. 配置管理规范

- 真实本地配置默认使用用户配置目录下的 `sigil.toml`，该文件可能包含密钥，默认不提交；workspace 根目录的 `sigil.toml` 只有显式 `--config <path>` 时才使用；配置示例以 `README.md` 的“配置要点”为准
- 新增配置项时，要同时考虑默认值、兼容性和文档说明
- 用户主心智相关配置要谨慎暴露，不要把调试开关直接产品化

## 7. Desktop 与 TUI 产品规范

- 新能力默认先想清楚共享状态、命令与事件如何被 Desktop 和 TUI 消费，
  而不是先加 CLI 子命令
- 已同时存在于 Desktop 和 TUI 的行为变更必须检查两端；允许表面特有交互，
  但审批、恢复、任务与验证语义不得各自分叉
- 键位、状态栏、面板提示必须同步更新
- 审批体验优先级很高：写工具要尽量有 preview / diff / 导航
- session 体验要可持续使用，而不只是“一次性跑完一轮”
- 普通用户主路径只暴露高频、可解释、目标导向的操作；类似 permission policy matrix、artifact inventory multi-select、MCP command editing、theme token editing、LSP discovery/report_missing、terminal OSC52/mouse 兼容性等低频能力，应默认进入 advanced config、doctor 或独立高级流程
- 每次 TUI/config 变更的审计都要检查：用户是否需要理解新的内部术语、是否出现多个近似按钮、是否把一次性任务 action 放进长期设置页、是否把安全/信任决策拆成过多细项
- `AppState` 应保持 façade；输入、slash、modal、setup/config、session/resume、timeline/scrollback、tool focus、approval、worker bridge 和 command dispatch 维护在 `crates/sigil-tui/src/app/*`，避免 `app.rs` / `ui.rs` 重新膨胀
- TUI 状态流测试维护在 `crates/sigil-tui/src/app/tests/*_tests.rs`；新增或修复某个 flow 时必须优先补同域状态转换测试
- TUI worker runner 维护在 `crates/sigil-tui/src/runner/*`；协议、启动装配、运行 loop、审批桥接、事件桥接和 session/compaction flow 不要回填到 `runner.rs` 单文件
- TUI renderer 变更必须同时确认状态模型、事件流、键位提示和 README/governance 文档是否需要同步；纯 renderer 拆分也要跑对应 TUI renderer/state tests
- UI 快捷键变更必须覆盖 key mapping 与 `AppState` state transition tests，并确认 info rail / keyboard help / README 与真实 metadata 一致
- markdown renderer 变更必须覆盖 assistant timeline、tool preview 或 approval modal 的至少一个调用面，避免 options 增强只在单测中成立
- setup/config 状态模型拆分必须保留保存、关闭、dirty guard 和 modal 输入的持久化/状态机测试
- 新增或迁移单元测试时，必须遵守 `dev/governance/code-standards.md` 的测试目录规范：默认使用同层 `tests/<module>_tests.rs`，业务文件只保留 `#[path = "tests/<module>_tests.rs"] mod tests;` 声明；不要新增 inline tests、`module/tests.rs`、`module/test_support.rs` 或裸 `src/tests.rs`

## 8. Provider 与工具工程规范

### 8.1 Provider

- 保持 `DeepSeek-first`，但不要把仓库做窄成 `DeepSeek-only`
- provider 专项能力写在 provider crate，不反向污染 kernel
- Beta-only 能力要明确标识，避免误导成默认稳定能力
- chat 主链路必须保持真实 streaming；只有首个 chunk yield 前允许透明 retry，yield 后的错误应作为 stream error 暴露

### 8.2 Tool

- 工具必须考虑 workspace confinement
- 文件类工具必须拒绝绝对路径、`..` 和指向 workspace 外的 symlink；新增路径要校验最近存在父目录仍在 workspace root 内
- 写工具要考虑 preview、审批、失败回灌、恢复一致性
- shell 工具要特别注意工作目录、超时和错误输出结构化
- 大输出工具的验证必须同时覆盖 observed/persisted bytes、artifact completeness、model/display cap 和 stored-event cap；只验证最终字符串被截断不算通过
- artifact publish 必须发生在 descriptor append 之前；append 失败只允许留下可由 grace GC 清理的 orphan，缺失或损坏时不得静默重跑原工具
- typed artifact retrieval 必须覆盖 session scope、selector/output budget、full hash validation、repeated-read dedupe 和 body-free durable receipt

## 9. 会话与持久化规范

- session log 采用 append-only JSONL
- session JSONL、writer lease、lifecycle journal 及其 lease 必须创建为 owner-only 文件；writer 打开既有文件时必须修复宽松 mode，Doctor 必须报告最近 stream 的权限漂移
- tool result 正文使用 session sibling immutable artifact store；JSONL 只保存 V2 descriptor、facts 和 bounded view
- artifact ref 必须 opaque、session-scoped；fork 重新签发 ref，export 明确 completeness，delete/GC 复用 tombstone + grace lifecycle
- control state 不能只存在运行内存
- 入口层追加普通 control state 时优先复用 runtime session-control helper，避免 TUI / CLI / HTTP adapter 各自实现不同的 append/reload 行为
- response handle、continuation state、prefix snapshot、compaction record 等 durable control state 要有显式查询/恢复路径
- tool-output pressure、aging eligibility 和 artifact reachability 必须由 active incremental projection 随 append 前进；steady-state worker 通过 source-change wake/coalescing 调度，不得固定 cadence 轮询、全量重放 JSONL 或长期持有 data-file lock
- deterministic tool-output aging 必须先于 semantic compaction，并通过 exact frontier + context epoch compare-and-publish 激活；当前 epoch 的 cached prefix 不原地改写
- 任何恢复相关设计，都要优先考虑“进程重启后是否还能正确继续”

## 10. RFC-0071 资源权威 / 沙箱生命周期约束（R71.6+）

- application 启动只选择一次 epoch（legacy 或 new current-schema），选择结果以 content-addressed 的 startup cutover manifest 呈现；同一 application instance 发布不同 manifest 即 fixed-forward 拒绝
- new current-schema epoch 的 mandatory adapter readiness 任一 probe 失败，application 必须 fail closed，不得部分启动；对 18 个 mandatory adapter channel 的 probe 必须是真实调用（storage admit/finalize round trip、surface contract 校验、admission gate 可达性），不得用空跑或"无输出即通过"代替
- ExecutionExtension 的 production seam 必须由同一 composed `ManagedExecutionServiceV1` 提供：runtime 只能把已解析的 long-lived stdio plan 映射为 planner/broker 绑定的 managed request，sandbox 通过真实注入 launcher 启动并返回 opaque managed process lifecycle；Local 缺少 launcher 时必须保持 `ProviderUnavailable`，不得回退到裸 MCP `Child`
- `SignedUpdaterCache` 必须由 `sigil-updater::ProductUpdaterState` 这个独立 product-plane owner 管理：CLI、TUI、Desktop 只能调用 typed updater API，不得自行拼 cache path、创建 staging file 或执行 atomic replace；该 owner 不消费 agent/session grant，runtime probe 只有在真实 owner 注入 composition 后才可通过
- Desktop borrowed native save 必须由 Tauri native code 取得用户选择、经 host-private typed client 提交 registration capsule，由 server-side `BorrowedNativeSaveServiceV1` 做 no-follow parent observation、content-hash verification、create-new atomic publish，并只返回 closed receipt；raw destination 不得进入 renderer、public DTO、OpenAPI 或 receipt，Tauri 不得保留 direct `OpenOptions(create+truncate)` writer
- Desktop borrowed configuration 必须由 host-private typed client 提交 one-shot registration capsule；server-side `BorrowedConfigurationServiceV1` 固定配置 root、在 update lock 内重验 expected bytes，并执行 bootstrap 或 versioned atomic replace，返回带 previous/committed identity 与 version 的 closed receipt；renderer/public DTO/OpenAPI 不得携带 raw config path，任何 `RootConfig::save_if_unchanged`/direct replace 旁路都不得作为 Desktop production writer
- 发布 new epoch 后，当前 binary 只创建/读取 current-schema session；old-schema session 明确 unavailable，不得按旧 schema 解释
- 禁止 per-consumer flag、V2/V3 dual write、legacy allocator fallback 与 active process provider 切换
- 该 epoch 选择的 gate 测试与 conformance runner：`cargo test -p sigil-runtime resource_global_cutover`、`cargo test -p sigil-kernel current_schema_only`、`./scripts/run-r71-global-cutover-conformance.sh`

## 11. 评审标准

一个变更准备合入前，至少应该满足：

- 架构边界没有被破坏
- TUI 用户心智没有被命令式入口反向带偏
- 用户操作复杂度已被审计：新增入口是否服务高频用户目标，是否可以合并为更粗粒度动作，是否应改放到当前任务 action、doctor、配置文件或高级流程，而不是进入默认菜单、footer、slash command 或 `/config` 主路径
- 文档和实现一致
- 关键验证已完成
- 后续扩展点没有被临时实现锁死
