# RFC-0037 Cross-platform CI Reliability V1

状态：in progress / R37.0-R37.4

创建日期：2026-07-17

基线：

- Main CI: [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml)
- Supply-chain policy: [`dependency-supply-chain.md`](../../governance/dependency-supply-chain.md)
- Predecessor: [RFC-0036 Code Intelligence Public API Hardening V1](0036-code-intelligence-public-api-hardening-v1.md)

## 1. Summary

Sigil publishes macOS and Windows artifacts, but its pull-request CI proves the complete workspace
only on Linux. Windows currently runs one `sigil-http` durability suite and macOS is exercised only
by release/distribution workflows. Dependency license, source and advisory checks also rely on
manual release-preparation runs instead of a durable repository gate.

This RFC adds a bounded platform matrix to normal CI, replaces the narrow Windows-only job with
workspace compilation plus platform-relevant tests, adds a dependency supply-chain workflow, and
enables low-noise weekly dependency update proposals. It improves evidence; it does not claim that
Windows has a restricted execution backend or that a locally parsed workflow has already passed a
hosted runner.

## 2. Goals

1. Compile the complete workspace and all targets on hosted macOS and Windows runners.
2. Run platform-relevant kernel/runtime/tool/transport suites on both non-Linux platforms.
3. Preserve the Windows durable-journal regression inside the broader platform job.
4. Enforce Cargo license, source, ban and advisory policy when dependency inputs change and on a
   weekly schedule.
5. Propose Cargo and GitHub Actions version updates weekly with bounded PR concurrency and grouped
   low-risk updates.
6. Keep every hosted-only claim explicit until a pushed workflow produces remote evidence.

## 3. Non-goals

- No Windows restricted sandbox backend or change to existing sandbox fallback semantics.
- No promise that platform checks prove every TUI terminal integration or installer surface.
- No product behavior, durable event, protocol, configuration or dependency change.
- No release, version, tag, artifact publication or branch push.
- No automatic merge of dependency updates.

## 4. Platform CI contract

The main CI workflow adds one `platform-check` matrix after the Linux workspace check:

- both runners execute `cargo check --locked --workspace --all-targets`;
- macOS runs `sigil-kernel`, `sigil-tools-builtin`, `sigil-code-intel` and `sigil-runtime` library
  tests, covering the macOS execution backend and shared runtime contracts;
- Windows runs `sigil-kernel`, `sigil-mcp` and `sigil-runtime` library tests plus
  `sigil-http --all-targets`, preserving durable replace/reopen coverage;
- `fail-fast` is disabled so one platform failure does not erase evidence from the other;
- no environment skips, `continue-on-error` or release-only trigger hides failures.

Linux remains the exhaustive test/Clippy/coverage surface. Platform CI is an additional compile and
semantic portability gate, not a second release workflow.

## 5. Supply-chain contract

A dedicated workflow runs on Cargo manifest/lock/policy changes, relevant workflow changes,
`main`, manual dispatch and a weekly schedule:

1. `cargo-deny` enforces the committed `deny.toml` license, source, ban and advisory policy.
2. `cargo-audit` independently checks the lockfile against RustSec while carrying only the exact
   advisory exceptions already documented in `deny.toml` and the dependency ledger.
3. Every action and tool version is explicit; permissions remain `contents: read`.

The two jobs stay separate so a license/source failure is distinguishable from an advisory
failure. Exceptions remain code-reviewed repository state; the workflow does not introduce an
untracked allowlist.

## 6. Dependabot contract

`.github/dependabot.yml` monitors the root Cargo workspace and GitHub Actions weekly. Cargo
minor/patch updates are grouped, action updates are grouped, major Cargo updates remain isolated,
and each ecosystem has a small open-PR limit. Dependabot proposes changes only; normal CI and
supply-chain gates still decide whether a proposal is acceptable.

## 7. Implementation slices

1. R37.0: RFC, baseline inventory, hosted-evidence boundary and execution ledger.
2. R37.1: macOS/Windows workspace check and platform-relevant test matrix.
3. R37.2: dependency supply-chain workflow and local policy proof.
4. R37.3: low-noise Dependabot configuration and static validation.
5. R37.4: local macOS validation, workflow audits, docs/diff gates and completion calibration.

## 8. Acceptance criteria

- Main CI has non-optional macOS and Windows workspace/all-target compile jobs.
- The Windows matrix still runs `sigil-http --all-targets` and the old duplicate job is removed.
- macOS and Windows run the platform-specific package sets defined by this RFC.
- Supply-chain workflow runs both `cargo deny check` and `cargo audit` policy equivalents.
- Advisory exceptions are identical to the committed policy/ledger and carry no wildcard bypass.
- Dependabot monitors Cargo and GitHub Actions weekly with bounded PR concurrency.
- Workflow and Dependabot YAML parse locally; local macOS commands pass.
- Status text distinguishes local/static evidence from the first future hosted Windows run.
- No user documentation or site update is required because product behavior is unchanged.

## 9. Validation

```bash
ruby -e 'require "yaml"; Dir[".github/**/*.yml"].each { |path| YAML.parse_file(path) }'
cargo check --locked --workspace --all-targets
cargo test --locked -p sigil-kernel -p sigil-tools-builtin -p sigil-code-intel -p sigil-runtime --lib
cargo deny check
cargo audit --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0436
./scripts/check-docs.sh
git diff --check
```

The equivalent Windows hosted commands become externally verified only after the workflow is
pushed and GitHub Actions runs it.

## 10. Progress

- R37.0 complete. Baseline inventory confirms full Linux CI, one Windows-only
  `sigil-http --all-targets` job, macOS release/distribution builds, committed `deny.toml`, and no
  normal supply-chain workflow or Dependabot configuration. The design preserves the Windows
  durability suite, sets exact platform package scopes and treats future hosted execution as
  delivery evidence rather than local completion evidence.
- R37.1 complete. Main CI now has a fail-fast-disabled macOS/Windows matrix that compiles the
  complete workspace and all targets, runs the RFC platform package suites, and preserves the
  Windows HTTP durable-journal suite without the previous duplicate job. The first local macOS
  run exposed a PID-publication race in two cancellation tests; both now wait for a complete PID
  rather than mere file existence. Five focused Bash and three terminal cancellation repetitions,
  the complete workspace/all-target check, and the full macOS package set pass (144 code-intel,
  1,090 kernel, 570 runtime and 184 tools tests; one Docker conformance test remains opt-in).
