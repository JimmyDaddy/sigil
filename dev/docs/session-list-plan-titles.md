# 显式 Plan 的会话列表标题

显式 `/plan` 不创建供 provider 使用的普通用户消息；原始目标由同一会话的 `PlanReviewAttempt`、`source = ExplicitPlanCommand` 和 `explicit_objective` 持久保存。列表投影在没有真实用户消息标题时，可以把这个目标作为展示标题，使只有计划的会话能够通过目标中的词语搜索。

真实 User 或已提升跟进消息的标题始终优先，和它们相对 Plan 的事件先后顺序无关。生命周期中已有的手动名称、系统生成名称继续覆盖普通投影标题，手动名称优先级保持不变。其他会话绑定、自动 Plan 路由及缺少目标的记录不提供这个后备标题。

标题复用现有安全清理和长度限制。这项投影不添加 UserMessage，不改变 provider 上下文、Plan 审阅/执行资格、审批或 append-only journal，也不把计划正文全部放进搜索索引。

会话目录投影 revision 更新为 6；旧版未命名缓存通过现有按工作区 reconcile 机制重建，无需删除用户缓存或修改原始会话。暂态读取失败和真实无效源的既有处理边界保持不变。

本地资格由 kernel 的 `session_list_explicit_plan_title_` 与 runtime 的 `catalog_explicit_plan_title_` 用例验证；真实 Desktop/TUI 展示和搜索需要另行原生复验，不能由这些测试宣称通过。
