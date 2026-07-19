# RFC-0044 Desktop Shell MVP V1

状态：complete / R44.0-R44.6 complete

创建日期：2026-07-19

基线：

- Desktop/app server: [RFC-0016](0016-desktop-app-server-productization.md)
- Stable local protocol and real serve: [RFC-0026](0026-stable-machine-protocol-and-real-serve.md)
- SQLite desktop session catalog: [RFC-0042](0042-sqlite-projection-and-desktop-session-catalog-v1.md)
- Desktop runtime bridge: [RFC-0043](0043-desktop-runtime-bridge-v1.md)

## 1. Summary

RFC-0043 已冻结 one `sigil serve` per workspace、机器可读 bootstrap、per-launch bearer、stdin owner pipe、
durable reopen 和 HTTP-only desktop contract。继续停留在 adapter 层不会再产生新的产品证据：下一步应建立真正
的桌面壳，但第一批必须先证明 launcher 能安全持有 child、token 和 shutdown ownership，而不是先搭一个不能运行
任务的空界面。

本 RFC 冻结 Tauri 2 + React/TypeScript/Vite 技术栈和桌面边界，并按 launcher supervisor、typed HTTP client、
workspace/history shell、conversation/run、approval/cancel/verification、packaging/dogfood 六个可验收切片实施。
桌面只是 TUI 之外的第二个 adapter，不复制 agent loop，不直接读取 SQLite/JSONL，也不改变 TUI-first 的产品定位。

## 2. Research and trade-off decision

### 2.1 Official framework evidence

