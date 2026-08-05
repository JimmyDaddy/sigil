# RFC-0062 Harness-owned Tool-output Spooling and Result Conformance V1

状态：proposed / design complete / implementation deferred

### 2026-08-04 partial implementation status

R62.0–R62.5 的核心契约已在 `worktree-rfc-0059-verify` 落地，但本 RFC **尚未满足全部 acceptance criteria**，
因此状态保持 `proposed / design complete / implementation deferred`，不得写成 implemented：

- R62.0 完成：bounded error summary（1 KiB）、capture 失败 terminal fallback、`unavailable => has_more`
  推导移除（Prompt 1）、root-run cumulative preview 失败测试翻转、并行结算改为 declaration-order batch。
- R62.1 完成：`ToolResultRecordedV3` clean cutover（新执行只写 V3；含 V2 tool-result 的 session 在
  decode 时以 bounded `UnsupportedSessionSchema` 拒绝，文件不改写、不迁移、无 alias/default）。
- R62.2 完成：harness-owned 双 staging spool（spawn 前创建 plan+sink、stdout/stderr 独立 staging、
  canonical stdout-then-stderr 双 segment、observed 128 MiB 与 preview/artifact 分离、drain-to-EOF、
  逐流 redaction 后执行 8 MiB/8 MiB reservation-reclaim 结算（combined <=16 MiB）、segments 从
  redaction 后的真实布局推导、**三轴账本基于 policy-safe 尺寸**（eligible=脱敏后完整长度、
  policy_projected=eligible 之和、truncation=persisted<eligible 或 raw staging cap 截断，
  扩张型脱敏如 token=x->token=[redacted] 完整保存且不误报）、**staging crash-safe**（unix
  unlink-after-open 使 kill -9/断电不残留原始字节，finalize 与 sink Drop 双路径兜底关闭句柄后
  删除）、write 失败标记 storage Unavailable、capture ownership 丢失显式失败）；`attach_bounded_shell_artifact`
  删除；10 MiB bash 输出完整捕获验收通过。
- R62.3 完成：root-run cumulative preview counter 删除；per-assistant-batch 两阶段 allocator
  （512 B floor、64 KiB batch cap、128 results、declaration order）接入 agent 主循环；普通工具
  执行、审批拒绝与授权错误分支全部在 assistant batch 结算（settlement 失败先 abort join
  dependencies 再尽力消费剩余 result，保证已完成线程显式终态）。
- R62.4 完成（kernel/provider 侧）：`ProviderToolResultMessageV1` + `ModelMessagePayloadV1` typed
  payload；Anthropic `is_error` 只读 typed outcome（含 contradicting-JSON fixture）；Gemini
  GenerateContent 同批 function responses 合并为单一 user Content；OpenAI/DeepSeek exact call_id
  验证；MCP stdio `isError` → 结构化 tool error（保留 actionable content）。
- R62.5 部分：`ToolArtifactAvailabilityChangedV1` 已接入 ControlEntry（generation guard、状态机、
  durable append、TUI audit 渲染）；runtime `garbage_collect_session_artifacts` 按
  durable-disable-before-delete 顺序 append，**session 加载或 disable append 失败时 GC fail-closed
  中止**（artifact body 保留）；GC 读取当前 (generation, state) 恢复中断状态（Available -> disable、
  DisabledPendingDelete -> 直接删除、terminal -> 跳过）；pressure projection 的 availability
  reducer 在**单数与复数 binding 接口**上都应用 ledger（TUI/HTTP 显示读取与模型读取一致拒绝），
  并校验 generation 连续性（乱序/跳代 fail closed）；测试
  `artifact_gc_appends_durable_disable_before_delete_and_expired_after`、
  `artifact_gc_fails_closed_when_durable_disable_cannot_be_written`、
  `availability_disable_event_denies_retrieval_binding`；
  **active-reader lease 集成、scratch quota/TTL 尚未落地**。
- R62.6 完成：TUI/HTTP/Desktop/Tauri IPC 消费同一 typed descriptor；`ToolDisplayViewV1` 携带
  `preview_truncated`/`truncation_reason`/`capture_completeness`，HTTP/Desktop DTO、OpenAPI 与
  生成的 TypeScript schema 均已同步并通过 drift check。
- R62.7 部分：V2 rejection、10 MiB 完整捕获、Anthropic/Gemini fixtures、availability 状态机、
  per-batch 预算、128-result floor 测试已过；`process_capture_canonical_hash_is_identical_across_chunk_schedulings`
  （dual-stream cap 确定性）与 `process_capture_redacts_secrets_that_span_chunk_boundaries`
  （secret 跨 chunk）已过；**PTY ordering e2e、MCP stdio/HTTP 等价 fixture、Desktop real-binary acceptance、
  paid provider smoke 未执行**。
- 全量 gate：`cargo fmt --all --check`、`cargo check --workspace`、`cargo test --workspace`（5411 passed）、
  `cargo check --workspace --target x86_64-pc-windows-gnu`（Windows target 交叉编译通过）、
  `cargo clippy --all-targets -- -D warnings`、`pnpm --dir apps/desktop check`、
  `./scripts/check-docs.sh`、`./scripts/generate-desktop-contract.sh --check` 全部通过。
- 已知残留：delegate/spawn 工具结果走 per-tool emit（batch 全量接入会改变 settle/completion 时序语义）；
  跨流 secret 拆分不参与脱敏（只按流内整体检测）；GC 物理删除后、Expired append 前崩溃时，ledger
  可能停在 DisabledPendingDelete（需 journal 化 tombstone 计划才能自动补 Expired，当前由
  DisabledPendingDelete 状态安全拒绝读取兜底）；retrieval budget 仍为 per-root-run 累计而非
  per-model-turn；Windows staging 依赖 FILE_FLAG_DELETE_ON_CLOSE + 显式 SDDL DACL
  （delete-on-close 不保证断电删除，grace GC 为兜底；本机为 macOS，Windows 行为仅通过
  x86_64-pc-windows-gnu 交叉编译验证，未做实机测试）；Windows staging 目录在创建任何文件
  前即设置 owner-only DACL（文件继承后逐文件校验），share_mode 仅保留 FILE_SHARE_DELETE；
  Windows CI 集成测试 `windows_staging_is_owner_only_before_unredacted_bytes_are_written`
  对真实 staging 路径断言：显式宽松父 ACL（DACL-only 的 everyone full control SDDL）被收窄、
  staging 目录与两条 live pipe 文件均在写入未脱敏字节前成为 protected DACL、第二 read-open
  以原始 `ERROR_SHARING_VIOLATION` 拒绝；
  不同 SID 的第二 principal 未声称（需跨账号 helper process，超出单测范围）。

创建日期：2026-08-03

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- [RFC-0005 Execution Backend](0005-execution-backend.md)
- [RFC-0012 Protocol/App-server Boundary](0012-protocol-app-server-boundary.md)
- [RFC-0013 Eval Harness](0013-eval-harness.md)
- [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0059 Durable Tool-result Artifacts V1](0059-durable-tool-result-artifacts-typed-retrieval-and-cache-stable-aging-v1.md)
- [RFC-0060 Structured Shell Risk, Approval Continuity and Terminal Execution V2](0060-structured-shell-risk-approval-and-terminal-execution-v2.md)

## 1. Summary

Sigil 已经按 RFC-0059 建立 tool-result artifact、bounded preview、typed retrieval 和 aging，但当前 Bash
大输出仍先被执行后端截成 head/tail，再由工具层把这段已经丢失中间内容的字符串保存为 artifact。与此同时，
工具失败正文仍可能进入 8 KiB facts 投影并使 agent run 失败，artifact 的完整性与可读取性也被折叠成一个
含义不稳定的 `has_more`。

这使系统出现一个反直觉结果：看起来已经“保存了完整输出”，实际保存的可能只是截断预览；命令本身已经按
预期非零退出，agent 却可能因为记录结果失败而中止；UI 提示“还有更多”时，后续字节又不一定真实可取。

本 RFC 将 process-backed tool output 的可靠性责任固定在 **harness/runtime**，不依赖模型临时决定是否使用
`> /tmp/file`、`tee` 或先写脚本。新的统一链路是：

