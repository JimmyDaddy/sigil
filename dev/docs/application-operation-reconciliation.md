# Application 操作与领域提交对账

Application 的 reservation 保留原始 K、F、预期 frontier 和日志代绑定。Worker 收到命令、公开事件已经发布、界面呈现了目标值，都不能替代领域提交。`Uncertain` 重试先查询实际 owner；只有 owner 的 durable proof 才能收敛到提交完成。

## 领域 owner 与原子批次

已附着的 `Session` 发行 `SessionApplicationOperationOwner`，其中的 writer/read capability 随 attachment 生命周期转移。Runtime 和 TUI 不从 session 路径重新推导此 authority。Application adapter 将完整 K 的 canonical digest、F、session scope 和结构化精确目标绑定为稳定 operation id；kernel 不依赖 application crate，也不解析用户文本决定目标。

普通受管 queue、plan 与 user-input decision 操作按以下顺序推进：

1. 实际 Session owner 写入 `ApplicationOperationPreparedV1`，锁定该 K 的 F 与目标。相同准备可复用，漂移被拒绝。
2. Application 进入 effect 阶段并持有 RA forward guard。TUI envelope 带实际处理 acknowledgement；后台 admission 线程等待 worker 穿过真实 dispatcher。channel enqueue 本身不会释放这一保护边界。
3. 领域校验成功后，原有业务记录与 `ApplicationOperationCommittedV1` 进入同一 crash-safe bundle。Marker 引用该批次真实 event id 和 canonical payload digest，不能在提交后另补一条记录声称原子性。
4. Application 查询原 K/F 对应的 owner 证据。对账核验 Prepared、连续的真实业务事件、payload hash、精确领域目标及 marker 后，使用实际 durable frontier 生成 receipt。

HTTP queue 使用同一 owner 的 `append_queue_mutation`，保留原 queue revision CAS，业务写入与 marker 在同一 writer lock、同一 bundle 内完成。TUI 控制批次在 writer lock 内复核其所依据的 queue projection。新的幂等控制意图即使目标值已经相同，也须提交属于自己的领域确认事件；不能根据当前值直接伪造 receipt。

## 父会话与实际子会话

`ApplicationOperationBindingV1.session_scope_id` 始终属于原 application authority；`domain_session_scope_id` 单独标记业务提交所在会话，缺省值兼容父会话。K、F 和 operation id 不因委派而改变。父 `SessionApplicationOperationOwner` 从 immutable `AgentUserInputRoute` 解析精确 request/generation/hash、child ref 与 child session identity，只能打开该现有子会话；缺失日志不能变成新建操作。

Task planner 与 background child 的 session factory 显式消费父 Session 携带的委派绑定。Prepared 与回答/续执行 marker 写在实际 child 的批次内，父 route/public projection 不补写 marker。RA 管理的 PlanReviewResearch 使用实际资源 bundle 的 child Session capability 交接；逻辑 SessionRef 不能转换成 RA 物理路径。父 mirror 与 child authoritative input 必须给出同一完整 identity，来源冲突会拒绝委派。

`ApplicationDomainCommitRef.source_session_scope_id` 记录实际 proof 所属日志。Application frontier 继续约束父级 CAS，只在 commit 与 frontier 同来源时比较 sequence。跨 child 的数字大小不能被当成父日志推进证据。生成 receipt 前还须校验 opaque proof 内真实 committed binding 与原请求解析出的 K/F/target/domain 一致，不能借用同一子会话的另一个操作证明。

Managed child 的只读恢复在资源 lease 内读取已验证 records；records 与普通分页 reader 使用同一个 Prepared/连续批次/payload/target reducer。读句柄与 writer capability 不随 receipt 逃逸出 RA bundle。

PlanReviewResearch 的 prepare/绑定通过 `mutate_research_session` 打开实际 child，原 answer 与 Committed 仍在该 child 的同一批次中提交。query/find 通过 `recover_research_session` 只返回 resolved binding 和 opaque proof；父会话推进到 Cancelled、DraftReady 等终态后，原历史 Waiting mirror 仍提供不可变 owner 身份。已识别的 research target 即使尚未提交，也不能退回普通父 owner 查询。普通 child 的 `observe_operation` / `observe_target` 和父 `attach_for_observation` 也只观察已有日志；缺失或坏尾保持不可用，不借查询取得写入或修尾能力。

## 重开与不确定结果

