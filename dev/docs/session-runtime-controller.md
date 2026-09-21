# 会话 runtime 切换的所有权与恢复

TUI 的当前会话路由切换由 `sigil-runtime::session_runtime_controller::RuntimeSessionController` 负责。控制器复用 `InteractiveSessionAttachmentLease` 缓存的 route mutation authority，不从界面 busy 状态推导静止，也不为新 worker 创建第二份 authority。

## 持有层与调用链

`AppState` 保存不可克隆的 `RuntimeTransitionOwner`。该宿主 handle 持有独立的 admission 线程、同一 application service、原始 application request、控制器，以及旧代或新代 worker 的真实线程 owner。它跨 worker 更换存活，旧 worker 的 shutdown 不会 join 发起切换的 controller 线程。

调用链为 `/model` 或设置后的应用动作 → launcher 后台 owner → application reserve / bind_effect → runtime controller → `RuntimeSessionWorkerHost` 的窄平台动作。普通 command 仍通过 application endpoint 交给原 queue/run owner；controller 只控制切换时是否可以接纳，不承接普通任务调度。

`RuntimeSessionProjectionBinding::replace_owner` 在原 observation 已退出后交换同一 session 的 projection owner。读取能力没有因交换而取得 route mutation 权限。切换期间不调度新旧 owner 混合的 observation。

## 持久阶段和成功点

同一个 session append-only stream 保存三类 `SessionRuntimeTransitionV1`：

1. `Intent` 冻结 command K/F、原 route/trust/revision、目标 route/trust 和配置指纹。持有原 authority 的 quiescence permit 后才写入，真实 foreground/background owner 存活时拒绝。
2. 关闭 endpoint，后台停止旧 worker；确认线程、effect cleanup 和 observation owner 已结束后，通过 session writer 的 crash-safe bundle 同时写 route/trust/reset 边界和 `Configured`。
3. 新 worker 从私有消息通道发出包含 operation、route revision、worker generation 和 boot identity 的 `RuntimeReady`。控制器验证完整匹配、追加 `Activated`，然后发布新 endpoint。application domain receipt 引用真实 Activated 的 event ID、sequence 和 checksum。

Configured 只说明目标配置已落盘。界面保留待选模型和输入草稿，Activated 与匹配 generation 的 live gate 都成立、对应 worker 尚存活时，才一起发布实际启动使用的配置、provider 和模型路由，再清除待选值；同一路由选择也经过实际启动验证。recent model、余额、目录等展示刷新不决定激活是否成功。

## 失败与关闭

意图写入前失败保持原 worker 和入口；停止开始后入口关闭且宿主保留旧 owner。Configured 后启动失败保留新配置，当前 worker 不可用，重试使用原 request 和 Intent。query 只对账对应 Activated，不从当前模型文本或消息入队推测成功。

显式重试经 `ApplicationPort::resume_session_runtime_transition` 返回原 application service，只允许原 `SelectRoute`。控制器先只读确认同一 Intent；服务核对原 K/F 和 owner effect binding，通过 `SessionRuntimeEffectResumedV1` 在原 command journal 重新发布 EffectStarted。随后取得原代 forward guard，再调用同一控制器。marker 写入与持有 namespace 文件锁的 forward guard 不重叠，避免重复锁住同一 namespace；dispatch 完成后释放 guard，才写领域提交索引。已封存代可以查询原提交，但不能恢复执行，也不能通过修改请求中的 generation 复用旧 Intent。

Intent 已落而 EffectStarted 写入失败时，后台只读对账将该事实交回 UI。即使原 worker 已返还主事件循环，下次同 route 选择仍把该 worker 交回同一宿主 owner，并沿原 request 继续。只有可靠读取确认尚无 Intent 的前置拒绝才允许创建新的操作；观察错误保留未知状态。

Activated 已提交但 live gate 发布失败时，普通查询仍只返回历史证据；显式恢复不会被这个历史 receipt 短路。服务重新校验同一 owner binding 并提交专属恢复 gate，再由 controller 校验仍存活的准确 Ready 并开放入口，不重复 spawn 或追加第二条 Activated。已结算记录只有保留相同 owner binding 时才能走此路径，封存代仍拒绝任何恢复 dispatch。界面不会把历史 receipt 单独显示成当前切换成功。

退出只设置宿主退出标记。后台操作在各边界检查标记，迟到 Ready 触发清理；不会因界面不再等待而宣称旧 worker 已停止。最终释放宿主 handle 前保留并 join 所有真实线程 owner，终端恢复先于退出等待。普通 launcher worker 同样承担这一义务：5 秒是慢退出提示阈值，超时后仍由显式退出协调保持 owner，最终结果依据真实 join 与资源/审计结算；等待超时不产生永久失败标记。正常路径关闭 admission 并发送真实 Shutdown，再完整收敛 worker、未完成 command admission、交互及投影 observation；转发线程也由实际 JoinHandle 收尾。Drop 只保留异常所有权兜底，具体见[退出清理](tui-shutdown-settlement.md)。错误返回即使尚未显式发送 Shutdown，仍走同一停止入口，不会因自身仍持 sender 而直接 join 等待 recv 的空闲 worker。会话重绑与失败清理使用同一 launcher 后台所有权边界；它们因 session 身份改变而建立新 application scope。

Setup 重建 AppState 时先对原 route/maintenance owner 调用 `invalidate_view`，再转移 handle。失效操作继续在后台清理，其结果不能更新新界面的模型、配置、notice 或 worker，也不能把旧 application scope 提供给新视图。cleanup 线程创建失败时保留原 JoinHandle 并在后续 poll 重试。

会话辅助查询也在 Setup 重建时取消并转入新状态的 retired 清理队列，只等待其旧 reader 真实退出，不接纳旧结果。最终退出提示阈值耗尽时保留 JoinHandle，显式退出协调等待实际 observation 后再释放 AppState；按值接管的 bootstrap/初次 worker 启动失败清理同样不会丢弃未完成线程。

## 回归入口

`session_runtime_controller_tests` 使用真实隔离 session writer 和可控 host 事实，覆盖同 route 激活、Configured 后失败重开、原 operation/K/F 重放、错误 Ready、停止期间退出及真实 execution owner 阻塞。其 `resume_tests` 通过真实 RA command writer，覆盖 EffectStarted 边界失败后的重开与同 Intent 恢复，以及旧代封存后拒绝恢复；有界等待同时检查 namespace 锁递归。

TUI `runtime_transition_tests` 使用生产 lifecycle poll、worker drain 和按键处理入口，在真实后台停止等待期间采样 128 次输入，并检查 p95 小于 100 ms、退出请求仍保留 worker owner。该测试不依赖 provider 网络请求。

外层退出回归覆盖终端先恢复、超过提示阈值后仍观察原 owner 并按最终事实返回，以及未提前发送 Shutdown 的错误退出可终止空闲 worker。投影任务 abort 后仍等待实际后台 observation guard 释放，取消 future 本身不构成已完成证据。
