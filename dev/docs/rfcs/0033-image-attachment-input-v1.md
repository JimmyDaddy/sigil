# RFC-0033 Image & Attachment Input V1

状态：accepted / R33.0-R33.5 complete

创建日期：2026-07-16

基线：

- Depends on: [RFC-0001 Durable Event Stream](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0010 Structured Compaction and Task Memory](0010-structured-compaction-and-task-memory.md)
- Depends on: [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)
- Implementation baseline: `1b493caf7184c07bd240020616a67aadceef3539`

## 1. Summary

本 RFC 为 TUI Build composer 增加 PNG、JPEG、WebP 图片输入。图片可从 system clipboard 或显式粘贴的本地路径加入，composer 显示可选择、可删除的 metadata chip。

原始字节进入 workspace-scoped content-addressed cache；durable session、PrefixSnapshot、export 和 compaction 只保存 hash、MIME、尺寸、长度、估算预算、artifact ref 与固定 placeholder。Provider wire 的 Base64 只在已验证的 process-local request materialization 中产生。

## 2. Goals

1. `Ctrl-V` clipboard image 与 pasted image path 成为 TUI-first 输入流程。
2. composer 显示编号、格式、尺寸、大小和 estimated visual tokens，并支持键盘删除。
3. provider-neutral durable message contract 不泄漏路径或原始字节。
4. exact provider/model capability 在任何 provider I/O 和 physical-attempt Started 之前 fail closed。
5. OpenAI Responses、Anthropic、Gemini 使用各自官方 image block；DeepSeek 与未知 OpenAI-compatible endpoint 明确 Unsupported。
6. compaction 永远使用固定 placeholder，不把 Base64 写入 JSONL 或 compact material。

## 3. Non-goals

- PDF、音频、视频、GIF、SVG、HEIC、远程 image URL；
- OCR、image editing/generation、截图工具或 drag-and-drop；
- queued follow-up、Plan、Task、agent mention、MCP/CLI/HTTP attachment input；
- 自动 cache retention、session delete 引用计数或 cloud artifact store；
- 猜测 OpenAI-compatible endpoint 的 multimodal wire；
- 精确复现每个 provider 的账单，只提供保守 admission estimate。

## 4. Durable message contract

`ModelMessage` 增加 `image_attachments`。每项包含 stable id、SHA-256、MIME、width/height、byte length、estimated visual tokens 和规范 artifact ref；resolved bytes 必须 `serde(skip)` 且 Debug redacted。

只有 user role 可带附件。内容追加稳定 placeholder，使 compaction、session review 和缺失 cache 的错误仍能说明历史中曾有图片，而不声称图片内容仍可读取。

Provider request fingerprint 覆盖 metadata 和 placeholder，不覆盖原始 bytes。Cache resolver 在 freeze 前重新校验 bytes 与 metadata 的 hash、length、format、dimensions。

## 5. Admission limits

V1 hard caps：

- 4 images / user turn；
- 8 MiB / image；
- 24 MiB total / turn；
- 8,192 px / dimension；
- 16,000,000 pixels / image；
- 16,384 estimated visual tokens / turn，按 `ceil(width / 28) * ceil(height / 28)` 计算。

这些是 Sigil 产品边界，不宣称等于 provider billing。任何 cap failure 都在 cache write/provider I/O 前发生。

## 6. Cache and lifecycle

路径为 `workspace_cache_root/attachments/<sha256>.<ext>`。写入必须同目录原子 no-clobber，existing blob 重新校验；读取拒绝 non-canonical ref、symlink、non-file、tamper、truncation、wrong format/dimensions。

session export 继续只导出 durable metadata/ref。V1 不在普通 run、resume 或 session delete 中隐式删除共享 blob；显式 retention 是后续独立能力。

## 7. Request and compaction semantics

普通 chat 与 frozen pre-turn candidate 在 freeze 前从 cache resolve 所有 retained image attachments。缺失/损坏 cache blob 使请求失败并返回可操作错误，不能静默只发 placeholder。

Portable semantic compaction candidate 和 OpenAI native compact request 清除 image blocks，只保留 durable placeholder。Compaction 不生成“仍能访问图片内容”的事实。

## 8. Provider capability and wire

`Provider::image_input_capability(model_name)` 是 exact-model、provider-neutral 查询，默认 Unsupported。

- OpenAI Responses：allowlisted current vision model family；`input_image` data URL。
- Anthropic：Claude 3/4 allowlist；`image` Base64 source。
- Gemini：Gemini 1.5/2/3 allowlist；`inline_data`。
- OpenAI-compatible、DeepSeek：Unsupported。