```text
child stdout/stderr
  -> harness-owned streaming spool
  -> bounded in-memory preview + independently bounded durable artifact
  -> explicit completeness and availability
  -> bounded provider result envelope
  -> Desktop/TUI shared display projection
  -> typed, audited, session-scoped retrieval
```

模型主动重定向仍是合法的任务行为，但只是“命令本身需要文件”或“跨工具调用共享中间结果”的选择，不再是
Sigil 避免上下文爆炸、保留完整日志或保证失败可恢复的必要条件。

本 RFC 同时修正 provider 和 MCP result conformance：Anthropic 得到 `is_error`，Gemini
GenerateContent 的并行 function responses 合并为同一 user turn，MCP 两种 transport 共享
`structuredContent` / `isError` 语义；OpenAI Responses、OpenAI-compatible 与 DeepSeek 继续使用各自的
标准 tool-result wire shape。

## 2. Decision summary

本 RFC 冻结以下决定：

1. **自动 spool 属于 harness。** 所有 process-backed tools 在 spawn 前创建 call-scoped staging sink，
   stdout/stderr chunk 到达时立即写入；不得等命令结束后从 `ToolResult.content` 反向构造所谓 full artifact。
2. **preview、artifact 与 execution resource limit 分离。** preview 截断不终止进程；artifact 写满后继续
   drain 并明确标记 storage truncation；只有 observed-byte、rate、time 或其他 execution resource limit
   才能终止进程。
3. **错误正文与错误摘要分离。** 模型获得 bounded structured summary 和 bounded preview；完整、可持久化的
   stdout/stderr 进入 artifact。任意大错误正文都不得突破 facts 上限或反向使 agent loop 失败。
4. **完整性是多轴状态，不是布尔值。** source、policy、storage、retrieval 分别记录；`has_more` 只表示
   当前确实还有可读取的内容。
5. **pipe 不伪造跨流顺序。** stdout/stderr 分别保序；普通 pipe 模式声明
   `separate_pipes_no_cross_stream_order`，PTY 才可声明单一 ordered stream。
6. **provider result status 是 kernel 的通用语义。** provider adapter 不从 JSON 字符串猜测成功/失败；
   Anthropic、Gemini、OpenAI 和 MCP 都从统一 outcome 显式映射。
7. **模型可见 scratch 与 harness-private spool 分离。** `$SIGIL_SCRATCH_DIR` 用于跨调用临时工作；spool
   位于 artifact store staging namespace，模型不可见、不返回本地路径。
8. **Desktop 与 TUI 使用同一 DTO 和能力判断。** 两个表面都显示同样的结果状态、截断原因和读取能力，
   不建立某个表面专属的输出语义。
9. **当前批次预算与历史上下文压力分离。** 每个 assistant tool-call batch 使用独立、按实际 preview
   bytes 结算的 64 KiB initial-preview cap；历史 tool result 总量由 RFC-0059 的 token pressure、保护窗口和
   next-epoch aging 管理。禁止用从 root run 开始只减不回的字节桶隐藏后续当前结果。

## 3. Relationship to RFC-0059 and RFC-0060

### 3.1 Amendment to RFC-0059

RFC-0059 的 artifact、typed retrieval、aging、context epoch 和 opaque ref 方向保持不变。本 RFC 对其做以下
收窄修订：

- process-backed tool 的 “streaming sink” 必须位于 execution backend，而不是 tool 返回之后；
- “RawArtifact” 更名解释为 `PolicySafeArtifact`，不承诺保存被 persistence policy 禁止的秘密或字节；
- “完整”必须同时满足 source、policy 和 storage 三个维度，旧 descriptor 不得被推断为完整；
- artifact cap 只控制持久化，不等于 process output flood cap；
- provider result envelope 增加显式 outcome，不靠正文约定表达 tool error；
- RFC-0059 §7.4 的 root-run cumulative initial-preview cap 在 V3 cutover 后收窄为 per-assistant-batch
  byte cap；跨 batch 的历史上下文不再消耗同一字节桶，而由 RFC-0059 §11-14 的 token-aware aging
  contract 管理；
- Bash 当前“已迁移到完整 streaming capture”的实现证据不成立，必须由本 RFC 的验收重新关闭。

除上述 process capture、result semantics 与 projection-budget amendment 外，本 RFC 不重写 RFC-0059。

### 3.2 Refinement to RFC-0060

RFC-0060 继续拥有 shell parsing、permission plan、approval、sandbox、foreground/terminal 分工和 process
ownership。本 RFC 只接管执行后的 stream capture、result settlement 与展示，不改变风险分类或审批决定。

有限 `bash` 与 persistent terminal 可以共享 capture primitive，但 terminal 的滚动 ring buffer、交互输入和
lifecycle 仍由 RFC-0060 管理。

## 4. Goals and non-goals

### 4.1 Goals

- 让 10 MiB stdout 的正常命令可以 exit 0，并在 policy/cap 允许时完整、分页读取；
- 对更大输出给出可解释的 storage/resource 状态，而不是把 preview truncation 误当执行失败；
- 保证非零退出、大 stderr、artifact 写入失败和 facts 投影失败都有确定的 terminal result；
- 限制 provider token、session JSONL、run event 与 UI 内存，不丢失已经允许持久化的 artifact；
- 统一 Bash、terminal、未来 process-backed tools 与 MCP result 的结果语义；
- 修正 Anthropic、Gemini GenerateContent 和 MCP 的协议映射；
- 为 Desktop/TUI 提供同一份可恢复、可审计的输出状态。

### 4.2 Non-goals

- 不把任意 shell 输出自动解释成结构化业务数据；
- 不向模型或 renderer 暴露 artifact 的真实文件路径；
- 不允许 artifact 绕过 secret redaction、workspace trust 或 retention policy；
- 不在本 RFC 中迁移 Gemini Interactions API；
- 不替代 task-specific 文件输出，例如编译器生成报告、测试框架生成 JUnit 或用户明确要求的文件；
- 不改变 tool approval、sandbox 或 mutation receipt contract；
- 不保证普通 stdout/stderr pipe 的全局时间顺序。

## 5. Confirmed current gaps

以下问题均由当前源码确认，不是纯推测。

### 5.1 Bash artifact 保存的是已截断结果

`crates/sigil-tools-builtin/src/execution_backends/output.rs` 的 collector 每个 stream 只保留 bounded
head/tail；达到更高 output alert 后通知 backend 清理 child。`shell.rs` 在进程结束后先拼接这两个 bounded
stream，再由 `attach_bounded_shell_artifact` 把 `result.content` 保存为 artifact。

因此当前 artifact 没有捕获已经丢弃的中间字节。`read_tool_artifact` 能分页读取 artifact，不等于 artifact
本身是完整原始输出。

### 5.2 非零退出可能被记录层放大为 agent failure

`shell.rs` 把非零退出的完整 bounded content 克隆到 `ToolError.message`；
`crates/sigil-kernel/src/session/tool_artifact.rs` 又把 error message 放入 facts，而 facts 上限为 8 KiB。
capture 返回错误后，agent tool-call processing 使用 `?` 传播，最终可使整个 run 失败。

工具失败应该成为模型可恢复的 tool result，而不是因为错误正文太长变成 harness failure。

### 5.3 三种容量上限被混为一谈

当前 `OutputAlert` 可直接触发 child cleanup。这把下列独立问题折叠成了同一个动作：

- 模型预览是否太长；
- durable artifact 是否达到 retention/cost 上限；
- child 是否产生了不可接受的资源洪泛。

结果是“不能继续存”或“不能继续展示”会被误解释为“命令必须被杀掉”。

### 5.4 stdout/stderr 拼接伪造展示顺序

当前 Bash result 先拼 stdout，再拼 stderr。两个 pipe 分别读取时无法证明跨流时间顺序；简单拼接既不是
真实 interleave，也不利于独立分页和诊断。

### 5.5 preview budget 的计费与作用域错误

V2 session facade 原先在捕获每条 result 时按单条最大 preview 预留并扣除 root-run budget；紧急修复已改为
按最终实际 emitted bytes 计费，但预算仍绑定整个 root run。长任务即使每一批只有一个很小结果，也会在历史
累计达到 64 KiB 后把新的当前结果降为零正文。并行 result 若按完成顺序结算还会产生非确定差异。

