# 统一命令执行与可见性

本方案落实 2026-09-12 的产品决定，替代 [RFC-0060](rfcs/0060-structured-shell-risk-approval-and-terminal-execution-v2.md) 中按有限命令与持久命令拆分工具的规则。

## 执行契约

模型使用唯一启动工具 `exec_command`，不再在 `bash` 与 `terminal_start` 之间选择。有限检查、构建、测试、后台服务与交互命令使用同一受管进程 owner；命令族只参与结构化权限分析，不决定工具入口或是否允许等待后继续。

| 参数 | 职责 |
| --- | --- |
| `command` | 要执行的命令，必填；字段名固定为 `command`，`cmd` 不作为别名 |
| `cwd` / `shell` | 显式工作目录与已建模 shell；继续参与权限与执行绑定 |
| `pty` | 是否需要伪终端，与命令持续时间独立 |
| `yield_time_ms` | 本次等待预算，默认 1000 ms，范围 0–60000 ms；耗尽时返回当前执行状态 |
| `max_runtime_secs` | 进程总时限，范围 1–86400 秒，省略采用受管 owner 的 24 小时上限；耗尽后停止进程并收敛清理结果 |

统一执行是 Core 基础能力；`ExecutionTerminal` 的 authority readiness 为必需条件，不再由可选 terminal 选择开关决定是否检查。已有持久 composition 中的 terminal 枚举值仍按原结构读取，不重写历史绑定。

升级时，旧 Core 的 13 项启动记录仅在原内容哈希、完整旧探针集合、实例、authority 与 generation 均通过验证后作为历史前驱。启动必须运行当前 14 项探针，并固定向前发布新的 generation；旧记录本身不能授权新执行，也不能被投影为当前 ready。被中断的升级重试复用已经选定的前向 generation。

执行身份由 host 生成。工具返回 `execution_id`、真实状态、可用的退出码、清理结果和有界输出。等待预算耗尽只能表示进程仍在运行；工具调用返回成功不能解释为命令执行成功。

后续使用 `exec_read` 读取有界输出页，`exec_wait` 等待退出或显式输出条件，`exec_input` 输入，`exec_resize` 调整 PTY，`exec_cancel` 取消。它们关联同一个 `execution_id`。输出读取与等待不重新启动进程，不重复授权原命令，不制造新的进程完成证据。

工具结果进入 kernel 前，读取、输入、resize、等待和取消的生命周期字段必须从同一份 owner 快照生成完整收据。`schema_version`、execution 身份、命令摘要、日志引用、generation 和状态由 host 提供，不要求模型填写。分页/操作结果与生命周期收据分别解释；读取结果内的嵌套状态与外层状态使用同一快照，不能将展示摘要当作完整持久化记录。

工具执行后的审计或状态投影失败时，已取得的真实工具结果仍须结算，剩余已声明调用明确记录为执行前中断；已启动但没有结果的调用保留结果未知，不自动重放。Task 的失败与真实原因传递到共享 application 终态和两端展示，不能改写成未满足子代理委派。存储本身不可写时保留原始失败和追加结算错误，不能声称记录已落盘。

## 权限与生命周期

已有 `permission.tools` 与带 subject 的规则中，旧工具名称的 Ask/Deny 继续作为新执行工具的保护性限制；旧 Allow 不授予扩大后的统一执行能力。显式配置当前工具 key 后采用当前策略，注册表仍只暴露新入口。

所有启动复用 immutable permission plan、结构化 Shell 分析、规范化路径、显式网络授权、受控环境和 RA 管理的资源。PTY 不意味着可信，命令名不意味着只读，等待预算不授予继续运行之外的权限。

不继承任意用户凭据、HOME 或 shell startup 环境。受管执行的保留环境变量由 authority-issued ExecutionTemp 绑定；跨调用临时文件使用 session-scoped `$SIGIL_SCRATCH_DIR`，不能把任意 `/tmp` 路径或错误的 TMPDIR 推断为已授权 scratch。

进程实际退出、取消、超时和清理分别保留真实结果。取消/超时后 owner 负责停止、回收与 artifact 收敛。每个执行的 deadline watchdog 由 process handle 持有，并在结束或 drop 时唤醒并 join；stdin 最多持有一个受管写入工作项，取消时中断并 join，不能丢弃阻塞写入后宣称清理完成。输出静默、用户未读取结果或模型结束等待，均不得使时限失效。旧日志保持 append-only，不重写历史工具名，不在恢复时自动重放悬空命令。历史调用名称可继续按当前记录结构展示，但注册表不保留第二套启动入口。

## Desktop 与 TUI

参数完成并可安全投影后即创建命令卡片。卡片保留脱敏命令和展开入口，依次呈现待执行、等待审批、运行中和最终结果。命令可见性不依赖审批弹窗、stdout/stderr 或可丢弃的实时进度。

运行耗时来自 owner 的真实 `started_at_ms`；待执行和审批等待不计入，静默/后台运行仍按秒更新，终态按最后 owner 时间冻结。运行中默认显示命令摘要和少量输出；长命令与多行命令可展开。不能只用 `Bash / running bash` 代替执行内容，也不能把状态提示伪装成 stdout。一次执行在等待、输入、输出、取消及最终 lifecycle 更新之间保持关联，返回仍运行的工具结果不能将卡片改成成功。

Task 的子调用在产品投影中使用 source-bound 调用身份；provider 的原始 call ID 与 durable 审批身份保持不变。审批事件的 `display_call_id` 仅将审批状态关联到对应命令卡，不参与审批请求、授权哈希或响应路由。执行进度以真实 execution ID 关联，独立于 provider 文本/参数 attempt 的替换与退休。并行 worker 的同名或同 call ID 命令不得互相覆盖。

会话重开从持久 ToolCall 安全恢复完整命令，并按 session 内唯一 execution ID 合并跨 run 续接；owner generation 决定终态，早于工具返回的终态也必须保留，不能被较旧 running 收据覆盖。恢复索引只持有有界元数据，命令正文按原始 envelope 精确 hydration 并计入页预算。

## 验收

- 唯一启动入口、有限命令被允许、等待预算与运行总时限独立。
- 真实静默进程返回仍运行后，能够等待、读取同一执行的结果。
- 非零退出、取消、超时、PTY 输入与 resize 保留真实结果与清理证据。
- 完整参数到达后立即可见；长命令、脱敏、无需审批和无输出均覆盖真实渲染。
- Task/并行调用通过真实 event bridge、实时预览与两端投影，不只直接调用 UI handler。
- runtime deadline 在没有模型轮询时生效；结果与 lifecycle 的顺序变化不造成假成功或重复卡片。
