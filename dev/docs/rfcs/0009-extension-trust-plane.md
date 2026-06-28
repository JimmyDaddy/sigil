# RFC-0009 Extension Trust Plane

状态：draft / roadmap candidate

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Related: [RFC-0005 Execution Backend](0005-execution-backend.md)

## 1. Summary

本 RFC 定义插件、自定义工具、技能包、context hook、compaction hook 和外部服务集成的统一 trust plane。核心原则是：扩展代码加载前必须完成静态 manifest 读取、digest 校验和 trust decision；扩展执行必须复用 tool permission、ExecutionBackend、egress、secret 和 durable audit。

## 2. Goals

- 防止本地扩展在授权前执行代码。
- 让扩展工具与内置/MCP 工具共享 approval、egress 和 audit 控制面。
- 扩展内容、版本、digest 或 capability 变化后使旧 trust decision 失效。
- 为未来 plugin-owned process 接入 RFC-0002 unknown-dirty recorder。

## 3. Non-goals

- 不在 MVP 中引入插件市场或自动安装生态。
- 不默认执行 JS/TS/npm 插件。
- 不让 extension hook 绕过 Context Engine trust labels。
- 不把扩展 trust UI 做成复杂 capability matrix 的普通主流程。

## 4. Static Manifest

Manifest must be readable without executing extension code.

```rust
struct ExtensionManifest {
    extension_id: ExtensionId,
    version: String,
    source: ExtensionSource,
    content_digest: String,
    declared_capabilities: Vec<ExtensionCapability>,
    entrypoints: Vec<ExtensionEntrypoint>,
}
```

Trust decision:

```rust
struct ExtensionTrustDecision {
    extension_id: ExtensionId,
    version: String,
    content_digest: String,
    install_scope: InstallScope,
    allowed_capabilities: Vec<ExtensionCapability>,
    secret_access: SecretAccessPolicy,
    network_policy: NetworkPolicy,
    approval_default: ApprovalMode,
    decided_by_event_id: EventId,
}
```

## 5. Capability Model

Initial capabilities:

- custom tool registration
- event hook
- context hook
- compaction hook
- env injection
- network access
- filesystem read/write
- MCP server declaration

Every capability must map to enforcement:

- ToolSpec permission
- ExecutionBackend requirement
- egress decision
- secret access policy
- mutation recorder when a process can write

## 6. Lifecycle

```text
discover static manifest
  -> verify digest/source/version
  -> present coarse trust decision
  -> append ExtensionTrustDecision
  -> load/register extension
  -> execute through controlled runtime
  -> append execution/egress/mutation receipts
```

Extension update invalidates trust when digest, version or declared capabilities change.

## 7. Product Surface

TUI `/config` should show:

- extension source
- trust status
- declared capabilities
- last execution summary
- egress/secret summary
- one recommended action: review/trust/disable

Detailed capability data belongs in inspect details, not the main footer.

## 8. Implementation Slices

1. Static manifest schema and digest validation.
2. Durable `ExtensionTrustDecision` projection hardening.
3. Extension registration gate before code load.
4. Tool/egress/secret policy integration.
5. Context/compaction hook integration after RFC-0006.

## 9. Acceptance Criteria

- Extension code cannot run before trust decision.
- Manifest digest change invalidates prior trust.
- Extension tool execution uses normal approval/audit path.
- Extension context output has trust/sensitivity/source attribution.
- Plugin-owned process execution records RFC-0002 unknown-dirty evidence.

## 10. Validation

Recommended checks:

```bash
cargo test -p sigil-kernel plugin
cargo test -p sigil-runtime plugins
cargo test -p sigil-tui config
```

## 11. Open Questions

- Whether extension execution should be in-process for trusted local extensions or always isolated.
- Whether npm-style package installation belongs in Sigil or an external package manager.
- Which extension capabilities should be hidden behind advanced mode.
