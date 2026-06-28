# RFC-0005 Execution Backend

状态：draft / slice 5 sandbox conformance tests implemented

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
  - profile 不开放 network access。
  - `sandbox-exec` 缺失或非 macOS 平台会 fail closed。
  - Apple 已将 `sandbox-exec` 标记为 deprecated，因此该 backend 是 enforcement MVP，不是最终跨平台 sandbox 策略。
- 已新增 `sigil-tools-builtin` 的 `LocalExecutionBackend`。
- 已将 non-interactive `bash` tool 迁移到 `ExecutionBackend::execute`。
- 已将 runtime 的 local tool registry 构建接入 `RootConfig.execution`。
- 已将 verification check runner 迁移到 `ExecutionBackend::execute`，并新增毫秒级 timeout 支持，避免 RFC-0003 policy timeout 在 backend 边界丢精度。
- 已将 `/task` orchestrator 接入 runtime 配置的 execution backend；自动或手动 `RunCheck` action 不再绕过 backend。
- 已增加 fail-closed policy：当配置要求 sandbox 时，`LocalBackend` 会拒绝构建工具 registry，而不是静默继续裸跑。
- 已补测试确认 `LocalExecutionBackend` 可以执行命令，并且不会声明 filesystem/network/process isolation。
- 已保留 `bash` 的 timeout、stdout/stderr metadata、exit-code error 和 scratch env 行为。

## 6. Productization Remains

- 根据首个 sandbox backend 增加更细的 profile presets，例如 `workspace_write`、`build_offline`、`build_networked`。
- 增加 Linux / Windows / container backend，并明确各平台 capability 差异。
- 扩展 sandbox conformance tests 到 Linux / Windows / container backend 和后续 profile presets。
- 迁移 persistent terminal 时必须单独处理 PTY、长进程、resize、kill 和恢复语义。
- MCP、插件和远端工具必须明确标注是否受本地 backend 控制；不能复用 shell sandbox 文案。

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
```

注意：`macos_seatbelt` 只证明 macOS non-interactive command backend 的最小 enforcement。完整跨平台 sandbox、profile presets、persistent terminal sandbox 和 MCP/plugin 进程隔离仍属于后续切片。
