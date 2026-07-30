# Sigil 依赖供应链台账

本文记录新增直接依赖的用途、owner、启用 feature、许可与安全边界。它是代码评审输入，不替代发布前的 `cargo audit` / `cargo deny` 或仓库认可的等价 gate。

## TUI / CLI 更新器

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `self_update` | `=1.0.0-rc.6`；关闭默认 feature，仅启用 `async,github,rustls,archive-tar,compression-tar-gz,checksums` | `sigil-updater/apply` | 只负责已准入 GitHub standalone archive 的 exact tag/asset 下载、SHA-256 校验、archive 解包、候选 binary `--version` 验证与 unattended replacement；不负责选择 release、解析 GitHub 安全字段或 package-manager 更新 | MIT；jaemk/self_update | 固定 RC 精确版本，避免预发布 API 漂移；只有 `immutable=true`、精确 target asset 和 GitHub `sha256:` digest 同时成立才进入该引擎。npm/Homebrew/Cargo/source/unknown 均不得原地替换 |
| `semver` | `1.0.27`；默认 feature | `sigil-updater/channel` | 解析当前版本与 release tag，并按 stable、beta 或当前已安装 prerelease channel 做严格隔离和版本排序 | MIT OR Apache-2.0；dtolnay/semver | `beta` 只接收 stable 与首个 prerelease identifier 为 `beta` 的版本；`current` 对 prerelease 只跟随相同 identifier，拒绝 alpha/beta/rc 串线 |

`sigil-updater` 自行使用 workspace `reqwest 0.12` 拉取 GitHub Releases，并显式请求
`X-GitHub-Api-Version: 2026-03-10`；client 只允许 HTTPS、禁止 redirect、设置连接/总超时并将
response 限制在 2 MiB。GitHub asset digest 只提供内容完整性，不等于发布者真实性，因此 apply
还要求 immutable release、精确 tag/target/asset、编译期 `github-release` 分发 marker，以及下载后
binary 的版本和 target 自报一致。release archive 构建脚本写入该 marker；npm launcher 只写入
`SIGIL_INSTALL_SOURCE=npm`，防止把包管理器安装误识别成 standalone。进程环境 marker 只能把
编译期 standalone 降权为 npm/Homebrew/Cargo/source ownership；`github-release` /
`standalone` 环境值不能把编译期 source/unknown 提升成可替换安装。

`self_update 1.0.0-rc.6` 当前传递引入 `reqwest 0.13`，因此 lockfile 暂时同时包含 0.12 与
0.13。0.12 只服务 Sigil 自有的 bounded discovery client；0.13 只在用户明确 apply 且已通过
Sigil release admission 后由 replacement engine 使用。升级 `self_update` 时必须优先复核能否
收敛该重复版本，并重新执行三平台 archive、digest 与 downloaded-binary 验证测试。

检查结果写入全局 cache 下的 owner-only、256 KiB 上限、同目录原子替换文件；24 小时内可复用，
过期后使用 ETag revalidate。自动检查只允许 release profile 的 standalone/npm/Homebrew 分发，
并在 CI、source build 或 `SIGIL_NO_UPDATE_CHECK` 下关闭；自动检查从不执行 apply。显式
`sigil update apply --yes` 和 TUI `/update apply` 也只能替换 standalone，其他来源只返回原安装器命令。

