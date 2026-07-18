# RFC-0041 Windows Restricted Execution Backend V1

状态：closed / R41.2 stop；未进入 R41.3-R41.5，Windows 仍为 truthful local unconfined

创建日期：2026-07-18

基线：

- Execution backend contract: [RFC-0005](0005-execution-backend.md)
- Windows native process semantics: [RFC-0039](0039-windows-terminal-process-semantics-v1.md)
- Shared process-tree owner: [`sigil-process`](../../../crates/sigil-process)
- Built-in backend implementations: [`execution_backends`](../../../crates/sigil-tools-builtin/src/execution_backends)

## 1. Summary

RFC-0037-RFC-0040 已建立稳定的 hosted Windows CI、PowerShell/cmd 语义、bounded output、
Job Object process-tree ownership 和 native credential-store conformance，E05.10 原先缺少真实 Windows
执行环境的 gate 已满足。但当前 Windows shell 仍是 `local unconfined`：Job Object 只能管理进程树，
不能阻止命令写入 workspace 之外、读取用户凭据或访问网络。

本 RFC 为 non-interactive shell 建立 Windows restricted execution backend 的证据驱动路线。实现必须先
证明受限 token、显式 handle inheritance、suspended launch、Job Object assignment 和 filesystem
boundary，再允许 `ExecutionBackendKind::WindowsRestricted` 进入公开配置。Restricted Token、Low
Integrity 和 Job Object 不能单独被宣传为完整 sandbox；任何没有真实 Windows conformance 的 capability
保持 `false`，选择不满足 profile 的 backend 必须 fail closed。

R41.0 只冻结边界、实现顺序和停止条件，不提前新增可配置但不可用的 enum，也不修改用户默认行为。

## 2. Research basis

- Microsoft [CreateRestrictedToken](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken)
  说明 restricted token 可禁用 SID、删除 privilege、增加 restricting SID；存在 restricting SID 时，
  object access 必须同时通过普通 SID 与 restricting SID 两次检查。子 token 不能通过再次 restriction
  恢复已经移除的权限。
- Microsoft [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw)
  说明 alternate primary token、environment、desktop 和 executable resolution 的调用约束。实现必须传
  exact application path，不能依赖 `lpApplicationName = NULL` 的歧义搜索。
- Microsoft [Mandatory Integrity Control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)
  说明 Low Integrity 默认不能写入 Medium Integrity object，但它不是 workspace allowlist；若把整个
  workspace 临时降为 Low，其他 Low Integrity process 也会获得新的写入机会。
