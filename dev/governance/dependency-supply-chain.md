# Sigil 依赖供应链台账

本文记录新增直接依赖的用途、owner、启用 feature、许可与安全边界。它是代码评审输入，不替代发布前的 `cargo audit` / `cargo deny` 或仓库认可的等价 gate。

## Desktop shell and checked frontend contract（RFC-0044 R44.2）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `tauri` / `tauri-build` | `2.11.5` / `2.6.3`；桌面默认 runtime | `apps/desktop/src-tauri` | 提供system-WebView native shell、local IPC和bundle build；capability仅向`main`window开放Sigil自有command，不启用generic HTTP、shell、filesystem、process、opener或remote URL permission | MIT OR Apache-2.0；tauri-apps/tauri | renderer不能取得bearer、child、loopback address或absolute state/session path；Linux CI安装官方WebKitGTK 4.1 prerequisites |
| `tauri-plugin-dialog` | `2.7.2`；Rust backend API only | `apps/desktop/src-tauri/commands` | 由Rust backend发起native folder picker；选择结果直接进入native manager，不向renderer开放dialog plugin command或filesystem scope | MIT OR Apache-2.0；tauri-apps/plugins-workspace | capability未列出`dialog:*`；transitive fs helper没有renderer permission，不能被frontend调用 |
| React / Vite / TypeScript | `19.2.7` / `8.1.5` / `5.9.3` | `apps/desktop` renderer | 构建stateful desktop view；production CSP禁止remote content、object/frame/base/form，renderer只调用窄Tauri IPC bridge | React MIT；Vite MIT；TypeScript Apache-2.0；各官方项目维护 | npm lockfile提交；Node 24 + pnpm 10.30.3在CI执行typecheck/test/build与high-severity audit |
| `openapi-typescript` | `7.13.0`，dev-only | `apps/desktop/contracts` | 从Rust server实际生成的OpenAPI snapshot机械生成frontend DTO；CI重新生成并byte-compare，避免手写wire contract漂移 | MIT；openapi-ts/openapi-typescript | 只生成type declarations，不生成renderer HTTP client，bearer仍只存在Rust typed client |
| Vitest / Testing Library / jsdom | `4.1.10` / `16.3.2` / `29.1.1`，dev-only | `apps/desktop` tests | 验证coarse workspace action、loading/empty/error和后续daily-loop interaction；真实server contract仍由Rust production-binary tests独立证明 | MIT；各上游项目维护 | test adapter不替代native integration evidence；无runtime bundle依赖 |

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

## Desktop launcher supervisor（RFC-0044 R44.1）

R44.1 没有引入新的第三方版本或来源。新增 `sigil-desktop` library 直接复用 workspace 已锁定的依赖：

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `ring` + `base64` + `zeroize` | 复用 workspace `0.17.14` / `0.22.1` / `1.8.2` | `sigil-desktop/launcher` | 系统 CSPRNG 生成 32-byte per-launch bearer、URL-safe 编码与 drop-time best-effort clear；token 不进入 argv、Debug、error 或 renderer | `ring`: Apache-2.0 AND ISC；其余 MIT OR Apache-2.0；RustCrypto/社区维护 | 不新增 crypto/source；只在 Rust desktop backend 持有 secret |
| `reqwest` | 复用 workspace `0.12.24`；`rustls-tls,json,stream` | `sigil-desktop/launcher` | 对真实 loopback child 做 no-proxy/no-redirect、deadline/response-bounded、bearer-authenticated `/server-info` equality handshake | MIT OR Apache-2.0；seanmonstar/reqwest | 不复用 server crate内部类型，不开放 generic renderer HTTP |
| `tokio` + `serde` + `serde_json` + `thiserror` | 复用 workspace版本/feature | `sigil-desktop/launcher` | bounded pipe/readiness/process wait、独立 DTO strict decode 和 path/token-free typed errors | MIT 或 MIT OR Apache-2.0；Tokio/Serde/社区维护 | 不增加 runtime/serialization 实现 |
| `nix` | 复用 workspace `0.28.0` `signal` feature | `sigil-process` | 将 desktop launcher 的 Unix child 配置为独立process group并在grace deadline后终止完整group | MIT；nix-rust/nix | 把通用process-tree primitive收敛到`sigil-process`；config/bootstrap仍留在desktop owner |

