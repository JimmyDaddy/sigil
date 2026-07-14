# RFC-0025 Context Compaction V2 整体代码与完整度 Review

- 日期：2026-07-14
- 评审对象：当前未提交工作区中 RFC-0025 / K25.1-K25.17 相关实现
- 对照基线：`HEAD`、`.repo-local-dev/rfcs/0025-context-compaction-v2-execution-plan.md`、`.repo-local-dev/sigil-context-compaction-technical-solution-2026-06-23.md`
- 范围：`sigil-kernel`、`sigil-runtime`、DeepSeek / Anthropic / OpenAI Responses provider、`sigil-tui`、相关测试与公开文档
- 结论：**未发现 P0；发现 10 个 P1、5 个 P2。当前不具备把 Context Compaction V2 整体标记为产品闭环完成或通过 release gate 的条件。**

## Findings

### [P1] 1. Safe-fold 会把明确标为 protected 的消息藏到 active boundary 之前

证据：

- `crates/sigil-kernel/src/session/compaction_plan.rs:128-166` 会把不完整 tool pair、control 和其他不可折叠事件标记为 protected。
- 同文件 `:193-209` 遇到 protected message 只执行 `continue`，仍会继续折叠更晚的普通 message，并把 `folded_through` 推进到 protected message 之后。
- `crates/sigil-kernel/src/session/context_projection.rs:181-221` 激活时只保留 checkpoint 以及 `stream_sequence > folded_through` 的原始消息，完全不使用 plan 中的 `protected_events` 或 `retained_event_ids`。

例如 `old user -> unfinished assistant tool call -> later user -> latest user`、tail=1 时，不完整 assistant tool call 会被标成 protected，但 `later user` 仍可被折叠并推进 boundary；最终 protected assistant 也从 provider-visible context 消失。

影响：safe-fold 的核心安全声明不成立，tool-call closure 与上下文完整性都可能被破坏。原始 JSONL 仍在，但下一轮 provider 看不到被“保护”的消息。

建议：boundary 只能推进到连续安全前缀末端；遇到 provider-visible protected message 必须停止推进，或者让 Applied 精确绑定并实际投影 retained/protected IDs。补 `plan -> apply -> reload -> provider projection` 反例测试。

### [P1] 2. 当前所谓 semantic checkpoint 实际没有语义压缩，也没有可靠保存当前目标和 active plan

证据：

- 生产路径在 `crates/sigil-tui/src/runner/worker_loop/compaction_runtime.rs:630-647` 固定传入 `objective: None`，并把 `in_progress`、`pending_actions`、`provider_continuity`、`model_notes` 全部设为空；注释也明确 semantic compressor I/O 属于后续阶段。
- `crates/sigil-kernel/src/task_memory.rs:425-459` 只从显式输入或 `TaskRun.objective` 获取目标；普通 conversation 不会把最新用户目标冻结为 durable objective。
- 同文件 `:450-548` 对 plan 类 control state 没有提取分支，`:565-583` 缺目标时写入 `No task objective recorded`，constraints、decisions、risks 同样为空。
- 激活后 `crates/sigil-kernel/src/session/context_projection.rs:181-221` 会用 checkpoint 替换 boundary 之前的原始历史，因此遗漏不是纯展示问题，而会真正从下一轮模型输入中消失。
- 技术方案 `:1724-1735` 明确要求保存 current objective、active plan、next actions、重要命令、失败和决策原因。

影响：长编码任务在 plan、决定和下一步离开 retained tail 后会失去连续性；“portable semantic checkpoint”这个产品文案高估了实际能力。

建议：先实现 source-bound 的 current objective 与 active plan projection，再决定是否加入无工具 semantic compressor。至少覆盖 ordinary chat、approved plan、task execution、连续 3-5 次 compact、reload/resume 的质量 fixture。

### [P1] 3. Portable apply 只有压缩后 fit proof，没有 before/after economics 和 minimum-savings gate

证据：

- `crates/sigil-kernel/src/session/portable_compaction.rs:260-310` 在 apply 前只校验 target material。
- `crates/sigil-kernel/src/compaction_token_proof.rs:228-266` 的 `RequestFitProof` 只证明 `after input + output reserve + safety <= context window`。
- `CompactionAppliedV2` 没有持久化 before/after token、minimum token/PPM 或 savings ratio。
- TUI DTO 与 modal 在 `crates/sigil-tui/src/runner/worker_loop/compaction_runtime.rs:537-548`、`crates/sigil-tui/src/app/compaction_flow.rs:45-70` 只显示 after-fit，却把它描述成 `target request: verified locally`。
- 技术方案 `:1805-1812`、`:2095-2102`、`:2364-2368` 要求 before/after、minimum savings、strategy 和 risk；收益不足或膨胀不得改变 active boundary。

