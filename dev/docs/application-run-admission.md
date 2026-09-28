# 应用运行接收凭据

Desktop/HTTP 与 TUI 的显式运行命令沿已有 application K/F 和 session writer 结算。接收表示既有运行 owner 已接受本次命令，不表示模型成功、任务完成或工具获批；工具审批、权限、取消、fresh skill/profile/Task 校验继续在原边界执行。

- 普通对话、附件、inline skill、Plan prompt、显式 Task 与 Task continuation 使用其真实 `ConversationRunStarted` 后的 `ConversationRunAcceptedV1`。原输入与附件、或原 Task/guidance 绑定 input digest；完整 typed options 与技能身份保留在 F。当前文件批注等 host context 的 materialization 不改写原输入身份。
- TUI profile 的真实 `AgentThreadStarted` 与本 K/F 的 operation marker 同批追加，绑定 profile 和安全投影后的 prompt hash。child-session skill 使用真实 `TaskDirectExecutionAdmittedV1` 与同批 marker，绑定既有 Task owner 实际使用的 objective hash。历史相同 profile/objective、当前投影或 UI 通知都不能替代本次操作记录。
- HTTP agent 与 Task continuation 使用已有 prepared foreground run 的真实生命周期。绑定在 owned blocking worker 中完成，错误与晚取消仍回到原 owner join/cleanup，不丢弃 prepared 资源。
- launcher 的 32 个未决操作保护保留；实际接收凭据使已结算项及时离开该集合。失败或未确认的 publication 不伪装 accepted，恢复仍查询精确 K/F 的持久记录。重放旧 K 不重新创建 run/thread/Task。

回归覆盖普通与 inline skill 各 34 次同文不同 K，调用真实 launcher queue/poll 与真实 worker，并核对持久条目和重启回放；Task/child skill 的真实 owner、profile 实际 child 调用，以及 Task continuation 错误任务/指导语绑定分别有定向反例。测试源码存在不代表验收通过，运行记录由对应执行报告保存。

增强运行若在 domain admission 前失败，返回的 Session 会解除本次 process-local binding；这不更改持久 K/F 的不确定状态。只有原 run handle 已成功 join、scope/K/F 完全匹配、原 admission thread 也已 join 后，launcher 才把该条移入既有恢复集合并释放活跃槽。恢复集合不自动重试或重放；UI 失败、超时、错误 scope、panic 或未完成 join 都不能替代该边界。定向回归要求连续 34 次真实 profile 前置失败后普通输入仍可运行，并保留每个旧 K 的可查询不确定状态。

同步 run 前置检查失败也沿同一归还边界：只有递归 dispatcher 已返回、未建立 active run，且当前 Session 取回的 binding 与本次原始 scope/K/F 完全一致，才发 process-local owner-return 通知。异步已取走 Session 的 run 继续由真实 JoinHandle reaper 发出；两种情况都要等 admission 线程 join 才迁入 retained recovery。该通知不是 accepted/no-effect 证明，不解决或重放旧 K/F，32 个活动槽保护保持不变。测试的未决恢复查询必须在原 worker owner 仍存在时执行；测试辅助 owner 的 Drop 会正常关闭该 worker，不能在其后把正常断连误判为服务装配故障。
