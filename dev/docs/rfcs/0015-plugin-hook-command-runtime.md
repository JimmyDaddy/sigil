# RFC-0015 Plugin Hook Command Runtime

状态：draft / E15.1-E15.7 implemented / sandbox handoff remains gated

创建日期：2026-06-29

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0005 Execution Backend](0005-execution-backend.md)
- Depends on: [RFC-0009 Extension Trust Plane](0009-extension-trust-plane.md)
- Related: [RFC-0006 Context Engine and Trust-labeled Retrieval](0006-context-engine-and-trust-labeled-retrieval.md)
- Unlocks: RFC-0009 E09.5, RFC-0005 E05.15, RFC-0002 E02.1 and RFC-0003 E03.2

## 1. Summary

本 RFC 定义受信任插件 hook 的执行运行时。目标是让 context hook、compaction hook、verification hook 和未来 plugin-owned process 通过统一 `ExecutionBackend` 执行，并产生 egress、secret、mutation 和 provenance evidence，而不是用 fake adapter 或 in-process callback 绕过控制面。

核心决策：

1. 插件代码加载前仍以 RFC-0009 静态 manifest、digest 和 trust decision 为前置。
2. Hook entrypoint 必须是 manifest 声明的 command/process，不是任意 runtime callback。
3. Hook command 必须通过 RFC-0005 `ExecutionBackend`，继承 sandbox/profile/fallback policy。
4. Hook 输出必须带 source、trust、sensitivity、egress 和 artifact metadata。
5. 任何可能写入 workspace 的 hook 都必须进入 RFC-0002 mutation / unknown-dirty accounting。

## 2. Goals

- 为 context/compaction hooks 提供真实执行路径，解锁 RFC-0009 E09.5。
- 为 plugin-owned process unknown-dirty recording 提供统一进程边界，解锁 RFC-0002 E02.1。
- 为 plugin verification side-effect integration 提供 receipt 与 snapshot 绑定，解锁 RFC-0003 E03.2。
- 为 plugin hook sandbox handoff 提供进程生命周期入口，解锁 RFC-0005 E05.15。
- 保持用户主路径简单：普通用户只看到 trust / disabled / last run / issue summary，不看到 hook matrix。
- 为 TUI、doctor 和 session audit 提供低复杂度 hook runtime surface，避免默认 UI 暴露 hook capability matrix。

## 3. Non-goals

- 不引入插件市场或自动安装生态。
- 不默认执行 npm/JS/TS 代码。
- 不让 hook 输出成为 trusted instruction；context output 仍按 RFC-0006 trust labels 处理。
- 不实现长期 daemon plugin runtime；第一阶段只处理 bounded command execution。
- 不把每个 hook capability 暴露成 `/config` 主路径开关。

## 4. Hook Command Contract

```rust
struct PluginHookCommand {
    extension_id: ExtensionId,
    hook_id: PluginHookId,
    hook_kind: PluginHookKind,
    command: Vec<String>,
    declared_effect: PluginHookEffect,
    timeout_ms: u64,
    input_schema_digest: String,
    output_schema_digest: String,
}

enum PluginHookKind {
    Context,
    Compaction,
    Verification,
    Event,
}

enum PluginHookEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}
```

Rules:

- Command path and args come from static manifest data already covered by trust digest.
- Static manifests may declare the hook contract directly; legacy manifests that only provide
  `event`, `command` and `args` derive the stable hook id from `event`.
- Trust decision may disable individual hook kinds, but ordinary user flow should expose coarse trust/disable actions only.
- Runtime rejects hook execution when manifest digest, version or capability digest no longer matches the trusted decision.
- E15.2 is implemented: trusted hook registrations carry plugin root, manifest
  hash, capability digest and trust evidence; `PluginHookExecutionRunner` executes
  hook commands through `ExecutionBackend`; started/finished control entries carry
  backend and capability evidence.
- E15.3 is implemented: hook stdout/stderr are converted into bounded, redacted
  `PluginHookOutputEnvelope` values; artifact references are capped and not
  auto-included into context.
- E15.4 is implemented: bounded hook stdout can be converted into
  `ContextSource::ExtensionProvided` context candidates with source event,
  trust, sensitivity, egress and snippet metadata; redacted output is excluded
  unless an egress decision is present.
- E15.5 is implemented: workspace-mutating or unknown-effect hook commands
  require a `MutationEventRecorder` before execution, capture pre/post
  workspace scans, emit `WorkspaceMutationDetected` when the verification
  scope changes, and fail closed to unknown-dirty evidence when scan coverage
  is unavailable.
