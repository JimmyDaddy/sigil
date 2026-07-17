# RFC-0039 Windows Terminal and Process Semantics V1

状态：complete / R39.0-R39.5 complete

创建日期：2026-07-17

基线：

- Terminal manager: [`terminal_process`](../../../crates/sigil-tools-builtin/src/terminal_process)
- One-shot shell tool: [`shell.rs`](../../../crates/sigil-tools-builtin/src/shell.rs)
- Execution supervisor: [`execution_backends`](../../../crates/sigil-tools-builtin/src/execution_backends)
- Predecessor: [RFC-0037 Cross-platform CI Reliability V1](0037-cross-platform-ci-reliability-v1.md)

## 1. Summary

RFC-0037 已证明完整 workspace 可以在 hosted Windows 上编译，但扩大
`sigil-tools-builtin` 测试范围后发现，Windows 文件工具已经可用，shell、terminal cwd、命令解释和
进程树清理仍混有 Unix 假设：默认执行 `sh -lc`、terminal path normalization 拒绝 Windows
prefix、PowerShell 命令仍可能经过 Bash 只读分类，取消依赖 `taskkill` 的外部命令返回码。

本 RFC 为 Windows 原生 local execution 冻结一套可验证语义：默认选择 PowerShell 7，缺失时
回退到 Windows PowerShell 5.1；显式 `cmd.exe` 使用独立参数模型；非 POSIX shell 不复用 Bash
AST 只读降级；Windows 子进程由 Job Object 持有并在取消/超时后给出结构化 cleanup receipt。
这些能力仍是 unconfined local execution，不是 restricted sandbox。

## 2. Research basis

