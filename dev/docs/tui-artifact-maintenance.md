# TUI 当前会话的工具输出维护

后台 artifact GC 与工具 capture/read 共用当前会话已由 Resource Authority 签发的 ArtifactStaging / ArtifactStore 租约。当前会话尚持有租约时，GC 不再对同一命名空间重复 acquire：重复准入会被一次性 capability 正确拒绝，并产生真实的维护失败提示；它不等于已有完整输出已经丢失。

## 持有与校验

内部 TUI worker 的 `ManagedTuiArtifactStoreLease` 通过 `Arc` 持有原 runtime 物理租约；GC task 借用同一 owner 到真实 completion，不从裸路径创建新的 store。runtime 的 `garbage_collect_session_artifacts_with_lease` 属于 physical-host seam，不进入 application / kernel 公共语义。

该入口要求相同 writer 实例、完整 canonical session 源路径、实际 durable session scope，以及同一 key 的成对 staging/store namespace digest。legacy `.jsonl` 使用其 stem；managed `records.jsonl` 保留既有交互 host 的 stem 映射，并接受独立 lifecycle route 已有的父 namespace 映射。两种既有映射均须通过同源、同 scope、同 writer 校验；其它 namespace、混合的两类 key、已关闭租约都不能借用。已有输出不迁移或重写。

没有 held lease 的维护仍经原 authority 准入入口。提供了不匹配 lease 时直接失败，不静默换 namespace、重发 capability 或掩盖错误。session 切换/关闭仍先取消并 join 后台维护；task 自身持有原租约，避免 GC 尚未完成时释放物理 owner。

## 会话切换与共享物理 namespace

既有 managed 会话的文件名都是 `records.jsonl`，因此其工具输出使用同一个既有 stem namespace。切换到另一个这样的会话时，目标预备借用当前同 writer、同 staging/store channel 与 namespace digest 的已准入物理 owner；每个会话仍由正常加载并校验的 `Session` 提供自己的来源与 durable scope，分别建立 kernel store facade。不会重复消费 capability，不把前一会话的输出 ref 授予后一会话，也不移动或重写已有输出。

目标预备失败只释放借用的持有者，原会话继续可读写；成功安装目标前仍等待旧后台维护结束。最后一个 lease facade owner 释放时才结算成对物理租约；仍有 peer 时，显式 finalize 被拒绝，不能提前关闭另一会话的资源。

共享 namespace 的 inventory 与 GC 按 facade 的 session scope 过滤；实际删除前仍扫描全部有效 manifest，保护其它会话以及共用正文的引用。无法解析或验证的 manifest 会中止清理；没有可认证 session 绑定的孤立 blob、staging 文件和旧 trash 保守保留。本轮只移动已精确退休的本会话正文，新 trash 绑定本会话，后续 prune 只处理该 scope 的 trash；这不是对既有孤立数据的迁移或批量回收。

kernel 的普通 capture 与 fork 先构造完整 descriptor，再一次调用 `publish_artifact(descriptor, bytes)`。managed backend 在同一物理 operation mutex 内完成 quota 校验、body 写入或去重、body hash/长度复验及 durable manifest 提交，GC 不能进入两阶段之间；旧 split 入口在 managed backend 明确拒绝。失败只返回发布失败，尚无 manifest 的正文保守保留。该边界不依赖 hash pin 或重放猜配对，不承诺跨进程崩溃原子性；原有 authority admission 与 durable 结果追加顺序继续生效。

## 清理与验收边界

两条入口复用原 GC 实现：保留 active roots、pin/read hold、retire frontier、RA proof、generation 校验及 durable disable-before-delete / expired-after-delete；新增读取后的 durable session identity 校验。此修复没有改变审批、取消、队列 promotion、工具结果或错误展示。

定向回归包括 `held_artifact_gc_`：同真实 lease 连续两次 GC、其它 active call 的完整输出保留、不同 source/scope/writer/namespace 拒绝、managed 源既有 key 兼容；`gc_task_runs_behind_one_typed_completion_event` 覆盖借用 owner 和真实 typed completion。真实 worker 回归另覆盖活动 run → enqueue → cancel → promoted builtin read → Published → 该结果之后的真实 GC completion。测试必须通过统一隔离入口，由本轮统一 Cargo owner 执行；未运行前不记通过。

`shared_artifact_` 回归覆盖同 namespace 的两个 scope、互相拒绝读取、预备持有者释放、peer finalize 拒绝、最后释放后重新准入，以及 GC 保留 foreign/未知数据；`switch_managed_sessions_borrows_artifact_owner_and_preserves_both_outputs` 经真实 worker 切换验证原会话与目标会话已有输出保持可读。这些定向测试与 native `cua-driver` 复验分别记账，不代替真实产品验收。

`atomic_artifact_publication_` 通过实际物理锁及 body/manifest helper，确定性地在两阶段之间启动 peer GC，确认提交前 GC 无法完成、提交后共用正文仍可读取；另覆盖相同正文的多 ref、manifest 重放、退休后重放、body 漂移与 manifest 冲突失败。kernel 的 combined-only backend 回归验证 capture/fork 都走组合入口，且 manifest 失败不能返回成功 descriptor。

原生验收须在新 binary 再走取消/排队/实际读取与完整输出路径，确认后台维护成功且无该失败提示。只读 journal 的 Published、内部读取或测试通过都不能替代原生呈现。原生首次已观察到维护错误与 Published 正常可以同时成立，应分别记录。
