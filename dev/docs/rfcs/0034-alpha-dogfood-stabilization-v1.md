# RFC-0034 Alpha Dogfood Stabilization V1

状态：accepted / R34.0-R34.1 complete / R34.2-R34.5 planned

创建日期：2026-07-16

基线：

- Depends on: [RFC-0028 Real-model Acceptance and Provider Conformance V1](0028-real-model-acceptance-and-provider-conformance-v1.md)
- Depends on: [RFC-0030 Alpha Feedback and Supportability V1](0030-alpha-feedback-and-supportability-v1.md)
- Depends on: [RFC-0033 Image & Attachment Input V1](0033-image-attachment-input-v1.md)
- Release baseline: `v0.0.1-alpha.4` at `f4e6c5aeea86b3283988efe20db44a0f97454f97`
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)

## 1. Summary

Sigil 已具备公开 npm、Homebrew、GitHub Release、真实 binary/PTY acceptance、脱敏 feedback 和 cost-bounded model eval，但这些证据仍分散在 release workflow、独立脚本和人工 session 中。RFC-0034 将它们收敛为发布后的 alpha dogfood 稳定化闭环。

本 RFC 不增加新的产品命令或遥测。它先验证公开渠道中的精确版本，再使用冻结的生产 binary 在隔离 workspace/state/cache 下执行可重复的离线真实入口 campaign。只有 dogfood 产生了可复核失败证据，才允许打开对应修复；既有 gated 架构项不会因“开始稳定化”而自动解锁。

## 2. Goals

1. 每次 alpha 发布后验证 npm、Homebrew、Release archive、checksum、attestation 与 doctor metadata 指向同一版本。
2. 用一个 runner 聚合已有 production-binary acceptance，避免人工遗漏或测试错误的 binary。
3. 每个 case 使用独立 workspace、state、cache 和 loopback fixture；失败不能污染后续 case。
4. campaign 总结只保存版本、commit、target、case 状态、耗时和相对 evidence 路径。
5. 将真实用户 session、公开 issue、`/feedback` report 和 opt-in model campaign 转成明确的修复触发条件。
6. 在 alpha.5 前形成可复核的 blocker、regression、accepted limitation 和 release-readiness 结论。

## 3. Non-goals

- 不增加 analytics、telemetry、crash upload 或后台网络上报；
- 不自动读取或上传 session JSONL、workspace 文件、prompt、tool input/output、credential 或环境变量；
- 不全局安装 npm/Homebrew 包，不修改用户现有 Sigil 配置；
- 不把 loopback acceptance 宣称为公网或付费 provider 验收；
- 不自动运行付费模型；真实 provider campaign 必须显式提供 config、case、预算和 deadline；
- 不因为 dogfood 存在就解锁 Remote MCP OAuth、persistent semantic graph、SQLite projection、physical worktree 或 Windows restricted backend；
- 不承诺透明恢复运行中的 shell、agent 或远程副作用。

## 4. Evidence tiers

| Tier | Evidence | Required boundary |
| --- | --- | --- |
| Distribution | Public npm/Homebrew install, GitHub archives, checksum and attestation | Exact published version and immutable release assets |
| Offline binary | Real `sigil` binary through headless/PTY entrypoints with loopback fixtures | No public provider request and isolated local roots |
| Stateful dogfood | Cross-turn compact/resume/checkpoint/feedback interaction | Fresh fixture, durable evidence and no hidden retry |
| Real provider | Explicit provider-backed edit/verification/plan campaign | User-supplied config, local cost admission and fixed deadline |
| User feedback | GitHub issue or reviewed local `/feedback` report | User explicitly chooses what to share |

Lower tiers cannot be presented as stronger evidence. A passing loopback case proves application wiring and product interaction, not remote provider quality or billing behavior.

## 5. Exact binary admission

The offline campaign runner must:

1. accept an explicit executable path;
2. run `sigil --version` before any case;
3. parse version, commit, target and profile from that output;
4. compute the executable SHA-256 before any case;
5. optionally require exact expected version, build-reported commit prefix and executable SHA-256;
6. reject a missing, non-executable or mismatched binary before creating case state;
7. record only a safe binary label, SHA-256 and parsed build identity, not its absolute local path.

The runner never builds implicitly. Building a candidate and selecting the binary remain separate, visible operations so a stale debug or release binary cannot silently satisfy the campaign.

## 6. Offline campaign contract

R34.2 aggregates these existing production paths:

