# RFC-0052 Desktop Conversation Continuity and Control V1

状态：complete / R52.0-R52.9 delivered 2026-07-23

创建日期：2026-07-22

依赖：

- [RFC-0001](0001-durable-event-stream-and-event-taxonomy.md)
- [RFC-0024](0024-tui-checkpoint-rewind-v1.md)
- [RFC-0025 Context Compaction V2](0010-structured-compaction-and-task-memory.md)
- [RFC-0026](0026-stable-machine-protocol-and-real-serve.md)
- [RFC-0027](0027-local-session-lifecycle-v1.md)
- [RFC-0045](0045-desktop-ui-ux-foundation-v1.md)
- [RFC-0048](0048-desktop-composer-and-transcript-v2.md)
- [RFC-0049](0049-desktop-skills-agents-workbench-v1.md)
- [RFC-0050](0050-desktop-conversation-library-and-settings-v1.md)

## 1. Problem statement

RFC-0043 至 RFC-0050 已建立真实 `sigil serve`、desktop shell、设计系统、结构化 composer、
Skills / Agents 浏览、Conversation Library、Settings 和 Support。但连续 dogfood 仍暴露出一组不能靠 CSS 修补的会话契约问题：

- workspace 启动或 recent reopen 失败后只有错误文案，没有稳定的 Retry、Open another、Diagnostics 和 Details 恢复路径；
- transcript、durable replay 与 live SSE 由 renderer 按文本内容去重，provider 归一化、失败 terminal、重连和重复 final 会导致错序或重复；
- 打开带 active run 的会话时，transcript load 与 run attach 是两个异步阶段，composer 会短暂误显示为可提交；
- active run 期间 composer 只允许 Stop，用户输入被保留为草稿但不能排队、steer 或精确 send-now；
- `/compact`、checkpoint restore 和 conversation fork 已有 durable owner，但 desktop catalog 仍缺少 typed route；
- 正常 loading、reconnecting、waiting approval、waiting subagent 与 finalizing 没有统一状态机，导致 loading 跳变、通知噪音和错误操作窗口。

这些问题共同指向同一个产品边界：desktop 缺少一个由 runtime/server 证明的 conversation continuity contract。
本 RFC 不增加新的 agent loop，也不让 React 成为 durable state owner。

## 2. Research and product baseline

