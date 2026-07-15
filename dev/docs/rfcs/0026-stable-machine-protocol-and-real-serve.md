# RFC-0026 Stable Machine Protocol and Real Local Serve

状态：accepted / P26.1-P26.5 implemented / complete

创建日期：2026-07-15

基线：

- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0012 Protocol and App Server Boundary](0012-protocol-app-server-boundary.md)
- Depends on: [RFC-0016 Desktop and App Server Productization](0016-desktop-app-server-productization.md)
- Related: RFC-0021 Web Data Tools implementation track

## 1. Summary

本 RFC 收口两个长期处于 library-only 或 preflight-only 状态的能力：

1. 为 `sigil run` 提供稳定、版本化的 JSON / JSONL machine output；
2. 让 `sigil serve` 启动真实的本机 HTTP/SSE listener，并通过同一 runtime application service 执行 agent run。

TUI 继续是第一用户入口。CLI machine output 与 HTTP server 都是 adapter，不复制 agent loop，不拥有 session truth，也不绕过 permission、approval、sandbox、egress disclosure、mutation 和 verification 控制面。

## 2. 立项基线与缺口

当前已具备：

- `sigil-kernel::PublicRunEvent`：versioned、provider-neutral 的跨 adapter run event；
- `HttpCommandEnvelope<T>`：command id、client id、session id、stream sequence 与 correlation guard；
- loopback listener、bearer auth、session/run registry、approval/cancel route、durable SSE cursor、OpenAPI；
- `sigil run` 使用真实 provider、tool registry、append-only JSONL session 与 CLI egress presenter。

当前缺口：

- `sigil run` 只有 human text output，没有稳定 JSON/JSONL contract；
- CLI 自己组装 provider、session、tool registry，HTTP 没有可复用 application service；
- `HttpRunDriver` 只有测试实现，registry 没有真实 run terminal 回写；
- `sigil serve` 只打印 preflight plan，不绑定 listener；
- HTTP disclosure replay buffer 没有真实可查询 product route；
- 没有从真实 `sigil` 进程验证 machine output 与 local server 的 E2E。

## 3. Goals

- 冻结 machine protocol V1 的 record、result、error 与 exit-code 语义。
- 复用 `PublicRunEvent`，不创建第二套 run event taxonomy。
- 抽取 CLI/HTTP 共享的 headless application run service。
- 让 HTTP run 支持真实执行、显式 approval、cooperative cancellation 与 terminal state。
- 默认仅绑定 loopback，并默认要求 bearer token。
- 让所有 session/control state 继续写入 append-only V2 JSONL。
- 提供真实 binary/process-level acceptance tests。

## 4. Non-goals

- 不把 TUI 改造成 HTTP client。
- 不默认开放远程 bind，不增加 cookie auth 或 wildcard CORS。
- 不引入 SQLite、daemon auto-start、desktop shell 或 multi-user tenancy。
- 不承诺 transient token/reasoning delta 在重连后可 replay。
- 不新建 `sigil-protocol` crate；如果未来存在第二个非 HTTP command transport，再用实证重新评估。
- 不在本 RFC 实现 session export/delete/retention；该能力由后续 RFC-0027 处理。

## 5. Machine Protocol V1

### 5.1 Canonical event

`sigil-kernel::PublicRunEvent` 继续是唯一公共 run event：

- `schema_version` 标识 event schema；
- `session_id + run_id + sequence` 标识有序 run stream；
- adapter 负责生成 `run_started`、`run_finished`、`run_failed`、`run_cancelled`；
- kernel `RunEvent` 通过既有 projection 进入同一 taxonomy。

P26.1 必须补齐 serialization golden tests。新增字段只能是 backward-compatible optional field；破坏性变化必须提升 schema version。

`PublicControlEvent.kind` 是稳定 routing field；`payload` 仍是 opaque diagnostic projection，V1 client 不得依赖其中的内部 `ControlEntry` 字段。

### 5.2 CLI records

JSONL 使用带 discriminator 的顶层 envelope：

```json
{"protocol_version":1,"record_type":"event","event":{"schema_version":1}}
{"protocol_version":1,"record_type":"result","result":{"status":"succeeded"}}
{"protocol_version":1,"record_type":"error","error":{"code":"execution_failed"}}
```

V1 record kinds：

- `event`：一个 `PublicRunEvent`；
- `result`：唯一 terminal machine result；
- `error`：run 创建前或 terminal result 无法产生时的结构化错误。