- E15.6 is implemented: trusted verification hook execution can be converted
  into normal `VerificationReceipt` evidence only when hook started/finished
  evidence and bounded output envelope match. The receipt is bound to the
  current workspace snapshot, check spec hash, execution backend/capabilities
  and sandbox/network receipt. Mutating hook executions become inconclusive and
  cannot satisfy final passed evidence. Hook stdout remains untrusted data and
  cannot decide the verdict by itself.
- E15.7 is implemented: `/config` renders coarse hook count/kind/effect
  summaries instead of per-hook command matrices, `/doctor` reports
  trust/effect/runtime hook issues, and session audit renders hook
  start/finish summaries with backend/status/output-size facts.

Implementation progress:

- E15.1 is implemented: hook id, kind, declared effect, timeout and input/output schema digests
  are projected into `PluginCapability::Hook` and therefore included in the plugin capability
  digest used by trust decisions.
- Hook command execution is implemented through E15.2, hook output envelopes
  through E15.3, context/compaction provenance conversion through E15.4, and
  plugin-owned command process mutation recording through E15.5. Verification
  hook receipt binding is implemented through E15.6. TUI/doctor/session-audit
  hook runtime surface is implemented through E15.7; future product surfaces
  must consume these receipts instead of implementing fake adapters.

## 5. Execution and Evidence

Execution flow:

```text
manifest trusted
  -> hook command requested
  -> policy/capability check
  -> ExecutionBackend selected
  -> hook process executes
  -> stdout/stderr/artifacts parsed with size limits
  -> egress/secret/mutation receipts emitted
  -> hook result converted to target subsystem input
```

Required evidence:

- `PluginHookExecutionStarted`
- `PluginHookExecutionFinished`
- `ToolEgressReceipt` or equivalent egress record when external data leaves local boundary
- `WorkspaceMutationDetected` or `UnknownDirty` when declared or detected effect can write
- context provenance item for context/compaction output

## 6. Context and Compaction Integration

Hook output must not be injected as trusted prompt instructions.

Context hook output becomes RFC-0006 context data:

- source = plugin hook execution id
- trust = extension trust-derived, never higher than workspace trust
- sensitivity = declared or detected
- egress decision = recorded
- inclusion reason = hook id and policy

Compaction hook output may propose a summary, but it cannot create verification evidence or mutate `TaskMemoryV1` facts without source references.

## 7. Product Surface

Default TUI/config surface should show:

- extension trusted / disabled / needs review
- hook categories enabled
- last run status
- last egress/mutation summary
- one action: review, trust, disable or inspect

Detailed hook command args, environment, sandbox profile and receipt ids belong in inspect/doctor/session audit.

Current implementation:

- `/config` Plugins shows hook count, hook kind/effect summaries, execution-backend/evidence guidance, and an inspect hint.
- `/doctor` reports `plugins:hooks` with trust/effect summary and remediation for review-needed or risky hook effects.
- Session audit renders hook execution start/finish summaries; raw internal hook execution records remain durable control entries, not default UI matrices.

## 8. Implementation Slices

1. Hook command manifest contract. Implemented in E15.1.
2. Hook execution runner through `ExecutionBackend`. Implemented in E15.2.
3. Hook output envelope and bounded artifact handling. Implemented in E15.3.
4. Context/compaction hook integration. Implemented in E15.4.
5. Plugin-owned process mutation recorder. Implemented in E15.5 for hook command processes.
6. Verification hook receipt binding. Implemented in E15.6.
7. TUI/doctor product surface. Implemented in E15.7.

## 9. Acceptance Criteria

- Untrusted or changed plugin manifest cannot execute hook code.
- Hook execution goes through `ExecutionBackend`, not direct in-process callback.
- Hook output has provenance and trust/sensitivity labels.
- Hook process can produce egress and mutation evidence.
- Context/compaction hooks cannot create verification evidence by summary text.
- Default user surface remains coarse and avoids capability matrix overload.
- Doctor and session audit can expose hook runtime status without requiring normal users to understand every manifest field.

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel plugin
cargo test -p sigil-kernel plugin_verification_hook
cargo test -p sigil-runtime plugins
cargo test -p sigil-runtime context
cargo test -p sigil-tui config_plugins
cargo test -p sigil-runtime doctor
```

## 11. Open Questions

- Whether hook commands should support only local executables first, or also MCP-provided hook processes.
- Whether plugin hook inputs should use files/artifacts instead of large stdin payloads.
- Which hook kinds deserve default enablement after trust, if any.