V3 必须移除 root-run cumulative byte bucket：64 KiB 只约束同一 assistant tool-call batch 的初始 preview；
历史累计压力由 token-aware aging 处理，不能反向剥夺当前 batch 的最低可见性。

### 5.6 `has_more` 同时表达不完整和不可读

现有 artifact hint 对已发布 artifact 普遍提示 preview 可能不完整；`has_more` 又可能因为 artifact
unavailable 被设为 true。此时 UI/模型看到“还有更多”，但并不存在可成功执行的下一页读取。

### 5.7 Provider 和 MCP 映射不完整

- Anthropic adapter 会合并 consecutive tool results，但未输出 `is_error: true`；
- Gemini GenerateContent adapter 把每条 tool result 生成为独立 user content，未按 parallel function call
  规则合并同批 responses；
- stdio MCP 把 JSON-RPC 成功响应一律当作 `ToolResult::ok`，忽略 result-level `isError`；
- Streamable HTTP MCP 能解析 `isError`，但会用通用错误文本替换 server 返回的可行动内容；
- MCP 两个 transport 没有共享同一个 `content`、`structuredContent`、`isError` conformance path。

## 6. Internet research

### 6.1 Model behavior or harness behavior?

两种行为必须区分：

| Observable behavior | Owner | Reliability |
| --- | --- | --- |
| command 本身包含 `> file`、`2>&1` 或 `tee` | 模型基于训练、prompt 和 tool description 作出的计划 | 启发式，不保证每次发生 |
| command 没有重定向，但结果显示“完整输出已保存，可分页读取” | harness/runtime 自动捕获 | 可设计、可测试、可审计 |
| 工具要求模型稍后用 `read` 读取一个临时路径 | 可能是 harness 的降级 UI，也可能是模型主动文件工作流 | 取决于路径所有权和生命周期 |
| tool result 返回 opaque artifact ref 与 bounded preview | harness contract | 推荐的稳定边界 |

模型可以通过 prompt 更频繁地选择临时文件，但 prompt 不能提供 crash consistency、cap、权限、GC、secret
policy 或 provider conformance。可靠行为必须由工具 schema、executor 和 session harness 提供。

### 6.2 Official protocol findings

- OpenAI Shell guide 把 local shell execution、stdout/stderr capture、outcome 和 `max_output_length` 交给
  integration runtime；非零退出也应保留输出，让模型判断恢复方式。
- OpenAI function calling 只要求用 `call_id` 返回 `function_call_output`，output 可以是 JSON、错误或文本；
  因此 Sigil 可以使用统一 bounded JSON envelope，而无需把完整正文塞回 context。
- Anthropic 要求并行 tool results 位于下一条同一 user message，并通过 `is_error` 表达工具错误。
- Gemini GenerateContent 的同批 function responses 应在下一个 user block 中一起返回，并通过 function
  identity/call ID 与调用关联；官方协议不把 response 数组的 declaration order 规定为唯一关联依据。Sigil
  仍为 deterministic replay、cache stability 和跨 adapter 一致性固定使用 tool-call declaration order；
  thought signature 保持在关联的 assistant function call 上。
- MCP tool result 原生包含 `content`、可选 `structuredContent` 和 `isError`；tool execution error 应作为
  `isError: true` result 返回，让模型能自我修正，而不是升级成 protocol error。

### 6.3 Current coding-agent implementations

调研使用 2026-08-03 可访问的 upstream HEAD；具体常量可变化，但 ownership 结论稳定。

| Project | Current implementation | Lesson for Sigil |
| --- | --- | --- |
| OpenAI Codex | `output-truncation` 在 harness 侧按 byte/token 做 middle truncation | bounded model view 不需要模型主动写临时文件 |
| Gemini CLI | `toolOutputMaskingService` 把大 tool output 保存到 session-specific tool-output 目录，并返回 preview/path marker | 自动 offload 是 harness 行为；Sigil 应进一步使用 opaque ref 而非路径 |
| OpenCode | `tool-output-store` 保存完整内容，返回 bounded head/tail，并执行 TTL cleanup | managed retention/GC 必须与 offload 一起设计 |
| Goose | `large_response_handler` 在阈值后写 tempfile 并返回路径 | 证明这一行为通常由 harness 实现；写入失败时回退完整正文并不安全，不采用 |

竞品不是本 RFC 的规范来源；协议规范与 Sigil 的安全、持久化约束优先。

## 7. Required invariants

实现必须同时满足：

1. **Bounded memory**：持续输出不要求在内存中 materialize 完整 `String`；
2. **Bounded context**：provider message、facts、run event 和 UI projection 都有独立硬上限；
3. **Truthful completeness**：任何被截断、discard、redact、读取失败或中断的输出都不能标记为 full；
4. **Durable-before-reference**：descriptor append 前 artifact 已完成 publish；
5. **No path authority leak**：模型、provider、Desktop renderer 和 TUI projection 只获得 opaque ref；
6. **Error as data**：tool execution error 默认是可恢复 result，不是 agent runtime failure；
7. **Harness failure is explicit**：spool/persistence/session writer failure 不伪装成 tool error；
8. **No blind retry**：结果持久化失败时不得自动重复执行可能有副作用的 tool；
9. **Deterministic projection**：并行 tool results 按 assistant 声明的 tool-call 顺序分配 preview budget；
10. **Provider-neutral kernel**：kernel 不引入 Anthropic、Gemini、DeepSeek 专属字段名；
11. **Surface parity**：Desktop/TUI 从同一 durable truth 派生状态；
12. **Policy before persistence**：未经允许的敏感正文不进入 artifact 或 crash dump。
13. **Protect current results**：历史 tool output 无论多大，都不能把新的 current batch preview 预算降为
    零；同一 oversized batch 也必须先为每个可安全显示正文的 result 分配 bounded minimum preview，再分配
    剩余预算。

## 8. Target architecture

```text
                     +----------------------------+
                     | ToolExecutionCapturePlanV1 |
                     +-------------+--------------+
                                   |
                         create before spawn
                                   v
+---------+ chunks  +--------------+---------------+
| child   +-------->| ProcessOutputCapture          |
| process |         | - stdout/stderr framing       |
+---------+         | - policy-safe stream filter   |
                    | - preview head/tail           |
                    | - artifact staging writer     |
                    | - observed resource meter     |
                    +------+-------------+----------+
                           |             |
                    finalize result   enforce only
                           |          execution caps
                           v
              +------------+----------------+
              | ToolResultRecordedV3         |
              | facts + descriptor + outcome |
              +------+-----------------------+
                     |
          +----------+-----------+-------------------+
          |                      |                   |
          v                      v                   v
  provider envelope      Desktop/TUI DTO    read_tool_artifact
  bounded + ordered      shared projection   bounded + audited
```

ownership 固定为三层，不使用含糊的 `sigil-process`/execution-backend 共享边界：

1. `sigil-kernel` 拥有 session-aware capture plan、policy-bound artifact sink contract、durable descriptor、
   availability event、retrieval authorization、provider-neutral outcome 和 materialization contract；
2. `sigil-tools-builtin` 的 execution backend 或其他执行工具 crate 拥有 pipe reader、preview collector、
   resource meter、sink 驱动和 settlement orchestration；tool-specific projector 也留在对应 tool crate；
3. `sigil-process` 只拥有 child/process-tree lifecycle、platform capability probe 和原始 pipe/PTY handle handoff；
   它不依赖 session、tool call、artifact store、retention、persistence policy 或 provider 类型。

`sigil-runtime` 只负责把 kernel artifact service/policy-bound sink factory 装配进 tool registry，不重新实现
capture 语义。renderer 不直接读取 artifact store。

## 9. Data contracts

类型名是冻结的设计目标；实现可在不改变语义和 serialized field 的前提下调整 Rust 内部拆分。

### 9.1 Capture plan

```rust
pub struct ToolExecutionCapturePlanV1 {
    pub session_id: SessionId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub media_type: String,
    pub stream_layout: ToolOutputStreamLayoutV1,
    pub preview_limit_bytes_per_stream: u64,
    pub artifact_limit_bytes_combined: u64,
    pub artifact_reservation_stdout_bytes: u64,
    pub artifact_reservation_stderr_bytes: u64,
    pub artifact_staging_limit_bytes_per_stream: u64,
    pub observed_limit_bytes_combined: u64,
    pub retention_class: ToolArtifactRetentionClass,
    pub persistence_policy: ToolOutputPersistencePolicy,
}

pub enum ToolOutputStreamLayoutV1 {
    SeparatePipesNoCrossStreamOrder,
    PtyOrdered,
    SingleStream,
}
```

