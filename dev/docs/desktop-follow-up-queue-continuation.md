# Desktop 后续消息队列的连续展示

暂停后续队列、取消当前运行，再恢复队列时，当前会话必须自动显示实际后续运行。
用户无需重开会话；主输入框的独立 Markdown 草稿继续保留。

## 状态与权限边界

- enqueue、resume、interrupt-and-run-next 成功后，renderer 重新查询当前工作区、当前
  HTTP 会话的 continuity。只有服务端返回 exact foreground owner 与 owner revision，才附着
  该运行的 SSE。队列命令成功本身不等于运行已开始，不得据此伪造 Running 或创建补偿运行。
- 恢复命令返回前，后续运行可能已经完成且队列变为空。展示端保留命令前已知的待交付事实，
  在既有有界 owner 探测结束后刷新 canonical display，显示实际保存的读取结果与最终答复。
- receipt 的 session、queue session 与 action 必须对应本次命令。未决、失败、结果不确定
  或错误作用域的 receipt 不触发成功后的附着；busy 时不能重复提交同一操作。
- 切换或关闭会话后，旧命令回包失效；它不能附着旧运行、抢回选择或在新会话弹出过期失败。
- Pause、Edit、Remove 仅改变队列内容或调度状态，不能被当作新运行启动事实。
  successor 仍由共享 runtime 调度，旧运行、Task 与审批的身份校验继续由原 owner/reducer 处理。

## 验证

`ConversationQueueContinuation.test.tsx` 通过真实 React 控件依次执行 Pause → Cancel →
等待旧运行完全 idle → Resume，并分别覆盖 actual live owner 附着后的 SSE，以及 receipt
前后快速完成、没有新 SSE 时的 canonical 结果刷新。另覆盖主草稿、未决/不确定结果、重复
点击、错误作用域和切会话晚回包。测试验证的是结构化交互；原生窗口的连续更新需另做实机复验。
