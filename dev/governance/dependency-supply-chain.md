# Sigil 依赖供应链台账

本文记录新增直接依赖的用途、owner、启用 feature、许可与安全边界。它是代码评审输入，不替代发布前的 `cargo audit` / `cargo deny` 或仓库认可的等价 gate。

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

## Context Compaction V2 portable tokenizer（K25.10/K25.13）

| 依赖 | 锁定版本 / feature | Owner | 用途与安全理由 | 许可 / 维护来源 | 当前结论 |
|---|---|---|---|---|---|
| `tokenizers` | `0.23.1`；`default-features = false`；仅 `onig` | `sigil-provider-deepseek/compaction_token_profile` | 仅加载 checksum-pinned、显式安装的 DeepSeek V4 Flash tokenizer，用于本地 exact token proof；正常 preview/apply 不下载模型文件 | `Apache-2.0`；`huggingface/tokenizers` | `cargo update -p tokenizers --dry-run` 未发现兼容更新；其 transitive `macro_rules_attribute -> paste 1.0.15` 命中 `RUSTSEC-2024-0436`（仅 unmaintained）。`paste` 仅参与构建期宏展开，不处理运行时用户/网络输入；在 `deny.toml` 以显式例外放行，必须在 tokenizers 或 macro_rules_attribute 移除该路径后删除并复扫 |

该 tokenizer 依赖的例外不是“已修复漏洞”的声明，而是发布前可见、可复核的临时风险接受：项目不得把 `paste` 用于运行时代码，也不得在未重新执行 `cargo deny check advisories` 和更新本台账的情况下扩大 tokenizers feature 或用途。

## 发布前扫描与显式例外（E21.17）

2026-07-12 使用 `cargo-audit 0.22.2` 与 `cargo-deny 0.20.2` 对启用 all-features 的 workspace 依赖图执行扫描。首次扫描发现 `crossbeam-epoch 0.9.18`、`quinn-proto 0.11.14` 与经 `syntect` 默认 plist feature 引入的 `quick-xml 0.39.4` 存在已公开漏洞。处置如下：

- 将兼容依赖更新至 `crossbeam-epoch 0.9.20`、`quinn-proto 0.11.16`；
- 将 `syntect 5.3.0` 改为关闭默认 feature，仅启用 `parsing,default-syntaxes,default-themes,regex-onig`，并将 `two-face 0.5.1` 对齐到 `syntect-onig`；这移除了不被 Sigil 使用的 plist/`quick-xml` 与 `yaml-rust` 依赖路径；
- `deny.toml` 限制依赖来源为 crates.io registry，执行许可白名单检查，并将重复版本保留为 warning 供后续收敛。

复扫结果为 `cargo audit` 零已知漏洞；`cargo deny check` 的 advisories、bans、licenses、sources 四项均通过。当前显式 advisory 例外有两项：`RUSTSEC-2025-0141`（`syntect` 只用 `bincode 1.3.3` 反序列化版本固定、编译进二进制的 syntax/theme dump）以及上文 K25.10/K25.13 记录的 `RUSTSEC-2024-0436`（tokenizers 的构建期 `paste` 路径）。两项都必须随上游迁移复核并删除，不得把例外误写成漏洞已经消失。

上述证据覆盖 E21.17 public WebFetch、stable websearch 与 user-root Streamable HTTP MCP cutover；最终发布结论仍以同一工作区的完整测试、Clippy、格式、文档和站点 gate 全绿为前提。
