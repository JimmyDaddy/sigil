# RFC-0038 Alpha Long-session Performance V1

状态：complete / R38.0-R38.5

创建日期：2026-07-17

基线：

- Session writer: [`session/writer.rs`](../../../crates/sigil-kernel/src/session/writer.rs)
- Timeline render store: [`timeline_render_store.rs`](../../../crates/sigil-tui/src/app/timeline_render_store.rs)
- Compaction: [`portable_compaction.rs`](../../../crates/sigil-kernel/src/session/portable_compaction.rs)
- Predecessor: [RFC-0037 Cross-platform CI Reliability V1](0037-cross-platform-ci-reliability-v1.md)

## 1. Summary

Sigil 已有线性 session writer、durable replay、portable compaction 和 block-owned timeline
render store，但当前常规 gate 主要证明正确性，没有保存 1k/10k event 与长 transcript 的可重复
规模证据。timeline 顺序 append 与尾部 rerender 还会全量修复 separator 并重建 prefix/hash，
其工作量随历史增长；这会把长会话 streaming 的成本绑定到整个 transcript。

本 RFC 先建立不依赖网络/provider 的长会话 evidence harness，再只优化被证据确认的 timeline
hot path。确定性结构事实进入普通测试；wall-clock 只进入 release-profile 周期/手工 artifact，V1
不把 hosted runner 时间设为合入阈值。

## 2. Goals

1. 为 10k durable event append/replay 保存 sequence、scan count、record count 与文件大小证据。
2. 为 portable compaction 在长历史上的 fold-plan 正确性保存规模证据。
3. 为 5k timeline entries 的 rebuild、顺序 append 与尾部 rerender 保存 release-profile 证据。
4. 让常见 timeline 顺序 append 与尾部 rerender 不再全量重建历史 index/hash。
5. 增加 weekly/manual evidence workflow，上传机器可读 JSON，不把测量噪声伪装成 correctness gate。
6. 保持 TUI 输出、scrollback、hit-test、session schema 和 compaction contract 不变。

## 3. Non-goals

- 不引入 SQLite/materialized projection、vector DB、persistent semantic index 或 virtualization。
- 不修改 session/control/event schema、durability、fsync、compaction admission 或 provider contract。
- 不新增用户命令、配置、面板、公开 benchmark API、crate 或第三方 benchmark dependency。
- 不为 V1 设置绝对毫秒 release gate；wall-clock 只用于趋势和前后对比。
- 不解锁 Windows restricted backend、physical worktree、remote MCP OAuth 或其他 gated 能力。
- 不发布版本、tag 或 release artifact。

## 4. Evidence contract

每个 scenario 输出一条 `SIGIL_LONG_SESSION_EVIDENCE` JSON record：

- `schema_version = 1`；
- 稳定 `scenario`；
- `scale` 和 `elapsed_ms`；
- scenario-specific `facts`，只包含 count、bytes、scan/rebuild shape 等非敏感值。

Harness 必须拒绝重复/缺失 scenario、错误 schema、负数或非整数 measurement。Rust evidence tests
继续断言 sequence、record count、fold boundary、render invariants 与 full-rebuild equivalence；只有
`elapsed_ms` 不参与 pass/fail。

## 5. Scale scenarios

### 5.1 Session append and replay

- append 10,000 个 normal durable events；
- sequence 必须严格为 `1..=10_000`；
- writer full scan count 必须为 1；
- replay 必须恢复 10,000 条并保持 tail identity；
- append 与 replay 分别记录 elapsed evidence。

### 5.2 Portable compaction planning

- 构造至少 1,000 个 completed message turns；
- fold plan 必须覆盖非空连续历史并生成稳定 boundary；
- 原始 JSONL 不删除、不重写；
- 记录 source record、folded event 与 materialization elapsed evidence。

### 5.3 Timeline render/update

- 从 5,000 个 mixed visible entries 建立 render store；
- 顺序 append 后必须与 fresh full rebuild 完全一致；
- 同一尾部 streaming entry 连续 rerender，最终输出、range、hash 与 full rebuild 一致；
- evidence 分别记录 initial rebuild、append batch 与 tail rerender。

## 6. Timeline optimization boundary

允许的优化限定在 `TimelineRenderStore` 内：

- 顺序 visible append 只更新前一个 visible block 的 separator、新 block、尾部 prefix counts 与
  cumulative hashes；
- hidden append 只追加零长度 range；
- 尾部 rerender 只从受影响的最后 visible boundary 重建 suffix；
- 非顺序 append、global render key 变化和非尾部复杂 rerender 继续 fail-safe 到 full rebuild；
- 每个 fast path 都必须与 fresh full rebuild 对比，不能用关闭 invariant check 换性能。

不在 store 外缓存 range，也不恢复 legacy flat mirror。