影响：即使 checkpoint 几乎不节省 token，甚至比被折叠内容更大，只要最终请求还能 fit，用户仍可确认并永久切换 active boundary；UI 还会把“能放下”误解为“值得压缩”。

建议：冻结原请求的 before material，分别生成 before/after evidence，用整数 token 与 PPM 公式执行 minimum-savings gate，并把 evidence、阈值和结果持久化、展示、可重放验证。

### [P1] 4. Clean install 没有 tokenizer provisioning，唯一 shipping portable path 默认不可达

证据：

- `crates/sigil-runtime/src/portable_compaction.rs:67-102` 只读取固定 cache 中已存在的 DeepSeek tokenizer，明确不下载。
- 下载 helper 虽存在于 `crates/sigil-provider-deepseek/src/compaction_token_profile.rs:345-429`，但生产代码没有调用；只有三个 ignored/联网测试使用。
- `crates/sigil-runtime/src/doctor/providers.rs:92-118` 不检查 tokenizer，也不给出修复动作。
- 当前测试反而固化了“缺 tokenizer 时 blocked”的负路径，没有真实 fresh-install happy path。

影响：新用户即使使用唯一开放的 `deepseek-v4-flash` profile，手动 `/compact`、idle auto 和 pre-turn portable compaction 都无法 apply。当前实现是 gated primitive，不是可交付能力。

建议：增加有明确网络披露/同意的 setup 或 doctor install action，或随发行物提供许可允许的校验后 artifact；补 fresh-install manual/idle/pre-turn E2E。

### [P1] 5. OpenAI Responses 在 `store=false` 时没有请求 encrypted reasoning，文档声明不成立

证据：