JSON mode 只输出一个最终 report；JSONL mode 输出有序 events，并以一个 `result` 或 `error` record 结束。machine stdout 不混入 human progress、session path 或 tracing line；安全 disclosure 和诊断保留在 stderr。

machine mode 必须在 preparation 开始前安装 `SIGINT` supervision：durable run identity 尚未建立时输出 cancelled error 与 exit `130`；identity 已建立后进入 durable cancellation request，并以同一 deadline 约束 owned execution join 和 quiescence。只有 `Cancelled` terminal 已落盘后才能输出 `status=cancelled` 和 exit `130`；无法证明 clean terminal 时输出 failed/error 与 exit `1`。进入 JSONL 的 optional extension warning 必须先通过配置绑定的 secret redactor。

### 5.3 Result and error

V1 result 至少包含：

- `session_id`
- `run_id`
- `status = succeeded | failed | cancelled`
- `final_text`
- `session_log_path`

V1 error 至少包含：

- 稳定 `code`
- 安全 `message`
- `retryable`

不得把 API key、header value、raw credential、未脱敏外部 URL 或 provider-private payload 写入 machine error。

### 5.4 Exit codes

V1 固定：

| Code | Meaning |
| --- | --- |
| `0` | run succeeded |
| `1` | runtime/provider/tool execution failed |
| `2` | invocation or configuration invalid |
| `130` | run was cooperatively cancelled |

Clap 自身的 usage error 继续使用 `2`。未被可靠分类的错误必须归入 `1`，不得根据 provider 错误字符串猜测更细分类。

## 6. Shared Application Service

`sigil-runtime` 新增 provider-neutral application run service，负责：

- 加载 `RootConfig`、解析 canonical workspace 与 `SigilPaths`；
- 创建或加载 V2 JSONL session，并恢复 workspace trust；
- 附加 URL capability store、mutation recorder 与 egress presenter；
- 构造 configured provider、tool registry、eager remote MCP 与 run options；
- 接受 injected event handler、approval handler 与 cancellation owner；
- 返回 session/run id、final result 与 session log path。

服务必须显式区分三类交互面：

- non-interactive CLI：`Ask` 不能阻塞等待，保持结构化 `approval_required`；
- adapter-managed policy：在 owned blocking worker 中执行 `deny` / `allow_readonly`，但不把自动策略决定伪装为 explicit user action；
- externally-interactive adapter：允许 HTTP approval endpoint 驱动 explicit user action。

同步 `ApprovalHandler` 的等待只能运行在 owned run thread/blocking owner 内，不能阻塞 Tokio async worker。Cancellation orchestration 必须先 durable record request，再激活 cancel、解除 approval wait、等待 bounded quiescence，并按 cleanup evidence 区分 cancelled 与 interrupted。

共享 service 必须让 execution 与 control 共同持有 foreground session lease，直到自然终态或 cancellation terminal 已落盘；公共 event dispatcher 必须串行完成 sequence 分配与投递，并在 root terminal 后拒绝迟到事件。`ExternallyInteractive` 只能使用 explicit-user-action approval handler 和 owned blocking execution 入口，误配必须在 provider dispatch 前 fail closed。

它不负责：

- stdout/stderr rendering；
- HTTP routing/auth；
- TUI state；
- provider-specific request DTO。

CLI 与 HTTP 必须调用这一服务，不能保留两份 agent assembly。

## 7. Real Local Server

### 7.1 Lifecycle and safety

- V1 只允许 bind loopback，默认 `127.0.0.1:0`；
- 默认从 `SIGIL_HTTP_TOKEN` 读取 bearer token；
- command surface 始终要求 bearer token；
- 缺少 required token、`--no-token` 或 non-loopback bind 时 listener 不启动；
- 启动后打印 actual bound address，但不打印 token；
- `Ctrl-C` 触发 listener graceful shutdown；active run 使用 cooperative cancellation，不声称撤销已发生的 shell/remote side effects。

### 7.2 Production run driver

production driver 必须：

- 后台执行共享 application service；
- 把内部 `RunEvent` 投影为 sequenced `PublicRunEvent` 并发布到 `HttpLiveEventBus`；
- 将 approval request 注册到 registry，并只接受 guard 完整且未过期的 decision；
- 将 cancel route 连接到 `RunCancellationOwner`；
- 将 finished/failed/cancelled terminal 回写 registry；
- run 结束后移除 active approval/cancellation state。

同一 adapter session 同时只能有一个 foreground run。Command de-duplication 必须先原子 reserve `(session_id, client_id, command_id)`；并发重复请求等待/重放同一 receipt，同 key 不同 payload 必须 fail closed。

