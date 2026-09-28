# 工具参数与 provider 流完整性

工具参数可能在普通工具模式下违反 JSON 语法或工具 schema。provider adapter 负责真实流终结、调用身份和片段拼接完整性；kernel 工具边界负责参数解析、领域校验及有界修正。完整收到的非法 JSON 必须原样交给工具验证，不能由 adapter 猜测补括号或改变用户语义。

DeepSeek Chat 的 `tool_calls` 结束必须覆盖全部已观察的调用索引，具有完整 provider 身份和工具名称，完成参数字节数必须等于累计片段字节数。缺少工具终结的 EOF / `[DONE]`、带未完成调用的 `stop`、`length` / `content_filter` / 未知结束原因、身份漂移和终结后的新增调用均拒绝。普通文本流原有 EOF / `[DONE]` 兼容行为保留；不能据此声称所有 provider 的流协议均已资格化。

每个有工具的 DeepSeek Chat 请求记录实际工具 schema 模式：启用、明确关闭或不支持 schema 后回退。工具流结束或失败记录封闭的结束原因类别、调用数量、参数片段数、字节数及完整调用数。正常无工具请求不增加工具诊断记录。

诊断通过 `ProviderChunk::Diagnostic` 到 `ControlEntry::ProviderDiagnostic`，使用当前 physical attempt 的准确 correlation / causation 追加为 `DiagnosticRecorded`。它们保持私有，不进入模型消息、public outbox、完成证据 frontier，也不作为 generation 或 durable output/effect 阻止原本允许的 provider recovery。诊断没有自由文本、参数、工具名称、路径或原始响应；Chat envelope 解析错误仅保留 JSON 错误类别及行列位置。TUI 的 tracing 使用 sink，因此排障应读取会话中的结构化诊断，不能依赖调高日志等级。

严格 schema 是降低错误概率的额外保障。`Auto` 遇到不能忠实表达的 schema 仍允许标准工具请求；不得通过移除领域校验或无条件接受未满足实际执行条件的最终答复来制造成功。内置 `read_tool_artifact` 的 selector 使用带互斥类型标签的完整 `anyOf`，与实际解码类型保持相同的分支集合。

验证覆盖完整但非法参数保留、部分工具响应被拒绝、身份漂移、诊断无原始参数、真实 HTTP EOF / DONE 边界，以及 physical attempt 诊断落盘重开后的因果关系和无输出权限语义。测试通过统一隔离入口运行。

同一会话中已持久化的工具调用 ID 不能在后续 provider turn 再次声明。kernel 在新的 Start 或无 Start 的 Complete 首次发布之前检查当前会话既有调用身份，避免先追加第二个 preamble/result 再使会话投影损坏。正常单个调用的 Start → 参数片段 → Complete 仍共用同一 ID，不同会话的 ID 互不占用；不重命名 provider ID，也不放宽审批。若错误响应此前已经发出其它合法的临时文本或工具参数预览，沿现有 partial-discard 和真实 run terminal 退役；原会话保持可读并可用真正的新调用继续。既有坏历史不会被静默重写。
