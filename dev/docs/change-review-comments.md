# 逐行变更评论

评论复用当前会话的普通发送和排队流程。用户选择已记录的 checkpoint、文件、旧/新侧行号及范围，输入实际评论；host 根据原始已提交 diff 重新校验 checkpoint digest、tool call ID、diff digest、路径和行号。它不把评论转成工具审批、写权限或文件恢复请求。

当前文件的 Current、Changed、Unknown 都只是展示信息，不妨碍用户讨论原始变更。`fully_restorable` 也不是评论门槛。源版本、会话或行范围不匹配仍拒绝；后续实际修改继续走原有读取、审批、before-hash 和验证链。

TUI 按 `Alt-R` 打开记录列表，`Up/Down` 选回合，Enter 读取原始差异。`Left/Right` 选文件，`Up/Down` 选行，`Shift-Up/Down` 扩选范围，Tab 切换 Old/New。Enter 进入评论，输入或粘贴文字后再 Enter 加入批次，可在不同文件/回合继续添加。`Ctrl-S` 发送批次；正在运行时进入原来的 follow-up 队列。共享 materializer 的预算为每批最多 16 条、每条 4,096 字节、每个范围最多 200 行，原有 diff 展示预算继续有效。

读取与确定性 materialize 在有 JoinHandle 的短任务中完成。结果只返回相同 request、session 的浮层，关闭或切换会话后的迟到结果不发送。排队上下文在创建原 queue command 及其哈希之前完成，不在 durable effect 内改写正文。确定性排队材料不读取当下工作区文件，而明确要求执行时重新读取；普通发送的 typed refs 也不会在 TUI adapter 中丢失。

原输入框草稿始终保留。发送失败保留评论可重试；正常发送在本次 exact prompt 和 submission intent 的实际 RunStarted 后关闭，排队在原 command 的持久回执确认后关闭。操作没有静默写文件或调用额外摘要模型。

测试覆盖真实 mutation/preview 产生的 diff→选侧/范围→评论→materialize→普通发送/运行中排队，文件 Changed 阳性、缺失旧侧行阴性、失败保留、原草稿与源文件不变、迟到/foreign scope、实际启动回执与旧 intent。是否已通过以 repo-local A4a 执行记录和主线程 gate 为准。

Desktop 从会话的恢复面板打开检查点的“审阅改动”。点击旧侧或新侧行号选中行，Shift 点击扩展范围；添加评论后可继续选择其他文件，再“发送评论”。当前普通输入框草稿不会改变。正在运行时同一批评论走现有 follow-up 队列，原 diff 上下文在构造队列 command 的 K/F 前由 host 验证并确定性装配，不在 effect 内改写已绑定正文；队列确认前显示“正在发送评论”，失败保留批次。普通发送以 host 的 run-start admission 回执为确认，不伪造模型响应。

Renderer 只持有 exact source refs 和评论，不拥有路径读取、恢复文件或 generic HTTP 能力。`desktop_checkpoint_review` 查询与现有 start/queue native allowlist 传递收窄 DTO；评论通过正常 session user input 持久化。Task continuation 携带的同类批注附入原用户 guidance，仍属于来源材料，并要求执行时读取当前文件。文件漂移不代表原始 immutable diff 失效；损坏/foreign digest、不存在的侧行号与预算超限仍拒绝。