plan 是执行前不可变输入，并纳入 execution audit。tool 不可在输出变大后自行放宽 cap。对于
`SeparatePipesNoCrossStreamOrder`，两个 reservation 之和必须等于 combined payload cap；single stream/PTY
只为实际 stream 保留全部 quota。

`ToolExecutionCapturePlanV1` 是 `sigil-kernel` 的 session-aware contract。execution backend 从中派生不含
session/artifact authority 的本地 `ProcessStreamCaptureConfigV1`，并接收一个已经绑定 policy 和 call authority
的 opaque sink handle。`sigil-process` 只返回 child/pipe handle，既不接收这两个 plan，也不构造 artifact ref。

```rust
pub struct ProcessStreamCaptureConfigV1 {
    pub stream_layout: ToolOutputStreamLayoutV1,
    pub preview_limit_bytes_per_stream: u64,
    pub artifact_payload_limit_bytes_combined: u64,
    pub artifact_reservation_stdout_bytes: u64,
    pub artifact_reservation_stderr_bytes: u64,
    pub artifact_staging_limit_bytes_per_stream: u64,
    pub observed_limit_bytes_combined: u64,
}
```

### 9.2 Segment descriptor

```rust
pub struct ToolOutputSegmentV1 {
    pub stream: ToolOutputStreamV1,
    pub artifact_offset: u64,
    pub persisted_bytes: u64,
    pub eligible_bytes: u64,
    pub observed_bytes: u64,
    pub preview_bytes: u64,
    pub preview_truncated: bool,
    pub storage: ToolStorageCompletenessV1,
}

pub enum ToolOutputStreamV1 {
    Stdout,
    Stderr,
    Combined,
}
```

普通 pipe 不使用 reader-arrival interleaved frame 作为最终 artifact。两个 reader 分别写入 bounded staging，
finalize 后生成 canonical manifest，正文固定按 `stdout`、`stderr` 顺序各写成至多一个连续 segment；因此
descriptor 有界且 selector offset 稳定。该顺序只表示存储布局，不表示跨流时间顺序。PTY 使用单一
combined ordered segment。

V1 的普通 pipe 默认 payload budget 为 16 MiB，stdout/stderr 各预留 8 MiB；每个 stream staging 最多
保留 16 MiB policy-safe eligible bytes。settlement 后按以下确定算法回收另一条流未使用的 reservation：

```text
unused_stdout = stdout_reservation - min(stdout_eligible, stdout_reservation)
unused_stderr = stderr_reservation - min(stderr_eligible, stderr_reservation)

stdout_persisted = min(stdout_eligible, stdout_reservation + unused_stderr)
stderr_persisted = min(stderr_eligible, stderr_reservation + unused_stdout)
```

`eligible` 指 persistence policy/redaction 后允许保存的字节，不是 raw observed bytes。算法与 reader 调度、
chunk 大小和 EOF 到达顺序无关，且 persisted 总量不会超过 combined cap。canonical artifact hash 覆盖 bounded
manifest 和选定的连续 stdout/stderr bytes；manifest 具有独立 4 KiB hard cap，不计入 16 MiB stream payload
budget。相同 policy-safe stream bytes 必须得到相同 segment、offset 和 hash。每个 segment 独立记录 storage
completeness；result-level storage 只要任一有输出的 segment truncated 就归约为 `TruncatedAtLimit`。

### 9.3 Completeness

```rust
pub struct ToolResultCaptureCompletenessV1 {
    pub source: ToolSourceCompletenessV1,
    pub policy: ToolPolicyCompletenessV1,
    pub storage: ToolStorageCompletenessV1,
}

pub enum ToolSourceCompletenessV1 {
    Complete,
    Interrupted,
    ResourceLimited,
    ReaderFailed,
}

pub enum ToolPolicyCompletenessV1 {
    Preserved,
    Redacted,
    EphemeralOnly,
    Rejected,
}

pub enum ToolStorageCompletenessV1 {
    Complete,
    TruncatedAtLimit,
    Unavailable,
}

```

capture completeness 是 tool settlement 时冻结的历史事实，后续 GC、磁盘故障或 retention 变化不得改写。
动态读取状态由 §9.4 的 append-only availability projection 提供。

派生字段定义：

```text
full_output_available =
  source == Complete
  && policy == Preserved
  && storage == Complete
  && current_availability.state == Available

has_more_retrievable =
  current_availability.state == Available
  && persisted_bytes > bytes_represented_by_current_view
```

`Redacted + Complete storage` 只能声称“完整保存 policy-safe projection”，不能声称“完整原始输出”。

### 9.4 Append-only artifact availability

retrieval availability 是 artifact 发布后会随 GC、损坏或外部丢失变化的 control state，不属于 immutable
capture completeness。`ToolResultRecordedV3` 只保存 immutable artifact descriptor 和
`initial_availability`：成功 publish 时为 `Available`，没有 artifact ref 时为 `Unavailable`。后续状态只能由
append-only event 改变：

```rust
pub struct ToolArtifactAvailabilityChangedV1 {
    pub artifact_ref: ToolArtifactRef,
    pub expected_generation: u64,
    pub generation: u64,
    pub previous: ToolArtifactAvailabilityStateV1,
    pub next: ToolArtifactAvailabilityStateV1,
    pub reason: ToolArtifactAvailabilityReasonV1,
    pub changed_at: Timestamp,
}

pub enum ToolArtifactAvailabilityStateV1 {
    Available,
    DisabledPendingDelete,
    Expired,
    Missing,
    HashMismatch,
    Unavailable,
}
```

projection 以 descriptor 的 `initial_availability`/generation 0 为 seed，按 session log 顺序归约匹配
`expected_generation` 的事件；generation 必须单调递增。stale、重复或非法状态迁移作为 bounded corruption
diagnostic 拒绝，不做 last-write-wins 猜测。Desktop、TUI、HTTP、provider model view 和
`read_tool_artifact` admission 都读取这份 current projection，不直接相信 descriptor 的初始值。

允许的 V1 状态迁移：

```text
Available -> DisabledPendingDelete -> Expired
Available -> Missing
Available -> HashMismatch
Available -> Unavailable
DisabledPendingDelete -> Expired
```

terminal state 不返回 `Available`；恢复 artifact 需要未来独立的 republish contract，V1 不支持原地复活。

GC 固定使用 durable-disable-before-delete：

1. typed retrieval admission 在 `Available` generation 上获取 bounded active-reader lease；
2. append + fsync `Available -> DisabledPendingDelete`，current projection 立即拒绝新的 reader lease；
3. 等待旧 generation 的 active-reader lease drain；超时则取消本轮 GC，不删除 body，由 reconciler 重试；
4. 删除 artifact body，并 fsync 必要目录元数据；
5. append + fsync `DisabledPendingDelete -> Expired`；
6. 如果步骤 2 后崩溃，body 可以暂时存在但不可接受新读取，reconciler 等待旧 lease 后重试删除；
7. 如果步骤 4 后、步骤 5 前崩溃，状态仍安全地拒绝读取，reconciler 检测 body 缺失并完成 `Expired`；
8. 不允许先删 body 再追加 disable event，也不允许在旧 reader lease 尚未 drain 时删除，因为两个窗口都会
   破坏真实读取能力。

读取时发现 missing 或 hash mismatch，typed retrieval 先拒绝返回正文，再以 generation guard append 对应
availability event；并发读/GC 根据同一 generation lease/admission 收敛。availability event 和 read receipt
都只记录 metadata，不记录 artifact body。

### 9.5 Outcome and bounded error summary

```rust
pub struct ToolResultWireSemanticsV1 {
    pub outcome: ToolResultOutcomeV1,
    pub error_kind: Option<ToolErrorKindV1>,
}

pub enum ToolResultOutcomeV1 {
    Success,
    ToolError,
}

pub struct ToolErrorSummaryV1 {
    pub kind: ToolErrorKindV1,
    pub message: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub retryable: bool,
}
```

