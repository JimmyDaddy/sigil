# Plan / Task / Execute 消融

## 目标与最小闭环

本次以 2026-09-11 工作区中的生产路径为基线，按用户要求删除不提供独有能力或保护的抽象、鉴权和验证，修复无法继续的产品路径。此前的快照、模型目录、诊断阻断修复保留为基线。

用户的最小闭环是：提出任务 → 必要时研究并展示计划 → 保存、修改、拒绝或批准 → 执行 → 遇到失败可补充信息、重试或取消 → 如实展示结果。普通执行只装配实际使用的执行角色；计划是可读的工作约定，显示清单不授予执行权，也不决定重复审批是否有效。

职责只保留在实际发生的位置：

- 研究会话保留已经读取的代码、工具结果和生成的文字；受限收口回合只提交 `draft` 或 `no_plan`。
- 本地计划决定核对目标计划及会话的持久事实，不以当前模型连接、最新显示卡片或编译缓存决定是否可操作。
- 批准原子记录用户决定与稳定 Task 身份。重复请求返回同一 Task，执行进度不改变审批身份。
- 执行器在实际使用 provider、工具、验证或工作区时处理其前置条件。未使用角色和未采用的仓库候选不阻断执行。
- kernel 统一裁定 Task 是否完成；资源结算、取消胜出、真实写入冲突和必需验证失败仍不能被解释为成功。

## 消融清单与依赖

| 编号 | 消融对象 | 实施范围 | 实验与反例 | 依赖 |
| --- | --- | --- | --- | --- |
| B01 | 本地决定前的 provider 恢复和 composition 门槛 | runtime application control / Plan 决定 | 连接移除或功能配置改变后仍能保存、拒绝及批准；错误会话/Plan hash 不写入 | 独立 |
| B02 | Desktop 重新解释 allowed actions、无候选时隐藏重试 | PlanCard / ConversationPanel | 无 draft/candidate 的失败规划可重试；busy 与未提供的操作仍禁止 | 独立 |
| B03 | 可变 checklist 参与 Run 幂等鉴权 | PlanExecutionService | 执行推进清单后重放审批返回原 Task；错误内容/权限仍拒绝 | 独立 |
| B04 | 最新展示卡片作为目标 Plan 的操作权限 | runtime Plan action | 生成新 Plan 后旧 Plan 的合法决定仍可提交；已批准/替代/拒绝或正在修订的 Plan 不被误操作 | B01 |
| B05 | Revise 自动复用失败指导 | runtime / TUI 修订入口 | 失败后重新输入要求；旧命令重放不能覆盖新一代输入 | B04 |
| B06 | 未使用的 compile input、重复提交前校验 | runtime / TUI / application | 文字计划直接持久化；错误 source/attempt、冲突 draft 仍拒绝 | 独立 |
| B07 | 新建 finalizer 会话并重建缩减上下文 | runtime Plan generation | 原研究全文与工具历史进入同会话受限收口；未知文字不自动批准，收口不能研究/写入，取消后不新发请求 | B06 |
| B08 | 未广告的旧 Plan 提交协议 | kernel agent | 当前 `draft`/`no_plan` 正常；旧工具名不能创建计划或 Task | B07 |
| B09 | direct Task 初始化全部未使用角色 | runtime Task role construction | 仅 Executor 可用即可开始及继续 direct Task；Executor 实际故障仍准确返回 | 独立 |
| B10 | 尚未有 TaskPlan 时拒绝用户指导 | runtime / kernel continuation | 指导持久化并进入 planner；重启不丢失，物理请求恢复不被新文字改写 | B09 |
| B11 | 未采用的仓库验证候选解析作为执行前置 | runtime verification materialization | 损坏但未采用的配置不阻断；显式必需检查仍约束完成 | 独立 |
| B12 | runtime 子 runner 重复裁定根完成 | runtime child runner / kernel completion | 完成依据集中于 kernel；未结算副作用、活跃参与者及取消竞态不能误完成 | B09 |
| B13 | 零生产调用的旧 candidate compiler / adoption generator | kernel Plan / intent materialization / test-only factory | 删除旧算法；固定历史 fixture 仍可投影、校验和恢复 | B06 |
| B14 | 发布评测清单作为自动 Task 的运行许可 | runtime / TUI routing / model eval / Doctor | 无清单但 executor 可用时可路由 Task；无 executor、无工具能力、Manual 和 durable kill switch 仍限制能力 | 独立 |
| B15 | 控制读取隐式触发启动恢复 | kernel Session / runtime control loader | 活跃修订、工具调用及取消请求在控制读取后保持原状态；真正启动恢复仍结算中断工作，空会话和损坏日志仍拒绝 | B01 |

生成与执行两组实现可并行；主代理负责决定语义、跨表面闭环、集成和验证。共享文件按函数分配修改范围，保留工作区已有改动。

## 实验方法

逐项删除候选层，用真实生产入口的定向回归证明正向流程，并保留该层可能保护的反例。测试通过统一隔离入口，不使用用户的配置、凭证或 state/cache。先前全量测试已按用户要求停止；本轮只运行证明具体消融结论的定向实验，不重复全工作区测试。

未使用的实现应连同调用端一并删除；仍用于已持久化事实读取与恢复的格式，不以“旧”字为理由删除。没有运行真实在线 provider 或 GUI 时，不能把离线 request/dispatch 证明写成在线产品验证。

实施与验证结果记录在 `.repo-local-dev/ablation/plan-task-execute-2026-09-11.md`。
