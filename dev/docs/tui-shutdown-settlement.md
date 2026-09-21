# TUI 退出清理与最终结算

本方案落实 2026-09-13 的退出修复决定，补充[运行时切换](session-runtime-controller.md)与[统一命令执行](unified-command-execution.md)。取消权限仍属于原 run/resource owner，launcher 的等待结果不生成资源清理证明。

## 等待与结果

用户退出先关闭新工作准入并恢复终端。主 worker、admission/interaction、会话切换与维护、辅助查询、当前及退休会话的投影 observation、事件转发与 bootstrap 收尾均由显式退出协调保持所有权。先向独立 owner 发停止请求，再等真实工作完成并 join，正常路径在汇总结果前释放所有已结束 owner；Drop 仅用于异常兜底。

5 秒是共享慢退出提示阈值，不是清理失败的证据。到期后显示当前收尾阶段和耗时，继续观察同一批 owner；不能重启命令、重新获得取消权限或把 owner 放入无人等待的后台任务。最终成功可以超过提示阈值，实际 panic、资源释放失败和必要审计失败始终保留。

内部区分清理中、已完成与已确认失败。只有所有 join 已消费、原作用域 effect/task 归零、资源 receipt 与必要持久化结算成功，才能正常退出。零计数本身不能清除真实故障；曾经等待超时也不能永久遮住后来的完整成功。

## 取消持久化

取消仍先追加原请求的 Requested，再由唯一 owner 激活取消。前台 2 秒等待目标到期只产生非终态进度，原 owner 持有 session 与任务，继续收敛；期间不接纳新 run 越过关闭的 forward gate。用户进一步退出时仍走同一停止上下文。

原 owner 收到真实 join/清理结果后追加一次 Finalized。这里的 `cleanup_complete` 证明该 run scope 的 root join、effect/task 和资源清理，不单独证明整个应用退出成功。现有 public run terminal 依赖 Finalized；其后必要的 MCP、agent/task 或 public run 审计若失败，保留已记录的物理清理事实，同时锁存 worker 失败并使最终退出返回错误。取消成功写 Cancelled；真实失败或无法确认资源结算时写 Interrupted/cleanup-incomplete。观察阶段不提前写错误终态，因此正常迟到成功无需增加第二套持久格式。既有历史终态保持 append-only，不将之前已写的 Interrupted 改成 Cancelled；重开遇到未结算请求，沿用现有保守恢复语义，不从进程消失推导成功。

## 观测与执行边界

阶段观测区分取消激活、进程停止、输出排空、资源结算、审计持久化与线程 join。阶段计时只保留有界分类/耗时/进行中数量，不包含命令正文、路径、凭据，也不代表完成凭证。实际阻塞步骤必须有持有并回收的 owner；仅取消 future 或执行 timeout 不能宣称已开始的同步 I/O 停止。

## 验收

- 通过可控同步点覆盖提示阈值前后完成，验证先提示再成功，最终退出结果与完整持久记录一致。
- 真实 Core `exec_command` 在前台等待时退出，验证命令取消、最终资源 receipt、唯一取消结算和 worker join；命令已返回后台运行且 run 已完成时退出，仍由原命令 owner 停止并回收。
- 清理失败、panic、审计失败在计数归零后仍为失败；重复等待不能清除故障。
- 终端先恢复；当前与退休 owner、辅助线程、切换/维护和事件转发均完成显式收尾。
- 所有自动化测试经[统一隔离入口](test-isolation.md)，不访问真实用户会话、配置或凭据。
