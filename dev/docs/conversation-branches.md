# 对话分支的用户入口与身份绑定

对话分支沿用 kernel `ConversationForkProjection` 与 runtime
`LocalSessionLifecycleService::fork_session_at_turn`。它复制选择的已完成回合及之前的安全对话历史，
在目的 session 记录来源；不重写源对话，不复制活动 Task/approval authority，也不恢复工作区文件。
source session identity、exact turn digest、目标 connection/model 与目的幂等键仍由共享 lifecycle 校验。
checkpoint、文件快照与恢复预览不构成普通对话分支的前置条件。

## TUI

`Alt-B` 在空输入框或活动区焦点下打开当前会话的回合选择器。编辑中的输入框保留原有按词移动。
选择器消费共享 `application_conversation_recovery_view`，只呈现已 finalized 的 turn、来源 session
和最多 360 字符的安全 prompt preview。预览只用于选择，不能替代 exact digest。

`Up/Down` 选择，`Enter` 将当前 source session、所选 digest 和当前显式 model ref 转为既有
`ApplicationRecoveryAction::ForkConversation`。worker 使用同一个 lifecycle fork，再通过已有
session transition 重新装配新 session。fork 完成后回到可编辑 composer，保留原草稿，不调用 provider，
也不提交下一条 user message。活跃前台、后台或 terminal owner 的切换约束继续由 transition 校验。

选择器只保存进程内展示状态。load/result 用 request id 和源 session 绑定；关闭、刷新或切换后的
迟到结果不能覆盖当前视图。fork 正在提交时保留该操作视图直到真实成功或失败，避免源 worker 已
切换而 UI 丢掉回执。失败保留草稿与可重新选择的回合；成功后复用原 worker rebind 流程。

## 验收边界

- 无 checkpoint 的普通对话可选择更早的已完成回合，后续回合不被复制。
- 无关工作区文件变化不阻断分支；实际 source identity/digest 或目标 route 错误仍拒绝。
- 分支不调用模型、不改变源 transcript、不自动提交草稿；共享文件继续由正常工具权限处理。
- App、application adapter 与真实 worker/lifecycle 的测试分别证明展示、typed 转换与持久行为，
  不把纯 UI fixture 当作真实 provider 或完整产品运行证据。

## Desktop

已持久化的用户/助手消息旁可选择“从这里分支”。该入口刷新同一共享 recovery projection，
按消息的 durable stream sequence 定位所属已完成回合，显示来源会话与安全 prompt preview；
未完成回合明确提示等待完成。异步返回与源 workspace/session、当前选择绑定，旧响应不能覆盖新选择。
确认沿现有 exact digest/model 的 fork 命令打开新会话，不启动模型。失败保留所选回合；
原会话草稿留在原会话，新分支使用独立草稿。

projection 对活跃 session 使用已有协调 read observer，不重新打开 writer，也不因当前 session
持有自己的 writer 而拒绝读取。它仍逐条校验 durable source scope。

## 幂等创建与中断恢复

Desktop 与 TUI 的展示 request id 只关联界面结果；目的分支身份使用已准备 application operation 的 K/F
派生身份。因此同一 durable 请求重试回到同一分支，应用重启后新的请求不会因为展示计数器重复而
重新打开旧分支。恢复目的时还会核对它实际记录的创建模型路由，不能以新的目标参数描述旧模型分支。

目的 session 的创建身份、composition、ConversationForked 来源、完整安全前缀与重新绑定的外部
provenance 通过现有 writer append intent 作为一批提交。中途写失败后，同一请求重试先让目的
writer 恢复该批，再校验实际复制内容与 exact source。仅有 marker 或预期计数、缺少可恢复完整批的
旧残缺记录不能作为成功证明，也不会自动重写。已创建分支后来追加的正常对话不影响其初始复制证明。

### 当前受管存储路径

Desktop 与 TUI 的消息分支使用既有 SessionLog authority 分配目标；catalog 的
`<namespace>.jsonl` 是逻辑引用，真实流仍位于受管 namespace 的 `records.jsonl`。
逻辑引用只记录谱系，不用于绕过目录、writer 或资源授权。源会话通过 exact scope 的
只读快照选取已完成 turn，目标身份、composition、完整 transcript 和谱系仍由同一初始
原子 bundle 持久化；相同操作恢复同一目标，不同意图分配不同目标。

存在已发布工具输出时，复制通过源 owner 的现有 artifact facade（空闲时才取得既有
受管 lease）及目标 artifact lease 完成，目标得到新的会话范围/ref，字节/hash与来源一致。
纯文本 prefix 不要求 artifact grant。所有新取得的资源在成功、失败后都显式结算；
legacy 文件式 API 仍保留原路径绑定检查，不将其作为 managed 引用解析器。

### HTTP 回执丢失与部分写入

HTTP 同样使用已准备 operation 的 K/F 派生目的身份。目的创建进入 writer 后的错误保留
`Uncertain`，不能据普通 I/O 错误签发“没有副作用”。同一请求查询先验证原 Prepared 绑定；
缺少源侧完成证明时只恢复已经存在的目的流，校验原 append intent、完整复制内容、source 与
创建 route，再由原 source owner 追加 `ConversationForkCommitted`。目的不存在时保持未知，
查询不会创建新分支，也不会自动再次派发原用户动作。

首条 record 尚未写出的合法初始 bundle 也由 kernel 原 intent 恢复；允许该恢复的现有 mutation
准入仍要求原 namespace marker、private regular record 与 writer lock，不能创建缺失对象。
没有有效原 intent 的空流仍拒绝，read-only recovery 保留原非空要求。

SessionLog 已认证 handle 在 finalize 失败后，仅释放该次活跃 holder；保留 durable quota、
poison 和原错误，不生成成功 receipt。runtime 在尚未进入 authority 的物理 frontier 错误也释放
原 holder。后续恢复必须重新准入并验证实际 frontier，不通过 UI 回执或错误文案伪造完成。
