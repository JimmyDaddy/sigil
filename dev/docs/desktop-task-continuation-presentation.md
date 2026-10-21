# Desktop Task 暂停与继续呈现

用户暂停 Task 后，任务和原执行 admission 保留，继续仍作用于同一 Task。暂停过程中产生的 `provider_recovery_cancelled` 表示 provider 请求恢复已取消；只有同一运行出现真实暂停/取消终态，或 canonical 页面已确认该终态后，Desktop 才退休这条重复恢复提示。尚未结算的取消、其他运行，以及真实连接、效果对账或预算阻断仍保留注意提示与操作。新运行的 typed owner 建立后，旧运行的恢复重放不会重新占据当前提示区，审计历史不删除。

继续命令的成功回包、continuity owner 或 attach 响应只确定当前运行身份，不据此把 Task 改成“正在执行”。同一 Task 的真实执行阶段事件到达后，renderer 才替换旧运行的暂停/结束展示；事件早于 owner 响应时，先按运行与 Task/entity 槽保留，待身份确认后按原运行序位归并。真实阻断或失败阶段也按原事实呈现，不制造执行成功。旧运行、其他 Task、无 owner 证明或非 durable 预览不能替换当前执行阶段，已完成/取消的 Task 也不能被旧阶段重新启动。

Task 卡片的暂停入口同时要求当前活跃运行 ID、该 Task 的执行事实运行 ID、原 admission 与真实运行阶段匹配。普通 Chat 的忙态不会让另一个已暂停 Task 显示为执行中；有其他运行时也不能再次提交继续。此处仅改变 renderer 呈现与现有控件资格，不修改模型维护的任务状态、清单或后端暂停/继续校验。

组件与归并回归覆盖原生观察到的 `RunStarted → TaskPhaseChanged(execution/running, started) → Checklist`，包括 owner 回包前后两种顺序、同 admission 暂停请求、旧运行/其他 Task/非 durable/已完成负例、真实 blocker 与恢复注意保留。原生暂停后提示退场、继续后待审批时 Task 状态和暂停入口仍需最终候选复验。
