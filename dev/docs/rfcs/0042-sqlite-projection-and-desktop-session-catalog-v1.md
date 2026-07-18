# RFC-0042 SQLite Projection and Desktop Session Catalog V1

状态：accepted / R42.0-R42.5 implemented / complete

创建日期：2026-07-19

基线：

- Projection contract: [RFC-0008](0008-thread-projection-and-agent-graph-observability.md)
- Local session lifecycle: [RFC-0027](0027-local-session-lifecycle-v1.md)
- Desktop/app server: [RFC-0016](0016-desktop-app-server-productization.md)
- Stable local HTTP protocol: [RFC-0026](0026-stable-machine-protocol-and-real-serve.md)

## 1. Summary

桌面端需要在进程重启后仍可分页、筛选、排序和搜索历史 session。当前 `GET /sessions` 只列出
`sigil serve` 进程内的 adapter session；`LocalSessionLifecycleService::catalog` 每次查询都要重新扫描并解析
workspace 的 JSONL，无法作为桌面历史首页的稳定查询面。这个具体产品查询同时满足 RFC-0008 E08.5 与
RFC-0016 E16.7 的升级条件。

本 RFC 增加全局、单机、可删除的 SQLite session catalog。V1 由 workspace-bound reconciler 把已验证的
V2 JSONL 和 lifecycle pin 状态投影为 compact row；桌面/app-server 通过独立的
`GET /session-catalog` 查询历史列表。JSONL 与 append-only lifecycle journal 继续是唯一事实源；SQLite
不可用于 active run、approval、tool progress、resume 正确性、delete/pin 授权或任何 forward effect。

## 2. Research basis