- VS Code Chat Sessions 把 running state、changes、archive/pin/delete/fork/export 作为会话级能力，而不是零散 toast：
  [Chat sessions](https://code.visualstudio.com/docs/chat/chat-sessions)。
- Zed 在 agent 正忙时明确区分 Queue、Steer 和 Send now；输入意图不会被静默丢弃：
  [Agent Panel](https://zed.dev/docs/ai/agent-panel)。
- VS Code checkpoints 只声明可证明的工作区恢复，不把 shell 或 remote side effect 描述为已撤销：
  [Chat checkpoints](https://code.visualstudio.com/docs/chat/chat-checkpoints)。
- WAI-ARIA combobox 要求 suggestion 与输入通过 `aria-controls`、`aria-activedescendant` 和明确展开状态关联：
  [Combobox Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/)。

Sigil 采用这些交互基线，但所有 action 必须服从现有 append-only session、approval、verification、queue、checkpoint
和 renderer capability 边界。

## 3. Product contract

### 3.1 Actionable recovery

Workspace、session 和 active-run recovery error 必须投影稳定的 machine code、用户安全说明和允许的 action 集合。
Desktop 只渲染服务端/原生层允许的 action，不根据 error string 猜测。

V1 action 集合：

- `retry_current`：重试同一精确操作；
- `open_another_workspace`：调用已有 native workspace picker；
- `open_diagnostics`：进入 Support / Diagnostics；
- `show_details`：展示 bounded、path-free、credential-free detail；
- `continue_read_only`：只在 transcript 已可靠加载但 live owner 不可恢复时提供。

Retry 必须幂等；切换页面、主题或展开 details 不得触发 retry。错误不可恢复时不得显示虚假 action。

### 3.2 Canonical conversation display items

Runtime/server 提供有界、provider-neutral 的 `ConversationDisplayItemV1`：

| Field | Contract |
| --- | --- |
| `display_id` | 在同一 durable session scope 内稳定；renderer 不用文本生成 identity |
| `display_order` | `{ session_stream_sequence, subindex }`；按二元组比较，稳定、单调且覆盖同一 durable record 投影多行的情况 |
| `source_event_id` | 可选的 durable source identity；durable item 必须提供，transient item 可缺失 |
| `kind` | `user_message`、`reasoning`、`assistant_message`、`tool`、`approval`、`checkpoint`、`notice` 或 `terminal` |
| `source` | `durable_transcript`、`durable_run_event` 或 `live_transient` |
| `run_id` | 可选的安全 adapter run identity，用于局部 reconciliation |
| `run_sequence` | 可选的 per-run 单调序号；只用于 live transport/reconciliation，不作为跨 run 展示顺序 |
| `status` | kind 允许的有界 lifecycle 状态；terminal 后不得回到 running |
| `content` | 继续服从现有 text/tool/output caps 和 secret projection |
| `reconciles` | live provisional item 被 durable item 取代时的稳定 identity；缺失时不得按文本猜测 |

规则：

1. 唯一 authoritative order source 是 kernel V2 session stream 的 `SessionStreamRecord.stream_sequence` 与 `event_id`。
   HTTP protocol journal 只负责 run 内 replay/transport，不拥有跨 run 展示顺序。
2. 任何要在重启后继续展示的 run/approval/checkpoint/terminal item，必须先具有 provider-neutral 的 session stream
   durable record；当前只存在于 HTTP protocol journal 的 item 必须先补 session record，再进入 canonical projection。
3. Runtime 从 source `event_id + subindex` 生成 `display_id`，从 `stream_sequence + subindex` 生成
   `display_order`。`message_id` 与旧 transcript `ordinal` 只作为内容/source metadata，不是 renderer identity。
4. Live transient 允许提前展示，但 durable successor 必须通过 `reconciles` 原位替换；不得 append 出第二个 final。
5. Assistant delta 只聚合到当前 run 的 provisional assistant item；terminal final 是唯一 durable final。
6. Provider 内容归一化、截断、空文本和失败/cancelled terminal 不改变 identity 规则。
7. Renderer 只能按 identity/order 合并，不得比较 prompt/final 文本去重。

### 3.3 Canonical page and frontier

`ConversationDisplayPageV1` 除 items 外必须返回：

- stable backwards cursor 与 request scope；
- `through_session_stream_sequence`，证明本页 projection 已覆盖的 session frontier；
- `terminal_frontier`，当前 run terminal 所绑定的 session stream sequence（如存在）；
- bounded gap/retention facts；
- live provisional anchor，包含当前 durable frontier、run id 与 run sequence，但不得伪装为 durable order。

Source mapping：

| Durable source | Canonical item |
| --- | --- |
| User/assistant/tool `SessionLogEntry` | 对应 message item；source event id/order 直接来自其 V2 stream record |
| `reasoning_trace` control | reasoning item；同一 record 多行使用稳定 subindex |
| approval/checkpoint/run lifecycle control | 对应 approval/checkpoint/terminal item；缺少 session record 时先补 provider-neutral durable control |
| HTTP run protocol journal | 只提供 replay transport 与 provisional reconciliation，不生成独立 durable display order |

同一 run 的 `RunFinished` lifecycle 与 durable final `AssistantMessage` 只能映射为一个用户可见 final：terminal control
更新该 final 的状态/frontier，不再生成第二段回答。

### 3.4 Continuity admission and state model

每次打开会话都必须执行 fresh `ConversationContinuityViewV1` probe，不能以 catalog snapshot 的
`foreground_run_id` 缺失作为“当前无运行”的证明。Probe 至少返回：

- durable session id 与当前 session frontier；
- foreground run id（可空）、opaque owner revision/lease generation；
- 当前 terminal/frontier facts 与允许的 recovery actions。

当 foreground run 存在时，attach 必须绑定 probe 返回的 owner revision。`run_no_longer_foreground`、owner changed 或
attach transport failure 后必须重新 probe；只有 fresh response 明确 `foreground_run_id = none` 才允许 composer 启用。
Transport failure 保留 transcript 并进入只读恢复，不把“无法证明 owner”降级为 idle。

Conversation surface 使用以下互斥主生命周期：

```text
loading_transcript
    -> checking_owner
        -> attaching_run
            -> live
            -> finalizing
            -> idle
    -> read_only_recovery
    -> error
```

- `loading_transcript` 完成前不展示可编辑 composer。
- `checking_owner` / `attaching_run` 是显式状态；在 fresh probe 证明没有 active run 前，composer 不得短暂启用。
- approval、subagent、reconnecting 与 replay gap 是可并存的 orthogonal attention flags，不伪装成互斥 lifecycle。
- `finalizing` 只在 terminal 已到达但 canonical page 的 `through_session_stream_sequence` 尚未覆盖
  `terminal_frontier` 时成立；覆盖后才进入 idle。
- 重复点击当前会话是 no-op，不重新 load/attach。
- attach 失败后保留已加载 transcript，并提供 Retry 或只读恢复，不留下无限 loading。

### 3.5 Durable follow-up queue and execution

Desktop 复用现有 append-only conversation queue truth，不创建 renderer-only queue。

Server 提供：

- bounded queue projection：entry id、order、kind、status、prompt preview、created/updated facts 和 generation；
- exact command：enqueue、edit、remove、reorder、pause、resume、interrupt-and-run-next；
- command identity、session scope、queue generation/CAS 和 foreground run binding；
- stale、terminal、owner-lost、permission 和 conflict 的 typed rejection。

Queue 只持久化 secret-safe prompt projection。Application owner 另持有 process-local exact prompt material，并按 durable
`prompt_hash` 校验；renderer 永远拿不到该 material 的旁路副本。Queue item 额外投影：

- `prompt_material = persisted_safe | available_process_local | requires_reentry`；
- `dispatchable` 与 typed `blocked_reason`；
- `requires_reentry` 表示重启后 exact material 已按安全契约丢失，条目仍可审计但不可 dispatch。

用户重新输入/编辑该条目时，server 重新执行 secret-safe projection，append 新的 queue edit revision，并只在当前
application owner 内保存 exact material。只有 `persisted_safe` 或 hash 校验通过的 `available_process_local` 条目可 promotion；
不得从 durable redacted text 反推或自动 dispatch exact prompt。

Composer 行为：

- idle：`Send`；
- running：默认 `Queue`，secondary action 为明确确认的 `Interrupt and run next`；
- waiting approval：允许编辑/排队，不绕过 approval；
- queued item 可在 queue drawer 中 edit/remove/reorder；
- `requires_reentry` 条目显示 redacted preview 与“重新输入”操作，不能伪装成可继续发送；
- V1 不提供没有 runtime owner 的 main-run `Steer`；child-agent follow-up 继续归 RFC-0049；
- `Interrupt and run next` 必须 cooperative interrupt 当前 run、等待 terminal frontier，再按最新 queue revision promotion；
- Stop 只取消当前 foreground run，不清空 durable queue。

Session-scoped runtime/HTTP queue scheduler 是 R52.4 的必要 owner，而不是 renderer side effect。它负责 process-local exact
prompt cache、terminal-triggered promotion/dispatch、restart 后 `requires_reentry` 降级、重复 command id、CAS stale、
cancel/terminal/promotion ordering 与单 foreground run admission。只完成 query/command 而没有 scheduler 不算 R52.4 完成。

### 3.6 Compact, checkpoint and fork routes

Desktop 只投影已存在 owner：

- `/compact`：preview -> explicit apply，保留 tokenizer/capability/stale-frontier/CAS 失败语义；
- checkpoint：list -> reverse diff preview -> restore controlled files；
- fork：从精确 turn/checkpoint 创建 conversation fork，原会话和文件默认不变；
- restore/fork receipt 必须进入 timeline，并支持重新验证。

UI 必须始终声明：checkpoint restore 只恢复有 durable evidence 的受控文件 mutation；shell、网络、remote、人工编辑和
其他外部副作用不会被撤销。

## 4. Architecture and ownership

```text
sigil-kernel
  existing append-only queue/checkpoint/compaction/session truth
        |
sigil-runtime
  canonical display projection + exact application commands
        |
sigil-http
  authenticated DTO, replay/query/command, OpenAPI
        |
sigil-desktop
  typed client and path/secret-free narrowing
        |
Tauri allowlisted commands/events
        |
React continuity reducer + recovery/queue/checkpoint UI
```

- `sigil-kernel` 不接收 desktop 文案或 DeepSeek 私有术语。
- `sigil-runtime` 负责 durable replay 到 display/queue/checkpoint application view 的确定性投影。
- `sigil-http` 负责 authentication、command idempotency、CAS/stale admission 和 generated contract。
- `sigil-desktop`/Tauri 不暴露 bearer、absolute path、process、generic HTTP/filesystem/shell。
- React 只拥有 draft、focus、drawer、selection、scroll anchor 等 presentation state。

## 5. Hard invariants

1. Session/control truth 继续 append-only、可持久化、可审计；SQLite 仍是 rebuildable projection。
2. Renderer 不按文本、时间戳或数组位置推断 durable identity。
3. Single-final、approval、verification、cancellation 和 active-run follower contract 不得回退。
4. Queue mutation 必须先 durable append 成功再更新 UI；失败不乐观显示成功。重启后需要 exact prompt 的条目进入
   `requires_reentry`，不得从 safe projection 恢复原文。
5. Reattach 不启动第二个 agent loop；owner 丢失时 fail closed。
6. Compact/checkpoint/fork 不绕过现有 exact binding、digest、snapshot、CAS 或安全提示。
7. Theme/navigation/drawer 不 remount active conversation，不丢 draft、IME、scroll anchor 或 focus。
8. 正常 progress 不弹 toast；只有需要用户处理的 failure、高影响 action receipt 或后台完成提醒进入通知中心。

## 6. Execution slices and commit boundaries

| Slice | Scope | Suggested commit | Completion evidence |
| --- | --- | --- | --- |
| R52.0 | Ledger calibration、contract、threat/decomposition freeze | `docs(rfc): define desktop conversation continuity` | RFC/plan/status、docs/diff gate |
| R52.1 | Actionable workspace recovery + fresh continuity probe/attach admission | `fix(desktop): add actionable workspace recovery` | HTTP/native/frontend probe, retry and stale-owner tests、dev smoke |
| R52.2 | Session-record-backed canonical display identity/order projection | `feat(server): project canonical conversation display order` | runtime/HTTP/OpenAPI/client property tests |
| R52.3 | Renderer reconciliation、attaching/finalizing、same-session no-op、scroll anchor | `fix(desktop): reconcile transcript and live activity` | reducer/reconnect/duplicate-final/AX tests |
| R52.4 | Queue projection/commands + session-scoped runtime/HTTP scheduler | `feat(server): own conversation queue execution` | CAS/idempotency/restart/promotion/OpenAPI tests |
| R52.5 | Composer Queue/Interrupt-and-run-next and queue management UI | `feat(desktop): add follow-up queue composer` | keyboard/IME/approval/active-run tests |
| R52.6 | Typed compact/checkpoint preview/restore/fork routes | `feat(desktop): expose conversation recovery controls` | durable receipt/diff/stale/safety tests |
| R52.7 | Attention states、loading、message actions、ARIA、notification polish | `fix(desktop): polish conversation attention states` | UI system/full-app/accessibility tests |
| R52.8 | Real restart/reattach/replay/queue/checkpoint dogfood and full audit | `test(desktop): gate conversation continuity` | real `sigil serve`、native dev、full gate、two audits |
| R52.9 | Completion ledger and docs close | `docs(rfc): close desktop conversation continuity` | no remaining P1/P2、status sync |

Dependency order:

```text
RFC-0050 -> R52.1
R52.0 -> R52.1
      -> R52.2 -> R52.3
      -> R52.4 -> R52.5
R52.3 -------------> R52.5
RFC-0024/0025/0027 + R52.2/R52.3 -> R52.6
RFC-0049 ----------> R52.7 attention projection
R52.1/R52.3/R52.5/R52.6 -> R52.7 -> R52.8 -> R52.9
```

## 7. Acceptance gates

- Workspace startup failure has working Retry, Open another, Diagnostics and bounded Details where applicable.
- Opening an active session never exposes an enabled composer before attach/no-active-run is proven.
- Reconnect, replay and transcript refresh produce one session-stream-ordered timeline and exactly one final answer。
- Running composer can queue a follow-up without losing draft；reload 后 safe prompt 可继续使用，需要 exact material 的条目明确要求重新输入。
- Stop does not erase queued follow-ups；interrupt-and-run-next requires an exact explicit action and waits for terminal admission.
- `/compact` no longer reports “no desktop route”; checkpoint preview shows changed files and reverse diff before restore.
- Existing conversation remains readable when live owner is unavailable; unsafe controls are disabled with actionable recovery.
- 1280×820 and 900×640 have no document-level horizontal scroll; loading transitions preserve content position.
- All new controls are keyboard reachable, have accessible names and do not interfere with IME composition.
- Contract drift、frontend check、targeted Rust tests、full touched gate and real native dogfood pass.

## 8. Non-goals and successor RFCs

- Subagent follow-up/cancel/close remains in RFC-0049 R49.4-R49.6.
- Desktop image/file attachment input belongs to RFC-0053; RFC-0033's TUI-only contract is not silently broadened here.
- Branch/dirty/files/line-count workspace projection belongs to RFC-0054; renderer will not execute `git`.
- TUI-equivalent theme families and syntax-theme preferences belong to RFC-0055; RFC-0046's `system|light|dark` contract remains valid here.
- Complete provider credential/endpoint/MCP/plugin configuration requires a separate settings contract.
- RFC-0051 Intent Stack is independent and remains deferred while conversation continuity is active.
- This RFC does not add remote daemon、multi-user、cloud scheduler、generic terminal/file browser or arbitrary session JSONL editing.

## 9. Completion evidence

- R52.1-R52.7 已交付 actionable recovery、canonical display、fresh continuity admission、durable follow-up queue、compact/checkpoint/fork route 与 attention/accessibility polish。
- R52.8 真实 `sigil serve` dogfood 覆盖 restart/reopen、model/effort binding、single-final ordering、queue restart dispatch、checkpoint restore 与 bearer boundary；`f0014d23` 修复重启队列的 fresh effort binding。
- 原生收尾发现并由 `ae7a4593` 修复 protocol journal schema 2→3 缺少迁移的问题；真实 424-event journal 原地迁移后，release sidecar 和 macOS 应用成功自动重开 `turbods` 并投影 30 条会话。
- Desktop 138 tests/type/UI/contract/build、full workspace fmt/check/test/strict Clippy、`sigil-http` 154 tests、900/1280 catalog scroll/AX gate 与两轮 contract/UI audit 通过；截至关闭未发现剩余 R52 P1/P2。
