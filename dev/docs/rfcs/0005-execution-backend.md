# RFC-0005 Execution Backend

状态：draft / E05.1-E05.7 implemented / E05.8 backend code implemented, Linux conformance gated by runner namespace support / E05.9 implemented with real Docker conformance / E05.16 minimal doctor implemented / productization remains

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

Persistent terminal 切片不把 PTY 伪装成 non-interactive bash。它先把 terminal task 的 backend kind/capability 写入 durable handle 和 tool metadata，让 projection、恢复和 UI 能区分 local process 与 local PTY 边界。E05.13 的前置 lifecycle metadata 已让 terminal handle 额外记录 enforcement backend、sandbox profile、backend capability summary 和 cleanup receipt；TUI terminal card 会显示 `local unconfined` / cleanup fact，避免把当前 local PTY 宣传成 sandboxed PTY。

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
- 已新增 execution network receipt：每次执行记录 `allowed` / `denied` / `unsupported` / `unknown`。Local 记录 `unknown`；macOS Seatbelt 当前记录 `unsupported`；Docker offline profile 通过 `--network none` 记录 `denied`，networked profile 记录 `allowed`。verification binding 和 `sandbox_profile_hash` 已纳入该 receipt，bash tool card 状态细节显示 network policy，避免只凭 capability bool 信任网络隔离。
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
  - `TerminalTaskHandle` 可记录 `enforcement_backend`、`enforcement_backend_capabilities` 和 `sandbox_profile`。
  - `TerminalTaskEntry` 可记录 terminal cleanup receipt；running/starting 不声明 cleanup，exited 记录 `not_needed`，cancelled 记录 best-effort completed，interrupted/unknown path 记录 unknown。
  - local process terminal 标记为 `local_process`，支持 cancel/output log，不支持 persistent PTY input/resize。
  - local PTY terminal 标记为 `local_pty`，支持 persistent PTY、input、resize、cancel 和 output log。
  - terminal tool result details 和 session restore payload 会带出这些字段，`TerminalTaskEntry::from_tool_result_details` 可从 metadata 重建；旧日志缺字段时保持兼容。
  - TUI terminal card 已展示 terminal enforcement boundary 和 cleanup status；当前 local process/local PTY 明确显示为 `local unconfined`，不宣传为 sandbox。
- 已补测试确认 `LocalExecutionBackend` 可以执行命令，并且不会声明 filesystem/network/process isolation。
- 已完成 E05.6 capability truthfulness 修正：macOS Seatbelt backend 不再声明 `network_isolation`，`build_offline` 会拒绝该 backend，sandbox conformance tests 不再把 loopback `nc` 行为当成网络隔离证明。
- 已完成 E05.7 capability matrix / selection contract：
  - 新增 `ExecutionCapability`、`ExecutionCapabilityRequirements`、`ExecutionSandboxFallback` 和 `ExecutionBackendSelectionDiagnostic`。
  - profile validation 不再只依赖 `supports_required_sandbox()`；`build_offline` 等 profile 会声明独立 filesystem / process / network requirement。
  - backend selection failure 能携带 requested backend、profile、missing capabilities、availability reason 和 fallback decision。
  - `fallback = "deny"` 默认 fail closed；`fallback = "prompt"` 在非交互 builder 中仍 fail closed；只有显式 `fallback = "unconfined"` 才可降级到 local。
- 已实现 E05.9 Docker backend MVP：
  - 新增 `backend = "docker"`。
  - Docker backend 必须显式配置 `[execution].container_image`，不会隐式选择或拉取镜像。
  - backend selection 会先检查 Docker daemon 和 configured image；missing daemon / missing image 会 fail closed。
  - command 通过 `docker run --rm --workdir <cwd> --mount type=bind,src=<cwd>,dst=<cwd>` 执行。
  - `network_allowed = false` 的 profile 会添加 `--network none`。
  - Unix 平台会传递当前 `uid:gid`，降低 root-owned workspace artifact 风险。
  - 该 backend 不覆盖 persistent terminal、MCP stdio server、plugin hook process 或 remote tool。
  - 已新增 ignored real-Docker conformance test，可通过 `SIGIL_DOCKER_CONFORMANCE_IMAGE=<local-image>` 显式运行。
  - 本机真实 conformance 已使用 `redis:8-alpine` 验证 daemon/image selection、workspace bind mount、offline network blocking 和 uid/gid ownership。