## Windows canonical path interoperability

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `dunce` | `1.0.5`；默认 feature | `sigil-runtime/isolated_workspace` | 只在已由操作系统 canonicalize 的路径上移除 Windows verbatim `\\?\` 前缀，使 Git for Windows 可消费 worktree destination，并让 confinement 比较使用同一表示；不解析用户输入、不放宽 symlink 或 workspace 边界 | MIT OR Apache-2.0；khuey/dunce | workspace 依赖图已包含同一锁定版本，本次仅由 runtime 直接复用；所有路径仍先经过 `symlink_metadata` 与 `canonicalize`，简化结果不得替代安全 canonicalization |

## Desktop shell and checked frontend contract（RFC-0044 R44.2）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `tauri` / `tauri-build` | `2.11.5` / `2.6.3`；桌面默认 runtime | `apps/desktop/src-tauri` | 提供system-WebView native shell、local IPC和bundle build；capability仅向`main`window开放Sigil自有command，不启用generic HTTP、shell、filesystem、process、opener或remote URL permission | MIT OR Apache-2.0；tauri-apps/tauri | renderer不能取得bearer、child、loopback address或absolute state/session path；Linux CI安装官方WebKitGTK 4.1 prerequisites |
| `tauri-plugin-dialog` | `2.7.2`；Rust backend API only | `apps/desktop/src-tauri/commands` | 由Rust backend发起native folder picker；选择结果直接进入native manager，不向renderer开放dialog plugin command或filesystem scope | MIT OR Apache-2.0；tauri-apps/plugins-workspace | capability未列出`dialog:*`；transitive fs helper没有renderer permission，不能被frontend调用 |
| `tauri-plugin-opener` | `2.5.4`；Rust backend API only，JS link interception disabled | `apps/desktop/src-tauri/commands` | 仅由Sigil自有`desktop_open_external_url`命令打开native校验后的credential-free `https:` URL；不开放path、program selector、generic scheme或plugin command | MIT OR Apache-2.0；tauri-apps/plugins-workspace | capability未列出`opener:*`；默认JS link injection明确关闭，renderer只能调用2 KiB上限的typed HTTPS route，失败回退copy |
| React / Vite / TypeScript | `19.2.7` / `8.1.5` / `5.9.3` | `apps/desktop` renderer | 构建stateful desktop view；production CSP禁止remote content、object/frame/base/form，renderer只调用窄Tauri IPC bridge | React MIT；Vite MIT；TypeScript Apache-2.0；各官方项目维护 | npm lockfile提交；Node 24 + pnpm 10.30.3在CI执行typecheck/test/build与high-severity audit |
| `react-markdown` / `remark-gfm` / `rehype-highlight` | `10.1.0` / `4.0.1` / `7.0.2`；ESM runtime | `apps/desktop/SafeMarkdown` | CommonMark+GFM AST渲染与随bundle的37种常用language grammar；不开启raw-HTML插件、remote grammar、auto-detect、remote CSS/font/script或image element | MIT；remarkjs/react-markdown、remarkjs/remark-gfm、rehypejs/rehype-highlight | 第三方API只存在于Sigil-owned renderer；URL二次限制为credential-free HTTPS，其他scheme降级为文本；lockfile、high-severity audit与bundle差异是R48.2 gate |
| `openapi-typescript` | `7.13.0`，dev-only | `apps/desktop/contracts` | 从Rust server实际生成的OpenAPI snapshot机械生成frontend DTO；CI重新生成并byte-compare，避免手写wire contract漂移 | MIT；openapi-ts/openapi-typescript | 只生成type declarations，不生成renderer HTTP client，bearer仍只存在Rust typed client |
| Vitest / Testing Library / jsdom | `4.1.10` / `16.3.2` / `29.1.1`，dev-only | `apps/desktop` tests | 验证coarse workspace action、loading/empty/error和后续daily-loop interaction；真实server contract仍由Rust production-binary tests独立证明 | MIT；各上游项目维护 | test adapter不替代native integration evidence；无runtime bundle依赖 |

### Conversation Markdown、数学公式与图表（RFC-0054）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `remark-math` / `rehype-katex` / `katex` | `6.0.0` / `7.0.1` / `0.18.1` | `apps/desktop/src/markdown` | 从 Markdown AST 识别 inline/display math，并由 KaTeX 生成本地 HTML/MathML；不开启 `trust`，不允许任意 URL、HTML 或远程字体 | MIT；remarkjs/remark-math、remarkjs/remark-math、KaTeX/KaTeX | 数学渲染独立 lazy chunk；失败局部降级为原始 LaTeX，TUI 不新增数学引擎而显示保真 source |
| `rehype-sanitize` | `6.0.0` | `apps/desktop/src/markdown` | 在 React 渲染前执行显式 Markdown schema，只允许 Sigil 使用的结构、class 与受限属性；不启用 `rehype-raw` | MIT；rehypejs/rehype-sanitize | 模型、历史 session 和 provider 内容均视为不可信；raw HTML、script、iframe、object 与不受控 URL 不进入 DOM |
| `mermaid` | `11.16.0` | `apps/desktop/src/markdown` | 只对闭合、限额且通过 admission 的 `mermaid` fence 按需渲染；`securityLevel=strict`、`htmlLabels=false`，不加载 CDN、remote theme、image 或 plugin | MIT；mermaid-js/mermaid | 不进入主 renderer chunk；交互 directive、外链、HTML 标签、control character 与过大输入先拒绝，失败保留源码；TUI 永不执行 Mermaid |
| `dompurify` | `3.4.12` | `apps/desktop/src/markdown` | 对 Mermaid 生成的 SVG 做第二道净化，删除 script、foreignObject、image、event handler、remote href 与逃逸 CSS，并验证预期 root id | MPL-2.0 OR Apache-2.0；cure53/DOMPurify | 只净化本地 Mermaid 结果，不接受通用 HTML；SVG 不能触发网络、导航或事件回调 |

RFC-0054 没有为 TUI 引入第三方 Markdown、数学或图表 runtime，也不启动额外进程、不创建临时文件、
不访问网络。Desktop CSP 保持 `default-src 'self'`、`img-src 'self' data:`、`object-src 'none'`、
`frame-src 'none'`，所有 CSS、font、KaTeX 和 Mermaid 资源随应用 bundle 分发。

依赖引入前后的 production build 对比：主 JS 从 `770.44 kB / 229.53 kB gzip` 增至
`818.91 kB / 247.35 kB gzip`；KaTeX 与 Mermaid 保持异步 chunk，其中 KaTeX core 为
`259.63 kB / 77.62 kB gzip`，Mermaid core 为 `36.27 kB / 12.07 kB gzip`，各图种实现继续由
Vite 拆分。`pnpm audit --audit-level high` 当前为零漏洞；`openapi-typescript` 传递图中的
`js-yaml` 通过 workspace override 固定为 `4.3.0`；其
`@redocly/openapi-core -> minimatch` 路径中的 `brace-expansion` 也固定为 `5.0.8`，以修复
`GHSA-mh99-v99m-4gvg` 的无界 expansion-length DoS，直到上游约束自然覆盖对应修复版本。两个
override 都由 contract regeneration、完整 desktop check 和 high-severity audit 验证。升级
KaTeX、Mermaid、DOMPurify 或 sanitize pipeline 时必须重跑不可信 URL/HTML/SVG 语料、安全审计和
bundle 差异检查，不得只以视觉 smoke 通过作为升级依据。

R44.2同时复用workspace既有`reqwest`、`serde`、`tokio`、`url`、`uuid`与`thiserror`。Rust typed client使用
launcher私有bearer、no-proxy/no-redirect client、bounded JSON response和opaque command IDs；它不依赖`sigil-http`
实现crate。`pnpm audit --audit-level high`在引入时无已知漏洞。

Tauri当前Linux WebKitGTK graph新增`MPL-2.0`和`Apache-2.0 WITH LLVM-exception`许可。前者只施加文件级
copyleft，Sigil不修改或再许可这些上游文件；后者是Apache-2.0的标准LLVM例外。两者均为OSI许可并已加入显式
allowlist，不代表放宽未知许可或git source policy。

RustSec复扫同时识别出Tauri 2.11.5当前上游图中的以下无安全升级路径项：

- Linux GTK3绑定的unmaintained组`RUSTSEC-2024-0411`至`RUSTSEC-2024-0420`，以及该路径的
  `proc-macro-error` `RUSTSEC-2024-0370`；这些crate只由Tauri/WebKitGTK Linux runtime/build graph引入，Sigil
  不直接调用GTK API。
- `glib 0.18.5`的`VariantStrIter` unsoundness `RUSTSEC-2024-0429`；Sigil与desktop adapter不构造或遍历
  `glib::VariantStrIter`。这是当前Tauri Linux传递依赖的受限风险接受，不是漏洞已修复的声明。
- `tauri-utils -> urlpattern`的unmaintained `rust-unic`组：`RUSTSEC-2025-0075`、`RUSTSEC-2025-0080`、
  `RUSTSEC-2025-0081`、`RUSTSEC-2025-0098`、`RUSTSEC-2025-0100`；该路径用于Tauri构建/URL pattern contract，
  不处理Sigil provider或tool网络数据。

这些ID与既有`RUSTSEC-2024-0436`、`RUSTSEC-2025-0141`一起在`deny.toml`和独立`cargo audit` job中精确列出。
升级Tauri、`tauri-utils`、`wry`或Linux WebKitGTK stack时必须先删除可消失的例外再复扫；不得把例外扩展到
Sigil直接依赖或其他调用面。R44.6 Linux package/runtime audit仍是发布desktop artifact前的阻塞门禁。

R44.3没有引入新版本或来源。`apps/desktop/src-tauri`新增直接复用workspace的`serde_json`与`tempfile 3.27.0`：
前者只编码bounded native recent-workspace file，后者在同一app-config目录写入、sync并原子persist临时文件。该文件
可以丢失/重建，不含token、session body或provider credential；其中的absolute workspace path永不序列化到renderer，
reopen仍由launcher/server重新验证。`tempfile`的`rustix`许可已由R44.2加入的
`Apache-2.0 WITH LLVM-exception`显式allowlist覆盖。

## Desktop launcher supervisor（RFC-0044 R44.1）

R44.1 没有引入新的第三方版本或来源。新增 `sigil-desktop` library 直接复用 workspace 已锁定的依赖：

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `ring` + `base64` + `zeroize` | 复用 workspace `0.17.14` / `0.22.1` / `1.8.2` | `sigil-desktop/launcher` | 系统 CSPRNG 生成 32-byte per-launch bearer、URL-safe 编码与 drop-time best-effort clear；token 不进入 argv、Debug、error 或 renderer | `ring`: Apache-2.0 AND ISC；其余 MIT OR Apache-2.0；RustCrypto/社区维护 | 不新增 crypto/source；只在 Rust desktop backend 持有 secret |
| `reqwest` | 复用 workspace `0.12.24`；`rustls-tls,json,stream` | `sigil-desktop/launcher` | 对真实 loopback child 做 no-proxy/no-redirect、deadline/response-bounded、bearer-authenticated `/server-info` equality handshake | MIT OR Apache-2.0；seanmonstar/reqwest | 不复用 server crate内部类型，不开放 generic renderer HTTP |
| `tokio` + `serde` + `serde_json` + `thiserror` | 复用 workspace版本/feature | `sigil-desktop/launcher` | bounded pipe/readiness/process wait、独立 DTO strict decode 和 path/token-free typed errors | MIT 或 MIT OR Apache-2.0；Tokio/Serde/社区维护 | 不增加 runtime/serialization 实现 |
| `nix` | 复用 workspace `0.28.0` `signal` feature | `sigil-process` | 将 desktop launcher 的 Unix child 配置为独立process group并在grace deadline后终止完整group | MIT；nix-rust/nix | 把通用process-tree primitive收敛到`sigil-process`；config/bootstrap仍留在desktop owner |

`sigil-desktop` 不依赖 `sigil-kernel`、`sigil-runtime`、`sigil-tui` 或 `sigil-http`。
`apps/desktop` 已引入的 Tauri、npm、codegen 与 updater/build-script 依赖由本台账对应章节单独审计；
后续变更仍必须同步补充版本、feature、license、capability 和供应链边界，不能以 launcher 本节覆盖。

## SQLite desktop session catalog（RFC-0042 R42.1）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `rusqlite` | `0.39.0`；`bundled`（`libsqlite3-sys 0.37.0`，SQLite 3.51.3） | `sigil-runtime/session_lifecycle/projection` | 为 desktop/local HTTP 提供可删除、可从 JSONL/lifecycle journal 重建的历史 session catalog；参数化 SQL、固定 schema、WAL、trusted schema off、bounded busy wait，不进入 run/approval/resume 事实链 | `MIT`；`rusqlite/rusqlite`，bundled SQLite 为 public domain | 直接依赖固定到 0.39.0：最新 0.40.1 的 build script 使用 Rust 1.94.1 尚未稳定的 `cfg_select!`，本仓库预检无法编译；升级前必须重新验证稳定工具链、三平台编译、`cargo deny` 与 `cargo audit` |
| `base64` | 复用 workspace `0.22.1`；默认 feature | `sigil-runtime/session_lifecycle/projection/query` | 编码 runtime-owned、generation/filter-bound 的 opaque keyset cursor；payload不含secret且不作为授权凭据，解码后仍执行schema、byte cap、filter hash与generation验证 | `MIT OR Apache-2.0`；`marshallpierce/rust-base64` | 只新增runtime直接消费，不引入新版本或来源；不能复用kernel单stream apply cursor |
| `url` | 复用 workspace 声明 `2.5.7`（lock `2.5.8`）；默认 feature | `sigil-http/listener` | 严格解析loopback HTTP query的percent encoding与UTF-8，再由adapter拒绝duplicate/unknown/bounded-invalid字段；不发起网络请求 | `MIT OR Apache-2.0`；`servo/rust-url` | 只新增HTTP crate直接消费，不引入第二套URL实现、新版本或来源 |

bundled 模式避免 desktop 分发依赖目标机系统 SQLite ABI，同时会增加二进制体积和 C build surface。数据库只
在 production `sigil serve` / Desktop catalog owner 显式初始化；普通 TUI/CLI startup 不创建它。
SQLite row 不保存 raw message、tool body、URL、secret、absolute source path 或 workspace root，且数据库
故障不能反向阻断 JSONL append、approval 或 run execution。

## Image & Attachment Input V1（RFC-0033）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `image` | `0.25.10`；`default-features = false`；仅 `png,jpeg,webp` | `sigil-runtime/image_attachment` | 对 bounded encoded image 做真实格式识别、尺寸读取与完整 decode；在解码前执行 byte cap、在完整 decode 前执行 dimension/pixel cap，不接受扩展名推断或 SVG/GIF 等 V1 外格式 | `MIT OR Apache-2.0`；`image-rs/image` | 仅 runtime controlled cache ingress/resolution 直接消费；不启用 AVIF、BMP、DDS、EXR、FF、GIF、HDR、ICO、PNM、QOI、TIFF、TGA 等无关 codec |
| `tempfile` | `3.27.0`；默认 feature | `sigil-runtime/image_attachment`、`sigil-kernel/config` | 在目标 cache 目录创建随机临时文件，完成 write+sync 后用 `persist_noclobber` 原子提交内容寻址 blob；用户配置在独占 sidecar lease 内以同目录 staged file 完成 sync + atomic replace | `MIT OR Apache-2.0`；`Stebalien/tempfile` | runtime 与 kernel 直接复用同一 workspace 版本；临时文件与最终文件同目录，不跨文件系统 rename，配置替换保留既有权限与显式 symlink target |
| `base64` | `0.22.1`；默认 feature | `sigil-provider-openai-responses, sigil-provider-anthropic, sigil-provider-gemini` | 只在 provider request material 已通过 metadata/bytes binding 与 exact-model capability admission 后，将受限 encoded image bytes 映射为三种官方 inline image wire；编码结果不进入 durable state、Debug 或 error | `MIT OR Apache-2.0`；`marshallpierce/rust-base64` | workspace 原有传递版本提升为显式直接依赖；DeepSeek 与 generic compatible 不依赖、不编码并在 transport 前拒绝 image input |
| `arboard` | `3.6.1`；`default-features = false`；仅 `image-data` | `sigil-tui/clipboard_image` | 只在空闲 Build composer 收到 `Ctrl-V` 时读取系统剪贴板 RGBA image；无图像时回退普通按键流，读取到图像后先做 dimension/pixel/RGBA binding 检查并编码 PNG，再进入统一 controlled cache admission | `MIT OR Apache-2.0`；`1Password/arboard` | 不持有全局 clipboard handle，不读取文本或 HTML，不把原始 RGBA、路径或 clipboard error 持久化；平台 image backend 是实现 Ctrl-V 图片输入所需的最小 feature |

R33.2 同时复用 workspace 已有的 `sha2`、`url` 与 `libc`。文件入口和 cache leaf 都以 no-follow regular-file 方式读取，大小、hash、格式、dimensions 与 canonical artifact ref 任一不一致即 fail closed；原始粘贴路径不会进入 session/export/provider request。

## WebFetch 受控传输（E21.9 / E21.17 public cutover）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `async-compression` | `0.4.42`；`default-features = false`；仅 `tokio,gzip,brotli,zstd,deflate` | `sigil-tools-builtin/webfetch` | 对 HTTP content-encoding 做显式 bounded streaming decode；关闭 reqwest 自动解压，decoded writer 先执行 hard cap，防止 compression bomb 无界展开 | `MIT OR Apache-2.0`；`Nullus157/async-compression` | 只由 WebFetch 直接消费；未启用无关 runtime/codec feature |
| `encoding_rs` | `0.8.35`；默认 feature | `sigil-tools-builtin/webfetch` | bounded body 完成后按 BOM / bounded charset label 严格解码；malformed 输入 fail closed，不做 lossy 隐式替换 | `(Apache-2.0 OR MIT) AND BSD-3-Clause`；`hsivonen/encoding_rs` | 仅处理 text/plain、text/html、application/xhtml+xml |

E21.9 同时复用 workspace 已有的 `reqwest`、`url`、`futures`、`thiserror` 与 `tokio`，没有为 HTTP client、URL parser或错误模型新增第二套实现。`reqwest` client 显式使用 rustls、redirect none、retry never、referer false、no proxy-by-default（仅消费 runtime authorized proxy plan）和四种 auto-decompression off。

## Streamable HTTP 内部协议核心（E21.14）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `hmac` | `0.12.1`；默认 feature | `sigil-mcp/streamable_http` | 用进程随机 key 对 live header value 生成不可持久化 HMAC-SHA256 binding；避免把 credential 的 raw hash、明文或可离线字典反推 verifier 放入 fingerprint | `MIT OR Apache-2.0`；RustCrypto/MACs | key 与 resolved secret 仅存在 live carrier；静态 pin 仍只覆盖 source metadata |

E21.14 复用 workspace 已有的 `reqwest`、`url`、`futures`、`regex`、`sha2`、`tokio` 与 `serde_json`。`regex` 使用 Rust 线性时间引擎校验已通过长度上限的 form pattern；remote client 禁用 redirect、retry、cookie、Referer与自动解压，并且只能消费 runtime 从 E21.9 shared destination guard 产出的 authorized dial plan。

## Stable MCP Search 内部适配层（E21.15）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `unicode-normalization` | `0.1.25`；默认 feature | `sigil-runtime/web_search_connector` | 在query的secret/PII扫描、字符/byte cap和durable disclosure之前执行NFC正规化，避免等价Unicode序列绕过exact wire与审计绑定 | `MIT OR Apache-2.0`；`unicode-rs/unicode-normalization` | 只处理bounded query文本；不做locale相关改写，不读取环境或外部数据 |

E21.15 其余实现复用workspace已有的`sigil-mcp` Streamable HTTP core、`url`、`sha2`、`serde_json`与`tokio`。E21.17 public cutover 后，bundled profile 仍使用固定 HTTPS endpoint、空 header 配置和空 client capabilities，且不读取 `EXA_API_KEY`；只有 stable `websearch` wrapper 可触发该惰性 profile，不注册 bundled raw MCP tools。

## Anthropic hosted continuation（E21.12）

E21.12 没有引入新的 workspace 第三方包。`sigil-provider-anthropic` 新增直接复用 workspace 已锁定的 `uuid`，仅生成 process-local continuation handle；handle 不携带query、URL、title、`encrypted_content`或`encrypted_index`，重启后不可恢复并按`InterruptOnRestart`安全降级。HTTP、SSE、序列化和secret carrier继续复用既有`reqwest`、`serde_json`、`sigil-kernel`契约，没有增加第二套client或加密实现。

## Context Compaction V2 encrypted continuation payload（K25.12B2）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `keyring` | `3.6.3`；`default-features = false`；`apple-native,windows-native,sync-secret-service,vendored` | `sigil-kernel/session provider_continuation_payload` | 为每个 session 保存随机 256-bit master key；production backend 只能访问系统 credential store，缺失/不可读 key 直接 fail closed，不能创建替代 key 读取已有密文，也没有 plaintext fallback | `MIT OR Apache-2.0`；`hwchen/keyring-rs` | Linux 仍通过 Secret Service 使用运行时 D-Bus 环境，但编译时 vendored `libdbus`，避免 CI、sandbox conformance 与 release 构建依赖宿主机预装 `libdbus-1-dev`；Linux-native CI 必须继续编译并运行对应恢复测试 |
| `ring` | `0.17.14`；默认 feature | `sigil-kernel/session provider_continuation_payload` | 仅使用 `AES_256_GCM` 与系统随机 nonce 加密 artifact/handle bytes；AAD 精确绑定 session scope 和 immutable committed manifest，密文/manifest/key 任一漂移均拒绝读取 | `Apache-2.0 AND ISC`；`briansmith/ring` | 不将 key、nonce、明文或 provider payload 写入 JSONL；发布前仍需把新增依赖纳入同一 workspace 的 `cargo audit` / `cargo deny` 复扫 |

K25.12B2 的 coordinator 强制 `stage ciphertext -> append+sync Committed -> atomic finalize`，且 `Invalidated/OrphanDiscovered -> Deleted` 只在物理删除已完成后追加。低层密文 store 与 key-store trait 不作为跨 crate API 暴露，避免 provider 直接绕过 append-only lifecycle；session key destruction/export rewrap 仍留给后续通用 session delete/export slice。

## MCP OAuth credential lifecycle（R40.3）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `keyring` | 复用 `3.6.3` 与既有 native feature 集 | `sigil-mcp/streamable_http/oauth_credential` | 保存绑定 server/resource/issuer/client/scopes 的完整 versioned OAuth record；load/store/delete 在 `spawn_blocking` 中访问 native credential store，任何 unavailable/rejected/oversize 都 fail closed，没有 config/session/file fallback | `MIT OR Apache-2.0`；`hwchen/keyring-rs` | record cap 固定为 Windows Credential Manager 最窄的 2560 bytes；超限明确拒绝，不在其他平台形成不可跨平台验证的宽松路径。真实 Windows/Linux/macOS keyring 仍由 R40.5 hosted gate 验证 |
| `zeroize` | 复用 `1.8.2`；默认 feature | `sigil-mcp/streamable_http/oauth,oauth_credential` | 包裹 serialized credential bytes、decoded secret fields、Basic client-auth 中间材料；公开 carrier 继续使用 kernel `SecretString`，无 serde contract 且 drop 清零 | `Apache-2.0 OR MIT`；RustCrypto | 只提供 best-effort memory clearing，不把它表述为内存取证防护；Debug/error/status 只输出 bounded 非 secret 投影 |

R40.3 没有引入新版本或来源；该切片完成时的两项显式例外已在后续供应链变更中继续按同一精确台账机制演进。

## Provider connection credential lifecycle（RFC-0056 R56.2）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `keyring` | 复用 `3.6.3` 与既有 native feature 集 | `sigil-runtime/provider_connections/keyring_store` | 保存绑定 credential ID、provider family、auth kind 和 rotation generation 的 versioned API-key record；所有 native 调用进入 `spawn_blocking` 并由 process-global mutex 串行，不对可能等待系统认证的调用施加无法取消底层 prompt 的短 timeout。只有显式 `storage.credential_store = "keyring"` 使用交互式 native store；新配置默认 `file`，`auto` 也只写 owner-only file backend | `MIT OR Apache-2.0`；`hwchen/keyring-rs` | 继续使用跨平台最窄的 2560-byte record cap；native macOS/Windows/Linux round trip 属于 R56.7 hosted conformance |
| `fs2` | 复用 `0.4.3` | `sigil-runtime/provider_connections/file_store` | 对 `~/.sigil/credentials.json` 的独立 sidecar lock 取得 shared/exclusive advisory lease，配合 bounded/versioned JSON、same-parent atomic publish 和 owner-only permissions，避免并发 reader/writer 丢失更新 | `MIT OR Apache-2.0`；`danburkert/fs2-rs` | file backend 是受权限保护的 plaintext credential store，不宣称加密；不得进入主 config、workspace、session、cache、log、snapshot 或 support data |
| `ring` | 复用 `0.17.14`；默认 feature | `sigil-runtime/provider_connections/credential,persistence` | 为每次 COW read-back 生成 process-local random HMAC key，只比较同进程 secret equality；key、tag 和 secret 都不持久化或进入 Debug/error | `Apache-2.0 AND ISC`；`briansmith/ring` | 该 fingerprint 不是 credential identity、cache key 或离线 verifier；比较完成即丢弃 |
| `zeroize` | 复用 `1.8.2`；默认 feature | `sigil-runtime/provider_connections/{keyring_store,file_store}` | 包裹 credential serialized record bytes、file read/write buffers 与 encoded record strings；公开 secret carrier 继续复用 kernel `SecretString` | `Apache-2.0 OR MIT`；RustCrypto | 仅提供 best-effort drop-time clearing；DTO、Doctor、session 和 config 不暴露 raw secret 或 credential ID |
| `libc` | 复用 workspace `0.2.186` | `sigil-kernel/config` | Unix 同目录 temporary config 使用 `O_NOFOLLOW`，并结合 create-new、parent identity 和 rename/fsync contract 拒绝 symlink/replacement 路径 | `MIT OR Apache-2.0`；rust-lang/libc | 只在 Unix cfg 下消费；Windows 继续使用 inherited ACL、`ReplaceFileW` / `MoveFileExW` contract |

R56.2 没有引入新的版本或来源。provider credential store 与 MCP OAuth store 使用不同 service、
record schema 和 public trait，避免把 API key、OAuth token 与 continuation key 的 scope/lifecycle 合并。

## Context Compaction V2 portable tokenizer（K25.10/K25.13）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `tokenizers` | `0.23.1`；`default-features = false`；仅 `onig` | `sigil-provider-deepseek/compaction_token_profile` | 仅加载 checksum-pinned、显式安装的 DeepSeek V4 Flash tokenizer，用于本地 exact token proof；正常 preview/apply 不下载模型文件 | `Apache-2.0`；`huggingface/tokenizers` | `cargo update -p tokenizers --dry-run` 未发现兼容更新；其 transitive `macro_rules_attribute -> paste 1.0.15` 命中 `RUSTSEC-2024-0436`（仅 unmaintained）。`paste` 仅参与构建期宏展开，不处理运行时用户/网络输入；在 `deny.toml` 以显式例外放行，必须在 tokenizers 或 macro_rules_attribute 移除该路径后删除并复扫 |

该 tokenizer 依赖的例外不是“已修复漏洞”的声明，而是发布前可见、可复核的临时风险接受：项目不得把 `paste` 用于运行时代码，也不得在未重新执行 `cargo deny check advisories` 和更新本台账的情况下扩大 tokenizers feature 或用途。

## HTTP durable journal filesystem primitives（RFC-0026 P26.4B）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `fs2` | `0.4.3`；默认 feature | `sigil-http/durable_io` | 对 protocol/disclosure journal 的 sidecar lock file 取得 OS advisory exclusive lease，拒绝同一路径双 writer；不用于跨网络协调，也不把 lock file 当 durable evidence | `MIT OR Apache-2.0`；`danburkert/fs2-rs` | `sigil-http` 新增直接消费；journal owner drop 后释放 lease，append 的 durability 仍由原子替换与 sync 单独证明 |
| `windows-sys` | `0.61.2`；仅 Windows target；按 owner 启用 `Win32_Storage_FileSystem,Win32_Foundation,Win32_Globalization,Win32_Security,Win32_Security_Authorization,Win32_Security_Isolation,Win32_System_JobObjects,Win32_System_Pipes,Win32_System_Threading` | `sigil-http/durable_io`、`sigil-process`、`sigil-tools-builtin/execution_backends` | HTTP durable journal 使用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`；`sigil-process` 统一 Job Object lifecycle ownership；tools 的 RFC-0041 私有探针使用原生 restricted-token、ACL、exact inherited-handle 与 AppContainer security-capabilities API，避免 runtime helper 或 shell wrapper | `MIT OR Apache-2.0`；Microsoft/windows-rs | 仅在 `cfg(windows)` 编译且未引入新版本；AppContainer/restricting-SID 仍是 hosted containment gate 后的私有探针，不构成公开 filesystem/network sandbox 声明；Unix 保持既有 rename/fsync 与 process-group 实现 |

