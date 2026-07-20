# RFC-0049 Desktop Skills and Agents Workbench V1

状态：planned

创建日期：2026-07-20

依赖：

- [RFC-0048](0048-desktop-composer-and-transcript-v2.md)
- [RFC-0008](0008-thread-projection-and-agent-graph-observability.md)
- [RFC-0009](0009-extension-trust-plane.md)

## 1. Problem statement

Sigil runtime/TUI 已具备 skill discovery、agent profile、trust/policy、background agent thread 和 append-only control truth，desktop 却没有可见投影。用户既不能理解 `$skill`/`@agent` 的来源和状态，也不能检查 active/done agent activity。

直接在 desktop 中增加文件编辑器或复制 TUI state 会破坏既有 trust、policy 和 durable ownership。本 RFC 只建立安全的 catalog、inspect、policy action 和 activity workbench。

## 2. Product contract

### 2.1 Progressive disclosure

1. Composer `$` 与 `@` completion 是高频入口。
2. Workbench 只在用户打开时展示 Skills / Agents 两个 tab，不常驻挤压 conversation。
3. Catalog row 默认只显示名称、来源级别、短说明与有效状态；details 也只消费 bounded sanitized projection。Raw manifest、native path 和 absolute source location 永不跨 IPC。
4. Active agent activity 在 conversation 顶部提供紧凑入口；done activity 自动降权但可检查。

### 2.2 Skill management

- 浏览 workspace/user/plugin 的 safe projection；支持搜索、按 enabled/trust/source 筛选。
- inspect 显示 invocation token、description、来源、trust、policy denial reason 与 bounded metadata。
- 当前 skill durable stream 只有 index/load evidence，没有用户 trust/enable decision。R49.2 必须先新增绑定 source scope、stable skill id、content digest、discovery snapshot 与 decision reason 的 append-only admission decision；在该契约完成前 desktop 只能只读 inspect 和调用 runtime 已经 admit 的 skill。
- enable/disable/trust action 只有在上述 durable decision、stale snapshot 与 reload projection 完成后开放；不得复用 agent-profile decision 或 workspace trust 伪装 skill decision。
- Desktop 不编辑 `SKILL.md`、不安装远程 skill、不向 renderer 暴露 absolute path。

### 2.3 Agent profile management

- 浏览 agent profiles，显示 model/effort/permission/result policy 的 safe projection。
- trust、invocation policy 和可用状态修改复用 runtime durable control entry；无权限或 invalid profile 必须可解释且不可执行。
- `@agent` 只引用已 admit 的 profile id；实际 child spawn、budget、tool scope、write isolation 和 result handoff 仍由 runtime 控制。
- Desktop V1 不提供 arbitrary profile file editor。

### 2.4 Agent activity

- 展示 active/done/failed/cancelled agent threads的稳定 id、profile、task summary、status、usage 和 bounded latest activity。
- 支持 inspect、send follow-up、cancel/stop、close/archive 等 runtime 已证明的 action；每个 action 按当前 thread state 精确启用。
- Desktop/HTTP 不能直接借用一次 run preparation 内部的 process-local `AgentSupervisor`。R49.4 必须先建立 session-scoped live owner：owner 绑定 durable session scope 与 foreground lease，跨请求只暴露 bounded thread command；restart 后缺失 live owner 必须 fail closed 并只展示 durable terminal/interrupt projection。
- 不声称 background child 是实时 remote worker，不把 safe-point follow-up 描述为实时 steering。
- 不直接展示 provider continuation payload、workspace path、raw tool arguments 或 secret-bearing error。

## 3. Architecture and ownership

- `sigil-runtime` 复用 `AgentProfileRegistry`、skill discovery 与 agent thread projection；skill decision 和跨请求 live supervisor owner 由本 RFC 明确补齐后再输出 bounded application DTO。
- `sigil-http` 提供 authenticated catalog/query/command endpoints 和 OpenAPI；所有 mutation 有 command identity、stale binding 与 durable result。
- `sigil-desktop`/Tauri 只做 allowlisted typed projection。
- `apps/desktop` 只管理 presentation/filter/focus state，不创建第二套 trust、policy、thread lifecycle 或 activity state machine。

## 4. Hard invariants

1. Workspace/user/plugin source 必须可区分，但 absolute path 与 raw manifest 不得进入 renderer。
2. Trust、enable、policy 与 thread mutation 必须在 runtime durable truth 成功后再刷新 projection。
3. Skill/agent description 是不可信文本，只按 plain text/safe Markdown 呈现，不能注入 DOM 或执行链接。
4. Agent tool scope、write isolation、approval、max_subagents 与 result handoff 不因 desktop action 放宽。
5. Invalid/unavailable 项必须可 inspect、可修复指引或可移除引用，不能成为不可操作的死行。
6. Workbench 不是 marketplace、文件管理器或第二个 config editor。
7. Live agent action 必须路由到 owning supervisor/session lease；只有 durable projection 而没有 live owner 时 action 保持 disabled/unavailable。

## 5. Execution slices

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| R49.0 | Contract、safe projection 与 mutation freeze | RFC/plan/status、threat/decomposition review |
| R49.1 | Typed skill/agent definition catalog | runtime/HTTP/OpenAPI/native/frontend contract tests |
| R49.2 | Skill admission decision contract、browse/inspect/filter/trust/enable/invoke | digest/snapshot/stale/durable mutation tests、invalid fixtures |
| R49.3 | Agent profiles browse/inspect/policy/invoke | profile admission/policy tests、composer integration |
| R49.4 | Session-scoped live agent owner and restart contract | owner/lease/restart/route tests、no orphan command |
| R49.5 | Active/done agent activity and controls | thread projection/follow-up/cancel/close tests、long activity fixture |
| R49.6 | Real-server dogfood and completion audit | real child-agent smoke、full-app/AX/security audit、no P1/P2 |

Dependency order:

```text
RFC-0048 R48.3/R48.4 -> R49.0 -> R49.1 -> R49.2
                                      \-> R49.3 -> R49.4 -> R49.5 -> R49.6
```

## 6. Acceptance gates

- 100 skills / 100 profiles 可搜索和滚动，不阻塞 composer/timeline。
- `$`/`@` suggestion 与 workbench catalog 使用同一 stable id 和 availability reason。
- trust/enable/policy 失败不会乐观显示成功；reload 后状态来自 durable projection。
- invalid skill/profile 可 inspect，且不会进入 executable suggestion。
- active child 可 inspect 和 cancel；done child 可读取 bounded result；follow-up 文案准确表达 safe-point semantics。
- live owner 丢失或 process restart 后所有 follow-up/cancel/close 命令 fail closed，不根据 stale renderer state 猜测成功。
- renderer capability、CSP、secret/path redaction、single-final 和 approval gates 无回退。

## 7. Non-goals

- 不支持 marketplace、远程安装、自动更新、在线模板或 arbitrary git clone。
- 不支持在 desktop 中编辑 skill/agent source file。
- 不新增多用户、remote daemon、cloud agent scheduler 或 unrestricted child filesystem。
- 不把 skill 与 agent 合并为一个模糊的“plugin”开关。