对账通过分页顺序读日志，只保留当前候选批次最多 1024 条、8 MiB 的业务 payload，不以整份日志大小设置新的公共 readiness 门槛。半条业务记录、半条 marker 或同步失败由既有 bundle intent 恢复；重开后查询仍使用同一 K/F 和 operation id。找不到真实提交时保持 `Uncertain`，不能以错误文本或相同当前值推导 `NoEffect`。

TUI pending 仅由 application receipt 的终态结束。Worker 消息可以触发一次同 K/F 对账；消息匹配不能自行删除 pending。真实 dispatch acknowledgement 与 Task 完成判断没有等价关系，外部执行尚未对账的窗口仍保留不确定状态。

每次 TUI admission 由其后台线程持有独立的 Tokio runtime，覆盖命令准备、提交与 durable frontier 对账。该 runtime 不借用 UI 事件循环的生命周期，退出恢复终端、销毁事件 runtime 或切换页面后，在途 admission 仍由原 JoinHandle 负责回收；其 blocking observation 实际结束、runtime 释放后才发布结果。超时仍保留线程 owner 并报告 cleanup incomplete。后台 panic 的线程、位置与原始原因优先呈现，线程 join 或其他退出清理错误作为附加诊断保留。

HTTP 的回答 receipt 只证明 decision 已提交。返回续执行链接前，还须从原回答 identity/hash/generation 与不可变父 mirror 找到精确 child，并确认实际 run registry 已登记该 child 且属于同一 HTTP session。注册或启动失败在当次调用中保留原错误；此错误观察仅按完整 K/F 绑定、单次消费并在调用结束时清除，不参与持久结算。重试原 K 仍只对账已提交回答，未登记 child 时返回无续执行链接，不能重新派发或把 answer proof 当成启动成功。

## 已接受答案的继续执行

恢复表单发出独立的 `ResumeCommittedUserInput` 动作，不重新提交答案。它使用新的 K，显式绑定原完整 K、F、operation id、实际 domain scope 与 UserInput request/generation/hash。原 K 只用于查询其已提交的 decision。

查找原始 proof 与准备新命令都在后台 admission 线程进行；准备完成后冻结请求，重复操作复用同一新 K。Worker 先重验原 decision 的 causal proof，再进入已有 `ResumeRecoveredUserInput` 领域恢复路径：检查 active/retired/background owner，重建真实 continuation 状态并核对原 command identity。已有领域 claim、started、released 与 resolved 状态继续控制是否允许恢复，新的 application 动作不提供绕过这些校验的能力。

缺少 causal context 的历史 K 保持 `Unknown` 并精确停止，不能凭当前状态认证为新 proof，也不补造迁移 marker；已有领域 owner 的恢复事实保持可审计。当前调用链在真实提交前准备原 marker，因此可以通过上述独立新 K 恢复。外部 continuation 的执行结果不会仅因 worker acknowledgement 被报告为已完成。

Research 的新 resume binding 由受监督 `PlanReviewRunRequest` 显式携带，并在真实 RA execution bundle 中重新校验、绑定到同一 child。答案重放仅确认原 decision；新的 K 只有该 child 实际提交 `ContinuationStarted` 后才能结算。原 decision K 不沿普通后续执行继续传递为新的 continuation 证明。

## 回归范围

定向回归覆盖：Prepared 前禁止附着、K/F 漂移、错误目标不能结算、partial domain/marker 和 sync 故障重开、queue CAS 拒绝不产生 marker、真实 worker 提交后关闭重开不重复执行、原 decision 不重派而 continuation 使用独立 K 的恢复链，以及实际 planner/background child 的委派、child answer/continuation 故障重开、父日志不产生 marker、跨 child source 漂移拒绝。测试结果由本次执行报告记录。

真实 RA research bundle 回归另外覆盖父 terminal 后重开查询原 K、同 child 错误 domain 拒绝、首个 continuation dispatch 丢失后用新 K 恢复，以及 answer replay 尚不能结算新 K 的边界。

TUI admission 回归还覆盖 Agent/Planner 答案已有真实领域提交、调用方 Tokio runtime 已释放后，由实际后台 admission 线程查询 proof、读取 durable frontier 并结算原 K；验证未重新派发答案。慢 admission 使用 Tokio blocking task，验证退出超时仍持有线程、释放后可完成 join；panic 回归验证原始原因与清理失败同时保留。