P26.4B 复用 kernel 的 `MAX_EVENT_BYTES` 与 SafePersist 文本投影，不为 HTTP journal 引入另一套 secret scanner 或 event-size 常量。journal 的 exclusive lease 只解决单机同路径 writer ownership；它不替代 append-only session evidence、command identity store 或跨进程服务选主。

## 发布前扫描与显式例外（E21.17）

2026-07-12 使用 `cargo-audit 0.22.2` 与 `cargo-deny 0.20.2` 对启用 all-features 的 workspace 依赖图执行扫描。首次扫描发现 `crossbeam-epoch 0.9.18`、`quinn-proto 0.11.14` 与经 `syntect` 默认 plist feature 引入的 `quick-xml 0.39.4` 存在已公开漏洞。处置如下：

- 将兼容依赖更新至 `crossbeam-epoch 0.9.20`、`quinn-proto 0.11.16`；
- 将 `syntect 5.3.0` 改为关闭默认 feature，仅启用 `parsing,default-syntaxes,default-themes,regex-onig`，并将 `two-face 0.5.1` 对齐到 `syntect-onig`；这移除了不被 Sigil 使用的 plist/`quick-xml` 与 `yaml-rust` 依赖路径；
- `deny.toml` 限制依赖来源为 crates.io registry，执行许可白名单检查，并将重复版本保留为 warning 供后续收敛。

