# RFC-0043 Desktop Runtime Bridge V1

状态：complete / R43.0-R43.5 implemented

创建日期：2026-07-19

基线：

- Desktop/app server: [RFC-0016](0016-desktop-app-server-productization.md)
- Stable local protocol and real serve: [RFC-0026](0026-stable-machine-protocol-and-real-serve.md)
- Local session lifecycle: [RFC-0027](0027-local-session-lifecycle-v1.md)
- SQLite desktop session catalog: [RFC-0042](0042-sqlite-projection-and-desktop-session-catalog-v1.md)

## 1. Summary

RFC-0042 已让桌面端通过鉴权的 `GET /session-catalog` 查询跨进程重启的历史 session，但历史行仍是
只读候选：当前 `POST /sessions` 只创建新的 durable session，不能把一个 catalog row 重新绑定为 process-local
HTTP session。与此同时，`sigil serve` 只输出面向终端的人类文本，未来桌面 launcher 需要解析非稳定文案，
也没有不依赖 PID 轮询的 parent-lifetime shutdown contract。

本 RFC 补齐 GUI 前的最后一层本机运行桥接：历史 session reopen、机器可读 bootstrap/server metadata、
workspace-scoped server topology、stdin ownership channel、重启后的 durable replay 和真实进程级验收。它不建立
桌面 UI，也不让桌面端直接读取 SQLite。

## 2. Trigger and local evidence

当前实现已经形成可复现的产品缺口：

1. `GET /session-catalog` 返回 `workspace_id + session_ref + session_id` 的历史候选。
2. `HttpSessionCreateRequest` 只有可选 `label`；`HttpSessionRunRegistry::create_session` 总是调用
   `HttpRunDriver::bind_session` 创建新 durable binding。
3. `sigil serve` 的 bind address 只存在于 `bind: ...` 人类文本中，且 RFC-0016 尚未决定单 workspace child
   还是 multi-workspace daemon。
4. 现有 production SSE、approval、cancel、command identity 和 graceful drain 已经足够，不应复制一套桌面
   agent loop。

## 3. Frozen decisions

1. **一 workspace 一 server process。** Desktop V1 为每个打开的 workspace 监管一个 `sigil serve`。不同
   workspace 的 registry、command epoch、projection reconciler、token 和 child lifetime 不共享。未来
   multi-workspace daemon 需要独立 RFC，不能让本 RFC 的 route 暗含跨 workspace selector。
2. **HTTP 是桌面唯一运行契约。** SQLite 仍是可删除 projection；桌面端不能打开数据库文件，也不能用 row
   existence 决定 resume。reopen 必须回到 workspace-bound lifecycle catalog 和 V2 JSONL 重新验证。
3. **projection row 是带 stale guard 的候选。** Reopen request 必须同时提交 relative `session_ref` 和当时看到
   的 durable `session_id`。source 消失、非 ready、identity 改变或当前无法验证时 fail closed。
4. **reopen 只建立 adapter handle。** 它不启动 provider、不追加用户消息、不执行工具。相同 durable scope 在
   同一 server process 中重复 open 返回同一个 process-local session snapshot；first label wins。
5. **token 由 launcher 生成。** Server 继续只从显式 environment variable 读取 bearer token；token 不进入
   argv、startup JSON、metadata、日志或 error。禁用 auth 不是受支持模式。
6. **stdin EOF 是 owner channel。** Desktop 以 pipe 启动 `sigil serve --shutdown-on-stdin-close`；pipe EOF/error
   与 Ctrl-C 任一先到都触发同一 graceful drain。V1 不使用可复用 PID 作为 parent identity，也不要求 server
   反向终止 desktop。
7. **bootstrap 是单行稳定 JSON。** `--startup-output json` 在 listener 完成 bind 后只向 stdout 输出一个 JSON
   object；diagnostic warning 仍只写 stderr。JSON 和鉴权的 `GET /server-info` 复用同一 DTO。

## 4. Goals

1. 通过稳定 HTTP endpoint 把一个已验证的 direct-child V2 JSONL session 重新绑定为 live adapter session。
2. 为桌面 launcher 提供可版本化、无 secret、无需正则解析文案的 bind/readiness/capability handshake。
3. 让 owner pipe closure 触发和 Ctrl-C 等价的 admission stop、run quiescence、listener drain 与 state flush。
4. 证明 server restart 后 catalog -> reopen -> run -> SSE replay/reconnect 仍走既有 durable contract。
5. 保持 TUI、普通 CLI run/resume 和当前文本 `sigil serve` 默认体验不变。

## 5. Non-goals

- 不实现 Tauri/Swift/其他桌面 UI，不新增 workspace picker 或窗口状态。
- 不实现 multi-workspace server、remote bind、multi-user auth、cookie/CORS 或 network daemon。
- 不让桌面直接查询 SQLite，不把 active/approval/progress 写入 projection。
- 不恢复 process-local HTTP run id、transient token delta 或未完成 external side effect；只恢复 durable session
  truth 和 durable protocol replay 能证明的事件。
