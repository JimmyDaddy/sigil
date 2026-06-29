# RFC-0005 Execution Backend

状态：draft / E05.1-E05.6 implemented / productization remains

创建日期：2026-06-28

基线：

- Roadmap: [Sigil Capability Roadmap v1.0 / Frozen](../sigil-capability-roadmap.md)
- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0002 Crash-consistent Mutation Protocol](0002-crash-consistent-mutation-protocol.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)

## 1. Summary

本 RFC 定义 Sigil 的 execution backend 抽象。目标是把“能不能执行”的 permission policy 和“执行后最多能影响什么”的 enforcement backend 分开，为后续 Seatbelt、Bubblewrap、Docker 或远端执行后端提供稳定接入点。

第一切片完成 non-interactive `bash` 的 `LocalBackend` 迁移：用户可见行为保持不变，但执行路径不再直接散落在 `bash` tool 内部。

第二切片增加配置驱动的 backend selection 和 fail-closed isolation policy。默认仍允许 `local`，但一旦配置显式要求 sandbox，当前 `LocalBackend` 不能被静默当作 fallback 使用。

第三切片将 RFC-0003 verification check runner 接入同一个 `ExecutionBackend`。验证命令不再直接 spawn 本地进程；`/task` orchestrator 使用 runtime 配置的 backend，并在缺少 backend 时 fail closed。

第四切片增加第一个 OS sandbox backend MVP：macOS `sandbox-exec` / Seatbelt 后端。它仅覆盖 non-interactive command execution，不覆盖 persistent terminal、MCP、插件或远端工具。

2026-06-29 审计补充：macOS Seatbelt backend 当前仍只能宣传为 non-interactive filesystem/process sandbox MVP。手动测试显示 loopback `nc` 行为未被可靠拦截；E05.6 已将 backend 的 `network_isolation` capability 下调为 `false`，避免 verification receipt 过度信任未证明的网络隔离。

后续切片增加 execution coverage labels，用于明确 shell、MCP、插件和远端能力分别由哪个边界控制。该模型只描述真实覆盖关系，不把 MCP、插件或远端服务宣传成本地 shell sandbox 保护。

Persistent terminal 切片不把 PTY 伪装成 non-interactive bash。它先把 terminal task 的 backend kind/capability 写入 durable handle 和 tool metadata，让 projection、恢复和 UI 能区分 local process 与 local PTY 边界。

## 2. Goals

- 提供 provider-neutral、tool-neutral 的 `ExecutionBackend` 契约。
- 为 backend 暴露明确 capability summary，避免把 LocalBackend 误宣传成 sandbox。
- 先迁移 non-interactive `bash`，不改变现有默认执行行为。
- 让后续 sandbox backend 能复用同一 request / receipt 边界。

## 3. Non-goals

- 本 RFC 不一次实现所有平台 sandbox。
- 本切片不迁移 persistent terminal / PTY。
- 本切片不承诺 MCP、插件或远端工具受本地 shell sandbox 保护。
- 本切片不新增普通用户可见操作面。

## 4. Core Types

Kernel 暴露：

```rust
pub trait ExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind;
    fn capabilities(&self) -> ExecutionBackendCapabilities;
    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_>;
}
```

`ExecutionBackendCapabilities` 必须描述 backend 实际能强制的隔离能力：

- `filesystem_isolation`
- `network_isolation`
- `process_isolation`
- `resource_limits`
- `persistent_pty`
- `workspace_snapshot`

`LocalBackend` 必须全部声明为 `false`。它只是兼容当前本地进程执行路径，不是 sandbox。

## 5. Implementation Progress

- 已新增 kernel-level `ExecutionBackend`、`ExecutionRequest`、`ExecutionReceipt`、`ExecutionBackendKind` 和 `ExecutionBackendCapabilities`。
- 已新增 kernel-level `ExecutionConfig` 与 `ExecutionIsolationPolicy`：
  - 默认 `backend = "local"`。
  - 默认 `isolation = "allow_local"`。
  - 显式 `isolation = "require_sandbox"` 要求 backend 提供 filesystem 和 process isolation。
- 已新增 `backend = "macos_seatbelt"`，在 macOS 上通过 `/usr/bin/sandbox-exec` 执行非交互命令。
  - profile 允许全文件系统读取。
  - profile 只允许写入命令 working directory。
  - backend 不声明 `network_isolation`；因此 `Sandboxed` verification policy 不会把该 backend 当成已证明强制禁网。
  - `sandbox-exec` 缺失或非 macOS 平台会 fail closed。
  - Apple 已将 `sandbox-exec` 标记为 deprecated，因此该 backend 是 enforcement MVP，不是最终跨平台 sandbox 策略。