复扫结果为 `cargo audit` 零已知漏洞；`cargo deny check` 的 advisories/bans/licenses/sources 四项均通过。当时建立的两项例外为`RUSTSEC-2025-0141`（`syntect`只用`bincode 1.3.3`读取版本固定、编译进二进制的dump）与上文记录的`RUSTSEC-2024-0436`。当前完整例外集合还包括R44.2在本文件开头逐项说明的Tauri传递路径；唯一事实源以`deny.toml`为准，所有例外都必须随上游迁移复核并删除。

上述证据覆盖 E21.17 public WebFetch、stable websearch 与 user-root Streamable HTTP MCP cutover；最终发布结论仍以同一工作区的完整测试、Clippy、格式、文档和站点 gate 全绿为前提。

## 常规自动化门禁（RFC-0037）

`.github/workflows/dependency-supply-chain.yml` 将上述发布前扫描提升为常规仓库门禁：

- Cargo manifest、lockfile、`deny.toml`、desktop npm manifest/lockfile 或 workflow 变化时运行，此外每周执行一次；
- push/PR 先从 exact base/head diff 分类 Rust 与 npm 输入：Rust policy/deny/audit 只在 Rust
  供应链输入变化时运行，desktop `pnpm audit` 只在 npm manifest/lockfile 变化时运行；定时和手动
  扫描仍同时覆盖两张依赖图，workflow/Dependabot 配置变化也 fail-safe 覆盖两者；
