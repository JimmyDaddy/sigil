# RFC-0032 Multilingual RepoMap / Context V1

状态：accepted / R32.0-R32.2 complete / implementation in progress

创建日期：2026-07-16

基线：

- Depends on: [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)
- Depends on: [RFC-0025 Context Compaction V2](0010-structured-compaction-and-task-memory.md)
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)
- Implementation baseline: `70373adb9b5ca8e8880ab08dc4790204386d21b4`

## 1. Summary

本 RFC 把 request-local RepoMap 从 Rust-only 扩展为 Rust、Python、JavaScript/JSX、TypeScript/TSX 和 Go，并把生产请求上下文升级为 Context V1。

Context V1 优先消费已经存在于同一 code-intelligence service 中的 warm LSP snapshot；只有 LSP disabled、unavailable、timed out 或没有当前 query 的相关 hit 时，才运行有硬上限的 request-local Tree-sitter fallback。该能力不引入持久 graph、embedding、网络、运行期 grammar 下载或新的 durable event。

## 2. Goals

1. 多语言仓库在没有 language server 时仍有 bounded symbol/file context。
2. LSP 可用时复用已有 cache，不在 request assembly 中发新 LSP request。
3. explicit path、token budget、workspace confinement、ignored/secret filtering 和 provenance 不回退。
4. 正常 chat、plan、queued pre-turn、compaction reprepare 和 headless run 使用同一 selection contract。
5. Context V1 durable prefix 能解释 included/excluded source 和 fallback 原因。

## 3. Non-goals

- persistent repo index、SQLite、semantic graph、vector database 或 embedding；
- 每轮全仓扫描或把完整 RepoMap 注入模型；
- Java/Kotlin/Swift/Ruby/C/C++/C#；
- Tree-sitter heuristic 冒充 resolved call graph；
- 在 request assembly 中启动 LSP、进程、网络或下载；
- 新增普通用户配置矩阵。

## 4. Language Contract

首批 adapter：

| Language | Extensions | Definition source |
| --- | --- | --- |
| Rust | `rs` | existing Rust document symbols + tags references |
| Python | `py`, `pyi` | Tree-sitter tags |
| JavaScript/JSX | `js`, `jsx`, `mjs`, `cjs` | Tree-sitter tags |
| TypeScript | `ts`, `mts`, `cts` | Tree-sitter tags |
| TSX | `tsx` | Tree-sitter tags |
| Go | `go` | Tree-sitter tags |

静态 registry 统一 grammar、tags query、extension 和 normalized symbol kind。grammar 是编译期依赖，不从 workspace 或网络加载。

## 5. Request-local RepoMap Contract

RepoMap 使用 `ignore::WalkBuilder`，尊重 Git/ignore filters，拒绝 symlink follow，并额外跳过 Sigil local state、generated/dependency/cache 和 secret-like 路径。

默认 hard caps：4,096 walked entries、640 source files、192 KiB indexed bytes/file、128 definitions/file、256 references/file、8,192 definitions、16,384 references、16,384 edges。

文件必须通过 bounded reader 读取；禁止先 `fs::read` 完整文件再截断。unsupported file 不消耗 source-file budget。

V1 只把 same-language unique definition reference 连为 heuristic `References` edge；ambiguous、cross-language 和 unresolved reference 不连边。所有 symbol/file/edge 排序必须 deterministic。

## 6. Selection Contract

每次请求：

1. 解析 explicit workspace-relative paths；
2. 从共享 `CodeIntelligenceService` 读取最多 35ms 的 warm cache snapshot；
3. 按当前 Unicode-aware query 对 warm rows 排名；
4. 有相关 warm hit 时使用 explicit path + LSP rows，跳过 Tree-sitter RepoMap；
5. disabled/unavailable/timed out/no relevant hit 时使用 explicit path + Tree-sitter RepoMap，并保留 bounded excluded provenance；
6. selection failure 不阻断 ordinary chat。

已经冻结的 provider request 不在 dispatch 时重新 selection。queued 和 compaction 路径在各自 preparation/CAS 边界 materialize Context V1。

## 7. Ranking and Snippets

优先级为 explicit path、LSP exact hit、Tree-sitter exact symbol、symbol/path token、lexical content。query tokenization 支持 CJK、identifier case/separator 和 path segment，稳定 tie-breaker 使用 path/range/name。

symbol snippet 以 range 为中心；最终 repository candidates 仍最多三个，并继续由 kernel token packer执行总预算。

## 8. Context V1 Wire and Durability