- Microsoft [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
  与 [UI restrictions](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_ui_restrictions)
  提供进程树、资源与部分 UI 治理，但 Windows Vista 之后 security limit 必须施加到具体 process，
  Job Object 本身不是 filesystem 或 network sandbox。
- Microsoft [process handle inheritance](https://learn.microsoft.com/en-us/windows/win32/procthread/inheritance)
  与 [PROC_THREAD_ATTRIBUTE_HANDLE_LIST](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)
  支持只继承明确列出的 stdio handle。backend 不得把父进程其他 inheritable handle 暴露给受限 child。
- Microsoft [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation)
  提供 file、network、process、window 与 credential boundary，但需要 profile、capability 和对象 ACL
  grant；它不是把普通 developer command 放进去就能透明运行的零成本 wrapper。

本地竞品代码复核得到两类实现：Gemini CLI 的 Windows helper 使用 Restricted Token、Low Integrity、
Job Object 和 suspended launch；Reasonix 对 read-only command 使用 AppContainer，对 writable command
使用 Low Integrity token、临时 ACL/label、per-root serialization 和 crash residue cleanup。前者说明
token/launch 可以保持较小，后者说明 writable workspace containment 的主要复杂度是 ACL/label mutation、
并发与 crash recovery，而不是 P/Invoke 数量。Sigil 不采用运行时编译 C# helper，也不复制未经自身
conformance 证明的网络或 filesystem claim。

## 3. Goals

1. 为 non-interactive shell 建立 native Rust Windows launch path：restricted primary token、明确
   privilege removal、exact executable、Unicode argv/environment、allowlisted stdio handles、
   `CREATE_SUSPENDED`、Job Object assignment-before-resume 和 bounded wait/cleanup。
2. 证明 `workspace_write` 的 filesystem boundary：workspace 内允许预期写入，workspace 外同用户可写
   路径被拒绝，且 symlink/junction/reparse-point、UNC、alternate data stream 和 path race 不绕过边界。
3. 复用 RFC-0005 capability/receipt/fallback contract；没有证据的 filesystem、network、process、
   resource、PTY capability 不得置为 `true`。
4. 保持 `sigil-process` 的窄职责。它可以继续提供通用 Job Object lifecycle owner，但 restricted token、
   ACL/AppContainer policy、shell argv、stdio capture 和 sandbox receipt 留在 execution backend 所有者。
5. 只在 Windows hosted conformance 通过后公开 backend、Doctor/TUI 状态和 EN/ZH 配置文档。

## 4. Non-goals

- 不在 R41.0-R41.3 支持 persistent terminal/ConPTY、MCP stdio、plugin hook、remote tool 或 desktop process。
- 不声明 Restricted Token 或 Job Object 单独等于 filesystem/network sandbox。
- 不通过 `taskkill`、PowerShell wrapper、`icacls` PATH lookup 或运行时编译/下载 helper 建立安全边界。
- 不实现 Windows Sandbox、Hyper-V VM、WSL、kernel driver、WFP/firewall rule 或 enterprise policy 管理。
- V1 不声明 network isolation。`build_offline` 必须 fail closed；network receipt 保持 `unsupported`，除非
  后续独立设计和 loopback/LAN/public-destination conformance 同时证明拒绝。
- V1 不支持 sandboxed PTY，`persistent_pty = false`。
- 不为了让测试变绿而递归降低整个 workspace 的 integrity label；这会扩大其他 Low Integrity process
  的写入面。若最终实现需要 label/ACL mutation，必须先满足第 7 节的 crash-consistency gate。
- 不改变默认 `execution.strategy = "local"`，不新增版本、tag 或 release。

## 5. Frozen security contract

### 5.1 Threat model

backend 假设宿主 Windows kernel 与 Sigil parent process 可信，受限 child、其 descendants、命令输入和
workspace 内容不可信。V1 必须防止 child 借 ambient privilege、inherited handle、未受管 descendant 或
普通路径写入影响 workspace 之外的用户状态。V1 不保护同一用户下已经控制 Sigil parent process 的攻击者，
也不把读取所有 host 文件、网络阻断或 kernel exploit 防护写成已实现能力。

### 5.2 Launch invariants

一次受限 launch 必须原子满足以下条件，否则在 resume 前终止 child 并返回 typed unavailable/error：

1. 先 canonicalize workspace/cwd 和 exact executable；拒绝空 program、模糊 application name、设备路径、
   不支持的 reparse point 和 workspace escape。
2. 从当前 primary token 创建 restricted primary token；至少使用 `DISABLE_MAX_PRIVILEGE`，最终
   restricting SID/group 组合由 R41.2 的 hosted probe 冻结，不能只靠删除 privilege 宣称 filesystem
   confinement。
3. 使用显式 Unicode environment block。`ExecutionRequest.environment_policy` 仍是唯一环境继承来源；
   backend 不偷偷增加父环境，也不删除用户 shell 已明确请求的变量。
4. 只 duplicate stdin/stdout/stderr 所需 child-side handle，并把这些临时 duplicate 标为 inheritable；
   `STARTF_USESTDHANDLES` 的三个 handle 必须与 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 完全一致，
   `CreateProcessAsUserW` 必须使用 `bInheritHandles = TRUE`。spawn 返回后 parent 立即关闭临时 inheritable
   duplicate；额外 sentinel inheritable handle 必须有负向测试，不能以宽泛 inheritance 修复 launch。
5. 通过 `CreateProcessAsUserW` 使用 exact `lpApplicationName`、mutable quoted command line、
   `CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW`。
6. 在 initial thread resume 前完成 Job Object assignment；assignment、limit setup 或 owner registration
   失败时直接终止并 reap child。
7. output 继续使用现有 per-stream/combined hard cap、timeout/cancel arbitration 和唯一 termination cause；
   Windows backend 不维护一套较弱的第二份 output supervisor。
8. parent drop、timeout、cancel、reader failure 和 output limit 都必须终止整棵 owned tree、reap direct
   child 并给出 truthful cleanup receipt。

### 5.3 Capability claims

V1 公开时的最大 claim 上限为：

| Capability | Initial claim | Promotion gate |
| --- | --- | --- |
| `filesystem_isolation` | `false` until R41.2 passes | workspace write succeeds; sibling/user-state writes and reparse escapes fail; no unsafe residue |
| `network_isolation` | `false` | separate future RFC plus real network conformance |
| `process_isolation` | `false` until R41.1/R41.2 pass | restricted token, handle canary, suspended Job assignment and cross-process/UI negative tests pass |
| `resource_limits` | `false` | requested Job limits are applied and receipts match observed enforcement |
| `persistent_pty` | `false` | separate future productization slice |
| `workspace_snapshot` | `false` | RFC-0003 remains the snapshot owner |

`WindowsRestricted` 只有在 `filesystem_isolation && process_isolation` 同时有 hosted proof 后才可被
`execution.strategy = "sandbox"` 选择。只完成 token launch 时它仍是内部 probe，不是公开 backend。
这里的 `process_isolation` 沿用 RFC-0005 当前 contract：child 在 resume 前进入 owned process tree，不能
产生未受管 descendant，且 termination/cleanup 可证明；它不表示 AppContainer 级 kernel object、named
pipe、COM、credential 或 host-process confidentiality。Doctor/TUI 不得把该 bool 扩写成“与所有宿主进程
完全隔离”。若未来要声明更强 process boundary，必须另加相应 object/IPC conformance。

### 5.4 Supported profile

V1 只尝试 `workspace_write`：允许 workspace/cwd 内写入，workspace 外写入被 OS access check 拒绝；
读取和网络按 receipt 真实报告，不能从“未要求 network capability”推导为“已禁网”。`build_offline`
必须因缺少 network isolation 拒绝；`build_networked` 在 dependency cache read-only grant 未证明前也拒绝。

## 6. Filesystem containment decision gate

R41.2 必须在 hosted Windows 上按以下顺序做可抛弃 probe，不能先写公开 config：

1. 优先验证 **medium-integrity restricted token + restricting SID**。目标是让第二次 access check 只对
   workspace/scratch、required system/runtime read roots 和 exact executable roots放行，避免递归降低
   workspace integrity。SID 组合、ACE scope、inheritance、junction 和 toolchain compatibility 必须由
   测试证明，不能凭 API 文档假设。
2. 若 developer tool compatibility 无法通过，再验证 **AppContainer/LPAC + explicit capability/ACL grant**。
   该路径必须记录 profile lifecycle、capabilities 和 exact grant roots；不能给整个 user profile broad
   write access。
3. **Low Integrity + recursive workspace label mutation** 不进入 V1 默认实现。只有单独证明 journal-before-
   mutation、per-root cross-process lease、exact ACL/SACL snapshot、bounded restore、abandoned-owner recovery、
   concurrent-run serialization 和 residue sweep 后，才能重新评估。

任何候选 grant 必须使用 directory/file 分离的最小 access mask 与明确 inheritance flag；不得授予
`WRITE_DAC`、`WRITE_OWNER`、SACL mutation、`GENERIC_ALL` 或允许 child 改写 grant 本身的权限。R41.2
必须证明 child 不能改变 owner/DACL/SACL，并覆盖 NTFS hard link 与 file-ID alias；hard link 不是
reparse point，不能由 junction/symlink fixture 代替。

若前两种路径都不能在不扩大 host 写入面且不破坏常用 PowerShell/Rust/Git command 的前提下通过，
R41.2 的正确结果是记录 `unsupported` 并停止 R41.3，不以弱化 capability 或静默 local fallback 完成 RFC。

## 7. ACL/profile mutation rules

任何 grant 或 profile creation 都是安全状态变更，不是临时实现细节：

- mutation 前必须记录 owner、workspace identity、原始 security descriptor/hash、目标 SID/grant 和恢复阶段；
- 同一 canonical root 的不同 process 必须使用 OS-backed exclusive lease，不能只用进程内 mutex；
- child 只有在 grant durable、重新读取验证且 Job owner ready 后才能 resume；
- cleanup 先终止/reap child tree，再恢复 ACL/profile；恢复无法证明时 receipt 为 failed/unknown，并保留
  可定位 recovery record；
- crash recovery 只能清理 Sigil 自己带 exact owner marker 的 ACE/profile，不能执行 broad `reset`、
  `/remove` 或把用户自定义 ACL 归一化；
- unsupported filesystem、FAT/exFAT、network share policy、ACL inheritance conflict 或 enterprise policy
  必须 fail closed；
- 若采用 durable per-workspace grant 而非临时 grant，必须在公开文档和 Doctor 中明确显示并提供显式
  remove/revalidate action；它不能伪装成无副作用的运行时细节。

## 8. Ownership and implementation boundary

- `sigil-kernel` 只在公开 gate 满足后增加 provider-neutral `ExecutionBackendKind`/capability/receipt wire；
  不出现 Win32 handle、SID、token 或 AppContainer 类型。
- `sigil-tools-builtin::execution_backends::windows_restricted` 持有 Win32 launch、filesystem policy adapter、
  backend selection、receipt 和 non-interactive conformance glue。
- R41.1 暴露一个 crate-private、跨平台 probe façade：Windows 实现调用 native launcher，其他平台返回
  structured unavailable。这样可以在不增加 public enum/config 的情况下证明 non-Windows fail-closed。
- `sigil-process` 继续只持有通用 process-tree lifecycle。若现有 `ProcessTreeOwnerGuard` 无法安全接管
  suspended child，只能增加 provider/tool-neutral 的 assign-before-resume primitive，不能下沉 ACL、
  token、environment、stdio 或 sandbox capability。
- runtime/Doctor/TUI 只消费 typed selection diagnostic 和 receipt；不自己探测 SID、解析 Win32 error
  text 或把 backend unavailable 自动降级为 local。
- 公共配置、EN/ZH docs 与 site 在 R41.3 hosted proof 前保持不变。

## 9. Implementation slices

1. **R41.0 RFC and preflight**：官方 API、现有 contract、竞品代码、threat model、capability gate、停止条件
   与 execution ledger；不新增产品代码。
2. **R41.1 Native restricted launch foundation**：私有 Windows-only launcher、exact argv/env、restricted
   token、handle allowlist、suspended Job assignment、bounded output/cleanup；capabilities 仍不公开。
3. **R41.2 Filesystem containment proof**：按第 6 节顺序验证 restricting SID 与 AppContainer 路线；
   workspace/sibling/reparse/toolchain/concurrency/crash evidence 决定继续或 truthful stop。
4. **R41.3 Backend selection and receipts**：只有 R41.2 成功时才增加 `WindowsRestricted`、`workspace_write`
   selection、non-Windows fail-closed path、receipt binding 和 fallback tests。
5. **R41.4 TUI-first capability surface**：Doctor、`/config`、tool card、EN/ZH permissions/configuration/site
   显示 exact capability、unsupported network/PTY 和 recovery action；默认仍 local。
6. **R41.5 Hosted conformance and completion audit**：Windows real command matrix、affected workspace gate、
   supply-chain/docs/site、security/code-quality/complete-implementation review；无剩余 P1/P2 才关闭。

## 10. Acceptance matrix

R41.1 至少证明：

- child token 真实 restricted，未持有被删除 privilege；失败发生在 resume 前；
- exact executable/argv 支持空格、Unicode、PowerShell/cmd native exit code；
- stdin/stdout/stderr 工作，额外 inheritable sentinel handle 不可见；
- timeout、cancel、output limit、reader failure 和 parent drop 清理 descendants；
- assignment-before-resume 竞态有测试或 native invariant proof；
- 非 Windows build 路径只返回 typed unavailable，不伪造行为。

R41.2/R41.3 至少证明：

- workspace 内 create/modify/delete 成功，canonical sibling 和用户 temp/home 状态写入失败；
- junction/symlink/reparse point、hard link/file-ID alias、UNC、ADS、case-folding、long path 和 TOCTOU
  fixture 不绕过 boundary；
- child 不能取得 `WRITE_DAC`/`WRITE_OWNER`/SACL mutation 或改变 owner/DACL/SACL/grant 本身；
- PowerShell、Git 和 Rust small build 至少各有一个 representative command；
- concurrent same-root/disjoint-root、forced parent crash 和 cleanup retry 不留下不可解释 grant/residue；
- `build_offline`、persistent PTY、MCP/plugin process 和 non-Windows selection fail closed；
- receipt 的 backend、capabilities、network、resources、cleanup 与实际执行一致。

R41.5 的 hosted Windows proof 是完成条件，本机 cross-compile、mock Win32 API 或只检查 token 字段都不能
替代真实 access-denied、process-tree 和 residue conformance。

## 11. Validation plan

```bash
cargo test -p sigil-kernel execution_backend
cargo test -p sigil-process
cargo test -p sigil-tools-builtin windows_restricted
cargo clippy -p sigil-kernel -p sigil-process -p sigil-tools-builtin --all-targets -- -D warnings
cargo fmt --all --check
./scripts/check-docs.sh
./scripts/check-pages-site.sh
cargo deny check
cargo audit --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2024-0436
git diff --check
```

真实 Windows suite 必须在 hosted runner 上执行 ignored/native conformance，并将 access result、token
capability、cleanup 和 residue检查输出为不包含 secret/path 内容的结构化摘要。任何平台 claim 只在对应
run 绿后写入 RFC progress/STATUS。

## 12. R41.0 result

当前代码 inventory 确认：`ExecutionBackendKind` 只有 Local/Seatbelt/Bubblewrap/Docker；Windows shell
复用 `sigil-process` Job Object 但 receipt 仍 truthful `local unconfined`；现有 `workspace_write` 要求
filesystem + process isolation，因此单独增加 Restricted Token backend 会在 capability validation 前后都
形成误导。

R41.0 决定不直接复制 Gemini 的 Low Integrity helper，也不直接复制 Reasonix 的递归 ACL/label mutation。
下一步只打开 R41.1 private native launcher；R41.2 filesystem proof 仍是公开 backend 的硬 gate。若该 gate
失败，保留 Windows local execution 与明确 unsupported 状态比发布弱 sandbox 更正确。

## 13. R41.1-R41.2 result and stop decision

R41.1 在 hosted Windows 上证明了 private native process primitive：exact executable/argv/environment、
restricted primary token、allowlisted inherited handles、suspended launch、Job assignment-before-resume、
bounded output，以及 timeout/cancel/reader failure/parent-drop descendant cleanup。该证据只支持进程生命周期
与 privilege 收缩，不支持 filesystem 或 network sandbox claim。

R41.2 依次验证了两个候选，并均触发第 6 节的停止条件：

1. `WRITE_RESTRICTED` 只有在 restricting set 同时包含 workspace SID、logon SID 和 Everyone 时才能稳定
   初始化 Rust/Windows runtime；该兼容集合可写入显式授予 Everyone 的 workspace 外路径，因此不能作为
   filesystem isolation。
2. classic AppContainer 的 profile-first launch、显式删除、跨进程 lease 和 abandoned-owner recovery 均
   通过。package SID 也确实取得了 workspace 根与既有文件的最小 DACL 写权限；但 hosted child token 的
   integrity RID 为 Low，真实 create/modify/delete 仍在普通 Medium workspace 上被拒绝。依据 Microsoft
   MIC contract，DACL 不会绕过 mandatory no-write-up。让该候选可写需要递归降低 workspace integrity 或
   等价的 SACL mutation，而这条路线已被第 6-7 节排除，且此前 ACL 恢复探针已证明递归恢复会归一化用户
   子对象 descriptor。

因此 `workspace_write` 的 inside-write gate 未通过，后续 reparse/hard-link/toolchain 矩阵没有安全前提；
R41.3-R41.5 不进入实施。仓库不增加 `WindowsRestricted` public enum/config、capability、Doctor/TUI surface，
也不修改用户默认行为。Windows shell 继续明确报告 `local unconfined`。

2026-06 出现的 Microsoft
[`Experimental_CreateProcessInSandbox`/Bound File System API](https://learn.microsoft.com/en-us/windows/win32/secauthz/createprocessinsandbox)
不用于本 RFC 的补救：官方仍将其标记为 experimental，header 尚未公开、仅支持 Windows 11，并且当前 contract 要求
`inheritHandles = FALSE`，无法直接满足 Sigil 已证明的 exact stdio handle allowlist。若 API 稳定且能证明
stdio、Job nesting、filesystem alias 与最低系统版本，应通过新的 RFC 重新预检，而不是在本 RFC 中静默
替换 backend。