- `cargo-deny 0.20.2` 的官方 action release 按已提交的 `deny.toml` 检查 advisories、bans、licenses 和 sources；
- `cargo-audit 0.22.2` 独立复扫 `Cargo.lock`，只携带`deny.toml`与本台账已说明的精确例外；
- 两个 job 都是阻塞门禁，不使用 `continue-on-error`，且 workflow 权限仅为
  `contents: read`。
- 扫描前先运行 `scripts/check-supply-chain-policy.py` 及其单测，确保 `deny.toml`、workflow
  和本台账中的 advisory 例外一致；安全扫描器从 crates.io 按锁定版本安装，不信任跨 run
  缓存的可执行文件。

新增、删除或修改 advisory 例外时，必须原子更新 `deny.toml`、本台账和 workflow 的
`cargo audit --ignore` 参数，并在本地重新执行：

```bash
cargo deny check
cargo audit \
  --ignore RUSTSEC-2024-0370 \
  --ignore RUSTSEC-2024-0411 \
  --ignore RUSTSEC-2024-0412 \
  --ignore RUSTSEC-2024-0413 \
  --ignore RUSTSEC-2024-0414 \
  --ignore RUSTSEC-2024-0415 \
  --ignore RUSTSEC-2024-0416 \
  --ignore RUSTSEC-2024-0417 \
  --ignore RUSTSEC-2024-0418 \
  --ignore RUSTSEC-2024-0419 \
  --ignore RUSTSEC-2024-0420 \
  --ignore RUSTSEC-2024-0429 \
  --ignore RUSTSEC-2024-0436 \
  --ignore RUSTSEC-2025-0075 \
  --ignore RUSTSEC-2025-0080 \
  --ignore RUSTSEC-2025-0081 \
  --ignore RUSTSEC-2025-0098 \
  --ignore RUSTSEC-2025-0100 \
  --ignore RUSTSEC-2025-0141
```

workflow 定时运行只能证明默认分支的最新依赖状态；发布仍需按对应 release RFC 执行完整
workspace、文档、站点和分发 gate。

`.github/dependabot.yml` 每周检查 Cargo 与 GitHub Actions 版本。Cargo minor/patch 合并为
一个更新组，major 保持独立 PR；Actions 更新合并为一个组。两个 ecosystem 的开放 PR
上限分别为 3 和 2，不自动合并，仍必须通过普通 CI 与上述供应链门禁。