- Context V1 through `sigil run --output json`;
- Web V1 through a real TUI PTY and loopback MCP/provider/HTTP fixtures;
- `/feedback` preview/export privacy flow through a real TUI PTY;
- terminal attention default-off/explicit-BEL behavior through a real TUI PTY;
- image path/clipboard/provider-wire/session/compaction flow through a real TUI PTY and loopback providers.

Every case receives its own output directory. The aggregate runner continues after a failed case, writes a terminal manifest and exits non-zero when any selected case fails. Clipboard cases may only be skipped through an explicit flag; the manifest must record the skip reason.

The runner starts every case with a minimal environment allowlist. It replaces `HOME`, XDG roots and temporary storage with case-owned directories; does not inherit provider credentials, Sigil config overrides or ambient HTTP proxy state; and configures non-loopback HTTP/HTTPS attempts to fail against a closed local endpoint while preserving loopback fixture access. Existing case scripts still own their finer workspace/state/cache setup, but they cannot accidentally fall back to the user's provider or config environment.

## 7. Evidence and privacy

The aggregate `manifest.json` and `summary.md` may contain:

- schema version and campaign terminal status;
- selected case ids and their passed/failed/skipped status;
- duration and relative report/log directories;
- parsed Sigil version, commit, target and profile;
- executable SHA-256;
- fixed privacy statements.

They must not contain:

- absolute binary/workspace/state/cache paths;
- provider request or response bodies;
- prompt text, tool arguments, file content or diff;
- environment names or values, credentials, private endpoints or session log content.

Raw case artifacts remain under ignored `.repo-local-dev` output by default. They are local debugging evidence and are never committed or uploaded automatically.

## 8. Failure classification

Each observed failure must be classified before code changes:

- `distribution_blocker`: public install, asset, checksum, attestation or version mismatch;
- `product_regression`: a previously accepted user flow fails on the frozen binary;
- `privacy_or_safety_blocker`: evidence leakage, hidden network, permission bypass or state contamination;
- `provider_variance`: remote behavior differs while local contracts remain correct;
- `environment_limitation`: terminal/platform prerequisite is absent and the case was explicitly skipped;
- `harness_defect`: the acceptance runner is wrong while the product path remains valid.

Only evidence-bound failures unlock fixes. A harness defect must be repaired in the harness slice and cannot be counted as a product regression.

## 9. Implementation slices

1. R34.0: formal RFC, execution plan, status ledger and decomposition audit.
2. R34.1: manually run the public distribution smoke against `alpha.4` and record the exact workflow result.
3. R34.2: add the isolated offline production-binary campaign runner, runner contract tests and developer instructions; execute it against the released binary.
4. R34.3: add one stateful real-PTY campaign for compact/resume/checkpoint/focus/reply-deduplication boundaries that are not already proven together.
5. R34.4: run an explicit bounded real-provider dogfood matrix and fix only evidence-backed product/provider regressions.
6. R34.5: reconcile issues, accepted limitations and evidence; run final gates and prepare the next alpha readiness conclusion.

## 10. Acceptance criteria

- Published Distribution Smoke passes npm on Linux/Windows/macOS ARM/macOS Intel, Homebrew on both macOS architectures, and Release checksum/attestation checks.
- Offline runner refuses an incorrect binary identity before executing cases.
- Offline cases cannot inherit user provider credentials/config or reach a public provider through ambient proxy state.
- Selected cases execute through the existing real binary and real PTY entrypoints, not a test-only agent loop.
- Every case is isolated and the aggregate manifest is written on both success and failure.
- Aggregate evidence contains no absolute private paths, prompts, raw provider material, credentials or session content.
- A failed case cannot prevent later cases from producing evidence.
- Paid/public-provider work remains explicit and cost-bounded.
- No gated architecture item is opened without its original trigger evidence.

## 11. Validation

```bash
./scripts/test-published-distribution-smoke.sh
python3 scripts/alpha-dogfood-campaign.py --help
python3 scripts/test-alpha-dogfood-campaign.py
python3 scripts/alpha-dogfood-campaign.py \
  --binary <released-sigil-binary> \
  --expected-version 0.0.1-alpha.4
./scripts/check-docs.sh
git diff --check
```

R34.3-R34.5 add their own targeted PTY, provider, docs and full-workspace gates before they enter `done`.

## 12. Progress

- R34.0 complete：RFC、execution slices、privacy/evidence contract、exact-binary admission and dependency order accepted on 2026-07-16.
- R34.1 complete：GitHub Actions run `29475042404` passed the exact `alpha.4` public distribution matrix: four npm platforms, two Homebrew architectures, GitHub archive checksums, artifact attestations and doctor build/privacy metadata.
