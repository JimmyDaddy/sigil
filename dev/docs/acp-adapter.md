# ACP 编辑器适配

`sigil --config <配置文件> acp` 通过 stdio 提供 ACP v1。stdout 只交给协议 SDK，运行日志与披露走 stderr。它是既有共享 application run 的编辑器 adapter，不增加 agent loop、审批 owner 或进程 owner。

支持 initialize、独立会话、文本和 ResourceLink 输入、流式文字/思考/工具通知、permission request、取消，以及 `session/load` 恢复。ResourceLink 以结构化引用进入用户输入，不自动读取 URI，也不据此扩大工具权限。未声明 image、audio、embedded context、远端 MCP、客户端 filesystem/terminal 等可选能力。

每个 canonical cwd 装配自己的既有 authority composition；配置的工作区必须与 cwd 一致，使用客户端所选工作区时设置 `workspace.root = "."`。MCP stdio 声明和显式环境值只用于对应会话的后续 run，不写配置、不修改进程全局环境。载入会话时使用本次客户端提供的 MCP 声明；历史审批不会因此授予新启动权限。

ACP session ID 是版本化、不含主机路径的句柄，包含随机 nonce 和 durable scope。adapter 用 canonical 工作区的稳定身份及 nonce 推导既有 managed namespace。`session/load` 对该 exact namespace 做 lifecycle reopen、scope 校验及共享 session/provider binding，完成后才重放历史。客户端 ID 不拼成文件路径，不扫描整个会话库，也不把任意 session ID 当作跨工作区授权。重启后使用相同工作区、配置和 ACP ID 可继续原会话。换工作区、篡改 nonce/scope、需要 provider route recovery 或真实不确定的旧执行仍通过共享约束处理。

历史通过现有协调 reader 分页发送，保留 user/assistant/reasoning 身份。历史工具输出仅展示文本，不重放 pending approval、effect 或执行命令；图片省略和有界文本截断明确标注。历史重放与同会话新 prompt 共用既有 admission 互斥，不影响其他会话。

Stop 会请求当前 run 的取消 ticket，等待执行 owner 与真实 cleanup/finalize；只有确认 Cancelled 才返回 `stopReason=cancelled`。普通 provider 取消异常不覆盖这个结果，真实 Interrupted、audit 和 cleanup 错误则保留。连接断开会请求取消并 join 已接管的任务，再结算后台工作；错误不会只送入已关闭的响应通道而丢失。非关键会话标题在 foreground 释放、终态响应送出后由同一 retained task 执行，下一 prompt 无须等标题；断连仍等待该有界维护结束。

同步审批回调运行在共享 application run 已接管的 blocking worker 中，只驱动当前 permission request future，不创建或嵌套 Tokio runtime。审批身份和可选项来自原 broker；客户端拒绝、Stop 和断连均结束同一个等待，未收到允许不得写入。

执行内部失败使用协议 InternalError，非法 prompt 内容使用 InvalidParams；普通失败不会伪装成 Authentication required。

物理 provider 重试使用独立 message ID。已发出的旧 partial 无法由 append-only 协议撤回，因此 adapter 明确提示废弃，再开始新消息；迟到的旧 attempt preview 不混入新回答。

## 验证

定向 Rust 测试通过统一隔离入口执行：

```sh
python3 scripts/run-isolated-tests.py -- cargo test -p sigil --bin sigil acp:: -- --test-threads=1
```

其中 SDK fixtures 使用官方 SDK 的真实 Channel 连接和 loopback HTTP provider，覆盖流中取消及 durable receipt、物理重试、ResourceLink、不透明身份、重启恢复、跨工作区/篡改拒绝和旧上下文续问。它们不替代真实编辑器验收，也不证明真实远端模型成功率。完整产品验收应在独立编辑器 profile 中覆盖多会话、审批拒绝、Stop、断连后载入旧会话并继续；证据和版本应单独记录。
