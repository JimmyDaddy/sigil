# Sigil 依赖供应链台账

本文记录新增直接依赖的用途、owner、启用 feature、许可与安全边界。它是代码评审输入，不替代发布前的 `cargo audit` / `cargo deny` 或仓库认可的等价 gate。

## WebFetch 内部传输（E21.9）

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

E21.15 其余实现复用workspace已有的`sigil-mcp` Streamable HTTP core、`url`、`sha2`、`serde_json`与`tokio`。runtime-private bundled profile使用固定HTTPS endpoint、空header配置和空client capabilities；不读取`EXA_API_KEY`，且E21.17前没有RootConfig、默认route或用户文档入口。

## Anthropic hosted continuation（E21.12）

E21.12 没有引入新的 workspace 第三方包。`sigil-provider-anthropic` 新增直接复用 workspace 已锁定的 `uuid`，仅生成 process-local continuation handle；handle 不携带query、URL、title、`encrypted_content`或`encrypted_index`，重启后不可恢复并按`InterruptOnRestart`安全降级。HTTP、SSE、序列化和secret carrier继续复用既有`reqwest`、`serde_json`、`sigil-kernel`契约，没有增加第二套client或加密实现。

本台账只证明当前内部/test-only实现的版本、feature、许可和安全用途已经人工核对。E21.17 原子公开前仍必须运行并保存依赖维护性、许可和漏洞扫描证据；任一 supply-chain gate 未完成都不得开放 public WebFetch/remote MCP 配置或宣称 Web V1 ready。