- 不新增 session path 输入；reopen request永远只接受relative `session_ref`，不接受客户端提交的absolute path。
  现有process-local `SessionSnapshot`仍保留其既有path字段，本RFC不做破坏性wire清理。
- 不修改 TUI session browser，也不新增普通用户配置项。

## 6. Ownership and API

### 6.1 Runtime truth validation

`sigil-runtime::LocalSessionLifecycleService` 新增 typed reopen resolver：

- 输入 kernel `SessionRef` 和 expected durable session id；
- 重新扫描 bounded direct children、拒绝 symlink/non-file/oversized/budget/legacy/invalid；
- 要求 exact `session_ref` 与 durable id 同时匹配；
- 返回 canonical source path 只给 runtime/driver，不进入 HTTP DTO。

随后 production driver 使用现有 `bind_application_session(..., Some(path))` 重新加载 V2 stream，并再次比较
scope/path。SQLite 不参与这个授权判断。

### 6.2 HTTP reopen

新增鉴权 endpoint：

```text
POST /sessions/open
{
  "session_ref": "session-....jsonl",
  "session_id": "...",
  "label": "optional"
}
```

成功返回 `200 SessionSnapshot`。同一 durable scope 已注册时返回现有 snapshot；新注册时也返回 200，避免
客户端从 status code 推断不可靠的进程内创建竞态。失败映射：

- malformed/unsafe/oversized request：`400 invalid_session_open_request`；
- source 不存在：`404 durable_session_not_found`；
- source 非 ready 或 expected identity 漂移：`409 durable_session_not_ready` /
  `durable_session_identity_changed`；
- bounded lifecycle scan或durable load不可用：`503 durable_session_unavailable`。

错误不得包含 absolute path、raw stream 内容或 SQLite diagnostic。

### 6.3 Bootstrap and metadata

`sigil serve --startup-output json` 输出的 V1 object 与 `GET /server-info` 一致，至少包含：

- `schema_version`、`protocol_version`、`server_version`；
- `workspace_id`、实际 `bind_addr`；
- `authentication = "bearer"`；
- `shutdown_on_stdin_close`；
- frozen capability booleans：session catalog、durable session reopen、durable event replay、live events、
  approval、cancellation。

`GET /health` 继续只证明 listener 活着且不要求 auth；完整 metadata 要求 bearer auth。stdout JSON 不包含
token、token-env 名称、workspace root、state path 或 session path。

## 7. Lifecycle and restart contract

1. server 先完成安全 config/token校验，再创建 workspace-bound durable journals/driver/projection。
2. listener bind 成功后构造 immutable server-info；只有此时才发布 startup line。
3. owner pipe EOF/error 或 Ctrl-C 进入同一 shutdown future；registry停止接收新 command，SSE/live listener按
   既有 bounded drain收尾，driver等待 owned run quiescence。
4. Desktop launcher必须持有 child handle、stdin write end 和 per-launch token；launcher退出时先关闭 pipe，
   bounded等待 child，超时后再使用平台 process-tree owner终止。该 launcher实现属于未来 desktop shell，
   本 RFC 用 production-binary process test冻结 child-side contract。
5. 重启创建新的 process-local HTTP epoch/id；客户端重新查询catalog并open，不复用旧 adapter session id。
   Durable SSE cursor仍按 durable session/run scope校验，wrong-scope/ahead cursor fail closed。

## 8. Implementation slices

1. **R43.0 Contract and topology freeze**：完成 inventory、truth/security boundary、API/error/DTO、owner channel和
   acceptance ledger。
2. **R43.1 Durable session reopen**：runtime typed resolver、driver binding、registry idempotency、HTTP route、
   OpenAPI与同层 tests。
3. **R43.2 Machine bootstrap and metadata**：startup output mode、immutable server-info、authenticated route、
   secret-absence和文本模式回归。
4. **R43.3 Owner-channel lifecycle**：stdin EOF shutdown、Ctrl-C parity、admission/drain tests和跨平台可编译
   contract。
5. **R43.4 Process E2E**：真实 binary catalog -> open -> run -> reconnect/restart，以及owner pipe close验收。
6. **R43.5 Completion audit**：OpenAPI/architecture/reference同步、full gate、security/code-quality/implementation
   completeness review。

## 9. Acceptance matrix

- ready direct V2 row能open并继续run；request只含relative ref和expected id。
- missing、deleted、symlink、legacy、invalid、oversized、budget-exceeded和identity drift全部fail closed且无绝对
  路径泄漏。
- 相同 durable scope顺序/并发重复open只形成一个process-local adapter session。
- projection row即使stale或数据库不可用，也不能绕过runtime truth validation。
- JSON bootstrap是单行合法JSON，实际port、workspace/protocol/version/capability准确且不含token/path。
- `/server-info`未鉴权返回401；鉴权结果与startup JSON一致。
- stdin pipe关闭后server完成graceful shutdown；默认交互模式不会因普通terminal stdin状态意外退出。
- restart后旧adapter id不复用；重新catalog/open后能继续durable session。
- macOS/Linux/Windows均可编译；真实平台process测试按现有CI能力执行。

## 10. Validation plan

