# RFC-0040 MCP Production Reliability and OAuth V1

状态：active / R40.0-R40.1B complete; R40.2 next

创建日期：2026-07-17

基线：

- MCP stdio client: [`sigil-mcp`](../../../crates/sigil-mcp)
- Streamable HTTP core: [`streamable_http`](../../../crates/sigil-mcp/src/streamable_http.rs)
- Remote MCP runtime: [`remote_mcp.rs`](../../../crates/sigil-runtime/src/remote_mcp.rs)
- TUI MCP surface: [`sigil-tui`](../../../crates/sigil-tui)
- Predecessor: [MCP/Web Data Tools architecture baseline](../sigil-rust-agent-core-technical-solution.md)

## 1. Summary

E21.17 已将用户根配置的 stdio 与 Streamable HTTP MCP 接入真实 TUI/CLI runtime，但远端
Streamable HTTP 只支持匿名或静态 credential；遇到 OAuth challenge 时只能返回
`OAuthUnsupported`。同时，RFC-0039 的 hosted Windows 运行发现并行 MCP fixture 偶发同时超过
5 秒 initialize 阈值，且 stdio Windows cleanup 仍调用外部 `taskkill`，没有复用已经在 shell /
terminal 路径证明过的 Job Object ownership。

本 RFC 先收口 stdio 生命周期与并发启动证据，再按 MCP Authorization 2025-11-25 实现用户根
Streamable HTTP 的 Authorization Code + PKCE。OAuth 是显式、TUI-first 的用户流程：401 只投影
`authentication required`，不会在 eager startup 或 agent tool call 中自动打开浏览器。metadata、
registration、token、refresh 与 revoke 请求仍逐次通过 Sigil 的 durable egress disclosure、共享
budget 和 destination guard。token 只存系统 credential store，不写配置、session、support bundle、
日志或错误文本。

## 2. Research basis

- [MCP Authorization 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)
  要求 HTTP transport 使用 OAuth 2.1 security best practices、Protected Resource Metadata、
  Authorization Server Metadata、PKCE 和 RFC 8707 `resource`；client credentials 必须安全保存。
