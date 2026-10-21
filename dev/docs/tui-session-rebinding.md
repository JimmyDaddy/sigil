# TUI 设置保存后的会话重绑定

设置保存可重建当前会话的 worker；随后新建或切换会话时，应用服务必须绑定目标会话的新作用域。该流程继续使用本次启动已有的 managed command journal owner，不能因为旧服务还保留命令恢复资料而再次申请相同 namespace。

`TuiApplicationJournals` 仅属于内部 TUI host：

- command reservation store 在同一 authority writer、application、subject 和 workspace 内共享，保留原命令的 K/F、CAS、恢复及审计记录。
- delivery ACK 按完整 `ApplicationScope` 隔离；同作用域重绑定直接克隆缓存的强 owner。其它仅由缓存持有的闲置作用域在同一 mutex 内完成析构与租约释放后移除，避免最后一个观察者析构与重新申请并发，也不永久保留历史作用域。跨工作区、subject、application 或 writer 的重绑定被拒绝。
- session projection、worker endpoint、会话目标绑定及实时权限继续由每个会话服务各自持有。共享日志不授予新的会话或工具权限。
- 已完成且确认激活的配置切换控制器，只在界面已离开原会话、旧 worker 的实际 shutdown/join 成功、原控制器未持有 worker 或清理失败时退休，释放原会话 attachment。未完成、未确认激活及失败恢复 owner 继续保留；不修改 session 独占规则或丢弃命令恢复资料。
- host 装配失败保留原错误，并标记 worker 未就绪、清除乐观运行状态；这类本地失败不再凭空生成 provider 连接故障。若失败后没有 worker，再次提交会保留草稿并明确拒绝，不创建永久 Preparing 或向失效 endpoint 排队。

回归覆盖两次真实配置 CAS 保存及 `ConfigSaved` 共有 transition、新建会话、保留旧服务时返回旧作用域、跨工作区拒绝，以及新会话通过本地脚本 provider 实际执行内置读取、取消下一次真实请求并等待 durable 取消终态。另验证 ACK 最后观察者释放后的同作用域重绑定，以及闲置缓存移除完成后实际 RA namespace 可以重新申请。单元 application port 有意关闭物理配置发布，因此 fixture 明确调用真实配置 CAS publisher 后再消费 typed publication；真实 F3 保存入口仍需不可变候选的原生复验。

既有新建/切换会话 command 只返回精确 `Uncertain(EffectStarted)`，没有独立的 typed operation settlement target。本批不把该回执改写或宣传成 `Settled`；回归保留原 command id、K/F 与恢复阶段断言，再等待匹配路径的 `NewSessionStarted` / `SessionSwitched`、attachment 转移及新作用域 Ready。

取消回归使用真实 Ctrl-C 输入返回 `CancelRun`，进入 production 共用 handler 的当前 worker stop owner 路线，并核对 durable Requested/Finalized 的同一 request、run scope、取消终态和零存活资源；不直接构造运行绑定或伪造 `RunStarted`。

定向验证：

```sh
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib configured_session_lifecycle_tests -- --test-threads=1
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib attachment_failure_is_not_provider_failure_or_a_ready_run
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib tui_journal_delivery -- --test-threads=1
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host --lib runtime_transition -- --test-threads=4
```

测试通过不代表真实模型或原生终端已复验。完整资格以本轮不可变候选的“F3 保存 → `/new` → 真实读取 → 取消/恢复”证据为准。