新 request message：

- header：`Sigil Context V1`；
- schema：`sigil_context_v1`；
- placement：`dynamic_suffix`；
- selection policy：`warm_lsp_then_request_local_tree_sitter`；
- id：`context:v1:<sha256>`。

included/excluded row 继续携带 source、source ref、trust、sensitivity、egress、repo revision、token cost、score breakdown、reason、body ref 和 bounded snippet。

Context V1 只通过既有 `PrefixSnapshotCaptured` 成为 durable request evidence；不创建 RepoMap event、LSP cache event 或独立 index artifact。TUI 必须同时读取旧 Context V0 snapshot 和新 Context V1 snapshot。

## 9. Ownership

- `sigil-code-intel`：adapter、bounded walk/read、tags、RepoMap、warm LSP query ranking。
- `sigil-runtime`：共享 service/tool surface、request resolver、selection、candidate ranking/snippet。
- `sigil-kernel`：Context V1 render、packing、trust/sensitivity/egress、prefix materialization。
- `sigil-tui`：所有 production path wiring 和 V0/V1 provenance surface。

`sigil-kernel` 不依赖 Tree-sitter，也不暴露 provider 或 language-server 私有类型。

## 10. Implementation Slices

1. R32.0：调研、技术方案、正式 RFC、执行计划和 status。
2. R32.1：多语言 adapter、grammar dependencies、tag extraction 和 fixtures。
3. R32.2：ignore-aware bounded RepoMap、caps、unique-reference edges 和 deterministic tests。
4. R32.3：Unicode query、ranking、symbol-centered snippet 和 Context V1 kernel wire。
5. R32.4：shared warm-LSP resolver/tool surface 和 normal/headless production wiring。
6. R32.5：queued/compaction wiring、TUI V0/V1 provenance 和 docs/site。
7. R32.6：real binary acceptance、binary-size evidence、workspace gates 和完整度/质量审查。

## 11. Acceptance Criteria

- 五类语言 fixture 的 definitions 可检索，JSX/TSX 分支有覆盖；
- ignored、secret、generated、symlink、oversize 和 cap 行为 fail closed；
- ambiguous symbol 不产生 reference edge；
- CJK/code/path query 和 symbol-centered snippet 有 deterministic tests；
- warm LSP relevant hit 可证明跳过 Tree-sitter fallback；无 hit 才 fallback；
- normal、plan、queue、compaction、headless 全部 materialize Context V1；
- durable prefix 使用 V1，旧 V0 session 仍可在 TUI inspect；
- 不新增 persistent graph/cache/session artifact 或 durable event；
- release binary size delta、targeted/full gates、docs/site 和两轮审查有记录。

## 12. Validation

```bash
cargo test -p sigil-code-intel context
cargo test -p sigil-runtime context
cargo test -p sigil-kernel runtime_context
cargo test -p sigil-tui context
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
./scripts/check-pages-site.sh
git diff --check
```

## 13. Progress

- R32.0 complete：互联网/竞品调研、language/admission/durable contract、commit/gate 边界已冻结；implementation 从 R32.1 开始。
- R32.1-R32.2 complete：编译期 grammar adapter 覆盖 Rust、Python、JavaScript/JSX、TypeScript/TSX 和 Go；request-local RepoMap 已切换到 ignore-aware deterministic walker、bounded UTF-8 reader、硬上限和 same-language unique-reference heuristic。由于 adapter 在 R32.2 接入前没有 production consumer、无法独立通过 strict Clippy，两个相邻切片合并为同一个可独立通过 gate 的 semantic commit。
- R32.3 complete：新 request wire 使用 `Sigil Context V1`、`sigil_context_v1`、固定 selection policy 和 content-addressed `context:v1:<sha256>`；runtime query/ranking 支持 Unicode/CJK、identifier separator/case，source candidate 保留 Tree-sitter symbol range 并以定义位置为中心生成 bounded snippet。旧 Context V0 durable prefix 的 TUI reader 保持不变，R32.5 再扩展为 V0/V1 双读。
- R32.4 complete：runtime tool surface 同时返回 registry 与绑定到同一 `CodeIntelligenceService` inner 的 request resolver；resolver 在 35ms 内只读 warm cache，query-relevant hit 使用 explicit path + LSP 并跳过 RepoMap，miss/disabled/timeout 使用 request-local RepoMap 并保留 excluded LSP provenance。TUI normal/plan 与 headless application preparation 已消费该 resolver；queue/compaction 留在 R32.5 的 freeze 边界接入。