```bash
cargo test -p sigil-runtime session_reopen
cargo test -p sigil-http session_open
cargo test -p sigil-http server_info
cargo test -p sigil --bin sigil serve_startup
cargo test -p sigil --test serve_process_tests desktop
./scripts/check-touched.sh --tier standard
./scripts/check-docs.sh
git diff --check
```

R43.5 触及 durable resume、public HTTP contract 与真实 process lifecycle，最终执行
`./scripts/check-touched.sh --tier full`。测试只证明本机单用户desktop bridge，不扩大为remote/multi-user server
或外部side-effect undo保证。

## 11. R43.0 result

本地 inventory确认：SQLite catalog已提供恰好足够的candidate identity，但HTTP只有new-session binding；共享
application service已能按existing path load V2 stream，lifecycle catalog已有direct-child/symlink/size/budget/
identity验证。最小正确实现是在runtime暴露typed ref resolver，并由production HTTP driver做第二次durable
binding校验，而不是让listener读SQLite或拼absolute path。

V1 topology冻结为one `sigil serve` per workspace。Parent lifetime采用opt-in stdin owner pipe，避免跨平台PID
reuse/start-time fingerprint问题；server-info在actual bind之后生成，文本/JSON只改变adapter输出，不进入TUI
主心智。预实现独立本地审计未发现需要新增crate或修改kernel event wire的理由。

## 12. R43.1 result

- runtime新增`LocalSessionLifecycleService::resolve_session_for_reopen`与typed error/binding；每次open重新执行
  bounded direct-child lifecycle catalog，只有ready V2 source与expected durable identity同时匹配才返回canonical
  path。
- 新增strict `bind_existing_application_session`；missing source、symlink/non-file和durable load失败不会退化为
  create-new。production HTTP driver同时比较resolver与重新load后的scope/path，drift fail closed。
- HTTP新增`POST /sessions/open`和`SessionOpenRequest` OpenAPI schema；request只接受bounded direct `.jsonl`
  ref、persistence-safe expected id与bounded label，错误映射为400/404/409/503的path-free code。
- registry按durable scope线性去重；重复open返回first process-local handle/label，不创建run、provider request或
  durable append。production `sigil serve`注入与SQLite projection相同的workspace lifecycle owner，但open不读
  projection。
- targeted runtime、registry/route和production-driver tests通过，覆盖ready/missing/legacy/identity drift、
  missing path不创建、auth、wire confinement与duplicate idempotency。

## 13. R43.2 result

- `sigil serve`新增`--startup-output text|json`；默认text输出和现有调用保持不变，JSON模式只在listener bind
  成功、immutable metadata和可选owner watcher建立后输出一行并flush。
- startup JSON与鉴权`GET /server-info`复用`HttpServerInfo` V1 DTO，包含实际bind address、workspace/protocol/
  server version、auth mode、owner flag和冻结的capability集合；不包含token、token-env、workspace root或state path。
- production与non-production listener都先执行bearer auth；只有production surface发布server metadata，OpenAPI与
  route tests冻结401/200/503边界和schema。

## 14. R43.3 result

- `--shutdown-on-stdin-close`显式创建有名称、持有`JoinHandle`的stdin reader；EOF或read error通过oneshot与
  `Ctrl-C`竞争，胜者进入同一现有`serve_until_shutdown` admission stop和bounded drain。
- owner watcher在startup line发布前建立；默认模式不读取stdin，避免普通terminal EOF改变既有`serve`生命期。
- unit与真实process test证明owner pipe关闭后child在deadline内以0退出，stdout/stderr均不泄漏token。

## 15. R43.4 result

- production-binary fixture完成run terminal SSE、terminal cursor reconnect空suffix、graceful stop、server restart、
  historical catalog query、durable reopen、新epoch adapter id、继续run与第二次terminal SSE。
- 另一真实process fixture冻结单行JSON readiness、鉴权`server-info`等价、owner pipe close和secret/path absence。
- registry并发测试证明同一durable scope的八个同时open只形成一个process-local handle，first winner label保持不变。

## 16. R43.5 result

- 完成EN/ZH reference与changelog、RFC-0016/RFC-0026和核心架构说明同步；docs mirror/link/command/public-content
  checks与Pages site gate通过。
- 完成security/code/completeness审计并修正三项：owner watcher建立晚于readiness输出、unknown reopen字段与parse
  error code不一致、projection截断时保留的较老row被普通catalog cap误判为missing。最终reopen对指定direct child
  做独立stream/total-byte-bounded truth validation，不受list cap影响。
- `./scripts/check-touched.sh --tier standard`、`./scripts/check-touched.sh --tier full`与`git diff --check`通过；
  Windows GNU target `cargo check -p sigil`通过。本机Linux cross-check在进入Sigil代码前因缺少
  `x86_64-linux-gnu-gcc`阻断，Linux最终平台证据由既有CI matrix提供。
- 完成审计未发现剩余P0/P1/P2 finding。边界仍不包含desktop UI、multi-workspace daemon、remote/multi-user
  server或对shell/remote side effect的恢复承诺。