`ToolErrorSummaryV1.message` UTF-8 边界安全，默认上限 1 KiB。stderr/body 不复制到 message；模型通过
bounded preview 和 artifact ref 获取细节。

`HarnessFailure` 不进入 `ToolResultOutcomeV1`，而是 agent/session control-plane 的独立 terminal reason。

### 9.6 Provider-facing typed materialization

`ToolResultRecordedV3` 不直接交给 provider adapter，也不允许 adapter 从 canonical JSON 的 `status` 猜测
tool outcome。kernel 在每次 provider request materialization 时构造 transient、provider-neutral 的 typed
payload：

```rust
pub enum ModelMessagePayloadV1 {
    Text {
        content: String,
    },
    AssistantToolCalls {
        content: String,
        tool_calls: Vec<ModelToolCall>,
    },
    ToolResult(ProviderToolResultMessageV1),
}

pub struct ProviderToolResultMessageV1 {
    pub call_id: ToolCallId,
    pub output: String,
    pub wire_semantics: ToolResultWireSemanticsV1,
}
```

`ModelMessage` 的 provider-facing contract 在 V3 cutover 时使用 `ModelMessagePayloadV1`；tool result 不再只由
flat `role/content/tool_call_id` 三元组表达。`output` 是已经完成 batch budget 分配的 bounded canonical
envelope，`wire_semantics` 是 adapter 映射协议 error flag 的唯一权威来源。`CompletionRequest.messages` 中的
tool-result entry 必须携带这个 typed payload，不能在 request assembly 前把它重新压平成字符串。

固定 materialization 链路：

```text
live/restore ToolResultRecordedV3
  -> apply current deterministic aging/projection policy
  -> ProviderToolResultMessageV1 { call_id, bounded output, wire_semantics }
  -> provider adapter wire mapping
```

约束：

- live execution、session restore、context epoch/aging 后重新 materialize 都从 durable V3 的
  `wire_semantics` 恢复 outcome；aging 可以缩小 `output`，不能改变或删除 outcome；
- adapter 必须 pattern-match `ModelMessagePayloadV1::ToolResult`，不得解析 `output.status` 决定
  Anthropic `is_error` 或其他协议状态；
- `call_id` 只有一个 typed authority，adapter 不再在 generic content 与旁路字段之间择一；
- malformed payload 在进入网络前作为 bounded local contract error 失败，不降级回 flat string；
- `read_tool_artifact` 是新的 tool call，拥有自己的 typed outcome，不借用被读取 artifact 的旧 outcome。

provider fixture 必须构造“typed outcome 为 `ToolError`，但 output 中省略或故意写入相反 `status`”的测试，
证明 Anthropic `is_error` 等 wire field 只依赖 typed semantics；另需覆盖 restore 与 aging 后 outcome 不丢失。

### 9.7 Durable record version

新 execution 写 `ToolResultRecordedV3`，至少包含：

- V2 已有 facts、model projection、artifact descriptor 与 audit identity；
- `wire_semantics`；
- `stream_layout` 与 `segments`；
- immutable `capture_completeness` 与 artifact `initial_availability`；
- preview 的实际字节数和 truncation reason；
- capture-plan hash 与 artifact hash。

当前项目处于开发期，本 RFC 采用严格 clean cutover，并服从
`dev/governance/code-standards.md` 的 current-schema-only 规则：

- V3 decoder 不读取 `ToolResultRecordedV2`，不添加 serde alias/default，不迁移、不补字段、不推断
  capture completeness；
- 含 V2 tool-result event 的旧 session 作为旧 schema 整体不可用，返回 bounded
  `UnsupportedSessionSchema { expected: "tool-result-v3", found: "tool-result-v2" }` 诊断；
- 不支持 V2/V3 mixed-version session，也不把 V2 materialize 给 provider、Desktop/TUI 或 aging pipeline；
- 若开发环境需要保留旧日志，用户只能在外部备份后创建新 session；本 RFC 不提供 silent importer；
- schema/version fixture 必须证明 V2 被稳定拒绝、错误不包含 artifact body，且不会部分恢复后继续执行工具。

未来若产品阶段必须迁移旧 session，应先单独修订治理规范并提交独立 migration RFC；不得在本 RFC 的实现中
顺手加入兼容分支。

## 10. Streaming capture protocol

### 10.1 Before spawn

执行后端按以下顺序准备：

1. 解析并冻结 permission/execution plan；
2. 通过 kernel sink factory 绑定 session/call/persistence policy，并创建 artifact-store 内 call-scoped staging；
3. execution backend 派生 `ProcessStreamCaptureConfigV1`，初始化 stdout/stderr bounded preview collectors；
4. execution backend 初始化 observed byte/rate/time meters；
5. `sigil-process` 创建 child/process-tree 并只交回 pipe/PTY handle；
6. 只有上述必要资源成功后才开始读取 child output。

staging object 名称随机化、owner-only，不进入环境变量，也不返回给模型。

### 10.2 While running

每个 chunk 进入统一 capture pipeline：

1. 更新 observed counters；
2. 检查 execution resource policy；
3. 经过 persistence policy/redaction stream filter；
4. 在 artifact cap 内写 staging；
5. 更新 bounded head/tail preview；
6. 发布 coalesced progress metadata，不发布无界正文。

跨 chunk secret pattern 必须用 overlap window 或等价 streaming matcher 处理，不能因 token 刚好跨 chunk
边界而漏检。无法安全完成 policy projection 时停止持久化正文、标记 `policy = Rejected` 或
`storage = Unavailable`，但仍按资源策略 drain child；不得把未审查原文作为 fallback 放进 result。

### 10.3 Independent limits

V1 默认建议值：

| Limit | Default | Effect when reached |
| --- | ---: | --- |
| in-memory preview | 64 KiB / stream | 继续执行、继续 spool，只保留 deterministic head/tail |
| durable artifact payload | 16 MiB / call | pipe 按 8 MiB/8 MiB 预留并确定性回收；超出部分停止持久化，继续 drain |
| per-stream staging | 16 MiB / stream | 只为 finalize quota 回收保留 bounded policy-safe candidate；不扩大 published cap |
| observed output | 128 MiB / call | 按 resource policy 终止 process tree，标记 `ResourceLimited` |
| facts | 8 KiB / result | projector 降级，不影响 tool terminal settlement |
| error summary | 1 KiB / result | UTF-8 safe truncation，完整错误留在 artifact |
| assistant-batch model preview | 64 KiB / batch | 两阶段按实际 emitted bytes 和声明顺序分配；不跨 batch 累计 |

这些默认值需要通过 eval 与产品成本数据校准，但三类 limit 的语义不可重新合并。

预期行为示例：

- 10 MiB stdout、exit 0：artifact 完整，命令成功，可分页读取；
- 10 MiB stdout + 10 MiB stderr：各持久化前 8 MiB，两个 stream 都标记 storage truncated；
- 100 MiB stdout、exit 0：在 16 MiB 后停止持久化，但可继续执行到成功，明确 storage truncated；
- 超过 128 MiB：终止 process tree，outcome 为 tool error，source 为 resource limited；
- 20 MiB stderr、exit 2：error summary bounded；artifact 保存前 16 MiB policy-safe bytes；agent 可继续恢复。

### 10.4 Settlement ordering

正常 settlement：

```text
child settled
  -> drain readers
  -> finalize policy/completeness
  -> fsync staging body and metadata
  -> atomic publish immutable artifact
  -> append ToolResultRecordedV3 descriptor
  -> publish run/UI projection
```

artifact publish 失败时记录 `storage = Unavailable`、无 artifact ref、
`initial_availability = Unavailable` 和 bounded terminal fallback；session append 失败则进入明确
`PersistenceFailureAfterToolExecution`，停止继续 agent loop，不重试 tool。恢复逻辑可根据 execution
receipt 与 staging GC 状态提示用户，但不能假设工具没执行。

### 10.5 Terminal fallback

即使 tool-specific facts projector、preview renderer 或 artifact finalize 失败，kernel 也必须能构造小型
`ToolResultTerminalFallbackV1`：

```rust
pub struct ToolResultTerminalFallbackV1 {
    pub tool_call_id: ToolCallId,
    pub outcome: ToolResultOutcomeV1,
    pub failure_stage: ToolResultFailureStageV1,
    pub summary: String,
    pub artifact_published_at_settlement: bool,
}
```

