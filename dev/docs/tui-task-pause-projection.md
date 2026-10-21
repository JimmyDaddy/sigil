# TUI Task 暂停的实时投影

Task 启动前已持久化的 `TaskRun`、直接执行 admission 与取消作用域，需要在内部 TUI worker channel 同步给当前界面。角色执行器后续只发布 Running 等增量，不能代替此前已写入的 admission。

新 Task（含 Chat 到 Task 的 typed handoff）、显式 `/task continue` 和普通对话的 typed `ContinueDurableTask` 三处执行边界，在角色 dispatch 前发送一次持有 Session 的快照。继续执行的同步发生在新取消作用域持久化成功后；同步失败仍经过原 root finalizer 结算。 TUI adapter 依照已校验的 typed receipt 区分恢复与追加指导：`ResumeTask` 不传指导文本，`ApplyCurrentRequestAsGuidance` 才携带原精确文本；与共享 runtime 契约一致，不从 prompt 推断。发送方不重新 append control 或 public journal；同一 channel 保持 `TaskRunStarted → TaskStateSynchronized → 后续角色事件/审批` 顺序。快照携带 session、public run、Task 身份与当时 public frontier。界面拒绝旧会话、旧运行、其他 Task、已经越过的 frontier 和重复快照，避免覆盖后来应用的实时记录。

暂停入口和 footer 共用同一个请求构建条件：当前运行必须显示为同一精确 Task，durable 当前 Task 处于 Started/Running，并拥有有效直接执行 admission。`runtime.active_task` 只选择当前界面对象，不生成或替代 admission。最终仍由 worker 验证请求 identity、Task、admission、当前取消作用域和运行状态。

待批审批可用 Esc 返回输入；可暂停时 footer 优先显示 F5、返回审阅和取消。F5 不批准或拒绝工具，不绕过权限、审批 subject 或取消结算；暂停后可通过原 `/task continue`，或由模型选择受限 typed continuation 恢复同一 Task。

定向回归包括真实 worker 和 application admission、脚本角色 provider、内置写入审批、Esc/F5、durable Paused 与 cleanup 资源归零，以及显式命令、普通对话的模型 typed resume 和 typed guidance 三条同 Task 继续路径审批写入成功。测试提供方按相同精确 id、name 和参数发送 Start、ArgsDelta 与 Complete；生产对冲突工具流的 recovery 保护保持不变。另验证旧 session/run/frontier/重复快照不覆盖界面、其他 Task/普通 run 不能获得暂停动作，保留 runtime 的 stale admission/作用域拒绝。脚本 provider、局部测试与原生真实模型是不同资格，不能互相替代。

建议由统一 Cargo owner 执行：

```sh
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib task_pause_projection_ -- --nocapture --test-threads=1
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib f5_ -- --nocapture
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib live_preview -- --test-threads=4
```

暂停请求显示简短的进行中提示；收到当前会话、当前 Task 的暂停结果后，在重放历史之后显示 `Task paused · /task continue to resume`，避免被工具中断卡埋住。重复或其他会话、其他 Task 的暂停消息不追加提示，也不改当前视图。Task 的 Paused 与根运行的 Cancelled 仍保留各自持久事实；工具中断、清理失败和 typed recovery 原因不改写。通用 Paused 完成消息的具体原因仍在时间线反馈中保留。
