# RFC-0016 Desktop and App Server Productization

状态：accepted / E16.1-E16.6 implemented / production closure completed by RFC-0026

创建日期：2026-06-29

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0008 Thread Projection and Agent Graph Observability](0008-thread-projection-and-agent-graph-observability.md)
- Depends on: [RFC-0012 Protocol and App Server Boundary](0012-protocol-app-server-boundary.md)

## 1. Summary

本 RFC 把 RFC-0012 的 protocol DTO / runner bridge 继续拆成面向桌面端和未来外部客户端的 productization 切片。目标是先提供本机安全 listener、command/event transport 和 durable replay cursor，不提前把 SQLite、远程访问或完整 OpenAPI 作为默认依赖。

核心决策：

1. TUI 仍是第一用户入口；app-server 是 adapter，不是 control plane。
2. Localhost listener 默认只绑定本机，remote access 默认关闭。
3. Durable event replay 和 transient live event 必须继续分离。
4. 桌面端优先复用 runtime product-view adapters，不提前强制 SQLite materialized view。
5. OpenAPI/SSE productization 必须复用同一 command envelope、approval stale protection 和 session truth source。

## 1.1 E16.1 Local Transport/Auth Decision

E16.1 决策如下：

1. MVP transport 使用 loopback TCP：默认 `127.0.0.1`，端口 `0` 由操作系统分配。
2. Unix domain socket / Windows named pipe 暂不作为 MVP 前置。它们可以在桌面适配器需要更强本机隔离、权限继承或路径型授权时重新评估，但不能阻塞 E16.2 listener。
3. Auth 默认必须启用 bearer token。现有 `sigil-http` 默认读取 `SIGIL_HTTP_TOKEN`，桌面 launcher 后续可以生成 per-launch token，但必须复用同一 validator 和 command envelope。
4. Remote access 默认关闭。非 loopback bind 不属于 MVP；后续如果支持，必须是显式配置，并继续 fail closed。
5. 浏览器客户端不使用 cookie auth，不启用 wildcard CORS，不提供 unauthenticated state-changing endpoint。Command endpoint 必须要求 `Authorization: Bearer <token>`。
6. 不依赖浏览器 Private Network Access / CORS preflight 作为唯一保护。浏览器可能会对 public site -> localhost/private network 请求增加 preflight 或 permission gate，但 app-server 仍必须自己执行 auth、origin/CORS 和 command envelope 校验。

当前代码已经与该决策对齐：

- `HttpServerConfig::default()` 绑定 `127.0.0.1:0`。
- `HttpAuthConfig::default()` 要求 bearer token，并使用 `SIGIL_HTTP_TOKEN`。
- `HttpServerConfig::validate()` 拒绝空 token env、关闭 token auth 和所有 non-loopback bind。
- `sigil serve` 通过 production driver 启动真实 listener，并在 bind 前完成以上安全检查。

E16.2 进展：

- `sigil-http` 已新增 `HttpLocalServer`，可按 E16.1 决策绑定 loopback TCP listener。
- Listener 只负责 HTTP/1.1 framing、bearer auth、JSON response 和 registry routing；agent execution 仍通过注入的 `HttpRunDriver`，不复制 agent loop。
- 已支持 `GET /health`、`POST /sessions` 和 `POST /sessions/{session_id}/runs`。
- `POST /sessions/{session_id}/runs` 使用 `HttpCommandEnvelope<HttpRunStartRequest>`，并通过 `HttpSessionRunRegistry::start_run_command` 做 command-id retry de-duplication。
- 未认证 command endpoint 返回 401；默认不启用 CORS，不提供 unauthenticated state-changing endpoint。
- `sigil serve` 已接入完整本机产品入口；listener/registry 仍是 adapter，不拥有 session truth 或 agent loop。

E16.3 进展：

- `HttpProtocolCursor` 已提供 SSE `id:` / `Last-Event-ID` cursor 格式。
- `HttpProtocolEvent` 已区分 durable 和 transient event。
- Durable event 带 `replay_id`，transient reasoning/text/tool-args delta 不带 replay id。
- `HttpProtocolEventBuffer::replay_run_after` 已支持按 cursor 补齐 durable events，并对 bad/wrong-scope/ahead cursor fail closed。

E16.4 进展：

- `HttpLiveEventBus` 已提供 bounded live event fan-out。
- `HttpLiveEventSubscriber::recv` 会把 subscriber lag 显式报告为 dropped live events。
- Transient events 可以 live delivery，但不提供 `replay_id`，也不会进入 durable replay 结果。
- `GET /runs/{run_id}/events` 已在 durable replay 后继续 live delivery，直到 terminal、disconnect、lag 或 shutdown。

E16.5 进展：