fallback 本身固定小于单条 durable event hard limit。只有 session writer 不再可用时才升级为 control-plane
failure。

## 11. Projection and budget allocation

### 11.1 Tool-specific facts

事实投影只包含结构化且 bounded 的诊断信息，例如：

- command exit code、signal、duration；
- stdout/stderr observed/persisted bytes；
- changed-files receipt 或 verifier summary；
- MCP structured result 的 schema-safe bounded facts；
- artifact ref、capture completeness 与可用 selectors。

current availability 不复制进 immutable facts；provider/display materialization 在读取 descriptor 和最新
availability events 后把它作为动态 overlay 注入 bounded view。

它不复制完整 stderr、stack trace 或 tool body。projector 必须提供 deterministic shrink ladder：

```text
full bounded facts
  -> drop optional arrays/details
  -> compact counters/status
  -> ToolResultTerminalFallbackV1
```

facts 超限不得返回使 agent run 中止的普通 capture error。

### 11.2 Current assistant-batch initial projection

initial preview 的 byte cap 只负责约束当前 assistant tool-call batch，不负责长期历史治理。默认：

```text
per-result target/hard cap   = tool-specific 8/16 KiB
current-result preview floor = 512 B when safe text exists
assistant-batch preview cap  = 64 KiB
max results per batch        = 128
```

512 B floor 与 128-result hard limit 保证最坏情况下 floor 总量不超过 64 KiB；无正文、binary-only、policy
rejected 或只能安全显示 structured facts 的 result 不虚构 preview，也不占正文 floor。

同一 assistant turn 的多个 tool calls 可以并行执行，但 initial model result commit 固定为：

1. 各 call 独立产生 bounded candidate；
2. 等本批需要返回的结果 settlement；
3. 按 assistant message 中 tool-call declaration order 排序；
4. 第一遍按声明顺序为每个有 safe text 的 candidate 分配 `min(candidate_bytes, 512 B)`；
5. 第二遍按声明顺序，在各自 tool-specific cap 内分配剩余 64 KiB budget；
6. 只按最终实际写入 preview 的 UTF-8 bytes 结算，不按 per-result maxima 预扣；
7. budget 不足的正文退化为 bounded facts + truthful completeness + opaque ref，不受 wall-clock completion
   order 影响，也不改变 typed outcome。

这同时满足 provider batch grouping、cache stability 和 deterministic replay。declaration order 是 Sigil 的
内部确定性 contract；仅在 provider 官方协议另有明确要求时才称为 wire requirement。一个新的 assistant
batch 总是获得新的 64 KiB budget；过去 batch 的 preview bytes 不参与本批分配。

### 11.3 Historical context pressure and latest-result protection

跨 batch 的 provider context 只使用 token/fit pressure 治理，不再维护 root-run remaining bytes：

1. 每次 provider request materialization 先保护 current batch、current turn、active/unpaired result、
   high-signal facts 和 RFC-0059 的 recent protected tool-token window；
2. 若请求存在 fit pressure，从 oldest eligible result 开始，结合 token size、signal density 和 artifact
   recoverability 选择历史 batch；
3. 先生成 deterministic facts/ref aged view，达到 minimum reclaim threshold 后才准备 candidate；
4. aging 只通过 exact-frontier、exact-epoch CAS 激活到 next context epoch，不原地改写已经发送的 bytes；
5. 仍不能 fit 时才进入 RFC-0057 semantic compaction；若移除全部可回收历史后 protected current batch
   自身仍不能 fit，只能运行 §11.1 的 optional-facts shrink ladder，最终返回明确 context-pressure terminal
   state，不能突破 batch hard cap，也不能通过清空 current preview floor 偷渡请求。

因此“保护最新结果”表示：历史压力不能删除本批已经获得的 initial projection；同一 oversized batch 内仍受
§11.2 的 per-result/batch hard cap，但每个可安全显示正文的 result 先获得 minimum preview，且 outcome、facts、
artifact ref 永不因正文预算而消失。

### 11.4 Four independent accounting planes

```text
capture bytes
  -> observed/resource meter + policy-safe artifact cap

current-batch preview bytes
  -> per-result byte cap + 64 KiB assistant-batch cap

historical provider tokens
  -> protected window + oldest-eligible deterministic aging + semantic compaction fallback

retrieval bytes/tokens
  -> read_tool_artifact per-call/per-turn budget + durable receipt
```

四个 plane 分别记 telemetry，任何一个达到上限都不能被解释为另一个 plane 的失败。特别是 artifact body、
TUI/Desktop display page 和 provider preview 不共享累计 counter；UI 可以流式展示或分页读取完整可用 artifact，
但 renderer 不常驻 materialize 全文。

### 11.5 Durable ordering and replay

每个 call 可以独立完成 artifact publish；batch allocator 只在本批所有需要返回的 result settlement 后运行。
allocator 先冻结 ordered call IDs、candidate hashes、policy version 和最终 per-call preview byte allocation，再按
declaration order append `ToolResultRecordedV3`。首个 post-tool provider request 只有在全部 result append 成功后
才能发送。

若中途 session append 失败，run 进入 explicit persistence failure，不发送 partial provider batch、不重跑
tool；已发布但未引用的 artifact 按 orphan grace 清理。restore 只重放已经 durable 的 V3 result 和 projection
hash，不根据 wall-clock completion order 重新分配预算。

## 12. Provider result conformance

kernel 先按 §9.6 构造 `ProviderToolResultMessageV1`；adapter 只做 wire mapping，不解析 `output` 正文猜
outcome。

| Provider path | Required mapping |
| --- | --- |
| OpenAI Responses | `function_call_output` + exact `call_id`; `output` 为 bounded canonical JSON string |
| OpenAI-compatible Chat Completions | `role = tool` + exact `tool_call_id` + bounded content |
| DeepSeek | 保持 provider crate 内的标准 `role = tool` mapping，不向 kernel 泄漏专项字段 |
| Anthropic Messages | consecutive/parallel results 合并到下一条 user message；error block 设置 `is_error: true` |
| Gemini GenerateContent | 同批 function responses 合并到下一条 user content；按 identity/call ID 关联；Sigil 额外固定声明顺序；thought signatures 原样关联 |

canonical JSON 至少包含：

```json
{
  "status": "success | error",
  "summary": "bounded human-readable summary",
  "facts": {},
  "preview": "bounded preview",
  "artifact": {
    "ref": "opaque-session-scoped-ref",
    "available": true,
    "has_more_retrievable": true
  },
  "completeness": {
    "source": "complete",
    "policy": "preserved",
    "storage": "complete"
  }
}
```

其中 `artifact.available` 和 `has_more_retrievable` 在每次 request materialization 时来自 current
availability projection，不是 `ToolResultRecordedV3` 中可永久复用的静态 facts。

adapter 可以按协议结构拆分字段，但不能增加无界正文或改变 outcome。canonical JSON 中的 `status` 是给模型
阅读和调试的冗余展示，不是 wire mapping authority；typed `wire_semantics` 与 JSON 不一致时必须在发送前
作为 kernel materialization invariant violation 失败。

### 12.1 Anthropic

- 使用 `ProviderToolResultMessageV1.wire_semantics.outcome` 映射 `is_error`；
- 多个 result block 必须先于同一 user message 中的普通 text；
- 未执行/取消的并行 tool call 也要得到具有明确 error/cancel status 的 result block；
- 不把一个 parallel batch 拆成多轮 user message。

### 12.2 Gemini GenerateContent

- 连续、属于同一 assistant model turn 的 function responses 进入一个 user `Content.parts`；
- function identity/call ID 是 wire association authority，不靠数组位置猜测对应关系；
- Sigil 为 deterministic replay、cache stability 和跨 provider fixture 一致性额外固定 response 为 function
  call declaration order；这是内部 projection policy，不宣称为 Gemini wire protocol 的强制顺序；
- assistant function calls 上的 thought signature 保持现有顺序和关联；
- 本 RFC 不把 GenerateContent shape 混用到未来 Interactions API；迁移必须单独设计 adapter。

### 12.3 OpenAI and DeepSeek

