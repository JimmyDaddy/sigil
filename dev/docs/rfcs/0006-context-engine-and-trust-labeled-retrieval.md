# RFC-0006 Context Engine and Trust-labeled Retrieval

状态：draft / E06.1-E06.5 and E06.7-E06.9 implemented / full repo intelligence gated

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 Sigil 的 Context Engine。目标是从“模型主动 read / grep / LSP”升级为“系统按任务、信任、敏感度和 token budget 主动组装可解释上下文”。

第一版不引入向量数据库、embedding 或完整 code graph。MVP 只做可审计的 context archive、BM25/session-repo retrieval、LSP/code-intel 输入、`ContextDigestV0` 和 token budget packer。

## 2. Goals

- 让 agent request 能解释自动带入了哪些文件、symbol、历史决策、diff 或 evidence。
- 保持 workspace trust 边界：未信任 workspace 的 `SIGIL.md`、`AGENTS.md`、README 和源码注释都是 untrusted repository data。
- 自动上下文不能绕过 secret / egress / workspace confinement。
- 把现有 LSP/code-intel 能力作为 context source，而不是只作为模型主动调用工具。
- 保持 provider prefix cache 友好：稳定前缀和动态 context 后缀分层。

## 3. Non-goals

- 不在 MVP 中引入 embedding、semantic vector DB、full call graph 或 impact graph。
- 不接入 extension context hook；extension hook 必须等待 RFC-0009 trust plane。
- 不让 context digest 创造新的 evidence。
- 不改变 durable event stream 的事实来源地位。

## 4. Core Types

```rust
struct ContextItem {
    id: ContextItemId,
    source: ContextSource,
    source_event_id: Option<EventId>,
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
    egress_decision: Option<EgressDecisionId>,
    repo_revision: Option<String>,
    token_cost: usize,
    score: Option<f32>,
    inclusion_reason: ContextInclusionReason,
    body_ref: ContextBodyRef,
}

enum ContextTrustLevel {
    System,
    UserProvided,
    WorkspaceInstruction,
    UntrustedRepositoryData,
    ToolObservation,
    ExtensionProvided,
}

enum ContextSensitivity {
    Public,
    Repository,
    PotentialSecret,
    Secret,
    External,
}
```

`WorkspaceInstruction` 只允许在 workspace trust 已满足时使用。否则同一文件进入 `UntrustedRepositoryData`，不能获得 instruction 优先级。

## 5. Context Pipeline

```text
User/task goal
  -> workspace trust snapshot
  -> context source discovery
  -> lexical/BM25/LSP retrieval
  -> sensitivity and egress filtering
  -> token budget packing
  -> prompt assembly with provenance
```

MVP source order:

1. Stable system/developer prompt.
2. Trusted user configuration and trusted workspace instructions.
3. Recent turns.
4. `ContextDigestV0`.
5. BM25 session archive hits.
6. Repo file/symbol hits.
7. LSP diagnostics/symbol/reference hints.
8. Verification and mutation evidence summaries.

Implementation progress:

- 已新增 kernel-level `ContextItem` provenance model。
- 已新增 `ContextDigestV0` 和 deterministic builder。
- 已验证 trusted workspace instruction 必须匹配 workspace trust label；未信任仓库内容不能伪装成 instruction。
- 已验证 included secret-like 和 external context 必须携带 egress decision。
- 已验证 digest 中的 `VerificationVerdict::Passed` 必须引用已有 receipt，不能由 digest 自己创造 evidence。
- 已新增 session-local archive + BM25 retrieval，返回 trust/sensitivity-labeled hits、snippet、score、token cost 和 truncation metadata；secret archive hits without egress are represented as excluded context.
- 已新增 code-intel context adapter，将 symbols、diagnostics、references、repo file hits 和 current diff 转为 provenance-labeled context hits；secret hits without egress are represented as excluded context.
- 已新增 deterministic token budget packer，输出 stable prefix、dynamic suffix 和 excluded context，保持 provider prefix cache 友好并记录 budget/secret exclusion reason。
- 已新增 TUI provenance summary view model，展示 context budget、top included sources、excluded reason summary、untrusted/secret warning 和一个 recommended action。
- 已完成 Context V0 runtime adoption：默认 agent request assembly 会在 stable memory prefix 之后注入动态 `Sigil Context V0` system message，来源包括 session archive BM25 hits 和 latest `TaskMemoryV1` context items；注入结果进入 `PrefixSnapshotCaptured.materialized_text`，可从 session audit 定位。
- Runtime adoption 对 context engine failure 采用安全降级：无可用 source、空 BM25、无 task memory 或 task-memory adapter 失败时，不阻断普通 request。
- Context V0 request rendering 不会为被排除的 secret-like / external item 输出 snippet；未信任 workspace instruction 仍不能提升为 trusted workspace instruction。
- 已完成 Context quality evidence pack：`ContextQualityEvidencePack` 记录 query、included/excluded context rows、token budget、source counts、exclusion reason counts、rank/score、trust/sensitivity/egress labels、truncation facts 和 recall/ranking/token-budget/safety findings。它用于判断 E06.6 是否真的需要打开，而不是直接实现 heavy repo graph 或 semantic retrieval。
- 已完成 Context quality evidence sweep：`scripts/run-context-quality.sh` 可生成 `context-quality.jsonl`、`summary.md` 和 `manifest.json`，用于把 E06.6 trigger decision 绑定到可复核 artifact。