registry 必须在创建 adapter session 时由 runtime driver 建立并校验 durable V2 scope/path binding；binding 失败时不得发布 session。foreground lease 只在 typed `finished` / `failed` / `cancelled` / `interrupted` terminal 回写后释放；相同 terminal 回写幂等，冲突 terminal 不覆盖先到达的状态。

driver unwind 属于 unknown execution state：registry 必须投影非终态 `execution_uncertain` 并保留 foreground quarantine，直到后续 durable terminal 确认或进程重启后由 session recovery 接管，不得回滚成可继续运行，也不得把 quarantine 伪装为可被覆盖的 terminal。command identity 只保留 bounded cryptographic fingerprint 与 bounded completion；容量到达上限时，新 key 返回 unavailable，既有 key 仍重放/冲突，不得淘汰后静默重新执行，也不能无界保存 prompt。P26.4B 的 durable event journal 不替代 command identity store。

P26.4C 在 listener bind 前使用独立、单 writer、原子替换的 durable command store 取代固定 256-key 进程内过渡窗口。每个 command 必须先持久化 reservation 才能执行；进程重启时未完成 reservation 封口为 `aborted`，成功 completion 可重放，冲突 fingerprint fail closed，达到 hard count/file-byte capacity 后只拒绝新 key。持久化 receipt 明确省略 prompt preview，并对 correlation/reason 等文本再次执行 SafePersist。store 每次成功 reopen 都持久递增 server epoch，production adapter session/run id 带 epoch，避免 process-local counter 重置后误命中旧 command identity。

`allow_readonly` 只能自动放行 read-only approval；`deny` 拒绝所有 gated tool call；`ask` 必须等待显式 approval endpoint decision。

### 7.3 Durable replay and live stream

`Last-Event-ID` replay 不能只依赖进程内 buffer。Adapter 必须维护 crash-safe、bounded 的 durable protocol journal，或从同一 durable session evidence 确定性重建 cursor；进程内 bus 只负责 transient live fan-out。SSE route 必须先 replay durable suffix，再持续订阅 live events，不能返回有限 body 后立即关闭。

P26.4C 的 listener 在 replay 前先订阅 live bus，发送 retained durable suffix 后继续输出匹配 run 的 transient/durable event，直到 run terminal、client disconnect、明确 live lag 或 server shutdown。live lag 输出 `stream_gap` 后关闭，client 用最后一个 durable cursor 重连；不为 transient event 伪造 replay id。listener 用 owned `JoinSet` 收割连接任务；graceful shutdown 先关闭 socket 与新 command 准入，再通过正常 cooperative cancellation 取消 active run，等待 production driver owner idle，最后关闭并 join SSE/HTTP connections。只有全部 owner 已释放才返回成功。

P26.4B 使用持久化 high-watermark 与 retained suffix：Unix 原子替换在 temp file sync、rename 和 parent-directory sync 全部成功后才允许 live publish；Windows 使用 replace-existing + write-through 的原生替换语义。同一路径由进程独占 lease 防止双 writer。被裁剪的 cursor 返回显式 `cursor_expired`，不能把不完整 suffix 伪装成连续历史；terminal stream watermark 随 retained terminal event 一起滚动回收，active stream identity 超出容量则 fail closed。cursor 绑定 durable session scope 与 adapter run id，避免 process-local adapter session id 在重启后复用造成 stream collision。

durable HTTP envelope 在 journal 前必须执行 kernel SafePersist 文本/JSON 投影，并沿用 `MAX_EVENT_BYTES` 拒绝 oversized 单事件；journal 还必须同时限制配置容量、反序列化记录数与整个文件字节数，不能先无界读取再验证。exact prompt、raw final answer、URL query/secret carrier 不能成为第二份 HTTP durable truth。provider opaque continuation blob 与 generic control payload 不进入 HTTP journal，只保留安全的 state/control identity 与显式 omitted projection；ToolResult、approval preview/schema 与 assistant/tool-call 文本使用共享 SafePersist policy。只有 session cancellation finalized evidence、protocol terminal append 与 registry terminal 三者均成功，driver 才能释放 typed terminal；任一环节失败均进入 `execution_uncertain` quarantine。取消若先于 preparation 完成，supervisor 使用同一 absolute deadline 等待 preparation owner 并完成 prepared cancellation control，或记录“agent execution 未 admitted”的 append-only terminal；deadline 到达时必须先 quarantine 并答复调用方，随后继续持有并 reap blocking owner，不得 detach 后伪造 cleanup complete。cancellation request/finalization 中的 session append、fsync 与 protocol publication 必须运行在 owned blocking worker，不能阻塞 Tokio async worker。

