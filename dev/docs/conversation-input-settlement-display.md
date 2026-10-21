# 等待用户输入后的工具结算展示

`AwaitingUserInput` 是当前 conversation run 的公开终态。用户随后取消、拒绝或提交回答时，
内核仍会为原 `request_user_input` 调用追加 V3 工具结算。展示层不能因 run 已结束而丢失原调用，
也不能用新活动 run 或恰好相同的 callId 猜测归属。

共享 runtime 在读取 `UserInputRequested` 时，只保存其 typed continuation 精确绑定的
assistant message、tool call、原 conversation run 和 Requested 展示锚。
完整 request identity 与 requestHash 一起保存；其中 logical root 可以是内部 Task run，
不要求与外层 conversation run 同名。该信息只用于展示，不授予执行、审批或继续权限。

V3 结算的 requestId、generation、requestHash 和 callId 全部匹配时，结果归回原 run，
并替换原 Requested 展示项及同 run 的 live tool slot。来源由精确 V3 一次消费，
或在原 run 以非等待状态终结时退役。新 run 复用 callId 不会取得旧来源。

尚未发布等待终态的合法 private-only 前缀，取消或拒绝结算采用
`DecisionAccepted → Resolved → V3 → ToolExecution` 顺序。因此 `Resolved` 只退休表单，
不提前清除该调用的展示来源；后续精确 V3 仍可恢复原锚。表单是否待答始终来自真实
`UserInputProjection`，保留展示来源不会让已解决的问题重新出现，也不会补造公开终态。
缺字段、错绑定或来源不存在时保留 V3 结果事实，但不推断 run 或替换锚。

完整历史重放与增量索引共用上述投影。索引只为该结算行保留命中的身份元数据，
冷页恢复不缓存额外正文，也不重新扫描历史猜测调用。
原 JSONL、内核 ToolResult、取消终态与公开 outbox 语义均不改写；取消澄清产生的
成功工具结算不等于 Task 被取消。

直接回归位于 `crates/sigil-runtime/src/tests/conversation_display_tests.rs`，覆盖真实内核
取消/拒绝和提交 continuation、原 run 与内部 Task root 区分、跨 run 复用 callId、
缺失或错误绑定、一次消费、真实 private-only 结算顺序，以及分页和索引冷页。
测试资格与原生 UI 复验分别记录；
通过投影测试不能单独证明原生界面已收敛。