- [Tauri 2](https://v2.tauri.app/start/) 使用系统 WebView，可把 Rust command 和 WebView 权限限制在显式能力内；
  [capability contract](https://v2.tauri.app/reference/acl/capability/) 能按 window/webview 绑定 command permission。
- Tauri 的 [sidecar support](https://v2.tauri.app/develop/sidecar/) 可以打包外部 binary，但本项目不把 owner lifecycle
  委托给 renderer shell plugin；child supervision 留在可测试的 Rust backend/library。
- Tauri 不捆绑同一浏览器引擎；[WebView version](https://v2.tauri.app/reference/webview-versions/) 随平台而异，
  因而 CSS、IME、clipboard、scroll 和 accessibility 必须做 macOS/Linux/Windows UI smoke，不能只以 Chromium
  开发环境为准。
- Electron 提供一致 Chromium 和成熟分发，但其官方 [process model](https://www.electronjs.org/docs/latest/tutorial/process-model)
  引入 main/renderer/preload 边界；官方 [security guidance](https://www.electronjs.org/docs/latest/tutorial/security)
  还要求持续维护 context isolation、sandbox、navigation、IPC sender 和 dependency/update surface。

### 2.2 Competitor evidence

- Goose `ui/desktop` 与 OpenCode `packages/desktop` 当前均使用 Electron + React/Vite；它们证明成熟 Chromium、
  Playwright 和多平台 packaging 的工程价值，也展示了 Node/main/preload/renderer 与浏览器 runtime 的额外 footprint。
- Goose 把 OpenAPI schema/client generation 纳入桌面构建，是 Sigil R44.2 应采用的 drift gate；Sigil 只借鉴
  contract generation，不让 renderer 直接持有 HTTP credential。
- DeepSeek Reasonix `desktop` 使用 Wails + React/Vite，并让 UI 直接绑定 Go kernel。Sigil 明确拒绝这种路线：
  RFC-0043 已规定 HTTP 是桌面唯一运行契约，桌面不能绕过 runtime/approval/session truth。

### 2.3 Frozen stack

| Layer | V1 choice | Reason |
| --- | --- | --- |
| Native shell | Tauri 2 | Rust ownership 与现有 workspace 对齐；capability allowlist；不捆绑 Chromium |
| Frontend | React + TypeScript + Vite | 适合状态密集的 conversation/history/approval UI，竞品已有充分工程证据 |
| Runtime transport | loopback HTTP/SSE only | 复用 RFC-0026/RFC-0043，不复制 kernel/runtime 或 SQLite access |
| Client contract | checked OpenAPI snapshot + Rust typed client + generated frontend DTO types | build/CI 检测 wire drift；HTTP/bearer 留在Rust，renderer只收窄化IPC DTO |
| Process owner | Rust `sigil-desktop` library | token、child、stdin pipe、bootstrap 和 fallback kill 不进入 renderer |
| Package layout | `crates/sigil-desktop` + `apps/desktop` | 前者可无 GUI 做真实 binary 测试；后者只拥有 Tauri/UI/packaging |

R44.0 只冻结 stack，不立即引入 Tauri/npm 依赖。R44.1 先交付可独立审计的 Rust launcher；R44.2 建立
`apps/desktop` 时才锁定具体 Tauri、React、Vite 和 codegen 版本并更新供应链台账。

## 3. Architecture and trust boundary

```text
React renderer
  -> narrow Tauri commands/events (no token, no raw filesystem/process API)
Tauri Rust backend
  -> sigil-desktop launcher + typed HTTP client
  -> owns one child/token/stdin pipe per open workspace
sigil serve
  -> sigil-http -> sigil-runtime -> sigil-kernel
  -> JSONL/lifecycle truth + rebuildable SQLite projection
```

Hard boundaries：

1. Renderer 永远拿不到 bearer token、child handle、absolute state/cache/session path、直接HTTP client或任意shell API。
2. Desktop 只通过鉴权 HTTP/SSE 操作 session/run/approval/cancel；不得依赖 `sigil-http` server internals、
   `sigil-runtime`、`sigil-kernel` 或直接数据库绑定。
3. 每个 workspace 有独立 child、token、HTTP epoch 和 lifecycle；V1 不建立 multi-workspace daemon。
4. UI 状态不是 durable truth。重启、窗口恢复和 reconnect 必须重新 catalog/open/snapshot/replay。
5. Tauri capability 默认 deny；只向主窗口暴露 frozen desktop commands。不开 remote content、Node integration、
   arbitrary URL navigation、generic shell、generic filesystem 或 generic HTTP plugin。

## 4. R44.1 launcher supervisor contract

### 4.1 Ownership

新增 library-only `sigil-desktop` crate。现有 crate 不适合承载该职责：

- `sigil` 是被监管的 server binary，不能同时充当自己的 desktop parent；
- `sigil-http` 只拥有 server framing/auth/registry，不能反向拥有 client process；
- `sigil-process` 只拥有通用 process-tree primitive，不应知道 config、bootstrap、bearer 或 HTTP handshake；
- `apps/desktop` 的 Tauri glue 后续应保持薄，不重复一套 supervisor。

依赖方向固定为 `apps/desktop -> sigil-desktop -> sigil-process + generic transport/serde`。`sigil-desktop` 不依赖
kernel、runtime、TUI 或 HTTP server crate。

### 4.2 Launch and readiness

1. 输入 exact `sigil` binary、config path、workspace root 与 bounded timeouts；启动前校验 binary/config/workspace
   基本类型，不把路径写入 error/display。
2. 用系统 CSPRNG 生成 256-bit per-launch token，只通过 `SIGIL_HTTP_TOKEN` environment 注入；argv、Debug、
   bootstrap、error 与 frontend command result 均不可包含 token。
3. 启动 `sigil --config <path> serve --startup-output json --shutdown-on-stdin-close`，stdin/stdout/stderr 使用 pipe；
   child 进入平台 process-tree owner。
4. 在总 startup deadline 内读取最多 16 KiB 的单行 bootstrap；拒绝 EOF、超限、非 JSON、unknown schema、
   non-loopback bind、非 bearer、owner flag false 或缺失 V1 capability。
5. 用同一 private token 对 `GET /server-info` 做一次 no-proxy/no-redirect/bounded HTTP 校验，要求与 bootstrap
   完全一致后才返回 ready。

### 4.3 Shutdown and failure

- 正常关闭先 drop stdin owner pipe，再 bounded wait；deadline内成功退出报告`graceful`。
- 超时后终止整个process tree并reap direct child；native non-success报告`forced`。如果owner shutdown恰好与
  fallback竞争并在deadline后以0退出，报告`graceful_after_deadline`；不能把该竞态伪装成已执行SIGKILL。
  整树终止失败且server也未证明successful drain时fail closed，不能只kill parent后报告成功。
- launch 任一步失败都执行同一 cleanup；Drop 是最后一道 synchronous process-tree kill 防线，不替代显式 async
  shutdown。
- stderr/stdout 由 bounded drain 消费以避免 child 阻塞，但 R44.1 不把 raw diagnostic 暴露给 UI；后续支持包只能
  经过既有 privacy/redaction contract。

## 5. Product scope

### Goals

1. 建立真实可安装桌面应用的最小技术/安全基础，而不是第三套 agent runtime。
2. 让用户选择 workspace、浏览历史、打开/新建 conversation、运行 prompt、处理 approval/cancel/verification。
3. 桌面退出、workspace close、server crash/restart 都有清晰恢复路径和真实 lifecycle ownership。
4. 保持 TUI 行为、快捷键和默认分发不变；desktop 是独立 opt-in artifact。

### Non-goals

- 不在 V1 实现 remote/multi-user server、cloud sync、mobile、IDE extension 或 browser-hosted UI。
- 不直接撤销 shell/remote side effect，不扩大 checkpoint/rewind 承诺。
- 不把 SQLite 变成 source of truth，不允许 renderer 查询 database/file paths。
- 不在第一版复制 `/config` 的全部高级配置；优先完成 daily loop。
- 不因 Tauri 存在就开放 generic shell/filesystem/network capability。

## 6. Implementation slices

1. **R44.0 Stack, topology and security freeze**：官方/竞品调研、framework decision、crate/app ownership、
   capability/process/HTTP boundaries、acceptance ledger。
2. **R44.1 Launcher supervisor**：`sigil-desktop`、CSPRNG secret carrier、bounded bootstrap、authenticated
   server-info equality、cross-platform process-tree ownership、graceful/forced cleanup、真实 `sigil` process E2E。
3. **R44.2 Desktop skeleton and generated contract**：Tauri/React/Vite app、deny-by-default capability、checked
   OpenAPI snapshot/drift gate、Rust typed client、generated frontend DTO、workspace picker、one-process-per-workspace
   manager、connection/crash state。renderer不直接调用loopback HTTP。
4. **R44.3 Workspace and history shell**：recent workspace、catalog pagination/filter/search、new/open session、
   loading/empty/stale/error/rebuild states；不直接读 SQLite。
5. **R44.4 Conversation and run surface**：message timeline、composer、run start、durable/live SSE merge、reconnect、
   terminal snapshot；不复制 event reduction。
6. **R44.5 Control and verification loop**：approval detail/decision、cancel/drain、verification recommendation、单项重跑、
   failure/receipt inspect；真实副作用边界与 TUI 一致。
7. **R44.6 Packaging and dogfood audit**：macOS/Linux/Windows package、signing/notarization/updater decision、
   accessibility/IME/clipboard/scroll smoke、crash/restart/upgrade、docs/site 和 full completion audit。

每个 slice 是独立 commit boundary。R44.2 以后才增加 npm/Tauri dependencies；R44.3 以后每个可见 surface 都要有
真实 server contract test 和至少一个 UI interaction test。

## 7. R44.1 acceptance matrix

- 真实 `sigil` binary 启动后 launcher 只在 `/server-info` 鉴权并与 bootstrap 一致时返回 ready。
- token 由 32 random bytes 生成；argv、public Debug/error、stdout/bootstrap、server-info 无 token。
- malformed/oversized/incompatible bootstrap、server-info mismatch、early exit、timeout 全部 fail closed并reap child tree。
- owner pipe close 后真实server在deadline内0退出；零/极短deadline必须进入bounded fallback分支并返回
  `forced`或竞态可证明的`graceful_after_deadline`，且不遗留direct child/process-group descendant。
- bind 必须 loopback；proxy environment 不参与 localhost handshake；redirect 不跟随；response bounded。
- launcher crate 不依赖 kernel/runtime/TUI/sigil-http，server crate也不依赖 launcher；`sigil` 只用 dev-dependency
  完成 production-binary acceptance。
- macOS/Linux/Windows 编译；平台 process-tree runtime evidence按 CI 能力执行。

## 8. Validation plan

```bash
cargo test -p sigil-process
cargo test -p sigil-desktop
cargo test -p sigil --test serve_process_tests desktop_launcher
cargo check -p sigil-desktop -p sigil
cargo clippy -p sigil-desktop -p sigil-process -p sigil --all-targets -- -D warnings
cargo fmt --all --check
./scripts/check-touched.sh --tier standard
git diff --check
```

R44.6 最终执行 full workspace、supply-chain、docs/site、三平台 package/runtime 与 security/code-quality/
implementation-completeness audit。R44.1 完成不等于已有桌面 UI 或可发布桌面 artifact。

## 9. R44.0 result

官方和本地竞品对照确认：Electron 的一致 Chromium/成熟 packaging 有价值，但会新增 Node main/preload/renderer
安全与更新面；Wails direct-kernel 路线违反现有 HTTP-only contract。Tauri 2 更适合把 process/token ownership 留在
Rust 且通过 capability 限制 WebView，不过系统 WebView 差异必须成为 R44.6 的三平台验收项。

仓库 inventory 同时确认首个 crate boundary 必须是 desktop-owned launcher，而不是修改 kernel/runtime 或让
Tauri glue 直接 spawn。R44.1 因此先以真实 production binary 验证 launcher/server lifecycle，再进入 UI scaffold。

## 10. R44.1 result

R44.1 已新增 library-only `sigil-desktop` crate，并把 secret、strict bootstrap/server-info protocol 与 launcher
supervisor 分离。launcher 使用 32-byte CSPRNG bearer、16 KiB 单行 bootstrap 上限、no-proxy/no-redirect HTTP
client和总startup deadline；只有 authenticated `/server-info` 与bootstrap完全相等时才返回ready。public Debug、
error与process arguments均不包含token或输入路径。

`sigil-process` 现在提供平台无关的process-tree配置/终止边界：Unix child进入独立process group并以group为单位
终止，Windows继续使用Job Object。显式shutdown优先关闭stdin owner pipe；超时后终止整树并reap direct child，
同时区分`forced`和可证明的`graceful_after_deadline`竞态。launch失败和Drop fallback也复用同一fail-closed边界。

真实production-binary验收覆盖authenticated ready、未鉴权401、正常owner-pipe退出、零deadline fallback和无效
config early-exit cleanup；Unix额外证明descendant不会遗留。完整workspace test、Clippy、rustdoc、docs、供应链与
Windows GNU target check均通过。Linux cross-target本机验证在`ring` build阶段因缺少`x86_64-linux-gnu-gcc`被环境
阻塞，未发现Sigil source-level error；Linux runtime/packaging evidence继续保留为R44.6/CI gate。

该结果只完成desktop process/security core；尚未创建可见窗口、frontend或可分发desktop artifact。R44.2现已
解锁，下一步建立Tauri/React skeleton、checked contract drift gate和one-process-per-workspace manager。

## 11. R44.2 result

R44.2已建立真实`apps/desktop` Tauri 2 + React/TypeScript/Vite应用和`sigil-desktop-app` workspace member。
主窗口capability只允许bootstrap、native workspace picker和close三个Sigil command；renderer没有dialog、filesystem、
shell、process或generic HTTP permission，production CSP禁止remote content。workspace path只在Rust picker callback与
manager中流转，IPC summary不包含path、bearer、loopback address或process handle。

`sigil-desktop`现提供独立typed HTTP client与one-process-per-canonical-workspace manager。client复用launcher私有
bearer和no-proxy/no-redirect transport，限制request timeout与2 MiB response；server-private session log path只可
反序列化且Debug脱敏，不具备IPC serialization surface。真实production `sigil`测试已证明重复打开复用同一server，
并通过typed client完成list/create/list后graceful close。

`sigil-http` OpenAPI通过repository script导出为checked JSON snapshot，再由`openapi-typescript`生成frontend schema；
CI和本地`pnpm check`会重新生成并byte-compare后执行typecheck、UI interaction tests和production build。macOS Tauri
debug native build已通过。Cargo/npm供应链扫描均通过；当前Tauri Linux上游GTK3、glib与urlpattern/rust-unic传递
风险已按精确RustSec ID记录并限制在desktop graph，R44.6 Linux runtime/package evidence仍是分发阻塞门禁。

该结果只交付可启动的安全桌面骨架与连接状态，不宣称已有history、conversation、approval或可安装bundle。
R44.3现可在不读取SQLite/JSONL的前提下实现recent workspace与HTTP catalog/new/open session surface。

## 12. R44.3 result

R44.3已交付workspace/history shell。native recent store只在Tauri app config目录持有canonical workspace path，
使用versioned bounded JSON与同目录临时文件原子替换；renderer只得到workspace ID、display name和open状态。
recent reopen仍重新经过`DesktopWorkspaceManager` canonicalization、config验证、独立server readiness与authenticated
HTTP contract，不把recent记录当授权或durable session truth。

新增IPC只暴露bounded catalog query、renderer-safe catalog row、create/open session summary。catalog经
`sigil-desktop` typed client请求`/session-catalog`，支持generation-consistent pagination、search、provider、pinned与
source-state filter；绝对路径、session log path和durable scope不会进入IPC。open只接受server catalog返回的direct-child
reference与durable ID，并由server再次验证JSONL/lifecycle truth。

React shell已覆盖recent reopen、workspace切换、history rebuild/loading/empty/degraded/error/stale cursor、load more、
new conversation和ready-only durable reopen。stale cursor不会把两代row拼接，而是要求从第一页刷新。4个UI interaction
tests覆盖空/recent、new/close、分页/open和stale恢复；8个native tests覆盖recent持久化/上限/脱敏、query/reference边界
与session私有字段丢弃。真实production server test已通过typed catalog和reopen后再graceful close。

该结果只建立conversation选择与process-local session handle；尚未展示message timeline、发送prompt或消费SSE。
R44.4现在可以在同一session handle上实现start run、durable/live event merge和terminal reconciliation。

## 13. R44.4 result

R44.4已交付conversation/run surface。`sigil-desktop`新增bounded SSE decoder和独立HTTP protocol envelope，校验
content type、schema、stream identity、durable cursor与SSE `id:`一致性，并把public run event收窄成不含bearer、
raw tool args或server-private path的renderer timeline DTO。桌面仍只消费`sigil serve`既有durable replay和transient
live fan-out，不复制agent loop或把UI状态写回session truth。

native backend现在拥有每个active run的background follower；45秒keepalive deadline触发reconnect，`Last-Event-ID`
只在durable event成功投递后推进，stream gap、EOF和transport failure都会从最后durable cursor重连。terminal event正常
关闭follower；若terminal frame丢失，则以`GET /runs/{run_id}` server snapshot收敛。workspace close与app exit会先abort
所有owned follower，再关闭对应server，避免detached task。

React surface新增message timeline、composer、run start和明确的live/reconnecting/terminal状态。timeline reducer以
assistant message为单一完成行：transient delta先增量显示，durable assistant message覆盖草稿，`run_finished`只校准
terminal而不再生成第二份reply。真实production `sigil serve`+provider fixture测试已证明run start、live delta、durable
assistant/terminal、terminal close和`Last-Event-ID` suffix replay；UI interaction test证明delta/message/finish只显示一份
assistant reply。

历史catalog仍只包含body-free metadata；R44.4没有通过直接读取JSONL/SQLite伪造旧message replay。打开旧session后可继续
新run，但历史正文浏览需要未来独立、bounded、server-owned transcript contract，不能从catalog越权推断。R44.5现可在
同一event/command边界上实现approval、cancel和verification/evidence control loop。

## 14. R44.5 result

R44.5已交付approval、cancel与verification control loop。approval card只显示server投影后的subject、preview、风险与
exact guard；决策仍必须携带session、run、call、policy digest和expiry guard。cancel必须由当前active run触发并等待
server supervisor确认，不把按钮点击提前显示为已终止，也不声称撤销已经发生的shell或remote side effect。

verification现在由`sigil-kernel`提供共享product reducer，TUI、HTTP和desktop消费同一套recommendation、exact rerun
binding、receipt、snapshot、changeset与failure locator语义；原TUI重复实现已删除。authenticated HTTP新增只读view和
command-envelope rerun端点，rerun复用同一session foreground lease、durable session truth与配置中的execution backend，
因此不会与前台agent run重叠，也不能由renderer构造任意命令。verification command receipt可持久化并幂等replay。

desktop renderer只接收bounded verification projection，并且只能执行server推荐的单项检查。需要人工approval的动作只
显示review指引，不会静默提升为verified。React交互测试覆盖approval、cancel、verification evidence与rerun；真实
production driver测试使用配置中的execution backend完成实际命令重跑，并证明结果重新进入durable receipt/evidence链。

由于desktop exact capability集合增加`verification`，server-info schema升级为2；HTTP protocol仍保持V1，旧schema会在
launcher readiness阶段fail closed。kernel/TUI/HTTP/native/frontend测试、contract drift与Clippy均通过。R44.6现可开始
sidecar packaging、system-WebView dogfood、三平台CI和完成度审计。

## 15. R44.6 result

R44.6已交付source-built desktop dogfood package。repository-owned staging script使用locked release build生成
target-triple-suffixed `sigil-runtime` sidecar；production shell只解析与desktop executable同目录的精确bundled sibling，
debug build才允许开发期`sigil` fallback。CI现在分别生成Linux `.deb`、macOS `.app`与Windows NSIS installer，实际
检查包内sidecar并运行`--version`；artifact只作为7天non-release dogfood产物，不进入公开release channel。

macOS真实package启动审计发现并修复了pre-setup读取managed state导致窗口创建前panic的问题。window和desktop state
现在都在Tauri setup中建立；启动失败只写入本机bounded、control-safe、Unix `0600`的4 KiB单行诊断文件，成功启动会
清理旧记录。重新打包后native process成功创建`Sigil`窗口，bundled runtime可执行，`.app`通过strict ad-hoc
`codesign`校验；没有Apple notarization凭据时仍明确跳过notarization。

frontend interaction suite现覆盖IME composition、paste、single reply、approval/cancel/verification和仅在用户仍停留
底部时自动滚动；timeline使用live-region语义。native、frontend、contract、full workspace、docs/site与supply-chain
gate均通过。最终GitHub Actions证据为CI `29673216377`、Desktop Package `29673216392`和Pages `29673216381`；过程中
发现并修复Windows PTY取消后reader drain覆盖`Cancelled`状态的竞态，以及Linux package listing格式导致的弱校验。

RFC-0044因此以“可从源码构建和dogfood”为边界完成，不宣称已经公开分发。正式installer发布仍要求Apple
Developer ID签名/notarization、Windows Authenticode签名、Linux desktop依赖/advisory复核，以及macOS/Linux/Windows
三平台人工system-WebView回归；V1未启用updater或自动更新channel。
