# TUI 取消后的工具完整输出恢复

活动 run 执行时会暂时取走 worker 的 `Session`。取消完成后，worker 从同一 durable journal 重载会话；若只从暂时为空的 `session.current` 取工具输出存储，后续晋升的排队请求便可能得到没有 artifact store 的会话。工具仍可能完成，但 V3 结果会记录输出不可用，用户无法查看完整正文。

## 恢复与权限边界

[`cancel_active_run`](../../crates/sigil-tui/src/runner/worker_loop/active_run.rs) 优先保留当前会话已附着的 store；会话被活动 run 持有时，复用 worker 持续持有的 `ManagedTuiArtifactStoreLease`。该 lease 来自 Resource Authority 准入的 ArtifactStaging / ArtifactStore，生命周期覆盖同一交互会话的多个 run。

取消过程的各次重载仍统一经过 `load_active_run_session`：只在 store 的 `session_scope_id` 与实际重载会话相同时重新附着。其它会话的 lease 不能借此进入当前会话；没有匹配 store 时不伪造可用输出。本修复不增加资源授权，不改变取消 owner、真实 join、cleanup 结算、公开终态或排队晋升条件，也不重放已取消的工具。

## 已有隔离证据

[`cancellation_settlement_tests.rs`](../../crates/sigil-tui/src/runner/tests/cancellation_settlement_tests.rs) 的两条 `cancelled_run_` 回归覆盖：

- `Session` 已被取走后，经真实取消/重载路径恢复同 scope 的 managed store，并成功 capture / resolve 一段中文工具输出。
- worker 保留的是另一 session 的 lease 时，重载会话不附着它。

两条测试已通过统一隔离入口运行并通过。它们证明存储恢复和 scope 隔离，尚不等于真实模型已执行晋升请求，也不等于 TUI 已呈现完整输出。正常取消、失败清理等其它既有测试仍分别约束取消终态。

## 原生验收 oracle

1. 使用独立测试身份、固定 binary hash 和含首/中/尾标记的只读 fixture。记录 session ID 与原文件 hash。先让真实 run 到达未批准写审批，再排队一条明确读取该 fixture 的请求；记录队列项身份。
2. 在 TUI 取消当前 run。只读核对同一 journal 的 `RunCancellationRequested` 与 `RunCancellationFinalized`：同一 cancellation scope、`Cancelled`、`cleanup_complete=true`，活动 task/effect 计数为零；未批准写调用没有实际执行，canary 不存在。
3. 核对该队列项的 `ConversationInputPromoted` 与新的实际 run 绑定，仅晋升一次。新 run 必须产生真实 read 工具调用及终态，不能用零工具回答代替。
4. 将新 run 的 call ID 绑定到 `ToolResultRecordedV3`：artifact 为 `Published`，descriptor 属于同 session/call，持久字节数与 digest 经既有受限读取验证，正文含预期标记；原只读文件 hash 不变。artifact 正文可能包含工具格式，不能把它的 digest 直接当原文件 hash。journal 本身只保存 descriptor 与有界 view，不保存完整正文。
5. 在 TUI 选中该 read 结果，使用实际完整输出入口并翻页或搜索，观察首/中/尾标记均可见，随后返回 Ready。保留对应原生画面。内部 artifact 读取只能证明数据层，不能替代这一步 UI 验收；模型最终答复也不能替代完整输出证据。

若模型没有调用 read、晋升未发生、输出因独立策略不可用，或驱动无法操作完整输出入口，分别记录实际边界，不能把本项写成原生通过。
