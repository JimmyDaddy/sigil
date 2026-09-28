# 项目插件审查与进程清理

项目插件沿现有信任、工具审批与受管进程生命周期工作。信任一个插件不等于批准所有工具调用；撤销信任也不能单独证明之前启动的进程已经停止。

## Desktop 的清理状态与重试

Desktop 设置页的项目插件面板通过当前会话的插件目录展示声明与信任，审查操作继续绑定显示中的 manifest hash 和 capability digest。运行中的会话也可以审查插件；不需要先结束普通任务。

面板分开展示信任与先前进程的清理状态：

- `confirmed`：host 已确认相关先前进程停止。
- `unknown`：现有观察无法确定先前进程状态。
- `unconfirmed`：请求的清理尚未确认完成。

清理字段缺失不构成停止证明。目录是跨操作的权威投影；单次操作返回 `confirmed` 不能清除目录里更早的未确认记录，重新启用后的空字段也不能清掉旧警示。目录刷新失败时保留已知未确认提示，并提供刷新入口。

信任已撤销但清理未确认时，原停用按钮显示“重试清理”，再次发送现有精确插件审查动作 `ReviewPlugin(enabled:false)`。只有目录明确确认停止后才收起这个重复清理动作。重新启用时，旧清理警示仍独立显示；此时“停用”仍明确意味着撤销当前信任，不伪装成仅清理旧进程的动作。客户端不持有进程、PID、文件路径或第二个清理 owner。

“插件决定已保存”仅说明信任决定，不意味着全部进程已经结束。停止确认来自 host 的受管资源结算与持久化事实，不能由 `enabled:false`、本地超时或父进程退出推导。

## 信任事件与清理证明

`PluginTrustDecision` 只撤销后续调用的信任。完整 ReviewPlugin 命令由后置
`PluginReviewCompletedV1` 与原 application K/F 的因果批次提交；后置结果引用
`append_controls_with_events` 返回的真实信任事件 ID，不按时间戳关联。
信任写入返回不确定错误时仍收尾全部已捕获初始化和已发布 generation，但不会伪造后置回执。
已确认的信任写入可以返回 `unconfirmed` 清理结果；这不等于进程停止。

目录按最新停用事件关联结果，迟到的旧回执不能覆盖新停用。清理判断读取已有
`ExtensionProcessLifecycleRecorded` 的插件来源、有效 scope 和 generation；只有同代
Stopped 能关闭已知进程。重启后空 weak registry、无来源记录或另一个 generation 停止
都不能补造旧进程的停止证据。无范围证据的旧缺口继续显示 unknown，保持功能可用。

TUI 的 Plugins 面板读取同一 durable 投影，在 warnings 中显示 unknown/unconfirmed；
已有 Disable 操作可再次尝试。当前工作无需全局 idle，启用后的工具按既有刷新路径可用。


### 插件技能的目录与读取

Desktop 的技能目录、精确调用绑定、TUI 技能选择及 `load_skill` 共用普通技能与当前会话已信任插件的合并索引。插件技能保留 `plugin-id/skill-id` 身份，不复制到普通技能目录。未显式声明技能 trust 时继承外层精确 manifest 的审核；技能显式 `disabled` 或 `needs_review` 仍保留，普通技能的默认信任不变。信任决定保存后目录立即可见；运行中的工具快照在现有刷新点更新。

模型实际加载插件技能前会重读会话信任和当前 manifest，再核对原技能描述及内容 hash。停用或 manifest 漂移会拒绝旧插件技能读取；普通技能与其他工具继续可用。