2026-06-29 审计补充：

- 当前实现是 V0 Context Engine：ContextDigestV0、session archive BM25、code-intel adapter、token budget packer、TUI provenance summary、deterministic context quality evidence pack 和 evidence sweep。
- 它不是完整持久 repo graph、semantic vector retrieval、call/impact graph 或跨 session repo knowledge。
- 这些更重能力由 `.repo-local-dev/rfcs/0006-context-engine/e06-06-persistent-repo-graph-semantic-retrieval-trigger.md` 作为 gated trigger 跟踪，只有真实 context-quality 证据或明确产品需求出现后才开工。

## 6. ContextDigestV0

`ContextDigestV0` 是 deterministic、最小、服务 packing 的摘要，不是长期 memory。

```rust
struct ContextDigestV0 {
    objective: Option<String>,
    active_files: Vec<PathBuf>,
    recent_commands: Vec<CommandReceiptId>,
    verification_state: VerificationVerdict,
    unresolved: Vec<String>,
}
```

Rules:

- Digest 只能引用 durable event、receipt 或 artifact id。
- Digest 中的模型推断内容必须标记为 inferred / unverified。
- Digest 不能产生 `VerificationVerdict::Passed`。

## 7. Retrieval MVP

MVP retrieval components:

- Session archive BM25 over compacted conversation, tool observations and evidence summaries.
- Repo lexical/BM25 over bounded text files and symbols.
- LSP/code-intel source provider for symbols, definitions, references and diagnostics.
- Git changed-files and current diff provider.

Each retrieval result must carry:

- source
- score
- snippet
- token cost
- truncation metadata
- trust/sensitivity labels
- inclusion or exclusion reason

## 8. Product Surface

TUI should expose context provenance without turning it into a policy matrix:

- current context budget
- top included sources
- excluded reason summary
- untrusted workspace warning when applicable
- secret/egress blocked summary

The main flow should provide at most one recommended action, such as `review trust` or `open context details`.

## 9. Implementation Slices

1. `ContextDigestV0` and context provenance model.
2. Session archive + BM25 retrieval.
3. Repo file/symbol retrieval using existing code-intel where possible.
4. Token budget packer with deterministic ordering.
5. TUI provenance summary.
6. Context V0 runtime adoption in request assembly.
7. Context quality evidence pack for V0 retrieval/packing inspection.

## 10. Acceptance Criteria

- Request assembly can list context sources and token cost.
- Untrusted repository content cannot become system instruction.
- Secret-like and external content require egress decision before provider context inclusion.
- Context Engine failure degrades to current memory + tool behavior and does not block ordinary chat.
- Tool output pruning affects provider context only, not durable audit.
- LSP symbols/references/diagnostics can be used as context source.

## 11. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel context
cargo test -p sigil-code-intel
cargo test -p sigil-runtime context
cargo test -p sigil-tui context
cargo test -p sigil-kernel context_digest
cargo test -p sigil-kernel context_item
```

Exact test filters should be added with the implementation slices.

## 12. Open Questions

- Whether repo retrieval should remain pure BM25 in MVP or add opt-in local embeddings later.
- Whether context archive should be per-session first or workspace-wide from day one.
- How much context provenance belongs in main TUI versus an inspect panel.
