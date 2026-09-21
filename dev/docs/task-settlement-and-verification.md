# Task 完成与验证职责

Desktop/TUI/HTTP 复用 kernel 的执行结果归约。普通 coding task 使用实际工具、可选进度列表和自然最终答复；模型不再提交完成声明、来源集合、证据 frontier 或原文 UTF-8 span。

## 执行结束与进度

`AgentRunOutcome::execution_disposition` 分类实际执行结果。direct、step 与 synthesis 需要有效的最终答复，并继续消费审批、取消、当前 recovery/effect blocker、明确选择的验证及依赖状态。已通过替代方案解决的历史审批拒绝保留审计，但不永久判失败。最终文本或 checklist 的 completed 均不能清除真实阻塞。`TaskChildSessionStatus::Blocked` 保留可继续语义，不伪装成完成。

任务执行器可调用 `update_task_checklist`，参数仅为 `items: [{text, status}]`。状态为 `pending`、`in_progress`、`completed`；支持空列表、单项、多项以及多个进行中项，最多 32 项。Task 身份、条目身份和 revision 由 host 绑定，模型不用复制内部 ID。只有模型调用才会更新 checklist；host 不按执行生命周期自动开始、完成或清空条目。更新追加到 session/control 日志，Desktop/TUI 消费既有 Task checklist 投影并展示所有进行中项。

Checklist 仅展示模型报告的进度，不授予权限、不驱动依赖调度，也不证明验证通过。缺失或无效更新不会封锁其他业务工具和最终回答；无效参数仍返回结构化工具错误。host 不在任务结束时自动勾选模型遗留的未完成项。并行 participant 不共享写入根任务的同一列表，既有 plan step 状态继续展示执行进度。

## 恢复与历史记录

`bind_direct_task_requirements` 和 `task_completion_claim` 不再作为运行工具提供；删除其来源绑定、完成证明、修正回合、专用暂停以及 worktree capture/restore 执行路径。新 participant result 不携带 completion claim。

已持久化的 claim、requirement baseline、rejection receipt 和 workspace capture 保留原数据结构与读取校验，capture 指向的 mutation artifact 继续受引用保护。这些历史记录不重新启用旧模型协议，也不自动变成新运行的成功证明。

普通 provider 恢复继续要求原 durable schedule 与 authority，不能仅凭旧 participant 的 Started 状态重放业务执行。启动清理保留尚未成功完成的 participant 或明确 Retained 的未清理工作树，失败、阻塞、取消或中断不会解除该保护，明确物理清理完成后才退出；不根据模型声明推断内容已安全保存。此变更不改写用户会话、不自动修复历史暂停任务。

## 检查目录与必需检查

配置/发现/信任 promotion 只建立 runnable CheckSpec 目录。`VerificationStateProjection::selected_policy` 在 scope 间合并已有 explicit policy，保持父级 required、trust 和其他约束；没有 policy 就得到合法零项集合。Direct、普通 run、step 与 check runner 都复用该选择规则。

accepted step 的 `check_spec_refs` 在 participant dispatch 前解析到实际 CheckSpec，追加同一个 `VerificationPolicyChangedEntry`，通过 `requirement_sources` 记录原 contract/version/index/digest 和既有 policy 来源。来源只解释同条 policy 中的 required 集合，没有独立生效状态；目录刷新不会覆盖已接受的 policy。找不到声明的检查返回 typed `UnresolvedVerificationRequirement`，不会降低为零项。原有无 provenance 的合法 policy 保留原 required 强度。

同一 Task/step 的新 accepted 且完整 committed contract set 可以替代其已 superseded 的旧版本来源。materializer 先核验原 version/index/contract digest，再移除旧来源的贡献并绑定新来源；新版本零项也会发布撤销后的 canonical policy。同一 check 仍有其他 owner 或既有 policy 来源时继续 required，原 trust、sandbox、timeout 和 skip 限制保持。缺失或未完整提交的新 contract set 不能用来撤销旧要求。替代不修改历史记录，重开从同一契约与 policy 日志归约，重复 materialize 不追加第二条相同 policy。

公共 readiness 先检查真实 trust、当前 pending check 与 `UnknownDirty`，再处理零项或已授权 skip。零项返回 `NotApplicable`，不会因为已知写入要求新增检查配置；真实未知 effect 即使有 skip 仍需要 owner 对账。有 required checks 时仍要求正确 scope、check/policy hash、snapshot 和 freshness 的 receipt。

`WorkspaceKnowledge::SnapshotUnavailable` 仅表示有界观察不完整。snapshot builder 不再凭缺少完整 manifest 合成 `workspace_snapshot_incomplete` mutation。普通 symlink/大文件等观察限制不能把已知写入变成未知 effect；原 mutation/recovery owner 产生的真实 `UnknownDirty` 和无法认证的历史未知事实仍保留阻塞语义。

## 验证边界

回归覆盖 direct、串行/并行 step 和 synthesis 无 claim 完成；首轮实际工具可用；单项 checklist 更新和重开读取；无效进度参数不封锁后续工具；host 不自动勾完未完成项；实际阻塞、取消、required verification 继续生效。真实 Git worktree 测试覆盖重开后的未结算副本保留，历史日志测试覆盖读取、损坏数据校验及 compaction 后引用保护。

检查选择回归继续覆盖 accepted revision 撤销旧来源、既有共源与安全限制保留、同 check ID 的新 hash 重新绑定，以及未完整提交的契约无法撤销旧要求。provider strict schema 与工具流完整性独立于完成声明，详见[工具流完整性](provider-tool-stream-integrity.md)。

这些测试验证执行与持久化契约；它们不证明模型输出的代码正确，也不替代真实模型和 Desktop/TUI 产品验证。
