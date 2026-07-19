# RFC-0045 Desktop UI/UX Foundation V1

状态：active / R45.0-R45.1 complete，R45.2 ready

创建日期：2026-07-19

基线：

- Desktop shell：[RFC-0044](0044-desktop-shell-mvp-v1.md)
- Desktop runtime bridge：[RFC-0043](0043-desktop-runtime-bridge-v1.md)
- Stable local protocol：[RFC-0026](0026-stable-machine-protocol-and-real-serve.md)
- Verification UX：[RFC-0023](0023-verification-ux-v1.md)
- Local session lifecycle：[RFC-0027](0027-local-session-lifecycle-v1.md)

## 1. Summary

RFC-0044 证明了 desktop 可以在不复制 agent loop、不给 renderer bearer 或通用系统能力的前提下完成
workspace、session、run、approval、cancel 与 verification daily loop。当前界面仍是工程验证壳：历史目录没有历史正文，
导航会让 active run 失联，workspace、session、run 和局部操作共享一个状态栏，代码、工具、diff、审批与验证又都降级为
普通文本。

本 RFC 建立两层统一规范：

1. **Agent Interaction Contract**：TUI 与 Desktop 共享 session、turn、run、tool、approval、verification、artifact、
   checkpoint 和 error 的语义与安全边界；平台可以采用不同布局与键位。
2. **Desktop UI System**：冻结 desktop 的信息架构、状态归属、领域组件、design token、响应式、键盘、焦点与
   accessibility gate。

实施顺序固定为 contract、bounded transcript、active-run reattach、ownership/IA、coding-agent components、
adaptive/accessibility system 和 completion audit。不得先用主题美化掩盖错误的信息结构。

## 2. Evidence and product decision