OpenAI Responses 没有通用 `is_error` field，使用 canonical envelope 的 `status` 即可；不可虚构协议字段。
OpenAI-compatible 与 DeepSeek 继续使用 `role=tool`。所有路径都必须保留 exact call id。

## 13. MCP conformance

stdio 与 Streamable HTTP 共用一个 `McpCallToolResultV1` parser/projector：

```rust
pub struct McpCallToolResultV1 {
    pub content: Vec<McpContentBlock>,
    pub structured_content: Option<serde_json::Value>,
    pub is_error: bool,
}
```

统一规则：

1. JSON-RPC error 是 transport/protocol failure；
2. JSON-RPC success + `isError: true` 是 tool execution error，映射 `ToolError`；
3. server 返回的 actionable error content 经过 policy/size projection 后保留，不能替换成固定通用文本；
4. 存在 output schema 时验证 `structuredContent`，失败产生明确 conformance error；
5. `structuredContent` 优先用于 bounded facts，`content` 用于人类可读 preview；
6. 大 MCP response 进入同一 artifact lifecycle，不直接塞入 session message；
7. 两种 transport 对同一 fixture 产生等价 outcome、facts 与 display projection。

## 14. Scratch and spool ownership

两个目录具有不同能力边界：

### 14.1 Model-visible session scratch

`$SIGIL_SCRATCH_DIR`：

- 供模型明确需要跨 tool call 共享的临时文件；
- session-scoped namespace，不得只是 workspace-wide 公共 `tmp`；
- owner-only 权限、配额和 TTL；
- shell tool description 说明适用场景、生命周期和不保证长期保存；
- 文件路径可能进入 command/result，因此必须继续经过 path trust 与 disclosure policy。

### 14.2 Harness-private call spool

artifact staging namespace：

- call-scoped、随机命名；
- 不注入 child env，不返回路径；
- 只有 artifact store/capture service 能访问；
- publish 后用 opaque session-scoped ref 读取；
- aborted/unreferenced staging 由 crash-safe GC 清理。

模型写 scratch 不能替代 harness spool；harness spool 也不能被模型当作通用文件系统。

## 15. Desktop, TUI and HTTP contract

共享 DTO 至少提供：

```text
status
summary
exit_code / signal / duration
stdout_observed_bytes / stderr_observed_bytes
persisted_bytes
preview_truncated
truncation_reason
full_output_available
has_more_retrievable
capture_incomplete
artifact_ref (opaque, when available)
allowed retrieval selectors
```

产品行为：

- Desktop 与 TUI 都只在 current availability projection 为 `Available` 时显示“查看完整输出/继续读取”；
- storage truncated 时显示“已保存前 N 字节”，不显示“完整日志”；
- resource limited 与普通 non-zero exit 使用不同状态；
- policy redacted 明确说明展示的是 policy-safe projection；
- UI paging 通过 typed command/endpoint，不访问文件路径或 generic filesystem API；
- renderer reload 与 TUI session restore 从 durable descriptor + availability events 的 current projection
  恢复同一状态；
- HTTP/CLI adapter 只能返回 bounded DTO，artifact body 通过授权的 typed retrieval endpoint 获取。

## 16. Security, crash recovery and retention

### 16.1 Security

- staging/artifact 默认 owner-only；
- artifact ref 绑定 session、call、hash 和 selector authority；
- read receipt append-only，记录 selector、范围、结果大小和 caller surface，不记录正文；
- redaction/persistence policy 对 chunk boundary、二进制数据和 invalid UTF-8 有明确处理；
- artifact filename、OS path、credential 和 secret 不进入 provider message；
- artifact 读取仍受 workspace/session lifecycle 与 extension trust plane 约束。

### 16.2 Crash recovery

staging lifecycle：

```text
created -> writing -> finalized -> published -> descriptor-referenced
                    \-> abandoned
```

启动或后台维护执行：

- published 且已有 descriptor：保留至 retention 到期；
- published 但无 descriptor：经过 grace period 后删除；
- writing/finalized staging：验证 owner、age 和 active execution lease 后清理；
- descriptor 指向 missing/hash-mismatch：保留 immutable descriptor，按 §9.4 追加 availability event，不伪造
  artifact；
- cleanup 失败只产生 bounded telemetry，不阻塞 Desktop/TUI 启动。

### 16.3 Retention

复用 RFC-0059 retention class；本 RFC 增加：

- TTL 从 descriptor publish/last authorized read 的明确策略点计算；
- active session、pinned evidence、mutation/verification artifact 使用更强保留；
- GC 只能按 durable-disable-before-delete 删除 body，append-only descriptor、availability event 与 audit
  receipt 保留；
- current projection 进入 `Expired` 后 `has_more_retrievable = false`；
- 任何竞品采用的“返回路径后固定天数删除”都不能直接替代 Sigil 的 session-aware policy。

## 17. Implementation slices

### R62.0 — Characterization and urgent correctness

- 固化 large stdout/stderr、non-zero large error、preview-budget 和 unavailable artifact 回归测试；
- 先让 oversized `ToolError.message` 降级为 bounded summary，避免 tool error 反向中止 run；
- 删除 `unavailable => has_more` 的错误推导；
- 记录当前 Bash artifact 只含 bounded output 的 expected-fail characterization。

### R62.1 — Capture contracts and artifact staging

- 增加 capture plan、stream layout、segment、capture completeness、availability event 与 outcome 类型；
- artifact store 支持 staging、atomic publish、hash 和 orphan GC；
- session schema 切换到 `ToolResultRecordedV3`；
- V2 session 通过 bounded `UnsupportedSessionSchema` 明确拒绝；不读取、不迁移、不 mixed-load。

### R62.2 — Process-backed streaming capture

- 将 stdout/stderr tee/spool 下沉到 execution backend；`sigil-process` 只交付 pipe/PTY handle；
- 拆分 preview/artifact/observed caps；
- 普通 pipe 使用独立 bounded staging、8 MiB/8 MiB reservation 回收和 canonical 双 segment publish；
- Bash 移除 `attach_bounded_shell_artifact(result.content)` 路径；
- terminal foreground 与后续 process-backed tools 复用 capture primitive；
- 明确 pipe/PTY ordering semantics。

### R62.3 — Projection, retrieval and settlement

- bounded error summary、facts shrink ladder 和 terminal fallback；
- 移除 root-run cumulative preview counter，接入 per-assistant-batch actual-byte deterministic allocator；
- current-result minimum preview、current/recent/high-signal protection 与 historical token-pressure aging；
- typed retrieval 支持 stream selector、byte/line page 和 literal search；
- capture completeness、`ToolArtifactAvailabilityChangedV1`、generation guard 与 audit receipt 接入 durable
  projection。

### R62.4 — Provider and MCP conformance

- kernel 提供 `ProviderToolResultMessageV1`，live/restore/aging 都从 durable V3 恢复 typed wire semantics；
- Anthropic `is_error`；
- Gemini GenerateContent batch grouping、identity mapping 与 Sigil deterministic order；
- OpenAI/DeepSeek exact-id regression；
- MCP stdio/HTTP 使用共享 parser/projector 和 schema validation。

### R62.5 — Scratch, retention and recovery

- `$SIGIL_SCRATCH_DIR` session namespace、quota、TTL 和 tool description；
- harness-private spool namespace；
- artifact/staging GC、hash mismatch 和 crash recovery tests。

### R62.6 — Desktop/TUI parity

- typed IPC/OpenAPI DTO；
- Desktop/TUI 统一 status、size、truncation、availability 和 paging；
- renderer reload、TUI restore 与 artifact expiry acceptance；
- 不新增 renderer filesystem capability。

### R62.7 — Eval and release gates

- deterministic large-output corpus；
- real process-tree kill/drain tests；
- provider request fixtures/golden tests；
- paid provider smoke 仅在已有预算和凭据时执行；
- macOS/Linux source-built Desktop/TUI real-binary acceptance；
- 同步 core solution、README/用户文档和 release notes。

## 18. Verification matrix

