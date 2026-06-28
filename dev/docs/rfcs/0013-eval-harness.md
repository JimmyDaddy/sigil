# RFC-0013 Eval Harness

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 Sigil 的 eval harness。目标是用可重复的 repo-local tasks 衡量 agent 是否真的更强，而不是只靠手工体验或单次模型成功。

Eval 不能替代单元测试。单元测试证明语义不变量，eval 衡量端到端产品行为、成功率、成本、审批次数和验证完成度。

## 2. Goals

- 建立可重复 eval case 和 fixture repo。
- 记录 verified / unverified final state。
- 区分模型失败、工具失败、权限阻断、验证失败、未验证完成和 sandbox denial。
- 支持安全/adversarial cases。
- 为 release 或重大能力变更提供趋势数据。

## 3. Non-goals

- 不把 eval 作为所有本地提交默认 gate。
- 不用单次模型成功/失败判断回归。
- 不替代 `cargo test`、clippy 或 coverage。
- 不把 provider 私有 metric 上移到 kernel。

## 4. Eval Case Model

```rust
struct EvalCase {
    id: EvalCaseId,
    fixture: RepoFixture,
    prompt: String,
    expected_outcome: ExpectedOutcome,
    required_verification: Vec<CheckSpecId>,
    security_expectations: Vec<SecurityExpectation>,
}

struct EvalRunRecord {
    case_id: EvalCaseId,
    repo_fixture_commit: String,
    sigil_version: String,
    provider: String,
    model: String,
    model_parameters_hash: String,
    tool_schema_digest: String,
    config_hash: String,
    sandbox_backend: String,
    os_toolchain: String,
    seed: Option<u64>,
}
```

## 5. Initial Case Families

Functional:

- small edit
- multi-file refactor
- failing test repair
- docs sync
- permission denial recovery
- context retrieval
- task replanning
- verifier failure
- checkpoint restore
- projection rebuild

Security/adversarial:

- malicious workspace instruction prompt injection
- README asks to read/upload secret-like file
- symlink escape
- path normalization bypass
- mutation after verification passed
- verification command mutates source
- `MutationPrepared` crash
- file written without commit event
- checksum mismatch
- read-only shell write denial
- child verification incorrectly inherited after merge

## 6. Metrics

Record:

- final run status
- verification verdict
- visible completion state
- tool calls
- token usage
- wall time
- approval count
- changed files
- evidence receipts
- sandbox backend
- context sources
- revert target
- extension trust decision
- dispatch trace completeness
- projection rebuild status
- network approvals

## 7. Runner Rules

- Model cases run multiple times when used for trend decisions.
- Deterministic fake-provider cases can run in CI.
- Heavy model evals run nightly or release-prep.
- Eval output is structured JSONL plus human summary.
- Failed eval must preserve session log and artifacts for inspection.

## 8. Implementation Slices

1. Deterministic conformance eval runner using fake provider/tool.
2. Fixture repo format and result schema.
3. Security/adversarial cases for existing RFC-0001/0002/0003 invariants.
4. Optional model eval runner.
5. TUI/CLI report command or developer script.

## 9. Acceptance Criteria

- Same eval case can rerun and produce comparable structured result.
- Eval distinguishes verified success from completed-unverified.
- Security cases cover prompt injection, path escape, checksum mismatch, stale verification and read-only write denial.
- Results include provider/model/config/tool schema metadata.
- Eval failures point to session log and durable evidence.

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel eval
cargo test -p sigil-runtime eval
```

Full model eval should not be part of ordinary local commit gates.

## 11. Open Questions

- Which fixture repos should be committed versus generated.
- How many repeats are enough for provider/model trend comparisons.
- Whether eval reports should live under `.repo-local-dev` or a dedicated `dev/evals` directory.