- Microsoft 将 [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
  定义为进程组的统一管理边界；关联后的子进程默认继承 job，
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 可以在 owner 关闭时终止关联进程。
- [`TerminateJobObject`](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-terminatejobobject)
  终止 job 中的所有关联进程；[`AssignProcessToJobObject`](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject)
  需要显式进程句柄和失败处理。
- PowerShell 官方 CLI 使用 `-NoProfile -NonInteractive -Command` 表达非交互命令；
  [`cmd.exe`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd)
  使用 `/d /s /c`，不能复用 POSIX `-lc`。
- Rust [`CommandExt`](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html)
  明确说明 Windows process creation flag 与 `cmd.exe /c` quoting 是平台专属语义。
- 本地竞品代码复核显示 Gemini CLI、OpenAI Codex 和 Reasonix 都把 PowerShell 作为 Windows
  独立 shell dialect；Gemini CLI 和 Reasonix 优先 `pwsh` 并回退 `powershell.exe`，且不会把
  PowerShell 命令继续当成 Bash 解析。

## 3. Goals

1. Windows workspace/cwd 接受 drive prefix 与 UNC 形状，同时继续执行 canonical workspace
   confinement 和 symlink escape 拒绝。
2. 将 shell executable、dialect、argv prefix 和用户可见名称建模为一个不可混用的解析结果。
3. Windows 默认优先 `pwsh.exe`，回退 `powershell.exe`；允许显式 PowerShell、cmd 和 POSIX
   shell executable，未知 shell fail closed。
4. PowerShell/cmd 命令保持 `Execute`；只有 POSIX dialect 可以使用现有 Bash AST 与只读
   family 降级。
5. one-shot shell、non-PTY terminal 和 PTY terminal 使用同一个 shell resolver，并在 tool
   description、task handle、result metadata 和 Doctor 中展示实际 dialect/backend。
6. Windows local process owner 使用 Job Object；assignment、terminate、wait 或 output drain
   任一无法证明时，状态必须为 failed/interrupted，而不是 `Cancelled`。
7. hosted Windows CI 运行真实 PowerShell、cwd、non-zero exit、timeout/cancel、descendant cleanup
   和 receipt conformance 测试。

## 4. Non-goals

- 不实现 E05.10 Windows restricted backend、AppContainer、restricted token、ACL virtualization
  或网络隔离。
- 不声明 Job Object 是 sandbox；它只证明本地进程生命周期 ownership。
- 不实现任意 shell 的语法分析器；未知 executable 不猜测参数模型。
- 不承诺 Docker/container PTY、remote MCP OAuth、WSL 自动路由或物理 worktree。
- 不新增默认 TUI 开关、slash command、版本、tag 或 release。
- 不把 hosted runner 通过扩写成所有 Windows 版本、终端 emulator 或企业策略都兼容。

## 5. Shell resolution contract

`ResolvedShell` 至少包含：

- `program: PathBuf`；
- `dialect: Posix | PowerShell | Cmd`；
- `display_name`；
- non-interactive/PTY command argv；
- 是否允许 Bash AST 只读分类。

默认 resolver 在 builtin registry 构造时解析一次并由 shell tool 与 terminal manager 共同持有；
permission preview 与 execute 不重新探测 PATH，避免两阶段选择不同 executable。显式 shell 则由
同一个纯函数按 path basename 解析。

默认选择：

| Platform | Order | Command argv |
| --- | --- | --- |
| Unix | `sh` | `-lc <command>` for terminal, `-c <command>` for one-shot |
| Windows | `pwsh.exe`, then `powershell.exe` | `-NoLogo -NoProfile -NonInteractive -Command <command>` |

显式 `cmd` / `cmd.exe` 使用 `/d /s /c <command>`。显式 POSIX shell 只接受
`sh/bash/zsh/fish` family，继续使用 `-lc`。显式 PowerShell executable 只按 basename
`pwsh/powershell` 识别。其他 executable 返回带 shell path 的结构化错误。

PowerShell script wrapper 在用户命令前固定 UTF-8 stdout/stderr encoding，在命令后传播 native
`$LASTEXITCODE` 和 cmdlet failure；不能让失败的 `cargo test` 因 PowerShell 默认 exit semantics
变成成功。显式 cmd 会先切换 UTF-8 code page，并保留最后一条用户命令的 exit code。

工具名 `bash` 在 V1 保持不变，避免同时增加第二个等价 provider tool；但它的 description 与
result metadata 必须说明 Windows 上实际使用 PowerShell，模型不得被提示编写 Bash 语法。

## 6. Permission and safety contract

- POSIX shell 保留现有 conservative Bash parser、destructive detector 与只读 family。
- PowerShell/cmd 永远不进入 Bash AST、Bash fast path 或 workspace-read-only session grant；每次
  至少需要 `Execute` permission，并绑定 exact command subject。
- terminal 的显式 shell executable 继续成为 permission subject。
- shell resolver 必须在 permission preview 与 execute 两个阶段得到相同结果；不得 preview
  PowerShell、execute cmd，或 preview `sh`、execute PowerShell。
- local Windows receipt 必须显式报告 unconfined backend；Job Object ownership 不改变 network、
  filesystem 或 sandbox capability。

## 7. Windows process ownership contract

每个 Windows one-shot/terminal child 创建独立 Job Object，并设置
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。child spawn 后必须立即 assignment；assignment 失败时终止并
回收 direct child，且本次工具调用失败，不允许静默退回仅 direct-child kill。V1 的 one-shot、
non-PTY 和 PTY 都是在普通 spawn 返回后完成 assignment；它证明 assignment 成功后由 job 持有的
child 及其后续 descendants，不声称具备 suspended-spawn 的无竞态进程准入。

取消、timeout、output-limit 和 reader failure 使用 `TerminateJobObject`，然后等待 direct child
和 output readers 收敛。只有以下条件全部成立才写 `ExecutionCleanupStatus::Completed`：

1. child 已成功加入 job；
2. job termination 或已退出状态可确认；
3. direct child 已回收；
4. bounded output drain 已完成。

PTY 由 `portable-pty` 创建进程；若 assignment 失败，PTY start 失败并调用 direct killer 收尾。
Windows ConPTY 的输入 sender、master 和查询响应器共享一个可关闭的生命周期控制；child exit、
cancel 或 capture failure 会释放输入与 master，使 output reader 可以观察 EOF。它仍不是 restricted
sandbox，也不改变 network 或 filesystem capability。

## 8. Product and diagnostic surface

- `bash` tool description 在 Windows 明确写出 PowerShell dialect、变量/重定向差异，并引导优先
  使用跨平台 file tools。
- terminal task/result 显示 resolved shell 和 local/unconfined execution backend；不新增常驻
  footer 控件。
- Doctor 增加离线 shell resolution 与 Windows process-owner capability check；不得启动网络或
  执行用户命令。
- EN/ZH configuration/permissions 文档说明 Windows shell 默认值、显式 shell 参数和 sandbox
  非承诺。

## 9. Implementation slices

1. R39.0：RFC、实现/竞品/官方资料 inventory、契约冻结与执行账本。
2. R39.1：复用 portable workspace path 语义，修复 terminal cwd/prefix/confinement。
3. R39.2：`ResolvedShell`、PowerShell/cmd argv、permission classification 与 tool metadata。
4. R39.3：Windows Job Object owner、one-shot/non-PTY/PTY cleanup receipt 与真实测试。
5. R39.4：Doctor、TUI/tool-card wording、EN/ZH 文档和能力边界。
6. R39.5：hosted Windows suite、完整 affected gate、代码质量与方案完整度审计。

## 10. Acceptance criteria

- Windows drive/UNC cwd 不因 `Component::Prefix` 被无条件拒绝，workspace escape 仍被拒绝。
- hosted Windows 的默认 shell 是可执行的 PowerShell；PowerShell/cmd argv 不含 `-lc`。
- PowerShell/cmd command 不会获得 Bash-derived `Read` 或 family grant。
- explicit unsupported shell 在 spawn 前 fail closed。
- non-zero exit、UTF-8 output、timeout、cancel 和 descendant process cleanup 都有 Windows 测试。
- cleanup receipt 不依赖 `taskkill` 的单一退出码；Job Object 失败不伪装为 completed。
- Doctor/TUI/docs 同时说明 resolved shell、unconfined local backend 和 restricted backend 非目标。
- Windows hosted suite 与 affected standard/full gate 通过，最终审计无剩余 P1/P2。

## 11. Validation

```bash
cargo test -p sigil-tools-builtin --lib
cargo test -p sigil-runtime doctor
cargo test -p sigil-tui tool_card
cargo clippy -p sigil-tools-builtin -p sigil-runtime -p sigil-tui --all-targets -- -D warnings
cargo fmt --all --check
./scripts/check-docs.sh
./scripts/check-pages-site.sh
git diff --check
```

Windows-only conformance 必须由 pushed hosted job 执行；非 Windows 本地测试不能替代该证据。

## 12. Progress

- R39.0 complete. RFC-0037 hosted evidence、完整 Windows tools test failure、当前 shell/terminal
  implementation、Microsoft/Rust 官方契约以及 Gemini CLI/OpenAI Codex/Reasonix shell resolver
  已完成 inventory。本地分解审计补齐了 frozen-per-registry resolver、PowerShell UTF-8 与 native
  exit-code propagation 三个原始方案缺口；路径、dialect、process owner、UX、hosted proof 和非目标
  已有独立验收项。
- R39.1 complete. Terminal cwd 删除了拒绝 `Component::Prefix` 的重复 lexical/prefix 实现，改为
  复用 file/change-set 已验证的 portable path helpers；Windows-only regression 同时覆盖 prefixed
  workspace cwd 与 prefixed external cwd rejection。macOS 上 tools 186/1 ignored、strict Clippy、fmt
  与 diff gate 通过；真实 Windows 运行留在 R39.5 hosted conformance。
- R39.2 complete. Registry 构造时冻结 native default shell：Windows 优先 `pwsh.exe`、回退
  `powershell.exe`，其他平台保持 `sh`；explicit shell 仅接受已建模的 POSIX、PowerShell 与 cmd
  dialect，未知值在 spawn 前失败。one-shot、terminal permission 与 execute 共用同一 resolver；
  PowerShell 使用 non-interactive/no-profile/UTF-8 wrapper 并传播 native/cmdlet failure，cmd 使用
  `/d /s /c`。非 POSIX 命令不再复用 Bash AST、readonly downgrade 或 family grant；tool/terminal
  metadata 同时报告实际 program 与 dialect。tools 191/1 ignored、focused permission regression、
  strict Clippy、fmt 与 diff gate 通过；Windows 实际 argv/exit/UTF-8 仍由 R39.5 hosted 证明。
- R39.3 complete. 新的 Windows-only process owner 为每个
  one-shot、non-PTY 与 PTY child 建立 kill-on-close Job Object；spawn 后 assignment 失败会先回收
  direct child 再 fail closed，worker/guard drop 也会关闭 owner。取消、timeout、output-limit、reader
  failure 与 direct-child-early-exit cleanup 均改用 `TerminateJobObject`，产品代码不再调用
  `taskkill`；只有 Job termination 与 direct-child/wait convergence 同时成立才记录 completed。
  `windows-sys` feature/owner/非 sandbox 边界已同步到供应链台账，并增加 UTF-8、non-zero exit、
  one-shot timeout descendant、terminal process/PTY cancel descendant 的 Windows-only regression。
  macOS tools 191/1 ignored 与 strict Clippy 通过；真实 descendant proof 已在 R39.5 pushed hosted
  job 收口。
- R39.4 complete. Doctor 新增纯离线 `terminal:shell` 与 `terminal:process_owner` 检查，明确输出
  resolved executable/dialect、lifecycle owner 和 `local_backend=unconfined`；检查不会执行用户命令
  或访问网络。bash/terminal task card 解析 shell program、dialect 与 backend metadata，并把本地
  backend 显示为 `local unconfined`。EN/ZH configuration、permissions/sandbox、terminal
  compatibility、changelog 与 site 已同步 Windows 默认 PowerShell、Job Object 仅负责生命周期、
  非 POSIX 审批更保守的边界。runtime doctor 51 项、TUI tool-card 58 项、focused tools capability、
  affected strict Clippy、fmt、docs/site 与 diff gate 通过；本机 RVM/dyld 启动异常时使用系统 Ruby
  执行同一检查入口并恢复全部脚本 shebang，未改变仓库 gate。
- R39.5 complete. Hosted CI run
  [`29552086182`](https://github.com/JimmyDaddy/sigil/actions/runs/29552086182) attempt 2 在真实 Windows
  runner 上通过 kernel 1085/2 ignored、MCP 156、builtin tools/terminal 140、runtime 541 与 HTTP 102；
  tools suite 覆盖 native PowerShell UTF-8/non-zero、drive-prefix cwd、one-shot/non-PTY/PTY Job Object
  descendant cleanup、ConPTY cursor query 和 output-drain convergence。attempt 1 的 Windows job 在
  tools 之前因四个并发 MCP fixture 同时超过 5 秒 initialize 阈值停止；只重跑 Windows job 后 MCP
  156/156 与后续平台 suite 全绿，未放宽产品 timeout。本地 workspace check/test/strict Clippy、
  tools 193/1 ignored、`cargo deny check`、`cargo audit`、docs、Pages site、fmt 与 diff gate 通过。
  最终代码质量与实现完整度审计未发现剩余 P1/P2。