直接调用 provider request mapper 也必须校验 role、resolved bytes 和 metadata binding。

## 9. TUI interaction

- 空闲 Build composer 中 `Ctrl-V` 读取 clipboard image；bracketed text paste 不改变。
- 粘贴单个现存 image path/`file://` URL直接加入 cache/chip，原路径不进入 prompt/session。
- input 起点按 Up 选择最后一个 chip，Left/Right 切换，Down 返回输入，Backspace/Delete 删除。
- 图片可以无文字提交；提交成功后清空附件。
- 非 V1 surface 保留当前附件并显示原因，不静默丢弃。

## 10. Ownership

- `sigil-kernel`：durable attachment、safe projection、limits、resolver trait、capability admission、placeholder/compaction semantics。
- `sigil-runtime`：controlled cache、path ingestion、resolver implementation、SigilPaths。
- provider crates：exact model capability 与 wire mapping。
- `sigil-tui`：clipboard capture、composer state/render/input、worker wiring。

## 11. Implementation slices

1. R33.0：research、technical solution、RFC、execution ledger。
2. R33.1：kernel durable/cache-resolution contract、limits、capability/compaction tests。
3. R33.2：runtime cache/path admission 与 supply-chain ledger。
4. R33.3：provider image wire 与 unsupported fail-closed。
5. R33.4：TUI clipboard/path/chip/delete/normal chat wiring。
6. R33.5：resume/error/session/compaction canaries、EN/ZH docs/site。
7. R33.6：real binary acceptance、full gate、implementation/code-quality review。

## 12. Acceptance criteria

- PNG/JPEG/WebP file path 与 clipboard PNG 可进入 composer；
- metadata chip、selection、delete、image-only submit 可操作；
- limits、tamper、symlink、missing cache 和 capability mismatch fail closed；
- OpenAI Responses/Anthropic/Gemini loopback body 含正确 image block；DeepSeek/compatible 零网络请求；
- JSONL、PrefixSnapshot、session export、portable/native compaction 不含 Base64/raw bytes/path canary；
- ordinary resume 可从 cache 重装载，compaction placeholder-only；
- full workspace、docs/site、dependency supply-chain 和两轮审查通过。

## 13. Validation

```bash
cargo test -p sigil-kernel image_attachment
cargo test -p sigil-runtime image_attachment
cargo test -p sigil-provider-openai-responses image
cargo test -p sigil-provider-anthropic image
cargo test -p sigil-provider-gemini image
cargo test -p sigil-tui attachment
cargo build --release -p sigil
./scripts/image-attachment-v1-acceptance.py --binary target/release/sigil
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
./scripts/check-pages-site.sh
cargo deny check
git diff --check
```

## 14. Progress

- R33.0 complete：官方 provider 协议、Rust clipboard/image 依赖和 Codex/Aider/Gemini CLI 竞品代码已调研；durable metadata、controlled cache、capability、compaction、TUI、limits 与 commit/gate 边界已冻结。
- R33.1 complete：kernel 已实现 provider-neutral durable attachment、进程内 bytes overlay、safe persistence、resolver/capability admission、请求 material schema V2 与 placeholder-only compaction contract；kernel 全量测试通过。
- R33.2 complete：runtime 已实现 workspace-scoped content-addressed cache、PNG/JPEG/WebP bounded decode、同目录原子 no-clobber 写入、path/file URL admission、no-follow regular-file 读取及 hash/format/dimension 重新校验；依赖台账、audit/deny 与 adversarial tests 通过。
- R33.3 complete：OpenAI Responses、Anthropic 与 Gemini 已实现 exact-model capability 和官方 inline image wire，provider request DTO 的 Debug 已 redacted；OpenAI/Anthropic native compaction 移除 image block，DeepSeek/generic compatible 在 mapper/transport 前 fail closed，五个 provider crate 全量测试与 strict Clippy 通过。
- R33.4 complete：TUI 已支持空闲 Build composer 的单图片路径/file URL 粘贴与 `Ctrl-V` clipboard image，附件以 bounded metadata chip 展示并可用方向键选择、Backspace/Delete 删除；图片-only normal chat 可提交，queue/Plan/slash/agent 路径保留草稿并 fail closed，worker 在 active run 期间拒绝图片排队，TUI 全量测试与 strict Clippy 通过。
- R33.5 complete：普通 resume、session scope 切换与 active-run cancel reload 均恢复 workspace-scoped resolver；durable metadata 可从受控 cache 重新 materialize，而 missing blob 会在 provider transport 前失败。Kernel durable/compaction canary、runtime safe export、TUI 1366/1370 tests（4 ignored）、strict Clippy、EN/ZH docs/site 与 diff gates 通过。
