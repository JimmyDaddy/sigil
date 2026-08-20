# RFC-0065: Cache-stable Runtime Contracts V1

状态：`implemented / verified`

创建日期：2026-08-17

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0013 Eval Harness](0013-eval-harness.md)
- [RFC-0057 Cache-stable Compaction and Conversation Continuity V3](0057-cache-stable-compaction-and-conversation-continuity-v3.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0059 Durable Tool-result Artifacts V1](0059-durable-tool-result-artifacts-typed-retrieval-and-cache-stable-aging-v1.md)
- [RFC-0061 Portable Session Route Rebinding and Recovery Control Plane V1](0061-portable-session-route-rebinding-and-recovery-control-plane-v1.md)
- [RFC-0062 Harness-owned Tool Output Spooling and Result Conformance V1](0062-harness-owned-tool-output-spooling-and-result-conformance-v1.md)

## 1. Summary

Sigil 已有 deterministic tool order、hash-only cache layout proof、provider cache usage、append-only
session/control plane 和 typed tool-result contract，但还缺少四个彼此关联的运行时硬契约：

1. 动态 Context 当前作为 leading System message 重新物化；内容变化会让其后的整段 conversation prefix
   失效。
2. assistant 同批普通工具仍串行执行；现有 declaration-order settlement 尚未与 bounded safe
   concurrency 结合。
3. `FrozenProviderRequestMaterial` 能证明当前进程内 exact request，却不能从 durable session 解释每个
   provider-visible message 的来源和可重建性。
4. 真实 DeepSeek cache smoke 只重复同一个 HTTP request，没有覆盖生产 Session -> Agent -> Tool ->
   next step -> follow-up turn，也没有把 local layout proof 与 provider cache usage 关联验收。

本 RFC 吸收 DeepSeek Harness 的可移植运行时契约，但不引入共享动态插件 runtime、宿主 JavaScript VM、
Web-first 产品层或弱化 Sigil 已有 approval/sandbox/session 边界。

## 2. Decisions

### 2.1 Context V2 是 durable、append-only、provider-visible 的尾部快照

- stable memory/system messages 与 tool schema 继续位于稳定前缀。
- 每次 context resolution 生成 canonical `RuntimeContextSnapshotV2`。
- snapshot 在 provider 中使用 `user` role，但以独立 `SessionLogEntry` 记录；它不伪装成用户聊天消息，
  不进入普通 transcript/card/title/search projection。
- snapshot 只在 canonical content 改变、上一快照被 compaction surface 隐藏，或需要从非空状态显式
  clear 时追加。
- 新快照声明 supersede 之前所有 Context V2 snapshots；旧 durable event 不删除、不重排。
- snapshot message id 绑定 canonical content hash 与当前 provider-visible tail identity。相同 surface 和内容
  可重建为同一候选；compaction 后重发不会与旧 event id/message id 混淆。
- ordinary request 在构建时先 durable append，再从统一 session projection 重建 request。
- pure pre-turn/compaction candidate 可以 process-local stage 同一 snapshot；任何 frozen first request 在
  provider physical-attempt `Started` 前必须把 staged snapshot 幂等写入 durable session，并验证写入后的
  projection 与 frozen request 一致。
- Context assembly 失败继续 fail-soft 为 `ContextAssemblySkipped`，但不得删除或隐式清空上一有效
  snapshot；只有成功解析出的 empty candidate 才能形成 clear snapshot。

### 2.2 ToolScheduler V1 只并发 body，所有 durable effect 保持有序

- 每个调用在 exact args、permission subjects 和 runtime capability 已解析后得到 provider-neutral
  `ToolConcurrencyClass`。
- 未声明、解析失败、写入、shell execute、network mutation、terminal、agent lifecycle、approval pending
  均为 `Exclusive`；只有工具显式证明只读且参数未扩大 effect 时才是 `ParallelReadOnly`。
- `ParallelReadOnly` 调用进入 bounded rolling pool；`Exclusive` 调用在其前后形成 barrier。
- preview、approval、permission/effect binding 仍在唯一 kernel executor 内完成，且 guard 只能保持或
  收窄权限，不能由后续 hook 重新 allow。
- body 可以乱序完成；tool result、post-policy、artifact settlement、ControlEntry、provider-visible
  message 与产品事件只按模型 declaration order commit。
- cancel 后停止启动新 body，等待已启动任务有界收敛；未启动调用写合法 synthetic interrupted result。
  cleanup 未确认时 run 只能记录 `Interrupted`，不能伪装为 `Cancelled`。

### 2.3 ProviderRequestEnvelope V1 是隐私安全的重建证明

