# RFC-0048 Desktop Composer and Transcript V2

状态：active / R48.0-R48.4 complete; R48.5 acceptance in progress

创建日期：2026-07-20

基线：

- [RFC-0047](0047-desktop-workbench-ux-reset-v1.md)
- [RFC-0046](0046-desktop-material-derived-design-system-and-theme-preferences-v1.md)
- [RFC-0026](0026-stable-machine-protocol-and-real-serve.md)

## 1. Problem statement

RFC-0047 已建立可工作的 conversation/composer 主界面，但 desktop 仍把输入和输出当作普通文本：

- Composer 只消费 `textarea`，不能发现、补全或高亮 `/command`、`$skill` 与 `@agent`；
- reasoning effort 没有进入 desktop run-context 和 run-start contract，用户无法查看 provider 支持范围或为一次 run 选择 effort；
- transcript 使用自制 Markdown 子集，缺少完整 GFM、链接、表格、任务列表和代码语法高亮；
- TUI command、skill、agent 能力已经存在，但 desktop 若自行硬编码会形成第二套语义和漂移源；
- completion、IME、keyboard、screen reader 与 active-run draft ownership 尚未形成统一的结构化输入契约。

本 RFC 将 composer 和 transcript 提升为共享 application capability 的 desktop 投影，而不是把 TUI UI 代码搬进 React。

## 2. Product contract

### 2.1 Shared extension catalog

Runtime 提供 provider-neutral、bounded、typed 的 composer catalog：

- `command`：稳定 id、trigger、label、description、availability、argument hint 和 execution kind；
- `skill`：稳定 id、显式 invocation token、display name、short description、trust/enable 状态；
- `agent_profile`：稳定 id、显式 invocation token、display name、short description、trust/policy 状态。

Catalog 是 discovery projection，不是执行旁路。Desktop 只能通过 typed Tauri/HTTP command 执行已注册 action；不得把用户输入拼成 shell、路径或 generic HTTP 请求。

每个 catalog item 必须绑定一种可执行路由，而不只是展示文字：

- `run_binding`：把已解析的 `skill_id` / `agent_profile_id` 作为 typed run-start binding 交给 runtime admission；
- `session_command`：通过稳定 command id 和独立 typed payload 执行 session/application action；
- `client_action`：只允许有限的纯 presentation action，例如打开已存在的 settings/workbench surface。

未知 command、不可用 extension 或参数不完整时必须返回 typed rejection。Desktop 不得把 slash token 重新拼回普通 prompt 来伪装执行成功，也不得复制 TUI dispatcher。

### 2.2 Structured composer

1. `/`、`$`、`@` 只在当前 token 符合触发边界时打开 accessible combobox。
2. `ArrowUp/ArrowDown` 移动选择，`Enter/Tab` 接受，`Escape` 关闭；IME composition 期间不得提交、筛选或接受 suggestion。
3. 高亮只表达已解析 token；未知 token 保持普通文本并给出非阻塞说明，不伪装为已启用能力。
4. Slash command 可以是本地 application action，也可以生成一次普通 run；两者由 registry 明确区分。
5. Draft 仍按 workspace/session 存在 native-owned renderer state；切换主题、打开 drawer、刷新 catalog 不得清空 draft。
6. active run 期间保留输入与可发现性，但是否 queue/execute 继续服从现有 run/queue contract。

### 2.3 Reasoning effort

- Run context 返回绑定 exact provider + model 的 `available_reasoning_efforts`、`default_reasoning_effort` 与 capability binding；空列表明确表示当前 provider/model 不支持。
- Run start 接受可选 `reasoning_effort`，runtime 在 provider dispatch 前校验 capability binding 未 stale、default 属于支持集合，且选择属于当前 exact provider + model 的支持集合。
- Run start 同时为 R48.3 预留 optional typed `skill_id` / `agent_profile_id` binding；binding 必须由 runtime registry 重新解析、校验 snapshot/trust/policy，不能信任 renderer description 或 invocation token。
- 选择只作用于精确的新 run，不改写 session provider/model identity，不改变 agent profile 的 durable default。
- DeepSeek 支持 `low/medium/high/max`；官方 OpenAI Responses 支持 `low/medium/high`；未证明支持的 provider 返回空列表。
- Renderer 不根据 provider 名称猜测范围，也不静默降级 `max`。

### 2.4 Transcript rendering

- Assistant/user 文本按 CommonMark + GFM 的受限安全子集渲染；raw HTML 默认禁用。
- 支持 headings、lists、links、quotes、tables、task lists、strikethrough、inline/fenced code。
- Fenced code 按受支持语言高亮，未知语言回退为 escaped plain code；不加载 remote grammar、font、CSS 或 script。
- 外部链接只允许 `https:`，渲染为 `rel="noreferrer noopener"` 且不允许当前 WebView navigation。打开动作使用只接受已解析 HTTPS URL 的 typed native command；command 不可用或 admission 失败时提供显式 copy fallback。本 RFC 不开放 `http:`、`file:`、`javascript:`、`data:`、自定义 scheme 或 unrestricted opener。
- copy、truncation、missing content、reasoning/final kind 与 tool projection 继续服从 RFC-0045/RFC-0047 的 bounded contract。

## 3. Architecture and ownership

```text
sigil-runtime
  shared command catalog + typed action/run binding + effort/extension admission
        |
sigil-http typed DTO + generated OpenAPI
        |
sigil-desktop typed client
        |
Tauri allowlisted command/DTO
        |
React StructuredComposer + SafeMarkdown
```

