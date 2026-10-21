# TUI 单一输入事件源

2026-10-21。TUI 的 owner loop 使用一个 Crossterm `EventStream` 接收键盘、粘贴、窗口大小和焦点事件。外部 worker、投影和计时唤醒不会重建或丢弃仍 Pending 的 `next()` future。

每轮最多处理 64 个已就绪事件。批次内继续对同一个 stream 的已持有 future 轮询一次；Pending 时立即结束批次，把原 future 留给下一轮等待。该探测使用 owner task 的真实 waker，不能用 no-op waker；只有消费了 Ready 结果才构造下一次 `next()`。这使连续输入有序，空队列不会忙轮询，取消、resize 和 worker/计时分支仍有机会获得调度。

不再将 `event::read/poll` 与 `EventStream` 混用；这是 [Crossterm 0.29.0 官方输入契约](https://docs.rs/crossterm/0.29.0/crossterm/event/index.html)的明确要求。错误事件和 stream 结束仍保留独立错误路径，不当作“暂时无输入”。raw mode、alternate screen 与退出清理所有权不变。

本次由静态代码审查确认原混用违反库契约，尚未证明它是先前原生 n/N/方向键未处理的具体原因。隔离回归覆盖事件顺序、批次预算、Pending 不空转、首次 waker 仍能唤醒、错误与结束状态；真实 Terminal/cua-driver 的键位、连续输入和退出仍应单独验收。使用录制 wrapper 与直接启动的结论分开记录。