`sigil-desktop` 不依赖 `sigil-kernel`、`sigil-runtime`、`sigil-tui` 或 `sigil-http`。后续 R44.2 引入
Tauri/npm/codegen依赖时必须单独补充版本、feature、license、capability和updater/build-script审计，不能以本节覆盖。

## SQLite desktop session catalog（RFC-0042 R42.1）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `rusqlite` | `0.39.0`；`bundled`（`libsqlite3-sys 0.37.0`，SQLite 3.51.3） | `sigil-runtime/session_lifecycle/projection` | 为 desktop/local HTTP 提供可删除、可从 JSONL/lifecycle journal 重建的历史 session catalog；参数化 SQL、固定 schema、WAL、trusted schema off、bounded busy wait，不进入 run/approval/resume 事实链 | `MIT`；`rusqlite/rusqlite`，bundled SQLite 为 public domain | 直接依赖固定到 0.39.0：最新 0.40.1 的 build script 使用 Rust 1.94.1 尚未稳定的 `cfg_select!`，本仓库预检无法编译；升级前必须重新验证稳定工具链、三平台编译、`cargo deny` 与 `cargo audit` |
| `base64` | 复用 workspace `0.22.1`；默认 feature | `sigil-runtime/session_lifecycle/projection/query` | 编码 runtime-owned、generation/filter-bound 的 opaque keyset cursor；payload不含secret且不作为授权凭据，解码后仍执行schema、byte cap、filter hash与generation验证 | `MIT OR Apache-2.0`；`marshallpierce/rust-base64` | 只新增runtime直接消费，不引入新版本或来源；不能复用kernel单stream apply cursor |
| `url` | 复用 workspace 声明 `2.5.7`（lock `2.5.8`）；默认 feature | `sigil-http/listener` | 严格解析loopback HTTP query的percent encoding与UTF-8，再由adapter拒绝duplicate/unknown/bounded-invalid字段；不发起网络请求 | `MIT OR Apache-2.0`；`servo/rust-url` | 只新增HTTP crate直接消费，不引入第二套URL实现、新版本或来源 |

bundled 模式避免 desktop 分发依赖目标机系统 SQLite ABI，同时会增加二进制体积和 C build surface。数据库只
在 production `sigil serve` / future desktop catalog owner 显式初始化；普通 TUI/CLI startup 不创建它。
SQLite row 不保存 raw message、tool body、URL、secret、absolute source path 或 workspace root，且数据库
故障不能反向阻断 JSONL append、approval 或 run execution。

## Image & Attachment Input V1（RFC-0033）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `image` | `0.25.10`；`default-features = false`；仅 `png,jpeg,webp` | `sigil-runtime/image_attachment` | 对 bounded encoded image 做真实格式识别、尺寸读取与完整 decode；在解码前执行 byte cap、在完整 decode 前执行 dimension/pixel cap，不接受扩展名推断或 SVG/GIF 等 V1 外格式 | `MIT OR Apache-2.0`；`image-rs/image` | 仅 runtime controlled cache ingress/resolution 直接消费；不启用 AVIF、BMP、DDS、EXR、FF、GIF、HDR、ICO、PNM、QOI、TIFF、TGA 等无关 codec |
| `tempfile` | `3.27.0`；默认 feature | `sigil-runtime/image_attachment` | 在目标 cache 目录创建随机临时文件，完成 write+sync 后用 `persist_noclobber` 原子提交内容寻址 blob；拒绝覆盖既有 blob并重新校验已存在内容 | `MIT OR Apache-2.0`；`Stebalien/tempfile` | 从 runtime dev-dependency 提升为直接依赖；临时文件与最终文件同目录，不跨文件系统 rename |
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

- Cargo manifest、lockfile、`deny.toml` 或 workflow 变化时运行，此外每周执行一次；
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