| Scenario | Required assertion |
| --- | --- |
| 10 MiB stdout, exit 0 | process succeeds; full artifact hash/bytes match; provider/UI bounded |
| 10 MiB stderr, exit 7 | tool error reaches model; agent loop remains usable; error summary <= 1 KiB |
| stdout/stderr interleave | per-stream bytes/order exact; no false cross-stream chronology claim |
| dual streams cross cap | persisted quotas and canonical hash stay identical across chunking/scheduling variations |
| PTY output | combined stream ordering preserved under PTY contract |
| artifact cap reached | process may complete; storage=`TruncatedAtLimit`; source reflects actual exit |
| observed cap reached | process tree terminated once; source=`ResourceLimited`; no orphan child |
| artifact writer failure | bounded fallback; no raw-body fallback; no automatic tool retry |
| facts projector failure | terminal fallback persists; run does not lose tool-call closure |
| session writer failure | explicit persistence failure after execution; no duplicate execution |
| secret across chunks | no secret in artifact, provider envelope, UI event or logs |
| invalid UTF-8/binary | deterministic media/encoding behavior; byte counts remain exact |
| parallel short results | actual-byte budget; stable declaration order across repeated runs |
| 100 sequential small-result batches in one root run | every new batch receives a fresh budget; no later non-empty safe preview becomes empty because of historical bytes |
| one 128-result oversized batch | every safe non-empty result receives its deterministic minimum preview; total preview <= 64 KiB |
| historical context pressure | current batch/current turn remain protected; oldest eligible results age only in the next epoch |
| no safe aging candidate under fit pressure | explicit context-pressure outcome or semantic-compaction path; current batch is not silently erased |
| expired artifact | durable disable precedes body deletion; `has_more_retrievable=false`; both UIs disable retrieval |
| GC races active reader | disable rejects new leases; body deletion waits for old-generation reader lease drain |
| crash after availability disable | body may remain but all surfaces deny reads; reconciliation completes deletion |
| crash after body deletion | state remains non-readable; reconciliation appends terminal `Expired` |
| missing/hash mismatch | append generation-guarded availability event; restore keeps descriptor but denies retrieval |
| Anthropic parallel error | one user message; ordered blocks; `is_error=true` |
| typed outcome contradicts JSON status | adapter follows typed outcome or fails invariant; it never parses status for wire flags |
| Gemini parallel calls | one user content; identity mapping exact; Sigil order stable across repeated runs; signatures preserved |
| MCP stdio vs HTTP | equivalent success/error/structured projection |
| crash before publish | staging cleaned after lease/grace; no dangling descriptor |
| crash after publish before append | orphan published artifact GC after grace |
| Desktop/TUI restore | same durable completeness/status and retrieval affordance |
| V2 session opened after cutover | bounded incompatible-schema diagnostic; no partial restore or tool execution |

最低工程 gate：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
pnpm --dir apps/desktop check
./scripts/check-docs.sh
```

## 19. Acceptance criteria

本 RFC 只有在以下条件全部满足后才能标记 implemented：

1. Bash 大输出 artifact 在 cap 内与 child 实际 bytes/hash 一致，不再来自 post-truncated `content`；
2. preview cap、artifact cap、observed cap 分别有单测和 real-process test；
3. large non-zero output 不会因 facts/error message 上限使 agent run 异常退出；
4. `has_more_retrievable` 与 append-only current availability projection 一致，GC 每个 crash point 都不会留下
   虚假 `Available`；
5. ordinary pipes 不声称跨 stdout/stderr 的全局顺序，并在不同 reader 调度/chunk 切分下得到相同 quota、
   segment 与 canonical hash；
6. provider message/result 批次稳定、bounded，并通过 Anthropic/Gemini/OpenAI/DeepSeek fixtures；adapter 的
   wire error flag 只读取 typed outcome，restore/aging 后 outcome 不丢失；
7. MCP stdio 与 HTTP 遵守同一 `isError` / `structuredContent` contract；
8. artifact/staging crash recovery、TTL/GC、active-reader lease、hash 和 policy tests 通过；
9. Desktop/TUI 同时完成 paging、expired、truncated、error 与 restore acceptance；
10. V2 tool-result session 按 current-schema-only policy 明确拒绝，不存在 alias/default/mixed-load 路径；
11. crate dependency audit 证明 `sigil-process` 未引入 kernel session、artifact、retention 或 provider 依赖；
12. core technical solution 和用户文档同步，不再暗示“模型必须自己写临时文件”；
13. root-run cumulative preview counter 已删除；连续小结果、oversized parallel batch、latest protection、
    oldest-eligible aging 和 no-safe-candidate 分支全部通过 deterministic regression；
14. full local gate 通过，目标平台与付费 provider gate 的未执行项被明确列出，不用本地 fixture 冒充。

## 20. Rejected alternatives

### 20.1 只改 system prompt，要求模型把大输出重定向到临时文件

拒绝。模型无法可靠预知输出规模，也无法提供 crash consistency、secret policy、retention 和 provider
conformance；不同模型的遵循率还会漂移。

### 20.2 永远把完整 stdout/stderr 返回 provider

拒绝。会突破 context、token、session event 和 UI 内存边界，并把私密输出扩大到外部 provider。

### 20.3 达到 artifact cap 就杀进程

拒绝。存储预算与执行安全不是同一个边界；许多正常构建/测试可以产生大量但有限日志。

### 20.4 artifact 写失败时回退完整正文

拒绝。这会在最需要 fail-closed 时突破 provider/session hard limit。只能使用 bounded terminal fallback。

### 20.5 直接返回本地临时路径

拒绝。路径泄漏 host layout、缺少 session authority、无法稳定跨 restore/remote surface，也会诱导模型绕过
typed retrieval。

### 20.6 把 stdout 和 stderr 按 reader arrival time 合并为“精确顺序”

拒绝。两个 OS pipe 的读取调度不能证明 child write 的全局先后；需要精确合流时应使用 PTY/单流协议。

### 20.7 为每个 provider 在 kernel 增加专属 result 类型

拒绝。kernel 只表达 outcome、facts、artifact 与 completeness；协议特定 grouping/field 留在 adapter。

## 21. Research references

Official specifications and guides:

- [OpenAI Shell tool](https://developers.openai.com/api/docs/guides/tools-shell)
- [OpenAI Function calling](https://developers.openai.com/api/docs/guides/function-calling)
- [OpenAI latest-model guidance](https://developers.openai.com/api/docs/guides/latest-model)
- [Anthropic parallel tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/parallel-tool-use)
- [Anthropic handle tool calls](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)
- [Gemini function calling](https://ai.google.dev/gemini-api/docs/function-calling)
- [Gemini 3 GenerateContent function calling](https://ai.google.dev/gemini-api/docs/generate-content/gemini-3)
- [MCP tools specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

Current implementation references inspected on 2026-08-03:

- [OpenAI Codex output truncation](https://github.com/openai/codex/blob/bb5054fe47abe73ecbbd454751066a28c89f4bb9/codex-rs/utils/output-truncation/src/lib.rs)
- [Gemini CLI tool output masking](https://github.com/google-gemini/gemini-cli/blob/f47d6c6f7a1308d81f9f57acf7d279f0928c5249/packages/core/src/context/toolOutputMaskingService.ts)
- [OpenCode tool output store](https://github.com/anomalyco/opencode/blob/c88facb2cc50661ab4ae7c768d076214a879e053/packages/core/src/tool-output-store.ts)
- [Goose large response handler](https://github.com/block/goose/blob/5ab0e6df34e69444f6f2016de40717a9f54bf816/crates/goose/src/agents/large_response_handler.rs)

## 22. Frozen conclusion

Sigil 不应把“请模型记得把 Bash 输出写临时文件”当作可靠性设计。模型可以选择文件工作流，harness 必须
无条件提供 bounded capture、policy-safe spool、truthful completeness、typed retrieval、durable settlement
和 provider conformance。

完成本 RFC 后，tool result 的核心语义将从：

```text
一个可能截断、可能超限、成功失败靠正文暗示的 String
```

收敛为：

```text
bounded facts + explicit outcome + truthful completeness
+ immutable policy-safe artifact + opaque retrieval capability
```

这既减少模型上下文和偶发提示工程，也让 Desktop/TUI、provider adapter、MCP 与 crash recovery 共享同一份
可验证事实。当前 batch 通过 byte caps 保持 deterministic bounded，历史 batch 通过 token pressure 和
cache-stable aging 管理；两者不再由同一个 root-run 递减字节桶耦合。