- `sigil-kernel` 只保留 provider-neutral `ReasoningEffort` 和 agent/session truth，不引入 desktop/TUI command 名称。
- `sigil-runtime` 负责 exact provider effort set、shared application command catalog 和 extension projection。
- `sigil-http` 负责 bounded wire contract 与服务端 admission；OpenAPI 是 frontend schema 的机械来源。
- `sigil-desktop` 和 Tauri 只做 typed mapping，不持有 UI 语义。
- `apps/desktop` 通过 Sigil-owned UI primitives 适配编辑器/Markdown 依赖，业务组件不直接散用第三方 primitives。

## 4. Hard invariants

1. Renderer 不取得 bearer、absolute path、process、shell、filesystem、generic HTTP 或 unrestricted opener。
2. Command completion 不等于 command authorization；所有写、网络、审批和 sandbox 行为继续走现有 runtime contract。
3. Effort 必须以 exact provider + model + capability binding 在服务端 admit；default 必须属于集合，model/capability stale 时 fail closed，不由 UI 隐藏错误。
4. Markdown raw HTML、remote asset、script、iframe、form 和 executable URL 不得进入 transcript DOM。
5. Structured composer 不得破坏 IME、draft、Enter/Shift+Enter、active run Stop、approval focus 与 single-final。
6. TUI 和 desktop 的 command/skill/agent discovery 来自同一共享 catalog 或共享 domain source，不允许长期维护两份硬编码清单。
7. Catalog item 必须有 executable route 或明确的 unavailable reason；“插入文字但没有执行 owner”的 item 不得标记 available。

## 5. Execution slices

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| R48.0 | Contract、research、dependency/admission 与 commit freeze | RFC/plan/status、independent decomposition review、docs gate |
| R48.1 | Exact effort capability 与 run request end-to-end | runtime/HTTP/OpenAPI/native/frontend contract tests、provider rejection tests |
| R48.2 | Safe CommonMark/GFM transcript 与 local syntax highlight | security/render/long-copy tests、dependency audit、bundle check |
| R48.3 | Shared application command/extension catalog、TUI metadata migration、typed action route 与 run binding | runtime/TUI help/state parity/admission tests、bounded DTO/OpenAPI/command tests |
| R48.4 | Structured composer、completion、高亮、IME 与 keyboard | interaction/AX/draft/active-run tests、catalog fixture |
| R48.5 | Real-server/full-app acceptance and completion audit | real `sigil serve` smoke、viewport/theme/zoom dogfood、no P1/P2 |

Dependency order:

```text
R48.0 -> R48.1
      -> R48.2
      -> R48.3 -> R48.4 -> R48.5
```

R48.1、R48.2 与 R48.3 在 contract 冻结后可独立实施；R48.4 必须等待 catalog。

## 6. Acceptance gates

- Composer 在 `/`、`$`、`@` 输入后 100 ms 内展示本地 suggestion，不发网络请求。
- 纯键盘可完成打开、筛选、选择与关闭，screen reader 能读出 label、position 和 description。
- 中文/日文 IME composition 期间 Enter 不提交，候选文字不触发 catalog action。
- DeepSeek exact supported model 显示四档 effort；OpenAI Responses exact supported model 不显示 `max`；unsupported/unknown model 不显示伪造默认值。
- 服务端拒绝不属于 exact support set 的 effort，且不会发 provider 请求。
- 可用 slash item 均能解析到 typed client/session/run route；未知、stale、disabled skill/agent 在 runtime admission 失败且不发 provider 请求。
- GFM table/task list/strikethrough/link 和常用 Rust/TypeScript/JSON/shell fenced code 正确渲染；恶意 HTML/URL 不执行。
- `https:` link 不能导航当前 WebView；typed native open、拒绝与 copy fallback 均有测试，DOM 保留 `noreferrer noopener`。
- 30/100 session、long transcript、active run、approval、tool error、light/dark、900 px 和 200% zoom 不产生 document horizontal scroll。

## 7. Dependency and supply-chain policy

优先选择广泛维护、纯前端、无 remote runtime fetch 的组合：

- CodeMirror 6 的最小 state/view/autocomplete 包，封装为 Sigil-owned structured composer primitive；
- `react-markdown` + `remark-gfm` + `rehype-highlight`，raw HTML 插件不启用；
- syntax grammar 和 theme 必须随 bundle 本地提供，CSP 不新增 remote source。

落地前必须记录锁定版本、许可、维护来源、bundle 影响和安全边界到 `dev/governance/dependency-supply-chain.md`，并通过 `pnpm audit --audit-level high`。

## 8. Research basis

- [OpenAI developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli)：slash command 在 composer 中可发现和执行。
- [OpenAI skills](https://learn.chatgpt.com/docs/build-skills)：显式 skill invocation 与 progressive disclosure。
- [OpenAI subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)：active/done agent activity 与 thread inspection。
- [WAI-ARIA combobox pattern](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/)：completion 的 keyboard 与 accessibility contract。
- [CodeMirror extension architecture](https://codemirror.com/docs/extensions/)：可组合的 editor state/view/autocomplete boundary。
- [react-markdown](https://github.com/remarkjs/react-markdown)：safe syntax-tree based React rendering boundary。

## 9. Non-goals

- 不实现完整 IDE editor、LSP、文件 tab、terminal 或 arbitrary document editing。
- 不新增第三方 command marketplace、skill installer、agent profile editor 或 remote plugin daemon。
- 不让 slash command 绕过普通 run、approval、session 或 verification contract。
- 不把 mobile layout 作为本 RFC 的完成条件；desktop 最小工作尺寸仍按 RFC-0047。