- 每个 physical attempt 绑定一个 provider-neutral `ProviderRequestEnvelopeV1`：route/model identity、
  canonical request hash、system/tool/history/dynamic hashes、source durable frontier、context epoch、
  cache-layout proof、reconstruction disposition 和不可重建来源摘要。
- safe durable material 必须能从封闭 frontier 重建并通过 canonical equality；raw prompt、secret、resolved
  image bytes 和 provider wire bytes 默认不进入 envelope。opaque response/continuation carrier 只有在 request
  exact value 与该 frontier 的 append-only control projection 完全相等时才可声明为 durable-reconstructable；
  任意 process-local 注入或不匹配都必须保留 `non_reconstructable` 原因。
- exact process-local overlay 必须记录 `non_reconstructable` 原因、source message id、safe projection hash 和
  exact material fingerprint binding；不能把“有 hash”宣称为跨重启 exact reconstruction。
- provider adapter 的 wire/tokenizer/profile evidence 属于 provider crate/private receipt，不泄漏到
  kernel 通用 API。

### 2.4 Cache conformance 按场景与 epoch 计量

- keyless test 逐请求断言：未发生 route/system/tool/history rewrite 的下一请求，是上一请求 provider-visible
  canonical material 的严格 prefix extension。
- 真实 provider lane 必须穿过生产 Session/Agent/tool loop，覆盖首次 user turn、tool follow-up、第二个
  user turn、resume 和 compaction epoch rotation。
- 指标至少分开：within-turn warm ratio、cross-turn warm ratio、full-session cumulative ratio、
  post-compaction first-request reset、provider miss without local mutation。
- 不把单一“99%”作为所有会话的固定承诺。SLO 按 provider route、最小稳定前缀长度和场景分别版本化；
  compaction、route/system/tool schema 变化必须作为显式 reset 解释。
- TUI 与 Desktop 普通 run 都显示同一 provider-neutral cache usage、last layout mutation 和 miss diagnostic。

## 3. Non-goals

- 不把 `sigil-kernel` 改成 provider-specific cache implementation。
- 不把全部 crate/registry 改成动态插件系统。
- 不允许模型在宿主 VM 中加载任意运行时代码。
- 不弱化 exact-args approval、prepared mutation、sandbox、network egress 或 credential boundary。
- 不把 raw secret/request bytes 为了“可重建”默认写入 JSONL、SQLite、telemetry 或 Desktop IPC。
- 不把 CLI/HTTP 提升为高于 Desktop/TUI 的产品表面。

## 4. Execution slices

### R65.0 Context V2 contract and durable projection

状态：`implemented`

- 新增 versioned snapshot entry/event、canonical renderer、clear/supersede semantics。
- ordinary build durable append 后重投影；pure candidate 只 stage，不写 session。
- frozen first request 在 pre-send barrier 前幂等 durable materialize。
- compaction/fork/resume/provider projection 覆盖新 entry。
- 验收：unchanged 不重复、changed append、clear append、resume byte/value equal、compaction re-emit、
  previous request strict prefix、ordinary transcript 不显示内部 snapshot。

### R65.1 ToolScheduler V1

状态：`implemented`

- 新增 provider-neutral concurrency classification 和 bounded scheduler。
- 首批只为明确只读 built-ins opt in；MCP 因第三方 annotations 与共享 transport/lifecycle
  状态仍 fail-closed 为 exclusive，其余亦默认 exclusive。
- 验收：bounded parallel、exclusive barrier、乱序 finish/顺序 commit、approval/deny、abort/drain、
  artifact/join cleanup、dynamic reclassification。

### R65.2 ProviderRequestEnvelope and runtime invariants

状态：`implemented`

- 新增 durable safe envelope/attempt binding、process-local exact proof 和 reconstruction verifier。
- invariant 覆盖 request reconstruction、event seq/turn state、tool call/result closure、产品 DTO committed source。
- 验收：safe exact reconstruction、ephemeral overlay explicit non-reconstructable、tamper/fork/route drift
  fail closed、日志/Debug/IPC 不泄露 exact bytes。

### R65.3 Production cache conformance and product parity

状态：`implemented_and_verified`

- keyless production composition fixture；recorded stream replay；TUI PTY 与 Desktop real-shell contract；
  key-gated real DeepSeek campaign。
- Desktop 普通 run cache telemetry 与 TUI provider-neutral parity。
- RFC-0062 Desktop real-binary acceptance 已闭合；paid provider lane 已在显式 opt-in 和成本 admission
  下通过。
- 验收报告绑定 current commit/build/route/model/config/corpus digest，验证外部 world state，不接受模型自报。

### R65.4 Selective lifecycle and diagnostics hardening

状态：`implemented`