- 已新增 `sigil-tools-builtin` 的 `LocalExecutionBackend`。
- 已将 non-interactive `bash` tool 迁移到 `ExecutionBackend::execute`。
- 已将 runtime 的 local tool registry 构建接入 `RootConfig.execution`。
- 已将 verification check runner 迁移到 `ExecutionBackend::execute`，并新增毫秒级 timeout 支持，避免 RFC-0003 policy timeout 在 backend 边界丢精度。
- 已将 `/task` orchestrator 接入 runtime 配置的 execution backend；自动或手动 `RunCheck` action 不再绕过 backend。
- 已增加 fail-closed policy：当配置要求 sandbox 或选择需要 sandbox 的 profile preset 时，`LocalBackend` 会拒绝构建工具 registry，而不是静默继续裸跑。
- 已新增 coarse sandbox profile presets：`unconfined`、`workspace_write`、`build_offline`、`build_networked`。
  - `workspace_write` 和 `build_*` 要求 filesystem + process isolation。
  - `build_offline` 额外要求 network isolation；macOS Seatbelt backend 当前不能满足该 profile。
  - `build_*` 标记 dependency caches should be mounted read-only，具体 mount enforcement 留给 backend implementation。
- 已新增 execution coverage labels：
  - `shell` 工具标记为 `local_backend_enforced`，由配置的 local execution backend 控制。
  - MCP 工具标记为 `external_mcp_server`，运行在 MCP server 自身边界中；local shell sandbox 不覆盖 MCP server。
  - 插件声明的 agent、skill 和 hook 能力标记为 `plugin_managed`，由插件 trust 决策治理；local shell sandbox 不覆盖插件代码。
  - 远端执行可标记为 `remote_service`，不受本地 shell sandbox 覆盖。
  - file/search/agent 等 kernel-mediated 工具不会复用 shell sandbox 文案。
- 已新增 persistent terminal execution metadata：
  - `TerminalTaskHandle` 可记录 `execution_backend` 和 `execution_backend_capabilities`。
  - local process terminal 标记为 `local_process`，支持 cancel/output log，不支持 persistent PTY input/resize。
  - local PTY terminal 标记为 `local_pty`，支持 persistent PTY、input、resize、cancel 和 output log。
  - terminal tool result details 会带出这些字段，`TerminalTaskEntry::from_tool_result_details` 可从 metadata 重建；旧日志缺字段时保持兼容。
- 已补测试确认 `LocalExecutionBackend` 可以执行命令，并且不会声明 filesystem/network/process isolation。
- 已完成 E05.6 capability truthfulness 修正：macOS Seatbelt backend 不再声明 `network_isolation`，`build_offline` 会拒绝该 backend，sandbox conformance tests 不再把 loopback `nc` 行为当成网络隔离证明。
- 已保留 `bash` 的 timeout、stdout/stderr metadata、exit-code error 和 scratch env 行为。

## 6. Productization Remains

- 增加 Linux / Windows / container backend，并明确各平台 capability 差异。
- 扩展 sandbox conformance tests 到 Linux / Windows / container backend 和后续 backend-specific profile enforcement。
- 为 persistent terminal 接入真正 OS sandbox backend。当前已记录 local process / local PTY backend metadata，但仍不表示 PTY 进程已受 Seatbelt/Bubblewrap/container 强制隔离。
- 将 execution coverage labels 接入更完整的 TUI/runtime detail views；当前已提供 kernel/plugin summary API 和测试覆盖。

## 7. Validation

已运行：

```bash
cargo test -p sigil-tools-builtin local_execution_backend_runs_command_without_sandbox_claims
cargo test -p sigil-tools-builtin local_execution_backend_policy_fails_closed_when_sandbox_required
cargo test -p sigil-runtime build_tool_registry_fails_closed_when_sandbox_is_required
cargo test -p sigil-tools-builtin bash_tool_
cargo test -p sigil-kernel verification_check_runner
cargo test -p sigil-kernel task_orchestrator
./scripts/check-touched.sh --tier standard
cargo test -p sigil-kernel root_config_loads_macos_seatbelt_execution_backend
cargo test -p sigil-tools-builtin macos_seatbelt
cargo test -p sigil-runtime build_tool_registry_accepts_macos_seatbelt_when_sandbox_is_required
cargo test -p sigil-tools-builtin sandbox_conformance
cargo test -p sigil-kernel execution_config
cargo test -p sigil-runtime build_tool_registry_fails_closed_when_profile_requires_sandbox
cargo test -p sigil-kernel execution_coverage
cargo test -p sigil-kernel plugin_capabilities_report_execution_coverage_boundaries
cargo test -p sigil-runtime mcp
cargo test -p sigil-tui config
cargo test -p sigil-tools-builtin terminal_process
cargo test -p sigil-kernel terminal
cargo test -p sigil-tui terminal
```

注意：`macos_seatbelt` 只证明 macOS non-interactive command backend 的最小 enforcement。完整跨平台 sandbox、persistent terminal sandbox 和 MCP/plugin 进程隔离仍属于后续切片。E05.3 的 terminal metadata 只让 durable state 清楚记录 local process / local PTY 能力边界，不提供额外隔离。E05.4 的 coverage label 只说明哪些边界不受 local shell sandbox 覆盖，不提供额外隔离。