- SQLite [WAL documentation](https://sqlite.org/wal.html) 允许同机 reader 与 writer 并发，但同一时刻仍只有
  一个 writer，并要求所有参与进程位于同一主机；因此 catalog 只放在 user-local state root，不支持网络
  文件系统。
- SQLite [PRAGMA documentation](https://www.sqlite.org/pragma.html) 提供 app-owned `user_version`，并建议
  application 在连接上关闭 trusted schema。V1 同时设置固定 `application_id`、`user_version = 1`、
  `trusted_schema = OFF`、`foreign_keys = ON`、bounded `busy_timeout`、WAL 与 `synchronous = FULL`。
- [`rusqlite`](https://github.com/rusqlite/rusqlite) 提供同步、参数化的 SQLite binding；V1 固定使用
  `0.39.0` 的 `bundled` feature，使 macOS/Linux/Windows desktop 分发不依赖目标机预装的 SQLite ABI。
  `0.40.1` 的 `libsqlite3-sys 0.38.1` 使用当前 Rust 1.94.1 尚未稳定的 `cfg_select!`，预检已明确拒绝；所有
  SQLite 调用都在 runtime/HTTP 的 blocking boundary 内执行。
- Goose 的 session list 使用 `(sort_at, session_id)` keyset cursor，并把 cursor 绑定到 filter hash；
  OpenAI Codex 保留 rollout JSONL，同时用 SQLite state/backfill 服务跨 thread 查询。Sigil采用相同的
  keyset/backfill经验，但不采用 Goose 的“SQLite 是 session truth store”语义。

## 3. Product query contract and trigger

首个 materialized-view consumer 是 desktop/local HTTP historical session catalog：

| Dimension | V1 contract |
| --- | --- |
| Family | `SessionList` |
| Scope | current workspace filter over a cross-workspace database |
| Surfaces | HTTP and future desktop; TUI may adopt later |
| Pagination | keyset, default 50, hard maximum 100 |
| Filtering | workspace, provider, pinned, durable source state |
| Sorting | `modified_at_unix_ms DESC, session_id DESC, session_ref DESC` |
| Search | case-insensitive bounded title search only |
| Fresh live state | forbidden; read `/sessions` and SSE separately |

这不是“预留桌面端”：一次 desktop history query 不应重新读取最多 4096 个、总计 512 MiB 的 session
stream；server 与未来 TUI/desktop 也不应各自重复 replay 同一批 JSONL。显式分页、筛选、搜索、排序、跨
进程重启与多 surface reuse 构成真实 query pressure，正式打开 E08.5/E16.7。

## 4. Goals

1. 从 V2 JSONL + lifecycle journal 可确定性重建 workspace session rows；删除数据库后重建得到等价查询
   结果。
2. 用 source fingerprint 跳过未变化的 stream，只解析新增或变化的 direct JSONL child；一次 workspace
   reconcile 的 row update、source cursor 与 stale-row removal 在同一事务内完成。
3. 提供 bounded keyset pagination、exact filter-bound cursor、provider/pin/state filter 与 title search。
4. 让 `sigil serve` 暴露鉴权的 `/session-catalog`，同时保留 `/sessions` 的 process-local live 语义。
5. schema mismatch、corruption、busy、partial scan 与 invalid stream 都返回 typed/degraded 结果，不静默把
   stale projection 当成事实。

## 5. Non-goals

- 不把 SQLite 变成 session、message、control、approval、tool execution 或 lifecycle truth source。
- 不把 raw prompt、assistant/tool body、tool arguments、URL、secret、absolute source path 或 workspace root
  写入 catalog。
- 不通过 projection 决定 delete、retention、pin、resume 或当前 run 是否 active。
- V1 不提供 message full-text search、FTS extension、cost analytics、cross-device sync、cloud database、
  network filesystem 或 multi-user server。
- V1 不改 TUI session browser，也不新增普通用户配置项或顶层命令。

## 6. Ownership

- `sigil-kernel` 继续拥有 provider-neutral session-list reducer、单 stream projection apply cursor identity 和
  query-pressure contract；不依赖 SQLite，也不出现 SQL 类型。
- `sigil-runtime::session_lifecycle::projection` 拥有 SQLite schema、reconcile、query DTO、独立的
  `SessionCatalogPageCursorV1` codec 与 recovery。HTTP keyset cursor 不复用 kernel 的单 stream
  `ProjectionCursor`。它复用 lifecycle 的 direct-child、symlink、size 和 validation budget。
- `sigil-http` 只解析 HTTP query、执行 blocking runtime call 并序列化稳定 DTO；不写 SQL、不 replay JSONL。
- `sigil` 在 `serve` startup 注入 workspace-bound catalog service。普通 TUI/CLI 启动不创建数据库。

数据库固定在 `<state_root>/projections/session-catalog-v1.sqlite3`，row 主键是
`(workspace_id, session_ref)`。一个 workspace reconciler 只能更新或删除自己 `workspace_id` 下的 row；
多个本机 `serve` 进程通过 SQLite writer serialization 协调，不能越界清理其他 workspace。

## 7. Durable and transactional contract

### 7.1 Truth and failure direction

JSONL append 成功不能因 SQLite unavailable 而回滚或失败。projection update/reconcile 失败只让历史查询
返回 unavailable/degraded；run、approval、session append 与 lifecycle operation 继续依据其各自 durable
contract。反过来，SQLite row 存在也不能证明源 session 可 resume 或可删除。

### 7.2 Source identity

每个 row 记录：

- `workspace_id`、relative `session_ref`、durable `session_id`；
- source bytes、modified milliseconds、last stream sequence/event id/record checksum；
- source state（ready/oversized/scan-budget-exceeded/unsupported-legacy/invalid）；
- provider、model、bounded title、message/control counts、latest usage/task/readiness；
- lifecycle-derived pinned bit；
- projection schema version and indexed timestamp。

source bytes/mtime 只用于普通本地文件变更的快速检测；成功重读后必须同时保存完整 source SHA-256 与 last
event/checksum，显式 rebuild 无条件重读并重新计算所有 bounded source。稳定的 invalid/legacy source 也按同一
metadata fingerprint复用，避免每次HTTP查询无意义递增generation并使cursor失效。V1 不把metadata fast path
宣称为同用户恶意篡改检测器：能够同时改写source bytes、mtime和SQLite的本地同用户攻击者不在catalog threat
model内。若scan不完整、目录不可验证或transaction失败，不执行stale-row deletion。

### 7.3 Reconcile transaction

一次 workspace reconcile 按以下顺序执行：

1. 验证 session directory 不是 symlink，读取并确定性排序 direct `.jsonl` child；排除 lifecycle journal；
2. 应用与 lifecycle catalog 相同的 entry/stream/total-byte limit；
3. 记录本 workspace 起始 generation，从 SQLite 读取现有 fingerprint，只为变化 source 读取完整 V2 records；
4. 校验单一 session identity、连续 stream/cursor 和读取前后 metadata 未漂移；
5. `BEGIN IMMEDIATE` 后重新比较 workspace generation；若已变化，回滚扫描结果并执行 bounded retry，禁止较旧
   scan 覆盖较新提交；
6. generation CAS 成功后 upsert changed rows，刷新 pin，只有完整 scan 时删除已不存在的本 workspace row；
   只有 materialized rows/pin/stale set 实际变化时才递增 generation；
7. commit 后返回 scanned/reused/updated/removed/degraded/retried counts。

单个 invalid/unsupported/oversized source作为 bounded state row 保留，但不携带上次 ready metadata冒充新鲜
结果。读取过程中发生 drift 时本次 transaction 中止，调用方可重试。

## 8. Schema and migration

V1 使用 SQLite `application_id` 和 `user_version = 1`。schema 初始化在 `BEGIN IMMEDIATE` 内完成；新建库只
创建固定 DDL 与 indexes，不执行来自数据库内容的 SQL。连接使用 parameterized statement，关闭 trusted
schema，并限制 busy wait。

正式版前仍不需要 legacy application migration，但必须处理三种状态：

- empty/new database：只有`application_id=0`、`user_version=0`且`sqlite_schema`没有任何user object时才创建V1；
- exact V1：正常打开；
- wrong application id、未知 user version 或损坏：typed incompatible/corrupt，不静默删除。

在写入persistent PRAGMA前必须完成database ownership/schema检查；新database由应用以no-follow、`0600`
预创建，projection parent收紧为`0700`，WAL/SHM继承并复核owner-only权限。

recovery API 必须先取得独立于 SQLite connection 的 OS-backed recovery lease，并确认本进程 connection 已
quiescent；其他 Sigil process 持有 lease 或活动 connection 时 fail closed。随后才把全局数据库及
`-wal`/`-shm` sidecar 移到同目录的 timestamped quarantine，再创建空 V1 并从当前workspace JSONL rebuild。
该显式操作会使其他workspace的cached rows失效，report在旧库可读时给出`invalidated_workspace_count`；其事实
源不变，并在各workspace下一次reconcile时恢复。自动HTTP query不执行quarantine/delete；恢复不能形成
old/new database split-brain，也不能伪装成只影响当前workspace的局部修复。

## 9. Query and cursor contract

query normalized 后生成 SHA-256 filter fingerprint，覆盖 workspace/provider/pinned/state/search/sort/schema。
runtime-owned opaque cursor 是 base64url JSON，至少包含 schema version、catalog generation、filter
fingerprint 与上一页末尾的 `modified_at_unix_ms/session_id/session_ref`。每页在同一 SQLite read transaction
内读取 workspace metadata 与 rows；cursor generation 不匹配返回 `stale_cursor`，filter 不匹配、版本未知、
字段越界或解码失败返回 `invalid_cursor`，都不退回第一页。客户端遇到 stale cursor 必须从第一页刷新。

title search trim 后最大 160 bytes，按 Unicode lowercase 的安全投影列做 escaped `LIKE`；`%`、`_` 与
escape char 不得改变查询语义。V1 不声称语言学排序或 tokenized full-text search。所有返回值都有 fixed
limit，response 包含 `generation`、`reconciled_at_unix_ms`、`degraded_source_count` 和 `next_cursor`。

## 10. HTTP boundary

新增：

```text
GET /session-catalog?limit=50&cursor=...&q=...&provider=...&pinned=true&state=ready
```

- route 与 `/openapi.json` 一样要求 bearer auth；
- production server 注入 catalog service；未注入的 library/test server 返回 503；
- malformed/duplicate/unknown query field、无效 bool/state/limit/cursor 返回 400；
- incompatible/corrupt/busy/reconcile failure 返回 503 和 bounded error，不返回 raw absolute path；
- HTTP使用独立白名单DTO，只暴露OpenAPI冻结的desktop list字段；source hash、stream cursor/checksum和内部
  task/readiness投影不进入wire；
- `/sessions` 继续只返回本进程 adapter sessions；active state、approval 和 progress 继续读取 registry/SSE。

## 11. Security and privacy

- database 与 parent directory 使用 user-local state root；Unix 新文件收紧到 mode `0600`，Windows 继承
  user profile ACL。V1 拒绝 symlink database path/parent。
- 不把 raw message/tool content写入 SQLite；title 复用 safe persistence projection并限制 160 bytes。
- HTTP error、DTO 和 cursor 不泄漏 database/source absolute path；cursor不作为 authorization token。
- query value、workspace id 和 cursor只能作为 bound parameters，不能拼入 SQL。
- WAL 只用于同机 local state；不支持 NFS/SMB/iCloud 等同步目录上的共享写入。

## 12. Implementation slices

1. **R42.0 contract and preflight**：冻结触发证据、schema、query/cursor、失败方向、依赖与执行账本。
2. **R42.1 SQLite store and rebuild**：新增 bundled rusqlite、path、V1 DDL、连接安全设置、全量 rebuild、
   delete-and-rebuild equivalence、mismatch/corruption tests。
3. **R42.2 Incremental reconcile and query**：source fingerprint、transactional stale cleanup、pin refresh、
   keyset cursor、filter/search/page tests。
4. **R42.3 HTTP desktop surface**：production injection、严格 query parser、`/session-catalog`、OpenAPI、auth/
   unavailable/invalid cursor tests，保留 live-state split。
5. **R42.4 recovery and observability**：bounded diagnostics、exclusive recovery lease、explicit quarantine/
   rebuild seam、busy/concurrent writer、partial scan、metadata drift tests。
6. **R42.5 completion audit**：dependency ledger、RFC-0008/0016/0026 progress、docs/site必要同步、cross-platform
   compile、full gate、security/code-quality/implementation completeness review。

## 13. Acceptance matrix

- 删除 SQLite 后从同一 JSONL/lifecycle inputs重建，normalized query response（除 generation/time）等价。
- unchanged reconcile 不重新 parse stream；append/change/delete/pin 能在下一次 reconcile 后准确反映。
- duplicate session id in不同 source不会覆盖 row；主键仍以 workspace/session ref为准并明确报告 identity冲突。
- gap、checksum corruption、mixed session id、symlink、oversize、scan budget、source drift 都 fail closed或形成
  明确 degraded row，不留下伪 fresh metadata。
- stable generation 内 keyset pagination 无重复/遗漏；generation变化时旧 cursor明确失效，cursor不能在不同
  filter/search/workspace 下复用。
- 同机两个 connection并发 read/reconcile 有 bounded行为；database busy不无限等待。
- 未鉴权 HTTP request 无法查询 catalog；HTTP response不含 raw prompt/tool output/absolute source path。
- `/sessions`、SSE、approval与active run行为不依赖 SQLite availability。
- macOS、Linux、Windows均使用 bundled SQLite compile；数据库只在 production `serve` 初始化。

## 14. Validation plan

```bash
cargo test -p sigil-runtime session_projection
cargo test -p sigil-http session_catalog
cargo test -p sigil --bin sigil serve
./scripts/check-touched.sh --tier standard
./scripts/check-docs.sh
./scripts/check-pages-site.sh
cargo deny check
cargo audit --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0436
git diff --check
```

R42.5 是跨核心 crate 的 durable/query语义变更，最终还需 `./scripts/check-touched.sh --tier full`。测试通过
只证明 projection 的 rebuild/query契约；不把它扩大为当前 run、approval或多用户远程 server的保证。

## 15. R42.0 result

当前 inventory确认：kernel已有 provider-neutral session-list reducer与严格 projection cursor；runtime已有
bounded direct-child lifecycle scan；`sigil serve` 已有 bearer auth、durable protocol replay与process-local
session registry。缺口是跨进程重启的历史 catalog、稳定分页/search/filter/sort、projection recovery，以及
将这些能力作为独立 HTTP read model注入 production server。

因此采用 runtime-owned bundled SQLite adapter，不新增 crate、不修改 kernel事件 wire、不替换 JSONL，也不
让普通 TUI startup承担数据库成本。R42.1从一次性完整 rebuild与delete/rebuild proof开始，增量与HTTP只在
该 durable contract成立后接入。

## 16. R42.1 result

- `SigilPaths` 新增 global `projections_root/session_catalog_db`，数据库位于 state root 而不是某个 workspace
  内；row replacement仍严格限制为当前 `workspace_id`。
- runtime新增 bundled rusqlite V1 store，固定 application id/user version、STRICT schema、WAL、FULL sync、
  trusted schema off、foreign keys、2秒 busy timeout和Unix `0600` database file。
- full rebuild复用 lifecycle direct-child、symlink、entry/stream/total-byte limits，从 kernel session-list
  reducer取得 compact metadata；raw message/tool body、absolute path和legacy/invalid content不写入数据库。
- 每个 ready row绑定 source SHA-256、last stream sequence/event/checksum；invalid、legacy、oversized和scan
  budget source只写 degraded metadata，不沿用旧 ready字段。
- workspace replacement在单一 `BEGIN IMMEDIATE` transaction中更新metadata、删除本 workspace旧row并插入
  rebuild结果；其他 workspace row保持不变。unknown schema/application id/source state均fail closed。
- 测试覆盖删除数据库后等价重建、pin投影、跨workspace隔离、legacy/invalid隐私、schema mismatch、未知
  state和symlink database拒绝。R42.1尚未接入 production `serve`，普通TUI/CLI不会创建数据库。

## 17. R42.2 result

- `reconcile()`先读取 workspace generation与既有source fingerprint，只为state/bytes/mtime变化的direct child
  replay JSONL；metadata-stable row直接复用，但仍从append-only lifecycle journal刷新exact session pin。
- scan完成后使用`BEGIN IMMEDIATE`重新比较generation；旧scan遇到其他writer的新commit会rollback并最多重试
  两次，不能覆盖更新结果。append/change/delete/pin、degraded/truncated/conflict metadata与generation在同一
  transaction发布；无material变化时只刷新reconciled time，generation保持稳定。
- runtime新增独立`SessionCatalogPageCursorV1`：base64url payload绑定workspace/filter hash、query schema、
  catalog generation和末行keyset。filter变化返回invalid cursor，generation变化返回stale cursor，不回退
  第一页。
- query默认50、最大100，按modified/session id/session ref稳定降序；支持exact provider/pin/state过滤与
  bounded Unicode-lowercase title literal search，`%`、`_`和escape char不获得wildcard语义。
- 测试覆盖unchanged reuse、append/delete/pin增量更新、generation CAS、三页无重无漏、filter-bound cursor、
  stale cursor、literal search、provider/pin/state filter和malformed/unbounded query拒绝。

## 18. R42.3 result

- production `sigil serve` 只在完成既有 durable HTTP journal/driver 装配后注入 workspace-bound catalog；启动
  warm reconcile失败只降级历史查询并给出bounded warning，不影响run、approval或session append。
- 新增鉴权的`GET /session-catalog`，同时保留`GET /sessions`的process-local live handle语义。library/test
  server未注入projection时在鉴权后返回503，SQLite错误不会泄漏database/source绝对路径。
- HTTP query parser显式拆分path/query，拒绝fragment、错误percent encoding、重复/未知field、非法UTF-8、
  bool/state/limit；runtime在filesystem reconcile前再次执行bound/filter/cursor验证。
- projection generation变化时旧cursor稳定映射为409 `stale_cursor`；invalid query/cursor分别映射为400，
  incompatible/corrupt/busy/reconcile失败映射为503，客户端可以确定何时重启分页。
- OpenAPI补齐分页、search/provider/pin/state参数、catalog DTO与400/401/409/503响应；HTTP集成测试覆盖auth、
  unavailable、真实durable JSONL查询、严格parser、stale cursor及绝对路径不泄漏。

## 19. R42.4 result

- 所有Sigil catalog connection在打开SQLite前先持有独立recovery lock file的OS shared lease；显式
  `quarantine_global_catalog_and_rebuild_workspace()`必须取得exclusive non-blocking lease。另一个Sigil进程/
  连接仍在使用catalog时
  稳定返回`RecoveryBusy`，不会rename活动数据库或形成old/new split。
- owner recovery在exclusive lease存续期间验证并移动全局database/`-wal`/`-shm`到同parent、0700的唯一
  quarantine目录，再创建空V1并从当前workspace JSONL/lifecycle truth完整rebuild；旧文件不删除，失败的新
  database也尽力隔离。API名称明确global invalidation，report返回可证明的旧workspace count、quarantine
  basename和bounded counts，不输出source/root绝对路径；其他workspace在下一次reconcile恢复cache。
- database、parent、recovery lease和SQLite sidecar的existing-path检查改为`symlink_metadata`/typed error，
  broken symlink不会先被follow并在外部创建target；lock/database文件保持0600。
- 修正partial scan语义：entry limit触发truncation时，普通incremental reconcile保留未扫描的既有row并发布
  `truncated_source_count`，不把不可证明的absence当成delete；后续complete scan才执行stale-row removal。
  显式full rebuild仍从bounded truth重建，不复用可能损坏的旧row。
- 测试覆盖quarantine后等价重建、active connection阻止recovery、broken lock symlink、partial/complete stale
  cleanup、source metadata drift和SQLite concurrent writer在2秒busy timeout内返回。

## 20. R42.5 result

- 完整度与代码规范独立审计首先发现：稳定degraded source导致generation抖动、workspace-wide rewrite、全局
  recovery语义不透明、`0/0`无关数据库被接管、broken symlink误删、safe text/160-byte约束缺失、SQLite初始
  权限窗口与HTTP storage DTO泄漏。修复提交`f67918bf`逐项关闭；复核未发现剩余P0/P1/P2。
- reconcile现在只upsert changed rows并删除已证明消失的row；稳定invalid/legacy可复用。新增adapter测试覆盖
  duplicate identity、gap/checksum/mixed identity、oversize/budget、corrupt SQLite、source/broken-directory
  symlink、cross-workspace recovery、concurrent read/reconcile、degraded multi-page cursor与HTTP invalid cursor。
- database ownership在persistent WAL PRAGMA前验证；新文件no-follow/0600、projection parent 0700，sidecar权限
  复核。safe-projected title/task objective遵守160-byte UTF-8上限，HTTP只返回OpenAPI白名单字段。
- RFC-0008 E08.5、RFC-0016 E16.7、RFC-0026 follow-up、EN/ZH reference/changelog与dependency ledger已同步；
  site首页没有app-server/API信息架构，未强塞developer endpoint内容，但Pages site gate仍完整通过。
- macOS full touched gate（fmt/check/workspace tests/Clippy）、docs mirror/link/command/content gate、Pages site、
  `cargo deny`与既有两项显式ignore下的`cargo audit`通过；Windows GNU bundled SQLite交叉编译通过。Linux
  cross-check在进入项目代码前因宿主缺少`x86_64-linux-gnu-gcc`而停止，因此Linux平台证据仍由native CI承担，
  不把本机尝试误报为通过。
- 非阻塞后续仅保留更深的test hardening：为reconcile注入端到端source-drift seam，以及把schema integrity
  从固定object name/type扩展为完整DDL/column fingerprint。当前实现会在publish前双重metadata检查，并在
  schema/query不匹配时fail closed；这两项不阻塞V1 desktop historical catalog contract。