- 只对可替换 seam 引入 RAII invocation lease 与显式 async retirement owner：retirement
  阻止新 invocation，`dispose_and_quiesce()` 等待已有 lease 归零后再 shutdown。不引入无法在
  Rust `Drop` 内 await 的名义 `RegistrationGuard`。
- doctor/debug surface 可输出实际装配的 provider/tool/context/effect surface 和 invariant status。
- 不提供 model-facing runtime mutation capability。

## 5. Validation ledger

每个 slice 先跑 targeted tests；覆盖面变化后再跑相关 crate gate。最终至少执行：

- `cargo fmt --all --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `cargo clippy --all-targets -- -D warnings`
- `pnpm --dir apps/desktop check`
- `./scripts/check-docs.sh`
- `./scripts/generate-desktop-contract.sh --check`
- keyless assembled-surface campaign
- 显式 opt-in 的真实 DeepSeek multi-turn cache conformance（需要 API key，且通过本地成本 admission）

真实 lane 只能通过带有显式付费确认的包装器执行：

```bash
python3 scripts/deepseek-real-cache-conformance.py \
  --confirm-paid-provider \
  --max-cost-usd 0.10 \
  --min-hit-ratio 0.90
```

真实付费验证不得在默认 CI 中隐式执行。若最终环境没有 opt-in/API key，代码、keyless fixture 和 admission
必须完成，但报告要明确标记 paid evidence missing，RFC 状态不得宣称该 acceptance 已完成。

### 2026-08-17 implementation ledger

- R65.0 已实现并通过 production release binary 黑盒验收：
  `scripts/context-v1-binary-acceptance.py` 经真实 `sigil run --output json` 覆盖 Rust、Python、
  JavaScript/JSX、TypeScript、Go 五种源码；15 个 provider microturn 均携带唯一 Context V2，且逐请求存在
  prefix snapshot。ignored/generated/secret-like/symlink/oversized canary 均未进入 request/session。证据位于
  `.repo-local-dev/context-v1-acceptance/context-v1-binary-acceptance.json`。
- R65.1 已实现：默认 exclusive、明确内建只读工具 opt-in、MCP 保持 fail-closed exclusive、
  最大 4 个 body 的 rolling pool、exclusive barrier、
  authorization/Started 串行、terminal/result 按 declaration order commit、取消 drain 与未启动 typed
  interruption 均有 regression coverage。
- R65.2 已实现：physical attempt 持久化 hash-only `ProviderRequestEnvelopeV1`；包含 canonical/segment hash、
  HMAC process-local fingerprint、durable frontier、context epoch 与 reconstruction disposition。重建器在
  shared lock 下读取恰好截至 frontier byte offset 的 JSONL prefix，证明 record boundary 与
  session/sequence/event/checksum，再从该 prefix（含 compaction sidecar）和显式 stable runtime/tool inputs
  走正常 request assembler，最后验证完整 canonical equality；doctor 执行同一 durable-prefix proof，并独立
  诊断 assistant tool-call 与 `ToolResultV3` 的未闭合、孤立、重复 id 和 tool-name 漂移，以及 execution audit
  未终止；legacy attempt 保持可读并给出 warning。doctor 不持有重建所需的 workspace memory/tool schema 等
  stable runtime inputs，因此不虚构完整 request equality；完整 equality 由 production composition lane 逐 attempt
  验证。
- R65.3 已实现 keyless production composition、真实 DeepSeek multi-turn ignored test、TUI/Desktop
  provider-neutral cache telemetry parity。recorded-provider lane 使用提交在仓库内的脱敏 versioned SSE
  fixture 重放 cold、warm tool turn 与 warm user turn cache usage，并验证 provider-neutral hit/miss mapping
  及重复 wire request；keyless lane 现覆盖 tool microturn、第二 user turn、resume、portable compaction 和
  新 epoch 首请求，并逐 attempt 重建 exact request；真实 DeepSeek lane 以 `2 + 1 + 1 + 1 + 1`
  Agent turn budget 硬限制为最多六次请求，并按六次 uncached input 预留费用，分别
  报告 within-tool-turn、cross-turn-resume、warm pre-compaction、second-resume、cumulative pre-compaction 与 first
  post-compaction ratio，同时断言 local history rewrite/reset。真实 lane 已具备
  `SIGIL_REAL_PROVIDER_CACHE_CONFORMANCE=1`、API key 与不高于 1 USD 的成本上限 admission。包装器在执行前
  冻结并哈希 exact test binary、
  HEAD + 当前相关源码树、route/model/config 与 scenario source；通过后把比例、pricing snapshot、
  process-log hash 和外部状态断言写入 git-ignored 本地 manifest，不记录 credential。Desktop real-binary
  已在 source-built Tauri + real `sigil serve` 下通过
  34 KiB `read_file` artifact 的两页读取、磁盘源 canary 核验、renderer reload、durable reread 与外部
  blob 缺失 fail-closed；另一路真实审批执行 34 KiB stderr + exit 7，验证 failed card、canonical paging
  与 agent continuation。Desktop/TUI typed `Expired` 及 runtime durable GC 另有回归覆盖。
- R65.3 的 key-gated 真实 DeepSeek campaign 已于 2026-08-17 在 official HTTPS route、
  `deepseek-v4-flash` 上通过。最终 attested run 严格执行 5 个 provider request，费用保守预留
  `$0.04497920`，观测到 within-tool-turn `99.03%`、cross-turn-resume `94.89%`、
  warm pre-compaction `92.90%`、cumulative pre-compaction `95.49%`、first post-compaction
  `84.34%`。manifest 同时证明 5 个 durable usage snapshot、pre-compaction local stable-prefix
  preservation、post-compaction history rewrite/reset、canonical tool-result world state 与逐 attempt exact
  request reconstruction；凭据未写入证据。第一次同场景 run 已达到缓存阈值，但暴露 envelope 无条件把已
  durable 的 DeepSeek reasoning continuation 标成 process-local；随后改为与 exact durable frontier control
  state 绑定，并把 continuation-state selection 从随机 `HashMap` 顺序改为跨进程稳定顺序。离线 carrier
  mismatch/fail-closed 与 production Agent reconstruction 回归通过后，第二次 campaign 完整通过。最终本地
  证据位于 `.repo-local-dev/deepseek-real-cache-conformance-pass-2/manifest.json`，manifest SHA-256 为
  `c3fd29461da49f322df14d7c5edbb60d5700e70f36fe099c881863cad42faf58`。
- 2026-08-18 的 V2 campaign 把付费验收扩展为严格 6 个 provider request，并增加第二次跨进程
  resume 窗口；在显式 `$0.10` admission 下按 6 次全 miss 保守预留 `$0.05397504`。观测到
  within-tool-turn `98.84%`、cross-turn-resume `94.72%`、warm pre-compaction `93.43%`、
  second-resume `91.96%`、cumulative pre-compaction `94.56%`、first post-compaction `49.33%`。
  manifest 同时证明 6 个 durable usage snapshot、逐 attempt exact reconstruction、tool-result world
  state、pre-compaction prefix preservation 与 post-compaction epoch reset；本地证据位于
  `.repo-local-dev/deepseek-real-cache-conformance-6-request-20260818/manifest.json`，manifest SHA-256 为
  `5072c59a87e02798b1202a910a63fadbd5f6adacaf09094d44bee541c43aa0bb`。
- 真实 TUI 缓存遥测黑盒验收已通过：`scripts/tui-cache-pty-acceptance.py` 以 current release
  binary、真实 PTY 和 TLS loopback DeepSeek fixture 执行两个普通 turn；durable session 含恰好
  2 个 usage snapshot，累计 `50,000 hit / 950,000 miss = 5%`，live 与第二个进程 resume
  后均显示 `cache=5%`。本地证据位于
  `.repo-local-dev/tui-cache-pty-acceptance/manifest.json`。
- R65.4 已实现 selective lifecycle owner：exact-generation RAII invocation lease、显式 async
  retirement 与 `dispose_and_quiesce()` 已接入 MCP local/remote refresh/deactivate/rollback；doctor
  同时报告近期 durable runtime invariant。
- 已通过：`cargo fmt --all --check`、`cargo test --workspace`、
  `cargo clippy --workspace --all-targets -- -D warnings`、`pnpm --dir apps/desktop check`、
  `./scripts/check-docs.sh`、Desktop contract drift check、keyless production cache conformance、release binary
  Context V2 acceptance。最终树已再次通过 `cargo check --workspace --all-targets`、
  `cargo test --workspace --no-fail-fast`、格式与 diff 检查；另通过 49 项 stateful PTY
  support regression、默认 stateful compaction/resume campaign 以及独立 cache telemetry/resume campaign。
  真实 DeepSeek 包装器的 offline parser/confinement tests 已通过，缺少
  `--confirm-paid-provider` 时在 build/provider 之前以 exit 2 拒绝。

## 6. Completion rule

只有 R65.0-R65.4 的代码、测试、文档、产品表面和所需验证全部关闭，且 RFC-0062 剩余验收同步结案，
本 RFC 才能改为 `implemented`。不得以单个 unit test、HTTP body equality、`cacheReadTokens > 0` 或某次
历史 session 的命中率代替完整验收。