- [OpenAI Codex app](https://openai.com/index/introducing-the-codex-app/) 把 project/thread、并行任务与 thread 内 review
  作为一等表面。
- [VS Code chat sessions](https://code.visualstudio.com/docs/chat/chat-sessions) 把状态与 session lifecycle action 放在
  session 导航中，而不是把 history 降级为只有元数据的列表。
- [Zed Agent Panel](https://zed.dev/docs/ai/agent-panel#reviewing-changes) 把代码改动审查从普通聊天文本中分离，
  支持文件与 hunk 级 review。
- [Apple Sidebars](https://developer.apple.com/design/human-interface-guidelines/sidebars) 要求 sidebar 层级扁平，
  在窄窗口改变呈现方式。
- [WCAG 2.2 Reflow](https://www.w3.org/WAI/WCAG22/Understanding/reflow.html) 要求缩放与窄 viewport 不依赖
  二维滚动完成主要任务。
- [Design Tokens Format](https://www.w3.org/community/reports/design-tokens/CG-FINAL-format-20251028/) 提供语义 token
  分层的通用依据。

Sigil 借鉴这些产品的任务层级、状态语义与渐进披露，不复制高密度 IDE、多 tab 或绕过审批的 unrestricted mode。

## 3. Hard invariants

1. Desktop 仍是 TUI 之外的 adapter；TUI-first 定位、TUI 行为和快捷键不因本 RFC 改变。
2. Renderer 只通过 allowlisted Tauri command/event 消费收窄 DTO；不得持有 bearer、child、绝对 state path、
   generic HTTP/filesystem/process/shell 能力。
3. HTTP/SSE 是唯一 runtime contract。历史正文由 server 受控读取 append-only session truth；renderer 和
   `sigil-desktop` 不直接读取 JSONL 或 SQLite。
4. SQLite 仍是可重建 catalog projection，不成为 transcript、active run 或 approval truth。
5. active run 必须始终可定位、可重新附着、可 cancel/approve，或明确显示已证明的 disconnect/gap；导航不得静默
   丢失监督权。
6. tool event、reasoning 和 progress 不得重复渲染为 final assistant reply；一次 durable assistant answer 只出现一次。
7. raw receipt、snapshot、changeset、run 和 message ID 默认进入 Details/Evidence，不占据主任务表面。
8. 高影响动作必须有与影响相称的视觉权重和确认；workspace close 在 active run 存在时不得由小型 `×` 静默终止。
9. 所有 replay、cache、render 与 diagnostic 都有数量和字节上限；超限必须显式标记，不能伪装成完整历史。

## 4. Shared Agent Interaction Contract

### 4.1 Domain hierarchy

```text
Workspace
  Session
    Turn
      Run
        Message / Tool / Approval / Verification / Artifact
```

- Workspace 是 runtime process、catalog 和 filesystem scope。
- Session 是 append-only conversation identity，可 reopen、fork、export、archive/pin/delete。
- Turn 是一轮用户意图与其结果；retry/rerun 不创建假消息。
- Run 是可运行、取消和观察的执行实例。
- Tool/Approval/Verification/Artifact 是 run 的领域对象，不是普通 assistant paragraph。

### 4.2 State matrix

| State | User meaning | Primary action | Persistence / recovery |
| --- | --- | --- | --- |
| Ready | 可输入下一轮 | Send | session truth |
| Queued | 输入已排队，尚未运行 | Edit/cancel queued input | 必须明确标记 durable 或 local-only；不得混淆 |
| Running | agent 正在生成或执行 | Cancel | run snapshot + live stream |
| Waiting approval | run 被安全决策阻塞 | Allow/Deny | pending approval truth |
| Cancelling | cancel 已确认，等待 terminal | Wait | supervisor acknowledgement |
| Reconnecting | 本地 follower 正在恢复 | Retry/inspect | cursor replay + snapshot |
| Failed | run 已终止且有可行动错误 | Retry/inspect | durable terminal/evidence |
| Completed | durable terminal 已确认 | Verify/continue | transcript + verification |

状态只从权威 snapshot、durable event 与显式 local follower state 推导；禁止由颜色、定时器或缺少事件猜测完成。

### 4.3 Copy boundary

- 主界面使用用户任务语言：`Running`、`Waiting for approval`、`Verification failed`、`Reconnect`。
- `HTTP epoch`、`private bearer`、`Rust`、`TUI-first`、内部 schema/version 和 opaque ID 只进入 Connection Details、
  Evidence 或 About。
- 错误同时说明对象、发生了什么、用户能做什么；不得用一个 global string 覆盖不相关 scope。

## 5. Desktop information architecture

宽窗口使用三域而不是一个纵向页面：

```text
Workspace / Sessions | Conversation | Review / Verification
```

- 左侧 navigation：workspace identity、session search/filter、session state 与 lifecycle action。
- 中央 conversation：唯一主滚动区，包含 transcript、live run、approval dock 与 composer。
- 右侧 inspector：按选中 tool/change/verification/evidence 显示详情；无选中对象时不占据主任务。
- 中等窗口：navigation 可折叠，inspector 作为 overlay/drawer。
- compact 窗口：一次只展示一个主 pane；pane 切换恢复焦点和滚动位置。
- 页面不得同时出现外层 document scroll 与 timeline 内层主滚动；长 tool output/diff 可拥有自身 bounded scroll。

## 6. State ownership

| Scope | Examples | Presentation rule |
| --- | --- | --- |
| Application | package/update/about | application surface only |
| Workspace | starting, connected, crashed, closing | workspace navigation/header |
| Session | opening, stale, archived, active run | session row/header |
| Run | running, cancelling, reconnecting, terminal | conversation run indicator |
| Domain object | approval, tool failure, verification failure | colocated card/dock/inspector |
| Transient action | copied, draft saved | local toast/status message |

健康状态必须清除旧错误。切换筛选器只影响列表结果，不卸载已经打开的 conversation 或 active-run follower。

## 7. Bounded transcript contract

R45.1 新增 authenticated server-owned transcript endpoint：

```text
GET /sessions/{session_id}/transcript?limit={1..100}&before={exclusive_ordinal}
```

Contract：

- 默认最新 50 条，响应按 chronological order 返回；`next_before` 用于向前翻页。
- 只投影 user、assistant 与 tool result；system/private control 不进入用户 transcript。
- 每条正文、每页正文和总 item 数均有硬上限；截断返回 `truncated` 与原始 byte count。
- image 只返回安全 metadata/count，不返回解析后的 raw bytes 或绝对路径。
- tool args、credential、session log path 与 server-private control payload 不进入 DTO。
- 请求必须校验 durable session scope；不存在、stale、超限和 corrupt 都返回结构化错误。
- renderer 将历史 page 与 live projection 按 turn/run arrival order 合并，不按 opaque `run_id` 排序。

R45.1 已完成：server-info schema V3 新增 `bounded_transcript_replay` capability；runtime 从同一份已校验
V2 records 投影 scope-checked page，OpenAPI/Rust client/Tauri allowlist/renderer DTO 同步，native IPC 丢弃 durable
scope 与 path。真实 `sigil serve` restart/reopen process E2E 已证明历史正文可读取，renderer pagination 保留 chronological
order 与滚动锚点。

## 8. Active-run reattach contract

- 打开 session 后，如果 server snapshot 有 `foreground_run_id`，native bridge 必须安装/复用该 run 的 follower，
  再向 renderer 返回 bounded attachment snapshot。
- native owner 保存 bounded active-run projection；新 listener 先获得 snapshot，再接收 live event，避免导航期间的 event gap。
- cache 超限时返回 `has_gap=true`；UI 显示“部分实时细节未保留”，并继续从 durable transcript/terminal 恢复，不能展示
  拼接后的假完整输出。
- reattach 后 cancel、approval 与 verification 使用同一 expected session/run guard。
- session filter、navigation pane、inspector 和 responsive pane 切换不得停止 follower。
- workspace close 若存在 active run，显示 run 数量和副作用边界；确认后才关闭 runtime。

## 9. Desktop domain components

### 9.1 Conversation and message

- `Message` 按 user/final/reasoning/progress 分层；reasoning/progress 默认折叠。
- `MessageContent` 支持安全 Markdown 基础、fenced code、inline code、引用、列表和复制；禁止 raw HTML/navigation。
- streaming 使用稳定容器，durable final 到达时原位 reconcile，不生成第二条 reply。

### 9.2 ToolCard and DiffViewer

- ToolCard header 固定包含 tool、状态、持续时间与风险；preview、input、output、error 渐进披露。
- shell output 使用 bounded monospace viewer；超限明确显示 omitted count。
- file change 使用 DiffViewer；文件/hunk action 只有在已有 backend contract 可证明时出现，不能伪造 apply/revert。

### 9.3 ApprovalDock

- Waiting approval 是高优先级 sticky dock，不是普通 section。
- 显示动作、scope、风险、preview 和决策有效期；一次/session 等权限语义必须与 runtime 一致。
- 打开时把焦点移到 dock，Esc 返回 composer；决策后原位显示 resolved result 并恢复先前焦点。

### 9.4 VerificationInspector

- 主表面显示 check、状态、摘要、失败定位和 `Rerun`/`Inspect`。
- receipt/snapshot/changeset 进入 Evidence；复制 ID 是次要操作。
- selected verification 与 diff/tool 共用 inspector，不创建新的主滚动区。

### 9.5 Composer and errors

- Enter 发送，Shift+Enter 换行，Cmd/Ctrl+Enter 也可发送；IME composition 期间不得误发。
- draft 按 workspace/session 保存并在 reopen 恢复；active run 时不得把输入无说明地整体禁用。
- `ErrorCard` 属于 workspace/session/run/domain scope，包含 retry/inspect；恢复成功后清除 stale error。

## 10. Desktop UI system

### 10.1 Tokens

所有颜色、spacing、type、radius、shadow、motion 和 z-index 通过 semantic CSS custom property 使用：

- light、dark 与 high-contrast 三组 color token；业务组件不得直接写 hex status color。
- spacing 采用有限刻度；同类 panel/card/action 不自行发明 gap、radius 或 border。
- motion 尊重 `prefers-reduced-motion`；状态不得只靠动画或颜色表达。

### 10.2 Interaction and accessibility

- 所有功能可用键盘完成，focus order 与视觉顺序一致，focus-visible 不被移除。
- pane/drawer/modal 打开与关闭执行 focus capture/restore；approval 与 blocking error 使用明确 announcement。
- streaming container 不逐 token 触发 screen-reader announcement；terminal summary 才进入 polite live region。
- 320 CSS px reflow、200% zoom、VoiceOver、键盘、IME、reduce motion、light/dark/high-contrast 纳入 gate。
- primary text/controls 满足 WCAG 2.2 AA；信息不只依赖颜色。

## 11. Execution slices and commit boundaries

1. **R45.0 Contract and information architecture freeze**
   - Commit: `docs(rfc): open desktop ui ux foundation`
2. **R45.1 Bounded transcript replay**
   - Commit: `feat(desktop): replay bounded conversation history`
3. **R45.2 Active-run attachment and control ownership**
   - Commit: `fix(desktop): retain control of active runs`
4. **R45.3 Conversation workspace and scoped state**
   - Commit: `refactor(desktop): focus the conversation workspace`
5. **R45.4 Coding-agent domain components**
   - Commit: `feat(desktop): add coding agent interaction surfaces`
6. **R45.5 Adaptive, themed and accessible UI system**
   - Commit: `feat(desktop): standardize adaptive accessible interface`
7. **R45.6 Completion audit and documentation sync**
   - Commit: `docs(rfc): close desktop ui ux foundation`

每个切片先跑 affected tests，再跑 desktop package/native contract；R45.6 才运行 full workspace、docs/site、OpenAPI
drift、Tauri capability、package smoke 与人工 system-WebView matrix。一个切片失败时停在该切片修复，不跨 commit 混入
后续主题或组件重构。

## 12. Acceptance

1. 打开任意 durable session 可以看到 bounded 历史正文并继续向前翻页；不存在 renderer JSONL/SQLite access。
2. active run 在 session/filter/pane 切换后仍可重新附着、查看、cancel 和处理 approval；gap 明确可见。
3. 主界面能稳定回答当前 workspace/session、run state、变更、verification 与下一步动作。
4. opaque ID 和内部机制不再占据 primary surface；Evidence 仍可审计。
5. timeline 不按 run ID 排序；assistant final 不重复；tool/reasoning/progress 使用独立语义。
6. workspace close 对 active run 有保护；错误恢复后不残留 stale global error。
7. composer keyboard/IME/draft 行为通过自动化测试。
8. 三栏、两栏和 compact 都只有一个主滚动区；320 CSS px 与 200% zoom 可以完成 daily loop。
9. dark/light/high-contrast、keyboard、focus restore、VoiceOver/reduced-motion gate 有可复核证据。
10. generated OpenAPI、Rust typed client、Tauri DTO/capability、renderer types 和真实 `sigil serve` contract 同步。