### 7.4 Disclosure route

真实 server 必须使用 production disclosure presenter，并让已认证 client 查询 disclosure replay records。旧 synthetic presenter 不能作为 production receipt。完成该 presenter 前，需要 disclosure 的 Web/remote MCP run 必须 fail closed。写入 replay surface 只证明 server 已安全接收 disclosure，不证明人类已阅读；文档与 event naming 不得扩大该声明。

P26.4B 的 production driver 在构造边界直接接收 durable disclosure journal 并组装 production presenter，不能注入 synthetic presenter；presenter 只在 bounded journal 原子持久化成功后返回 path-bound sink receipt，写入与 sync 运行在 owned blocking worker。synthetic in-memory presenter 只保留测试用途。P26.4C 的 production listener 通过 bearer-authenticated `GET /disclosures` 按 `Last-Event-ID` 查询 retained safe records；route 不扩大 receipt 为“人类已阅读”。

## 8. Durability and Recovery

- HTTP adapter session 必须绑定一个 durable V2 session scope/path；adapter id 只是 routing handle。
- 同一 adapter session 使用 foreground run lease 串行写入，不能从相同旧投影并发执行。
- registry 是 process-local projection，重启后可以为空；不得伪装成 durable session index。
- cancellation、approval、tool/mutation/egress evidence 继续通过既有 kernel/runtime writer 写入 session。

## 9. Implementation Slices

1. P26.1：protocol/result/error/exit contract 与 serialization goldens。
2. P26.2：shared application run service、durable session binding、approval/cancel orchestration，并迁移现有 text `sigil run`。
3. P26.3：`sigil run --output text|json|jsonl`。
4. P26.4A：linearizable command registry、session foreground lease 与 typed lifecycle。
5. P26.4B：production driver、durable replay journal、approval/cancel 与 production disclosure。
6. P26.4C：replay+live SSE、loopback bearer listener、real `sigil serve` 与 graceful drain。
7. P26.5：process E2E、OpenAPI/EN-ZH docs、full audit。

每个 slice 独立提交并通过对应 targeted gate；后一片不得在前一片 contract 未稳定时提前接线。

## 10. Acceptance Criteria

- JSON/JSONL fixtures 在相同输入下保持 schema 与 discriminator 稳定。
- machine stdout 可被标准 JSON parser 无额外清洗地消费。
- CLI 与 HTTP 使用同一 runtime application service。
- HTTP start 的真实 provider request、tool event、terminal result 与 session JSONL 可互相对照。
- stale/expired approval fail closed；cancel 进入 cooperative cancellation path。
- token 缺失或不安全 bind 不打开 socket。
- `sigil serve` 可由真实 binary 启动、查询 health、创建 session、启动 run、读取 terminal/event，并 graceful shutdown。
- TUI 默认入口与行为不变。

## 11. Validation

```bash
cargo test -p sigil-kernel public_run_event
cargo test -p sigil-runtime headless_run
cargo test -p sigil-http
cargo test -p sigil
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
git diff --check
```

## 12. Completion Result

P26.1-P26.5 已全部完成，并按独立 commit 边界交付：

- machine protocol V1、JSON/JSONL terminal 语义与 exit code 已冻结并具备 process fixtures；
- CLI 与 HTTP 共用同一 runtime application service；
- production server 使用 durable command identity、protocol event 与 disclosure stores，所有 command 都在执行前完成持久化 reservation；
- `sigil serve` 真实绑定 loopback bearer listener，支持 authenticated OpenAPI/disclosure route、session/run/approval/cancel、durable replay+live SSE 与 graceful drain；
- 真实 binary fixture 使用 deterministic loopback provider 完成 health、auth、session、run、terminal SSE、Ctrl-C、restart epoch，并证明 missing token、`--no-token` 与 non-loopback 均在 listener state 创建前失败；
- EN/ZH status、reference、user guide、safety 与 RFC-0012/RFC-0016 boundary 已同步。

最终 Full-Audit 通过 workspace fmt/check/test/Clippy、docs mirror/link/command metadata、Pages site、dependency advisories/bans/licenses/sources 与 diff gate；未发现剩余 P0/P1/P2 finding。V1 边界仍明确排除 remote bind、multi-user tenancy、daemon auto-start 与 session export/delete/retention。
