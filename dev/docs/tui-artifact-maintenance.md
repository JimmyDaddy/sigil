# TUI 当前会话的工具输出维护

后台 artifact GC 与工具 capture/read 共用当前会话已由 Resource Authority 签发的 ArtifactStaging / ArtifactStore 租约。当前会话尚持有租约时，GC 不再对同一命名空间重复 acquire：重复准入会被一次性 capability 正确拒绝，并产生真实的维护失败提示；它不等于已有完整输出已经丢失。

## 持有与校验

内部 TUI worker 的 `ManagedTuiArtifactStoreLease` 通过 `Arc` 持有原 runtime 物理租约；GC task 借用同一 owner 到真实 completion，不从裸路径创建新的 store。runtime 的 `garbage_collect_session_artifacts_with_lease` 属于 physical-host seam，不进入 application / kernel 公共语义。

该入口要求相同 writer 实例、完整 canonical session 源路径、实际 durable session scope，以及同一 key 的成对 staging/store namespace digest。legacy `.jsonl` 使用其 stem；managed `records.jsonl` 保留既有交互 host 的 stem 映射，并接受独立 lifecycle route 已有的父 namespace 映射。两种既有映射均须通过同源、同 scope、同 writer 校验；其它 namespace、混合的两类 key、已关闭租约都不能借用。已有输出不迁移或重写。

没有 held lease 的维护仍经原 authority 准入入口。提供了不匹配 lease 时直接失败，不静默换 namespace、重发 capability 或掩盖错误。session 切换/关闭仍先取消并 join 后台维护；task 自身持有原租约，避免 GC 尚未完成时释放物理 owner。

## 清理与验收边界

两条入口复用原 GC 实现：保留 active roots、pin/read hold、retire frontier、RA proof、generation 校验及 durable disable-before-delete / expired-after-delete；新增读取后的 durable session identity 校验。此修复没有改变审批、取消、队列 promotion、工具结果或错误展示。

定向回归包括 `held_artifact_gc_`：同真实 lease 连续两次 GC、其它 active call 的完整输出保留、不同 source/scope/writer/namespace 拒绝、managed 源既有 key 兼容；`gc_task_runs_behind_one_typed_completion_event` 覆盖借用 owner 和真实 typed completion。真实 worker 回归另覆盖活动 run → enqueue → cancel → promoted builtin read → Published → 该结果之后的真实 GC completion。测试必须通过统一隔离入口，由本轮统一 Cargo owner 执行；未运行前不记通过。

原生验收须在新 binary 再走取消/排队/实际读取与完整输出路径，确认后台维护成功且无该失败提示。只读 journal 的 Published、内部读取或测试通过都不能替代原生呈现。原生首次已观察到维护错误与 Published 正常可以同时成立，应分别记录。