## 7. Implementation slices

1. R38.0: RFC、baseline inventory、evidence/schema 与非目标边界。
2. R38.1: Rust evidence scenarios、Python collector/schema tests 与本地 baseline report。
3. R38.2: timeline 顺序 append suffix index fast path 与等价性/规模测试。
4. R38.3: timeline 尾部 rerender suffix fast path 与 mixed visible/hidden 回归。
5. R38.4: weekly/manual release-profile evidence workflow 与 artifact contract。
6. R38.5: hosted/local evidence 校准、完整度复核、STATUS 与 validation ledger。

## 8. Acceptance criteria

- 10k session scenario 证明一次 full scan、严格 sequence 与完整 replay。
- 1k-turn compaction scenario 证明 fold plan 不删除/重写 raw history。
- 5k-entry timeline scenario 输出 schema-valid evidence。
- 顺序 append 与尾部 rerender 的 fast path 不调用全历史 index rebuild。
- visible、hidden、separator、width/options invalidation 与 arbitrary rerender 仍匹配 full rebuild。
- 普通 CI 运行确定性 contract tests；weekly/manual workflow 运行 release-profile evidence 并上传 JSON。
- 没有绝对 wall-clock 合入阈值、`continue-on-error`、用户面或 durable contract 变化。
- RFC/status 明确区分本地 evidence、hosted evidence 与未解锁 gated 能力。

## 9. Validation

```bash
python3 scripts/test-long-session-evidence.py
python3 scripts/long-session-evidence.py --output target/long-session-evidence.json
cargo test -p sigil-kernel session_writer_long_session_evidence -- --ignored
cargo test -p sigil-kernel portable_compaction_long_session_evidence -- --ignored
cargo test -p sigil-tui timeline_render_store_long_session_evidence -- --ignored
cargo fmt --all --check
cargo clippy -p sigil-kernel -p sigil-tui --all-targets -- -D warnings
./scripts/check-docs.sh
git diff --check
```

Release-profile evidence 只在显式 harness/workflow 中运行，不进入每次普通 `cargo test`。

## 10. Progress

- R38.0 complete. Baseline confirms the linear writer has a 64-event scan-count test but no
  10k report; compaction has deep correctness coverage but no scale artifact; timeline append and
  rerender currently repair separators and rebuild all prefix/hash indexes. Evidence schema,
  scenario scale, fast-path boundary and non-goals are frozen above.
- R38.1 complete. Three ignored release-profile scenarios now emit schema-versioned evidence for
  10,000 durable records, 1,000 completed turns and 5,000 timeline entries. The collector rejects
  missing or duplicate scenarios, invalid schema and non-integer or negative measurements. The
  pre-optimization local baseline recorded one writer full scan, 10,000 replayed records, 2,000
  compaction source records with 1,999 folded events and unchanged raw bytes, plus a 2,225 ms
  timeline scenario whose 2,013 ms append and 200 ms tail-rerender phases dominated the 12 ms
  initial rebuild.
- R38.2 complete. Sequential visible append now repairs only the prior visible separator, appends
  suffix lines/ranges and extends prefix/hash indexes; hidden append records a zero-length range.
  Length drift and non-sequential shapes fail safe to a full rebuild, and regression tests compare
  the result with a fresh store.
- R38.3 complete. Tail rerender now rebuilds only the affected visible suffix while arbitrary
  rerender and global render-key changes retain the full-rebuild path. Mixed visible/hidden tail,
  separator, hash, range and explicit length-drift tests preserve full-rebuild equivalence.
- R38.4 complete. The weekly/manual `Long-session Evidence` workflow runs the collector in release
  mode and uploads its JSON artifact without an absolute time threshold. Hosted run
  [29519936563](https://github.com/JimmyDaddy/sigil/actions/runs/29519936563) passed with 10,000
  writer records and one full scan, 2,000 compaction source records/1,999 folds and unchanged raw
  bytes, and a 23 ms timeline scenario (11 ms rebuild, 12 ms sequential append and sub-millisecond
  250 tail rerenders).
- R38.5 complete. The local baseline and hosted result use different machines, so their total
  elapsed values are not treated as a hard speedup ratio. The nearly unchanged 12/11 ms rebuild
  calibration, together with append 2,013/12 ms and tail rerender 200/<1 ms, supports the intended
  algorithmic-shape conclusion. Final main-CI run
  [29531567548](https://github.com/JimmyDaddy/sigil/actions/runs/29531567548) passes contract,
  formatting, check, Clippy, rustdoc, all Linux test/coverage partitions, TUI PTY acceptance and the
  complete hosted macOS/Windows platform jobs. The completion audit found no remaining R38 slice;
  V1 keeps wall-clock values as trend evidence and unlocks no gated product capability.
