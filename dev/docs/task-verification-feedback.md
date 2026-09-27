# Task 终态验证失败的就地修复

本页定义 direct Task 的候选 final、真实检查与模型修复之间的边界，对应 2026-09-27 竞品优化报告 A5。报告保存在本地 `.repo-local-dev/review/`，稳定行为以本页和源码为准。

## 用户行为

Task 已有明确的 trusted checks，且策略允许 `trusted_only` 自动运行时，模型给出候选 final 后，现有 agent loop 执行这些检查。真实失败的命令身份、回执、源码快照与有界错误摘要会回到同一个 Task 的模型上下文；用户无需再发送一次“测试失败，继续修”。

模型通过 `respond_to_task_verification` 选择 `repair` 或 `blocked`，需要答案时使用现有 `request_user_input`。`repair` 可以和普通修复工具位于同一批调用中，决定必须在修复调用之前；host 不增加必经的单独决策轮次。`blocked` 暂停本轮并保留失败事实；请求输入沿现有用户输入生命周期暂停。

修复后再次出现候选 final 时，执行同一批当前有效的 trusted checks；只有真实通过的证据满足既有 completion criteria 才能完成 Task。首次检查成功不调用模型第二次。没有声明检查的普通 Task 不因此获得新检查；Chat、审批策略为 manual 的检查、未满足信任条件和未知副作用均保留原处理方式。

先前通过的检查所覆盖的源码再次变化时，`Stale` 会明确要求从第一项检查重新验证；每项真实通过后，原 readiness reducer 再推进其余要求。完成判断也独立拒绝 `Stale`，不能把缺失的动作提示当作检查通过。未知写入、未满足的信任条件和实际待决副作用仍需要各自的既有处理。

## 单一执行与权限所有者

实现留在原 `Agent` 循环，`DirectTaskRuntime` 仍只运行一个 executor。修复不创建第二个 Task、子循环所有者或独立 permission owner。

- `TaskVerificationFeedbackV1` 只保存 Task/admission/attempt、失败 receipt id、策略与快照 hash、模型选择和有界原因。命令输出通过既有 verification 索引和真实 `CheckFinished → CommandFinished` 来源读取，不复制到新的持久状态。
- `feedback_id` 由完整来源绑定确定，模型不能传 Task、receipt、hash 或路径来选择另一份执行许可。
- 选择 `repair` 只表示模型愿意处理失败，不授予写入、进程、外部目录或网络权限。工具仍经过原有权限、预览、审批和实际 effect 前置条件。
- 每次消费反馈前重新校验 Task admission、当前 attempt、策略、检查回执和相关源码快照。来源或策略漂移时不能沿用旧反馈；完成边界仍重新计算 readiness。
- 只有已记录的、非写入检查产生的真实失败证据进入修复路径。inconclusive、unknown、检查自身修改验证范围或待审批不会被推断为可自动修复。

修复继续复用原 Agent 的 `max_turns`、取消和资源预算，不设第二个“最多三次修复”门槛。现有命令输出是有界预览，无法据此可靠证明两次失败完全相同；反馈将进展标为 unknown，host 不猜测模型是否取得语义进展。模型忽略已交付的同一反馈并重复 final 时，本轮暂停；确实发起新的修复及检查则由原 run 预算约束。不同检查逐项改善可以继续，用户明确继续 Task 仍沿已有 continuation 入口。

## 取消与恢复

检查执行使用原 Task root cancellation handle。managed verification port 将该 handle 传给现有资源/进程所有者；取消后等待实际 check 退出、审计与资源结算，不能把响应超时写成成功取消或 Task 完成。

反馈和 typed 选择 append-only 落入当前 schema 的 session control。重启后：

1. 若已有精确 Pending 反馈，来源仍有效，且反馈之后尚无物理 provider 调用、历史也无未决物理 attempt，可以启动由该反馈 id 派生的唯一 continuation turn。
2. 若反馈之后已有物理调用，恢复使用其确切 logical id；无法确认发送/结果时仍走原 provider recovery，不补发一个新的调用。
3. 恢复不能把最初的 candidate final 当成新响应重放，不能重新授权已改变的源码、策略或另一 Task。

## 验证证据边界

`task_verification_feedback_*` 的离线回归使用 scripted provider 来固定模型选择，但实际调用 application Task continuation、managed 文件工具、真实 Python 检查子进程和 durable JSONL。它验证控制流及权限，不代表真实模型的修复成功率。

真实模型资格化与强制单独决策轮次的消融在本地 `.repo-local-dev/review/competitor-optimization-2026-09-27/A5-execution.md` 留存。不能仅从 fewer provider calls 推断时延收益，也不能把 fixture 的通过率扩大为一般仓库任务的成功率。