- `crates/sigil-provider-openai-responses/src/models.rs:4-20` 的 request DTO 没有 `include`。
- `crates/sigil-provider-openai-responses/src/request.rs:47-61` 构造 reasoning 请求时也没有加入 `reasoning.encrypted_content`。
- `crates/sigil-provider-openai-responses/src/mapper.rs:139-168` 会保存 completed output items，后续请求再复用，但缺失的 encrypted content 无法由本地重建。
- `docs/en/provider-openai-responses.md:53-57` 和中文对应段落声称该路径能保护并复用 encrypted reasoning content。
- OpenAI 官方 API reference 说明，`reasoning.encrypted_content` 只有在 `include` 中请求时才会返回，专门用于 `store=false` 等 stateless multi-turn 场景：[Create a response](https://developers.openai.com/api/reference/resources/responses/methods/create)。
- 本地竞品仓库 `sigil-competitor-repos/openai-codex/codex-rs/core/src/client.rs:851-855` 在启用 reasoning 时会显式加入该 include。

影响：reasoning model 的无状态多轮连续性可能丢失 provider-private reasoning item；公开文档给出了错误保证。现有测试只伪造带 `encrypted_content` 的返回对象，没有证明请求会索取它。

建议：reasoning 启用且采用 stateless continuation 时，在普通请求及需要保持同一 prompt-bearing material 的计数路径中一致加入 include；补真实的两轮 request/replay fixture。

### [P1] 6. 一个 durable physical attempt 可能包住多个实际 HTTP POST

证据：

- kernel 在 `crates/sigil-kernel/src/agent/provider_stream.rs:101-142` 只围绕一次 `provider.stream()` 写一个 Started/Terminal。
- OpenAI Responses 在 `crates/sigil-provider-openai-responses/src/provider.rs:369-405` 内部对 429/5xx 再 POST。
- OpenAI-compatible、Anthropic、DeepSeek、Gemini 分别在各自 `provider.rs:82-120`、`:115-155`、`:175-227`、`:122-177` 内做同类 status retry。

影响：一个名为 physical attempt 的 durable record 实际可代表两次 wire request。首次 5xx/429 不等于“服务端肯定未消费”，后一次成功会把整体记录成一个 Completed，破坏 request audit、queue recovery 和 no-replay 证据边界。

建议：每个实际 HTTP send 都必须拥有独立 Started/Terminal；或者禁止在收到 HTTP status 后于 provider 内部重试。只有能够证明 zero-wire 的 connect retry 才能留在同一个 attempt 内。

### [P1] 7. DeepSeek “Exact” proof 未绑定实际 resolved route/config

证据：

- `crates/sigil-runtime/src/portable_compaction.rs:23-27` 只按 provider/model 放行 profile。
- `crates/sigil-provider-deepseek/src/config.rs:18-75` 允许 base/beta/anthropic URL 与 strict-tools mode 被配置或环境变量覆盖，真实 provider 在 `provider.rs:150-170` 使用 resolved config。
- token counter 在 `compaction_token_profile.rs:181-196` 固定使用 `StrictToolsMode::Auto` 与默认 quirks。
- binding 在同文件 `:431-465` 硬编码官方 route、hosted fingerprint 和 corpus；通用 `TokenMeasurementBinding`（`crates/sigil-kernel/src/compaction_token_proof.rs:61-68`）没有实际 route fingerprint。

影响：自定义 endpoint 或不同 strict-tools 配置仍可能拿到“官方 hosted parity Exact”证据并启用 apply，实际 wire material 与 proof 不同。

建议：把 resolved route/profile fingerprint 冻结进 request material 与 token binding；第一阶段可以更简单地仅允许精确官方 route + 精确受支持配置，其余 fail closed。补 env override/custom endpoint/strict mode 负例。

### [P1] 8. Continuation payload 在 key-loss 和 manifest-only crash frontier 上不能 fail-closed 收敛

证据：

- `crates/sigil-kernel/src/session/provider_continuation_payload_coordinator.rs:168-185` 的 `persist_committed_payload` 总以 `create_key_if_absent=true` 进入 store。
- `crates/sigil-kernel/src/session/provider_continuation_payload_store.rs:370-382` 缺 key 时立即生成并写入替代 key，而 durable manifest 是否已经存在要到后续才检查。
- native materialization 在 `crates/sigil-kernel/src/session/provider_native_compaction.rs:320-351` 先持久化/finalize manifest，再 append candidate；两者间 crash 会留下合法 `ManifestOnly` pin（`provider_continuation.rs:1642-1666`）。
- `provider_continuation_payload_coordinator.rs:391-423` 对 finalized manifest 直接返回；全仓生产代码没有调用 `recover()`，调用点只有测试。

影响：已有 durable manifest 且 key 丢失时，重试会先修改 keyring；manifest-only ciphertext 与 retention pin 则可能永久残留。K25.12B2B 的 key-unavailable、orphan recovery 和 retention 完成声明不成立。

建议：持 payload lock 后先读取 durable projection，只有 manifest/stage/final 全不存在时才允许创建 key。把 recovery 接入唯一 session recovery owner，实现 `ManifestOnly -> source terminal proof -> OrphanDiscovered -> physical delete -> Deleted` 的幂等收敛。

### [P1] 9. 当前 release dependency gate 失败，且新增直接依赖未登记

证据：

- `Cargo.toml:54` 新增直接依赖 `tokenizers = 0.23.1`。
- `dev/governance/engineering-standards.md:56-61` 要求新增直接依赖同步维护供应链台账。
- `dev/governance/dependency-supply-chain.md:34-41` 的 K25 台账只记录了 `keyring` 与 `ring`，没有 `tokenizers`。
- 本次执行 `cargo deny --offline check advisories` 失败：`tokenizers 0.23.1 -> macro_rules_attribute 0.2.2 -> paste 1.0.15` 命中 `RUSTSEC-2024-0436`（unmaintained）。
- `cargo audit --no-fetch` 没有报告已知漏洞，但报告了 `bincode` 与 `paste` 两个维护性 warning；这不抵消仓库配置的 deny gate 已失败。

影响：K25.17 的 release evidence 不能成立。这里不是已知可利用漏洞结论，但确实是当前仓库发布策略下的阻塞项。

建议：优先调整 tokenizers 版本/features 或替换依赖链；若只能临时例外，必须有 owner、理由、删除条件和复核日期，并补齐直接依赖 ledger 后重跑 deny/audit。

### [P1] 10. 顶层 `completed` 把 durable foundation、窄 shipping slice 和完整产品目标混为一谈

证据：

- `.repo-local-dev/rfcs/0025-context-compaction-v2-execution-plan.md:3` 标记整体 `completed`，`:75` 把 release evidence 标成 done。
- 同一计划 `:43` 明确 session key destruction/export rewrap 延期，`:55-60` 明确 Anthropic/OpenAI native driver 不进入用户 apply，`:74` 的 model switch 只是在 idle 时启动 fresh session。
- 技术方案 `:2364-2369`、`:2383-2387`、`:2412-2416` 仍要求 economics、重复质量、native lifecycle recovery、key lifecycle、model switch transfer、coverage 和真实 TUI smoke。
- completion audit `:33-42` 没有 coverage、真实 `/compact` 键盘 smoke 或 supply-chain gate。
- 技术方案状态行 `:23` 又停留在更早阶段，与当前代码反向失同步。

影响：后续决策者无法区分“durable contract 已搭好”“窄路径已实现”和“用户能力已闭环”，也容易过早进入发布/收尾。

建议：顶层状态改为类似 `foundation-complete / product-closure-incomplete`；每个能力单独列 `complete / partial / deferred / blocked`，release evidence 必须绑定实际执行的 gate 与 artifact。

### [P2] 11. 关闭 compaction preview 不会取消 worker 中的 pending review

证据：

- `crates/sigil-tui/src/app/modal_flow.rs:674-688` 的 Esc 或 unavailable Enter 只清除 UI modal。
- `crates/sigil-tui/src/runner/protocol.rs:200-203` 没有 CancelReview 命令。
- worker pending 只在新 preview 时清空、preview 成功时设置、Apply 时 take（`scheduler.rs:2180-2268`）；session switch/new-session 也没有清理。
- idle auto 在 `scheduler.rs:464-468` 明确要求 pending 为空。

影响：用户以为已取消的 preview 会在后台跨 session 留存并永久挡住 idle auto；Apply 期间也没有独立 mutation-pending 状态，退出/切换的所有权不清楚。

建议：建立带 session scope 的 review/apply 状态机，增加 CancelReview，dismiss/session change 时显式清理；Apply 期间阻止退出/切换或安全 join 完成。

### [P2] 12. Snapshot、tokenizer 解析和 tokenization 同步阻塞唯一 TUI worker

证据：

- `crates/sigil-tui/src/runner/worker_loop/scheduler.rs:2180-2231` 在 command handler 中同步 prepare preview。
- `compaction_runtime.rs:587-594` 同步构建 workspace snapshot；底层会执行 git 枚举、逐文件读取/hash。
- `crates/sigil-runtime/src/portable_compaction.rs:94-102` 每次同步读取并构造 tokenizer，`compaction_token_profile.rs:143-169,333-341` 同步解析并 encode 大输入。
- `dev/governance/code-standards.md:58-64` 禁止在 async/control 路径直接执行阻塞 I/O 或长 CPU 工作。

影响：大型仓库执行 `/compact`、idle/pre-turn pressure 时可能表现为 TUI 卡死，同时拖住 cancel、shutdown 和其他 worker 命令。

建议：由有 owner/cancellation 的后台任务执行，阻塞部分使用 `spawn_blocking`；缓存已校验 tokenizer，并对 workspace snapshot 做 revision-aware cache 或增量策略。

### [P2] 13. Pre-turn compaction 成功后没有 lifecycle/notice

证据：

- `crates/sigil-tui/src/runner/worker_loop/scheduler.rs:777-809` 在 pre-turn apply + reload 成功后直接继续 queue promotion，没有发送 `V2CompactionApplied`。
- manual 和 idle 分别在同文件 `:2305-2311`、`:496-502` 发送该消息。
- App 只有在 `crates/sigil-tui/src/app/worker_bridge.rs:479-491` 收到消息后，才通过 `app/compaction_flow.rs:108-126` 生成 timeline notice。

影响：follow-up 发出前 context 已经发生 mutation，但用户完全看不到，也无法从 timeline 定位该 lifecycle，违反 TUI-first 可解释性要求。

建议：为 `PreTurnPressure` 增加明确 source，并确保成功路径恰好产生一个可 reload 的 lifecycle item/notice。

### [P2] 14. generation-bearing stream chunk 没有参与 no-consumption 分类防线

证据：

- `crates/sigil-kernel/src/agent/provider_stream.rs:117-140` 只根据 durable output refs 决定是否允许 provider 把错误分类为 pre-generation rejection。
- 同文件 `:200-351` 的 text、reasoning、tool-call 和 continuation chunks 只进入内存/UI，不把 physical attempt 标成已观察到 generation。
- `crates/sigil-kernel/src/session/provider_attempt.rs:705-708` 的判断只看已落盘 output event IDs。

当前唯一提供 typed classifier 的 OpenAI Responses 只从 HTTP status 生成该错误，因此这不是现有 admitted overflow 路径的直接复现；但通用 Provider contract 没有守住该不变量。未来 provider 若先产生 delta、再返回 typed rejection，会被错误记成 `ConfirmedNoModelConsumption` 并可能重试。

建议：第一条 generation-bearing chunk 到达时设置进程内 `generation_observed`；之后禁止 pre-generation classification，terminal 至少为 after-output 或 uncertain。补 Text/Reasoning/ToolCall/Continuation 后错误的 contract test。

### [P2] 15. 已移除的 raw legacy compaction 行可能被当作尾部损坏并修改源文件

证据：

- `crates/sigil-kernel/src/session/store.rs:69-91` 只有 raw JSON 能按当前 `SessionLogEntry` 成功反序列化时才返回 compatibility error。
- 已移除的 raw `control.compaction_applied` 不能按当前 enum 反序列化，也没有 StoredEvent envelope，因此会返回 `None`。
- `crates/sigil-kernel/src/session/recovery.rs:203-228` 把 EOF 处的 `None` 当作 tail corruption；`:68-98` 会截断并追加 recovery event，writer 在 `session/writer.rs:1026-1044` 调用该恢复。

影响：这不是要求兼容旧格式；问题是“明确拒绝且源文件不变”会退化为“把旧行当损坏并重写文件”，与 K25.2A 的 fail-closed 声明冲突。

建议：在 serde 前识别已知 raw legacy envelope shape，返回结构化 unsupported-format error，绝不能进入 tail recovery。补 removed compaction 位于 EOF/损坏尾之前的 byte-equality 测试。

## 实现完整度对账

| 能力 | 当前状态 | 结论 |
| --- | --- | --- |
| V2 event taxonomy、append identity、sidecar、append-only lifecycle | 大体完成 | durable contract、类型和 reducer 基础扎实；仍受 safe-fold 与 legacy recovery finding 影响。 |
| 手动 `/compact` preview/confirm/apply | 部分完成 | 有用户流程，但实际是 deterministic checkpoint；缺真实语义、before/after economics、cancel ownership 和 fresh-install 可达性。 |
| current objective / active plan / next actions continuity | 未完成 | 生产输入为空，TaskMemory 未投影 active plan。 |
| Portable fit/economics | 部分完成 | after-fit proof 已有；before evidence、minimum savings 与 durable metrics 未实现。 |
| 连续 3-5 次语义质量与 stale-fact removal | 未完成 | 现有测试主要验证结构/boundary，不验证长任务语义连续性。 |
| Idle automatic compaction | 部分完成 | 窄 DeepSeek 路径已接线；缺 tokenizer provisioning，取消 preview 还会阻塞它。 |
| Pre-turn pressure | 部分完成 | queue CAS/frozen path 已有；缺 fresh-install happy path和成功 notice。 |
| OpenAI Responses one-shot overflow | 部分完成 | 窄官方 snapshot 路径已实现；physical-attempt 证据、reasoning continuation 和供应链 gate 仍有缺口。 |
| Anthropic/OpenAI native compact wire + encrypted candidate | 部分完成 | provider driver/candidate materialization 已有，但明确不激活 boundary、不是用户动作。 |
| Native resolution/apply/resume | 未完成 | 没有生产调用链把 candidate 安全变为用户可用 active continuation。 |
| Model switch 上下文连续性 | 未完成 | busy rejection 是安全边界；idle fresh session 不等于可证明的 native/portable transfer。 |
| `compact -> resume -> fork -> compact -> resume` / rollback regression | 部分完成 | 有局部 fork 测试，没有完整用户链路和 rollback-past-compaction 回归。 |
| Session payload encryption | 部分完成 | AES-GCM/keyring 基础已实现；key-loss retry 与 manifest-only recovery 仍不闭环。 |
| Export rewrap / delete-time key destruction | 明确延期 | 执行计划和依赖台账均承认尚未实现。 |
| Coverage / 真实 `/compact` TUI keyboard smoke | 未提供完成证据 | completion audit 未列出；现有 Ready 测试主要手工构造 DTO，idle/pre-turn 主要是缺 proof 负例。 |

## 代码质量与架构判断

做得较好的部分：

- V2-only event、typed lifecycle、EventId correlation/causation、append/sync 和 fail-closed reducer 的整体方向正确。
- kernel 没有泄漏 DeepSeek 专属公共语义，provider-local wire/profile 基本守住 crate 边界。
- manual preview 未确认不写 durable lifecycle、raw transcript 不删除、provider-native payload 加密存放，这些原则符合仓库架构约束。
- 定向测试数量不少，错误/漂移/崩溃前沿已有较系统的 fixture 基础。

主要架构问题：

- 设计把“durable truth”“economics proof”“semantic continuity”“provider wire attempt”拆得很细，但生产装配只接上部分，导致类型看起来完整、实际用户路径缺关键输入或 owner。
- physical attempt 的 owner 在 kernel，wire retry 的 owner却留在 provider 内部，两层对“physical”的定义冲突。
- TUI worker 同时承担 command control、workspace snapshot、tokenizer loading/CPU 和 apply lifecycle，职责过重且缺异步任务所有权。
- profile identity 使用版本化内容 hash 是好方向，但没有把 resolved route/config 纳入冻结 material，Exact proof 仍可能与真实 I/O 漂移。

## 验证结果

本次实跑：

- `cargo test -p sigil-kernel compaction --lib --quiet`：61 passed。
- `cargo test -p sigil-kernel provider_continuation --lib --quiet`：39 passed。
- `cargo test -p sigil-tui compaction --lib --quiet`：29 passed。
- `cargo test -p sigil-runtime portable_compaction --quiet`：3 passed。
- `cargo test -p sigil-provider-openai-responses --quiet`：25 passed。
- `cargo clippy -p sigil-tui --all-targets -- -D warnings`：通过（并行评审执行）。
- `cargo clippy -p sigil-kernel --all-targets -- -D warnings`：通过（并行评审执行）。
- `cargo deny --offline check advisories`：**失败**，`RUSTSEC-2024-0436` / `paste 1.0.15`。
- `cargo audit --no-fetch`：退出码 0，无已知漏洞；有 `bincode`、`paste` unmaintained warning。

本次没有重跑完整 workspace test/clippy、coverage、真实 provider I/O 或真实 `/compact` 键盘 smoke。K25.17 completion audit 声称此前跑过 workspace test、fmt、clippy、docs 和 diff；本报告只把它作为既有记录，不把未在本次重跑的项目当作新证据。

## 测试缺口

- protected message 位于更晚 foldable message 之前的 plan/apply/reload/provider-projection 反例。
- ordinary chat、approved plan、task execution 的 current objective/active plan continuity。
- before/after/minimum-savings 的零收益、负收益、膨胀、整数边界和 replay fixture。
- fresh-install tokenizer setup 后 manual/idle/pre-turn 三条真实 happy path。
- worker-level manual preview -> confirm -> apply -> reload -> exactly-once 正向测试。
- dismiss preview、跨 session、Apply 中退出/切换的 ownership/cancellation 测试。
- pre-turn success 恰好一个 lifecycle notice。
- 每次实际 HTTP POST 对应唯一 physical attempt 的 retry fixture。
- streamed text/reasoning/tool-call 后 typed rejection 的 no-retry contract test。
- manifest 已 durable 但 key 丢失、manifest-only crash/restart/orphan/delete 收敛。
- raw removed legacy compaction fail-closed 且文件 byte-for-byte 不变。
- 连续 3-5 次 compact 的语义质量、resume/fork/rollback、stale-fact removal。
- coverage 与真实 `/compact` TUI keyboard smoke。

## 建议修复顺序

1. 先修 safe-fold boundary，冻结所有 apply；这是 provider-visible correctness blocker。
2. 修 physical-attempt/wire retry 与 continuation key/recovery；先恢复 durable truth 的可信度。
3. 实现 before/after economics + minimum-savings，并同步 TUI preview。
4. 实现 current objective / active plan / next actions 的真正 continuity。
5. 补 tokenizer provisioning、preview cancel ownership、pre-turn notice 和 worker responsiveness。
6. 修 OpenAI encrypted reasoning 与 DeepSeek resolved-route binding。
7. 收敛 dependency gate、capability status、coverage 和真实 TUI smoke。
8. 最后再推进 native resolution/apply、model-switch transfer、key export/delete 等明确未完成的产品能力。

## Review 限制

- 本次是当前 dirty worktree 的只读代码/完整度评审；除本报告外未修改实现文件。
- 未把工作区中与 RFC-0025 无关的既有 E21/文档改动纳入 finding。
- 未执行真实 provider 请求，因此 provider 结论来自 frozen request、wire builder、retry code、官方 API contract 与本地竞品实现的交叉验证。

## 修复执行记录（2026-07-14）

先行冻结提交为 `08a93697 fix(compaction): freeze context activation`。它让 manual、idle、pre-turn 和 overflow recovery 都在任何 checkpoint、provider count 或 active-boundary mutation 之前 fail closed；`/compact` 仍只能查看只读 fold preview。

| Finding | 修复结果 |
| --- | --- |
| P1-1 safe fold | Portable projection 现在基于 fold plan 的 retained/protected event IDs 投影 raw message；新增 protected message 位于 fold cursor 之前的 apply/reload/provider-projection 反例。 |
| P1-2 semantic continuity | `TaskMemoryV1` 提取并持久化最新用户目标与已接受 active plan；checkpoint provider projection 会渲染 active plan。当前仍是 deterministic extraction，不再把它称作已接入 LLM semantic compressor。 |
| P1-3 economics | Portable target 绑定精确 before/after evidence、绝对/PPM 最小收益门槛和重放验证；preview 显示 token 变化与阈值。 |
| P1-4 tokenizer provisioning | 新增显式 `sigil tokenizer install deepseek-v4-flash`，在下载前披露网络操作并校验 artifact；`doctor` 在缺失时给出同一修复命令。安装 tokenizer 不会解除冻结。 |
| P1-5 OpenAI reasoning | stateless reasoning request 显式请求 `reasoning.encrypted_content`；server-stored reasoning request 不多请求该字段。 |
| P1-6 physical attempt | 移除各 provider 对 429/5xx/推理 replay 400 的内部 HTTP retry；一次 logical stream 调用只会发送一次 POST。 |
| P1-7 resolved transport | DeepSeek 本地 exact proof 只接受 resolved default V4 Flash transport；自定义 route、user ID strategy 或 strict-tools policy 均 fail closed。 |
| P1-8 continuation recovery | payload key 只会在 durable manifest 不存在时创建；`Session::load_from_store` 现在拥有 recovery。修复过程中发现并修正了 startup tail-recovery 与 session identity 之间的短暂竞态。 |
| P1-9 supply chain | 记录 `tokenizers` 及其暂时无法上游消除的 build-time `paste` 例外、删除条件和复核责任；offline deny advisory gate 可通过。 |
| P1-10 status/docs | RFC-0025 改为 `frozen-pending-remediation`，旧 shipping conclusion 标记为 superseded；EN/ZH 用户文档不再声称 `/compact` 或 guarded overflow recovery 当前可 apply。 |
| P2-11 preview ownership | Esc/unavailable Enter 发送带 request ID 的 cancel；worker 仅清除匹配 pending review，并在 session 切换时丢弃它。 |
| P2-12 worker blocking | 当前所有 activation path 均在 snapshot、tokenizer parse 与 tokenization 前被硬冻结，因此发布构建没有可达的阻塞 compaction work。**解除冻结的前置条件**仍是将这些工作移交给具备 cancellation/ownership 的后台任务；不能只把常量改为 `false`。 |
| P2-13 pre-turn visibility | pre-turn 成功路径改为发送 `V2CompactionApplied { source: PreTurnPressure }`，与 manual/idle 共用恰好一个可见 timeline lifecycle，而不是无关联 notice。 |
| P2-14 generation guard | 第一条 generation-bearing chunk 会阻止后续错误被记为 `ConfirmedNoModelConsumption`；新增 text 后 rejection contract test。 |
| P2-15 legacy raw line | 识别 raw legacy `control.compaction_applied` 以及 legacy `SessionLogEntry`，返回结构化 compatibility error 且不触发 tail truncation；覆盖 EOF 损坏尾的 byte-equality 场景。 |

这轮修复不把 deferred provider-native activation、model-switch transfer、export rewrap/delete-time key destruction 或真实在线 provider/TUI smoke 伪装为已完成能力。它们仍是解除冻结后的独立交付与验收项。
