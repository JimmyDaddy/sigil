# RFC-0006 Context Engine and Trust-labeled Retrieval

状态：draft / slice 1 context digest and provenance model implemented

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
- 已验证 included secret context 必须携带 egress decision。
- 已验证 digest 中的 `VerificationVerdict::Passed` 必须引用已有 receipt，不能由 digest 自己创造 evidence。
- 尚未实现 session archive、BM25/repo retrieval、LSP source provider、token budget packer 或 TUI provenance summary。

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