- [MCP 2025-11-25 changelog](https://modelcontextprotocol.io/specification/2025-11-25/changelog)
  将 Protected Resource Metadata discovery、`resource` binding 与 URL-mode elicitation 等列为本版
  协议变化。本 RFC 只实现 authorization，不顺带打开 URL elicitation、sampling 或 tasks。
- [OAuth 2.0 Security Best Current Practice (RFC 9700)](https://www.rfc-editor.org/rfc/rfc9700)
  要求 authorization code client 防止 mix-up、code injection、redirect URI 与 token replay；本设计
  使用 exact issuer/resource/redirect binding、PKCE S256、随机 state 和单 pending flow。
- [OAuth 2.0 Protected Resource Metadata (RFC 9728)](https://www.rfc-editor.org/rfc/rfc9728)
  定义 resource server 如何发布 authorization server metadata 地址；Sigil 不再猜测
  `/authorize` 或 `/token`。
- [OAuth 2.0 Authorization Server Metadata (RFC 8414)](https://www.rfc-editor.org/rfc/rfc8414)
  与 [OpenID Connect Discovery](https://openid.net/specs/openid-connect-discovery-1_0.html) 提供两种
 规范允许的 discovery 路径；issuer 与返回 metadata 必须精确校验。
- [OAuth 2.0 for Native Apps (RFC 8252)](https://www.rfc-editor.org/rfc/rfc8252)
  推荐 loopback redirect 使用 IP literal 与随机端口；V1 只监听 `127.0.0.1`，另提供 headless
  manual callback paste，不监听 wildcard、LAN address 或固定公网 callback。
- 本地竞品代码显示 OpenAI Codex 使用系统 keyring 和 OAuth callback server；Gemini CLI 使用
  keychain 优先的 OAuth credential provider；OpenCode 对 callback `state`、超时、pending flow 与
  manual finish 建立显式状态。Sigil 采用这些可验证模式，但保留自身 durable egress 与不隐式联网
  的更严格边界。

## 3. Goals

1. 修复现有 remote lazy activate/refresh 误走 stdio declaration 注册链的问题；远端 refresh 必须
   transactional replacement，失败时保留旧 generation，不能报告 `Ready { added_tools: 0 }` 假成功。
2. MCP stdio 在 Windows 使用共享 Job Object owner，assignment/termination/wait 失败时 fail closed，
   不依赖 `taskkill` 文本或 PATH。
3. 用受控并发 fixture 区分真实 initialize timeout 与 hosted runner 抖动；不以单次重跑为由直接
   放宽产品 deadline。
4. 为用户根 Streamable HTTP MCP 支持 MCP 2025-11-25 Authorization Code + PKCE S256。
5. 实现 RFC 9728 protected-resource discovery、RFC 8414/OIDC authorization-server discovery、
   exact issuer 校验与 RFC 8707 `resource` binding。
6. 支持预配置 public client id；只有 server metadata 明确声明 registration endpoint 时才允许
   Dynamic Client Registration。DCR 失败不回退到猜测 client identity。
7. token、refresh token、client registration secret 与 expiry metadata 进入系统 credential store；
   store 不可用时给出 typed action，不落 plaintext fallback。
8. refresh 只在发送 MCP request 前因明确 expiry/skew 或显式用户动作执行；401 只改变 auth state，
   不透明 refresh/retry 已发送的 request。任何重试都是新的 physical attempt 和 durable authorization。
9. TUI 显示 `authentication required / signing in / signed in / refresh required / auth failed`，并提供
   Sign in、copy URL、manual callback、clear/revoke 等显式动作。
10. 用本地 mock authorization/resource server、真实 callback listener、key-store double、恢复测试和
   platform CI 证明授权流程，而不是依赖真实第三方账号进入常规 gate。

## 4. Non-goals

- 不支持 stdio MCP、plugin-owned remote MCP、legacy SSE 或 bundled anonymous Exa profile 的 OAuth。
- 不支持 client credentials grant、device authorization grant、service account、enterprise SSO 管理、
  mTLS、DPoP、JWT assertion 或 provider-specific identity SDK。
- 不实现 CIMD；V1 只消费静态 client id 或规范允许的 DCR。
- 不自动打开浏览器、不在 eager activation 中自动登录、不在 agent tool call 中弹出交互。
- 不将 system keyring 不可用静默降级为普通文件、环境变量、session JSONL 或 config TOML。
- 不宣称 logout 可以撤销已经发生的 remote side effects；revoke 与 local clear 只管理 credential。
- 不顺带实现 URL elicitation、sampling、tasks、legacy SSE、远端 MCP 自动信任或新版本发布。
- 不把共享 process owner 扩成 shell、sandbox、terminal 或 MCP framework；新 crate 只持有跨平台
  process-tree ownership、terminate/reap 和 cleanup proof。

## 5. Product scope and configuration

Threat/mitigation matrix：

| Threat | Mandatory mitigation | Failure behavior |
| --- | --- | --- |
| token audience/confused deputy | exact canonical resource + issuer + client + scopes + config fingerprint binding; RFC 8707 resource on authorize/token/refresh | reject credential and require a new sign-in |
| discovery poisoning / cross-origin pivot | RFC 9728 relationship checks, exact issuer, HTTPS-only endpoint, per-destination durable egress + DNS guard | stop before registration/token request |
| authorization code interception/injection | loopback IP literal, random port, exact redirect, PKCE S256, high-entropy single-use state, 5-minute TTL | consume/cancel flow without token exchange |
| token leakage | non-serializable secret carrier, redacted Debug/error, keyring-only store, bounded zeroizing buffers, canary tests | fail closed; never plaintext fallback |
| duplicate/ambiguous remote effect | no transparent retry after 401 or body-send ambiguity; new request requires new physical attempt | return typed failure and require explicit retry |
| refresh race/rotation loss | per-scope single-flight and atomic whole-record replace before exposing new snapshot | old snapshot becomes unusable; require sign-in if persistence fails |
| false logout claim | remote revoke and local clear are separate states/actions | report unproven revoke and retain/clear locally only by explicit choice |
| worker/UI deadlock | owned async flow with cancellation; modal consumes input exclusively; eager/headless never waits for callback | cancel and release listener/verifier without blocking worker loop |

OAuth 只适用于 `origin = user_root` 且 `transport = streamable_http` 的 server。原有匿名、literal/env
header 与 bearer-env 继续可用，但同一 server 不允许同时配置静态 `Authorization` carrier 与 OAuth。

公开配置增加一个可选的 `oauth` table：

```toml
[[mcp.servers]]
name = "example"
transport = "streamable_http"
url = "https://mcp.example.com/mcp"

[mcp.servers.oauth]
client_id = "sigil-public-client" # optional when DCR is advertised
scopes = ["mcp:tools"]             # optional; bounded and normalized
```

不接受 literal client secret。若 authorization server 通过 DCR 返回 confidential credential，该
secret 与 registration metadata 直接进入 system credential store，永不回写配置。`oauth` table 本身
只声明 public identity 与最小 scope intent，不表示已经登录。

配置验证必须：

- 限制 client id、scope count、单项 bytes 与总 bytes；拒绝控制字符、空 scope 和重复 scope；
- canonicalize MCP resource URI：去 fragment，保留规范相关 path/query，并要求 `https`；
- 拒绝与静态 Authorization/bearer credential 并存；
- 不把 token、authorization URL、authorization code 或 verifier 暴露给 serde/Debug。

## 6. Discovery and authorization contract

### 6.1 Authentication challenge

Resource server 返回 401 时，client 解析大小受限的 `WWW-Authenticate: Bearer` challenge。优先消费
规范的 `resource_metadata`；若缺失，按 RFC 9728 对 canonical resource URI 构造 well-known URL。
challenge 只更新 ephemeral activation state，不自动发 metadata 请求或打开浏览器。

### 6.2 Protected resource and authorization server metadata

用户执行 Sign in 后，Sigil 依次：

1. 为 protected-resource metadata destination 取得新的 durable disclosure/budget attempt；
2. 通过 shared destination guard 发送无 redirect、无 retry、无 cookie、无 referrer 的 bounded GET；
3. 校验 `resource` 与 configured canonical MCP resource exact match；
4. 从 `authorization_servers` 选择唯一、HTTPS、策略允许的 issuer；多个 issuer 且无确定选择时要求
   用户选择，不能取第一个；
5. 对 issuer 同时按 RFC 8414 与 OIDC discovery 的规范顺序查询 metadata，并校验返回 `issuer`；
6. 要求 HTTPS authorization/token endpoints、`S256` PKCE，以及 authorization code response；
7. 只有 metadata 明确给出 HTTPS registration/revocation endpoint 时才使用对应能力。

每个跨 origin metadata、registration、token、refresh、revoke endpoint 都是独立 network destination，
必须重新走 durable egress barrier 与 destination guard。一次 server approval 不自动批准任意 OAuth
issuer 或 token endpoint。

### 6.3 Authorization flow

每个 server 同时最多一个 pending flow。flow 使用系统 CSPRNG 生成 PKCE verifier、S256 challenge、
state 和 nonce-like flow id；它们只存在于拥有该 flow 的 runtime task 中，取消、超时、session切换
或 shutdown 即清除。callback deadline 为 5 分钟。

authorization request 至少绑定：

- exact authorization endpoint；
- selected client id；
- exact loopback redirect URI；
- `response_type=code`；
- PKCE S256 challenge；
- random state；
- configured minimal scopes；
- RFC 8707 canonical MCP `resource`。

TUI 默认提供 Open browser 与 Copy URL；打开浏览器需要用户显式按键。headless/浏览器不可用时，用户
可粘贴完整 callback URL。local listener 只 bind `127.0.0.1:0`，校验 request target bytes cap、state、
single use 和 exact redirect；不解析任意 HTML form，不监听 IPv6 wildcard 或 LAN interface。成功或
失败均返回固定、无 token/code 回显的最小 HTML。

token exchange 再次绑定同一个 redirect URI、verifier、client id 与 RFC 8707 resource。state、code、
verifier、token response body 和 authorization URL 不得进入 durable session、support bundle、tracing
或用户可复制的 error detail。

## 7. Credential lifecycle contract

credential scope key 至少绑定：server name、canonical resource URI、authorization-server issuer、
client id 与 normalized scopes。任一配置变化产生新 scope，旧 credential 不会被静默复用。

system credential store 保存一个 versioned、size-bounded record：access token、可选 refresh token、
token type、expiry、issuer/resource/client/scopes binding 与可选 DCR registration。所有 secret-bearing
buffer 在 drop 前 best-effort zeroize；公开状态只投影 `present / expiring / expired / unavailable`。

刷新规则：

- access token 在安全 skew 内过期时，MCP request 发送前执行一次 single-flight refresh；
- 同一 credential scope 的并发 request 共享 refresh owner，其余等待结果而不并发旋转；
- refresh request 绑定 issuer、client、refresh token 与 RFC 8707 resource；
- authorization server 返回新 refresh token 时，先原子替换完整 credential record，再销毁旧值；
- refresh token invalid/expired 时清除 access-token usability 并投影 `authentication required`，不循环；
- 任意 401 只使当前 request 失败并更新 auth state；不得在该 request 内自动 refresh/retry。后续显式
  refresh 或重新调用必须取得新的 credential snapshot、physical attempt 与 durable authorization；
- MCP body 已开始发送后的 transport ambiguity 同样不 retry；用户可重新触发只读/写调用。

静态 headers 与动态 OAuth bearer 是两个 carrier。每次 MCP request 在发送前从 runtime-owned
credential source 取得一次不可序列化 snapshot；snapshot 的 process-random-key HMAC 与 static
header fingerprint 一起绑定本次 authorization/dial plan。refresh rotation 产生新 snapshot 和新
fingerprint，不能原地改写激活期 `ResolvedHeaders`，也不能复用旧 dial plan。

Sign out 是两个可区分动作：若 metadata 有 revocation endpoint，先尝试 revoke；无论 remote revoke 是否
可证明，TUI 都明确展示结果并让用户单独确认 local clear。`Clear local credential` 删除 keyring record
并刷新 server state，但不声称 remote token 已撤销。

## 8. Runtime and TUI state model

`sigil-mcp` 持有协议纯类型、metadata/PKCE/callback/token codec 与 credential carrier；
`sigil-runtime` 持有 user-root policy、durable egress orchestration、single-flight flow/refresh owner 和
server activation；`sigil-tui` 只消费 typed state/action，不解析 OAuth URL 或 token response。kernel
不增加 provider/OAuth 私有字段。

server auth projection：

```text
not_configured
  -> authentication_required
  -> discovering
  -> awaiting_browser | awaiting_callback
  -> exchanging_code
  -> signed_in
  -> refreshing
  -> signed_in | authentication_required | failed
```

取消属于 terminal state，且不得留下 listener、pending verifier 或半写 credential。TUI MCP detail
modal 显示 resource、issuer（发现后）、scope、credential state 和最近 typed error；默认 MCP list 不
展示 token、code、完整 authorization URL 或重复的大段安全提示。

动作保持 modal-exclusive：Enter/明确 action button 才 Sign in；`O` open browser、`C` copy URL、`M`
manual callback、Esc cancel；signed-in 状态提供 Refresh 与 Sign out。composer 不得抢占这些按键。
CLI eager startup 只报告 actionable authentication-required 错误；V1 不增加交互式 CLI OAuth wizard。

## 9. Stdio reliability contract

Windows `sigil-mcp` stdio child 使用与 RFC-0039 同等的 kill-on-close Job Object semantics：spawn 后
立即 assignment，失败则终止并回收 direct child；timeout/cancel/drop 使用 Job Object termination，
wait 或 bounded stdout/stderr drain 未收敛时不能报告 clean shutdown。产品代码移除 `taskkill`。

initialize deadline 保持 absolute 与 bounded。测试增加 barrier-controlled N-way startup、单 server
timeout、slow-but-in-budget、cancel 和 descendant cleanup。hosted 并发证据若仍显示稳定资源竞争，
应优先限制 test fixture concurrency 或隔离平台 suite；只有真实产品 server 在正常机器上持续超过
deadline 的证据才调整公开 timeout。

## 10. Implementation slices

1. R40.0：RFC、官方/竞品/实现 inventory、security contract、执行台账与预审。
2. R40.1A：remote lazy activate/refresh 正确分流与 transactional generation replacement。
3. R40.1B：共享 `sigil-process` owner、MCP stdio Windows Job Object、`taskkill` 移除与并发 evidence。
4. R40.2：OAuth metadata/discovery、PKCE、DCR、loopback/manual callback 与 bounded protocol tests。
5. R40.3：system keyring credential、expiry/single-flight refresh、rotation、clear/revoke。
6. R40.4：runtime egress orchestration、config schema、typed activation state与 TUI-first auth interaction。
7. R40.5：mock conformance、真实 binary/manual smoke、Windows/Linux/macOS gate、EN/ZH docs/site 与终审。

## 11. Acceptance criteria

- lazy remote activation 真实连接并注册工具；remote refresh 成功替换 generation，失败保留旧 client /
  tools，且 `Ready` 不允许用未尝试 transport 的零工具路径假成功。
- Windows MCP stdio cleanup 无 `taskkill`，assignment/terminate/wait/drain 任一失败均 fail closed；
  tools 与 MCP 共用 `sigil-process` ownership contract。
- N-way initialize fixture 可重复区分 product timeout 与 test scheduling，不通过盲目增加 deadline 绿化。
- OAuth 只在 user-root Streamable HTTP 且显式 Sign in 后开始；eager startup 不打开浏览器。
- RFC 9728、RFC 8414/OIDC metadata、issuer、HTTPS endpoint、PKCE S256、state、redirect 与 RFC 8707
  resource 全部校验；缺一项不得 exchange token。
- loopback 只监听 `127.0.0.1` 随机端口；wrong state、duplicate callback、timeout、oversize request、
  manual malformed URL 全部 fail closed。
- token、refresh token、client secret、authorization code、verifier 不出现在 config/session/log/error/
  support bundle，keyring 不可用时无 plaintext fallback。
- refresh single-flight、rotation、expired/invalid refresh、401-after-send 和 revoke failure 都有测试。
- OAuth 所有网络请求逐 destination 通过 durable egress/budget/guard；redirect 与 reqwest retry 禁用。
- TUI auth modal 独占按键，能完成 open/copy/manual/cancel/refresh/sign-out，composer 不抢焦点。
- mock auth/resource server 与 real binary smoke 通过；affected workspace、strict Clippy、docs/site、deny/
  audit 和 platform CI 全绿；最终审计无剩余 P1/P2。

## 12. Validation

```bash
cargo test -p sigil-mcp
cargo test -p sigil-runtime remote_mcp
cargo test -p sigil-tui mcp
cargo clippy -p sigil-mcp -p sigil-runtime -p sigil-tui --all-targets -- -D warnings
cargo fmt --all --check
./scripts/check-docs.sh
./scripts/check-pages-site.sh
cargo deny check
cargo audit --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0436
git diff --check
```

Windows Job Object 与 platform keyring claims 必须由真实 hosted runner 执行；mock server 可以进入普通
CI，但真实第三方 OAuth account 不进入必跑 gate。

## 13. Progress

- R40.0 complete. 已完成 MCP 2025-11-25、OAuth RFC、当前 Sigil runtime 与 Codex/Gemini/OpenCode
  实现 inventory。独立预审未发现 P0；发现的 remote lazy/refresh 错误路由、动态 credential snapshot
  与多 destination egress 三项 P1 已进入冻结契约，并将原 R40.1 拆为 activation correctness 与共享
  process ownership 两片。docs link/mirror/command metadata 与 diff gate 通过；本机 Ruby 2.6 缺少
  `Array#filter_map`，使用只作用于 gate 进程的兼容 polyfill 执行了相同脚本，未修改仓库脚本。
- R40.1A complete. Product-surface activation/refresh 现在先按真实 transport 分流：lazy stdio 只进入
  declaration process path，Streamable HTTP 通过 durable remote activator。remote replacement 在旧
  generation 仍注册时完成 connect/initialize/pin/tools-list，失败保持旧 owner/tool；成功后才替换并
  retire。TUI invalid-endpoint regression 证明 lazy/refresh 不再返回零工具假成功；runtime 回归证明
  replacement preflight 失败后旧 generation identity 不变。runtime/TUI targeted tests、crate check、
  per-crate `--no-deps` strict Clippy、rustfmt 与 diff 通过。全 dependency Clippy 被同期未提交的
  `sigil-kernel/agent.rs` 三处 `needless_borrow` 阻塞，本切片未修改或吸收该并发工作。
- R40.1B complete. 新增最小 `sigil-process` crate，把内置工具既有的 Windows kill-on-close Job
  Object owner 收口为跨 crate lifecycle primitive；tools 通过兼容 façade 复用它，stdio MCP 的 local
  与 runtime-planned child 都在 spawn 后立即绑定，assignment 失败即 fail closed 并 bounded reap。
  MCP 清理已移除 `taskkill`，显式终止共享 Job Object 并等待 direct child；zero-surface/drop 路径新增
  Windows descendant conformance。4-server barrier fixture 在不增加 5 秒产品 deadline 的前提下稳定
  通过。macOS MCP 168/168、process 1/1、tools 193/193（1 ignored）、targeted strict Clippy、rustdoc、
  test-layout、fmt/diff 与 `sigil-process` Windows target check 通过；完整 MCP Windows cross-check 受本机
  缺少 MinGW C compiler 阻塞，真实 Windows 执行证据由 R40.5 hosted gate 收口。