- 已实现 E05.16 minimal doctor 展示：
  - `doctor` 输出 `execution:sandbox` 行，显示 backend、profile、fallback 和 capability summary。
  - sandbox backend 缺少配置或依赖时给出 remediation。
  - 显式 `fallback = "unconfined"` 时展示 warn，不把降级后的 local execution 宣传为 sandbox。
- 已实现 E05.12 resource / cleanup core semantics：
  - `ExecutionRequest` 支持 CPU time、memory、process count 请求字段；unsupported limits 不会被写成 applied。
  - `ExecutionReceipt.resources` 记录 applied limits、unsupported limits、timeout source 和 cleanup result。
  - Local non-interactive backend timeout path 使用 process group 做 best-effort cleanup，并把结果写入 receipt。
  - verification durable check payload 记录 `execution_resources`，TUI bash tool card 显示 timeout/cleanup 简短事实。
- 已实现 E05.8 Linux Bubblewrap backend code path：
  - 新增 `backend = "linux_bubblewrap"`。
  - backend selection 在非 Linux、缺 `bwrap` 或 namespace smoke check 失败时 fail closed。
  - non-interactive command path 使用 bwrap 构造 read-only host root、writable workspace/cwd、writable `$SIGIL_SCRATCH_DIR`、tmpfs `/tmp`、PID namespace、die-with-parent 和 offline `--unshare-net`。
  - 已有 Linux-only ignored conformance test 和手动 `Sandbox Conformance` GitHub Actions workflow；2026-06-29 首次 GitHub Ubuntu runner diagnostics 在 `bwrap --unshare-net` 阶段失败（`loopback: Failed RTM_NEWADDR: Operation not permitted`），因此 E05.8 仍不能标记 done，需要兼容的 Linux host/runner 证明 namespace/network conformance。
  - 手动 workflow 已调整为 preflight/report 模式：默认把不兼容 hosted runner 记录为 `unsupported runner` 并跳过 conformance，不把它伪装成 conformance success；手动输入 `require_conformance=true` 时仍会 fail closed。
- 已保留 `bash` 的 timeout、stdout/stderr metadata、exit-code error 和 scratch env 行为。

## 6. Productization Remains

- 增加 Linux / Windows / container backend，并明确各平台 capability 差异。
- 扩展 sandbox conformance tests 到 Linux / Windows / container backend 和后续 backend-specific profile enforcement。
- 为 persistent terminal 接入真正 OS sandbox backend。当前已记录 local process / local PTY backend metadata，但仍不表示 PTY 进程已受 Seatbelt/Bubblewrap/container 强制隔离。
- 将 execution coverage labels 接入更完整的 TUI/runtime detail views；当前已提供 kernel/plugin summary API 和测试覆盖。

2026-06-29 productization slice expansion:

- E05.7 Sandbox Capability Matrix and Backend Selection Contract：已实现 backend selection、fallback、diagnostics 和 profile requirement taxonomy。该切片是后续完整 OS Sandbox 的直接入口。
- E05.8 Linux Bubblewrap Backend MVP：backend code 已实现，要求真实 Linux+bwrap conformance 后才能从 gated 转 done。
- E05.9 Container Backend MVP：已实现 Docker non-interactive backend code path、fake-Docker 参数构造测试和显式 real-Docker conformance test；本机已用 `redis:8-alpine` 完成真实 daemon/mount/network/ownership 验证。
- E05.10 Windows Restricted Backend Spike：Windows restricted token / job object / cleanup 能力验证，必须有 Windows 环境。
- E05.11 Network Policy Enforcement and Receipt：已完成 network allowed/denied/unsupported/unknown receipt、verification binding/hash 集成和 bash metadata 展示；macOS Seatbelt 仍不宣传网络隔离。
- E05.12 Resource Limits and Process Cleanup：已完成 core semantics 和 Local non-interactive cleanup path；container/bwrap/Windows/PTY 等 backend-specific cleanup 继续由后续切片落地。
- E05.13 Persistent Terminal Sandbox Backend：pre-lifecycle metadata contract 已实现；完整 PTY/long-lived process sandbox lifecycle 仍 gated，等待 backend 支持 persistent PTY 或 container exec lifecycle。
- E05.14 MCP Stdio Sandbox Handoff：本地 stdio MCP server 通过 execution backend 或明确标记 outside local sandbox。
- E05.15 Plugin Hook Process Sandbox Handoff：未来插件 hook command runtime 必须经过 execution backend 或显式 unconfined/unsupported。
- E05.16 Sandbox Product Surface and Doctor：已实现 minimal doctor 展示；TUI tool/approval card 的更完整 coverage surface 仍可后续扩展。

E05.8 已新增手动 `Sandbox Conformance` workflow，通过 GitHub Actions 的 Ubuntu runner 安装 `bubblewrap` 后运行 ignored Linux conformance test。当前 `linux_bubblewrap` backend code 和 workflow 均已存在；2026-06-29 首次 workflow 运行在 host namespace diagnostics 阶段失败，原因是 runner 不允许 bwrap 配置 loopback network namespace。workflow 现已改为 preflight/report 模式：默认记录 unsupported runner 并保留 E05.8 gated；需要硬门禁时用 `require_conformance=true` 运行。下一步需要在兼容 Linux host/runner 上运行 ignored test 并记录结果。E05.9 同时保留 fake-Docker request construction 测试和显式 real-Docker conformance 测试；后者需要健康 Docker daemon 与本机已有镜像。

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
cargo test -p sigil-kernel root_config_loads_docker_execution_backend
cargo test -p sigil-tools-builtin docker_backend
cargo test -p sigil-tools-builtin docker_backend_checks_daemon
cargo test -p sigil-tools-builtin backend_selection
cargo test -p sigil-tools-builtin docker_execution_backend_builds_offline_container_command
SIGIL_DOCKER_CONFORMANCE_IMAGE=redis:8-alpine cargo test -p sigil-tools-builtin tests::docker_execution_backend_real_daemon_conformance -- --ignored --exact --nocapture
cargo test -p sigil-runtime doctor
cargo test -p sigil-tools-builtin execution_backend_records_timeout_cleanup_and_unsupported_limits -- --nocapture
cargo test -p sigil-tools-builtin execution_backend_timeout_cleans_process_group_children -- --nocapture
cargo test -p sigil-tui tool_card_render_bash_and_diff_previews_cover_no_output_and_truncation -- --nocapture
cargo test -p sigil-tui tool_card_parse_helpers_cover_fallbacks_defaults_and_metadata_sources -- --nocapture
cargo test -p sigil-kernel root_config_loads_linux_bubblewrap_execution_backend -- --nocapture
cargo test -p sigil-tools-builtin linux_bubblewrap -- --nocapture
ruby -e "require 'yaml'; YAML.load_file('.github/workflows/sandbox-conformance.yml'); puts 'ok'"
```

注意：`macos_seatbelt` 只证明 macOS non-interactive command backend 的最小 enforcement。Docker backend 已通过本机真实 daemon conformance，但只覆盖 non-interactive command execution。完整跨平台 sandbox、persistent terminal sandbox 和 MCP/plugin 进程隔离仍属于后续切片。E05.3 的 terminal metadata 只让 durable state 清楚记录 local process / local PTY 能力边界，不提供额外隔离。E05.4 的 coverage label 只说明哪些边界不受 local shell sandbox 覆盖，不提供额外隔离。