- `sigil-http` 已新增 `http_openapi_document()`，覆盖当前已实现的 MVP local command surface。
- OpenAPI 文档只描述已实现路由：`GET /health`、authenticated OpenAPI/disclosure replay、session create/list/get、run start/get/cancel、continuous replay+live SSE，以及 approval decision submission。
- Approval command route 已接入 listener，复用 `HttpCommandEnvelope<HttpApprovalDecisionRequest>`、stale approval guard 和 command retry de-duplication。
- 文档组件覆盖 bearer auth、command envelope、run-start payload、approval guard 字段、receipt 和 shared error response。

E16.6 进展：

- E16.6 当时选择 library-level HTTP route smoke + headless adapter proof，不进入完整桌面壳，也未在该切片接 `sigil serve` 产品入口或引入 SQLite projection；production entry 后续由 RFC-0026 完成。
- Listener 已补齐最小外部客户端查询/控制面：`GET /sessions`、`GET /sessions/{session_id}`、`GET /runs/{run_id}`、`POST /runs/{run_id}/cancel`。
- Run cancel 使用 `HttpCommandEnvelope<HttpRunCancelRequest>` 和 `HttpRunCancelCommandReceipt`，复用 command id retry de-duplication、session match 和 stale stream-sequence guard。
- `GET /runs/{run_id}/events` 提供 `text/event-stream` durable replay 和 live follow，并使用 `Last-Event-ID` cursor；transient live events 不承诺可重放。
- `desktop_adapter_smoke_surface_covers_list_cancel_approval_and_events` 覆盖 connect/list/start/cancel/approval/durable replay/transient-live-only 边界，证明外部/桌面 adapter 使用同一 registry/driver/protocol 路径，而不是复制 agent loop。

安全依据：

- OWASP CSRF guidance 建议 API 类状态变更请求使用不可预测 token，并可通过 custom request header 传递，避免依赖 URL 或 cookie。
- Chrome Private Network Access 方向说明 public site 到 private/local endpoint 的请求存在 CSRF/confused-deputy 风险，并会通过 preflight/permission 逐步限制；Sigil 不把这些浏览器限制当作服务器端认证替代品。

## 2. Goals

- 为桌面端预留真实 local app-server 入口。
- 将 RFC-0012 E12.4/E12.5 拆成更小的可验证切片。
- 支持 client retry、stale approval protection 和 durable event cursor。
- 保持 active turn / approval / transient progress 使用 live runtime state，不被 projection lag 误导。
- 明确何时才需要 SQLite projection escalation。

## 3. Non-goals

- 不默认开放远程访问。
- 不让 app-server 自己执行 agent loop 或 tool execution。
- 不在没有查询压力时引入 SQLite 作为 mandatory dependency。
- 不把 desktop-specific UI 决策塞进 kernel。
- 不承诺 transient reasoning/token deltas 可重放。

## 4. Server Boundary

`sigil-app-server` or equivalent adapter owns:

- local listener lifecycle
- local auth token / socket protection
- command envelope decoding
- SSE/WebSocket framing
- client connection registry
- transport-level diagnostics

It does not own:

- durable truth source
- verification reducer
- approval policy
- tool execution semantics
- context selection
- sandbox backend

## 5. Productization Slices

1. Local transport decision and auth model.
2. Localhost listener MVP.
3. Durable event replay cursor.
4. Transient live event stream.
5. OpenAPI command surface.
6. Desktop adapter smoke surface.
7. Projection escalation decision point.

## 6. Acceptance Criteria

- Local listener cannot be accidentally exposed remotely.
- Client retry does not execute commands twice.
- Stale approval cannot approve changed tool call.
- Durable SSE reconnect can replay from cursor.
- Transient events are marked live-only.
- Desktop/client adapter uses the same kernel/runtime paths as TUI.
- SQLite escalation remains gated by measured query pressure.

## 7. Validation

Recommended checks:

```bash
cargo test -p sigil-http
cargo test -p sigil-runtime projection
cargo test -p sigil-tui runner
```

## 8. Open Questions

- Whether OpenAPI should cover all commands in MVP or only session/run/approval commands.
- E16.7 何时由真实 desktop/server query pressure 触发 SQLite/materialized projection escalation。

## 8.1 Production Closure Track

[RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)
已在不引入 SQLite 的前提下完成 production closure：linearizable durable command identity、
foreground lease、production driver、durable replay、approval/cancel、disclosure、replay+live
SSE、loopback bearer listener、真实 `sigil serve`、process E2E 与 graceful drain 均已落地。
SQLite/materialized projection 仍只在真实 query pressure 出现后重新评估。

## 9. References

- OWASP CSRF Prevention Cheat Sheet: <https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html>
- Chrome Private Network Access preflight overview: <https://developer.chrome.com/blog/private-network-access-preflight>
