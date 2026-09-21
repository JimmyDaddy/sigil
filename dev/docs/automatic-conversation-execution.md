# 普通 Auto 执行契约

普通新请求直接进入共享 agent loop。Desktop、TUI、CLI 与 HTTP 共用 runtime 装配和 kernel 语义；代码不从自然语言关键词猜测功能，也不先强制模型进入单独的路由轮次。

首轮同时提供实际业务工具、可用的用户澄清工具，以及当前 capability 允许的正向 Task/PlanReview 操作。模型可以直接回答、先查资料、使用普通工具，或在任意后续轮次选择转交；一个范围清楚、只需一次局部编辑的小改动可以留在当前对话，不因出现文件修改就创建 durable Task。需要跨文件协调、多步骤持续执行、委派或 durable progress/recovery 的请求由模型选择 `start_task`，再由 durable Task 执行与验证。没有调用某个操作工具时，已有 Plan 或 Task 保持原样；查询状态也不要求继续执行。

模型按用户要求的结果选择操作，不靠关键词或文件数路由。一个自包含、只需一次局部编辑的小改动可以直接在当前对话完成。需要多文件/多工作流协调、持续多步骤执行、委派或耐久进度恢复时，模型才选择 `start_task`；可以先做只读检查，但必须在第一次写入或验证命令前启动 Task。`request_plan_review` 用于用户要求先看计划/设计，或确有重要未决选择妨碍安全执行的情况；用户已明确要求实施且范围充分时，不要把它当成常规前置步骤。

`start_task({title?})` 只把当前用户目标接入 direct Task，标题可选且仅用于显示；它不生成计划、不调用另一个 planner。`continue_existing_task` 和 `run_pending_plan` 分别保留其真实操作语义。模型只选择操作，host 负责绑定 durable 对象身份、校验 CAS/权限、等待未完成的工具与 child join，并追加审计记录。转交不增加权限、不绕过审批，也不把先前的失败改写成成功。

正向转交必须单独成批。与其他工具混用时整批返回可纠正的结构化错误，整批均不执行。普通正文和业务工具照常显示；内部控制结果只进入审计与后续模型上下文。Desktop、TUI、CLI 与 HTTP 不保留旧规划工具名、旧 planner/discovery/synthesis 阶段或历史空上下文恢复分支。

自动转交的上下文绑定 exact source message、转交前已结算的工具批次和 append-only receipt。不得混入后来追加的 parent 消息或未结算的 handoff 批次；queued source 仅在 durable promotion 身份匹配时加入。PlanReview 的研究与提交在同一个普通模型循环中进行：提交校验失败作为工具错误返回，模型可继续查资料并再次提交；没有固定 finalizer 阶段或额外修正轮数。完整的普通最终答复可作为未确认候选结束审阅，但不会生成 ready Plan 或授权执行；typed 提交才会将完整草案提升为 ready Plan。

首轮冻结请求按当前工具 schema、权限、interaction、source、消息、模型和执行契约校验。恢复只补本地 admission 缺口，不重放 conversation provider 请求或已经执行的 effect。同一循环提升新的 queued follow-up 后，旧 source 的转交入口撤下；新独立 run 会重新装配自己的 source authority。

Manual 不注入自动规划入口；显式 `/plan`、`/task` 与只读、取消、审批边界继续遵循各自的操作契约。
