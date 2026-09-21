# 命令日志的 RA 原子恢复

业务日志使用有界逐条 JSONL 回源，不保留全历史内存 map，也不以累计 4096 key 或 16 MiB 拒绝正常历史。单条协议记录保留独立大小边界。SQLite 索引位于独立 `ApplicationCommandIndex` namespace；打开和恢复 SQLite 之前先按已有数据库及 journal 计算容量，回源的行与来源 checkpoint 在同一事务中发布。索引丢失、损坏或 schema 不符时从 canonical 日志重建；完整末行仅缺换行时可在原锁内补分隔符并同步，其他坏尾部保留可读前缀并拒绝新的 forward effect。

RA 的 quota profile 限额按 class 累计。所有受管 durable writer 的 `RuntimeState` grant 因此统一使用 512 MiB 类别上限及不设累计条数上限；不同 owner 的 holder 限制仍独立保留，artifact 类别额度及整个 managed storage 的 522 MiB workspace 上限不变。恢复 registry 或 input history 的小额写入不会重新把已有命令日志与索引压到旧的 1 MiB 类别门槛，真实新增容量仍须先通过 RA 配额申请。

每个新意图冻结原 K/F、expected frontier 和 `CommandJournalBinding`；K 的既有含义不变。HTTP 原始 envelope 的缺代字段只代表 legacy generation zero；新意图从专用 binding 查询获取当前代，重试保留原 envelope，不能在恢复后重新绑定当前代。旧代未知 key 不得进入新代 dispatch，旧代已知 key 仍可经真实领域因果记录查询结算。RA 已 Activated 但本地索引接管失败时，重新预览仍返回同一恢复操作；物理 namespace setup 失败退出现有 holder 并保留 charge，解除故障后可在同一进程重试。

`ManagedStorageServiceV1` 的 control recovery 接口只接收 authority-issued namespace handle、物理摘要和确定 header，不向 runtime 暴露 RA 路径。恢复 registry 使用 `ApplicationControlRecovery` grant；旧代和固定后继代使用 `ApplicationControlLog` grant。

预览的 application wrapper 同时携带 RA 物理事实与 runtime 语义影响。RA 绑定 logical journal、operation、旧文件身份、完整原始字节摘要、后继代、header 摘要和不透明的 owner context 摘要；Unix 使用打开文件的 inode/metadata，Windows 使用实际 handle 的文件身份。runtime 逐条回源并从可重建索引流式统计最后验证通过的记录边界及其字节摘要、已知 K 总数、受影响 scope 总数和已知未决 K 总数；只保留前 16 个 scope 与前 32 个未决命令作为样本，并明确标记截断。坏尾部的字节数独立展示，其中新增命令数量为未知，不能当作零。完整影响摘要纳入 RA preview digest，RA 不解释 Application K。预览只为已分配的空后继 namespace 建立 uninitialized 写入封锁，旧代仍可用。确认时 runtime 重新扫描同一完整前缀并核对影响，RA 在原 namespace 锁内重新核对旧字节与文件身份；内容、身份或影响发生变化则拒绝该预览。逐条扫描与 RA mutation 不递归持有同一文件锁。确认先持久发布唯一旧代 selection，然后依次发布 `Prepared → Sealed → HeaderInitialized → Activated`。每阶段一个 immutable 文件，校验摘要连接前阶段；查询按代流式校验整个链，缺阶段、摘要断裂或另一个 operation 都不能替换已经选择的恢复。

selection 的私有 staging 在旧代 selection 前已完成文件和目录链同步。旧代 selection 已发布但 registry Prepared 尚未发布时，查询只在二者完全匹配的情况下恢复同一 Prepared。每次日志 append 和真正 dispatch 前，RA 检查旧代 seal；dispatch 的 pathless forward guard 持有同一个 namespace 文件锁到调用边界，避免与封存并发穿透。旧日志 bytes 不修改，旧坏尾部不需要成功 finalize 才能恢复。

新 header 写入私有 staging 后先同步文件，再通过 hard link 原子、不覆盖地发布 canonical `records.jsonl`；随后同步目录、admission marker 和直到受管 state root 的父目录链，最后提交 Activated。重试只复用完全相同的 header-only 文件；目标含业务记录或不同字节时拒绝初始化。Activated 后重复确认直接返回原恢复事实，不修改或检查后续业务尾部。部分 staging、文件同步、发布后目录同步或阶段同步失败均保留同一 operation 和后继代，故障解除后继续。

阶段 registry 的 durable 字节和条目在 RA 自身计算；新增阶段提前申请包含暂存开销的真实 managed-storage capacity。冷开查询先从校验通过的阶段链重建容量，专属 physical frontier 和 finalize receipt 不把缺少 `records.jsonl` 解释为零条元数据。历史阶段没有固定代数门限，增长受当前 grant 和 workspace 逻辑预算限制。`detach_namespace` 只退出现有 holder，保留同进程已有容量；它不以坏尾部或不可读状态推导已释放字节。

活跃 holder 与保留容量分别管理：共享同一 process quota book 的 authority service 同时只能持有一份相同 grant/namespace 的活跃 claim。仍在执行的 authority 操作保留该 claim；detach 或 service 释放且实际操作结束后，新的 holder 才能复用原容量。重复接纳失败不能释放现有 owner 的 charge。

故障验证涵盖：header 部分写入、文件/目录/父目录同步失败、旧 selection 与 registry 发布之间失败、四阶段重开、已激活后新增业务及坏尾重试、跨代校验链断裂、并发 dispatch/seal、旧 bytes 在预览后变化、相同字节替换到不同文件身份、40 个未决命令及 20 个 scope 的精确有界影响统计、影响篡改拒绝，以及退持有再接管和 metadata 冷开计量。平台目录同步错误会使恢复保持未完成；不会提前宣称 Activated。

Desktop 在“支持与诊断”中提供“命令历史恢复”卡片，作用域固定为当前已有 workspace/session。卡片独立于 provider inventory 和 diagnostics 是否可读，预览展示原命令代、字节数、摘要及预定新代，以及完整记录边界、已知命令与未决操作范围、截断样本和坏尾未知项。Desktop 用户明确确认后通过专用 Tauri allowlist command 回传完整、相同的 typed preview；TUI `/control-log preview` 展示同一语义影响，`/control-log confirm <digest>` 经 application port 回传同一 typed preview。失败保留同一预览供重试；换会话丢弃旧预览。renderer 不持有路径、bearer 或 generic HTTP/filesystem 权限，Rust typed client 检查返回代与确认绑定一致，并拒绝无法通过 JavaScript 精确表示的整数。异步恢复不停止或替换当前 conversation。

Desktop native 在每个新业务意图生成 command envelope 前，通过带 bearer/client ID 的窄 `GET /sessions/{id}/application/command-journal-binding` 取得宿主发放的日志身份和命令代。该绑定随同一信封冻结，审批、用户回答和计划决定的响应丢失重试只重发原信封，不再查询当前代。恢复成功更新本地最近绑定；精确 run/terminal/Task 停止仅使用已有绑定或缺代旧协议的安全停止通道，不新增查询前置。renderer 无需接收或选择命令代。

通用 HTTP application commands 的精确 `RunCancel` / `CancelTerminalTask` 与专用停止入口一样跳过 projection refresh。精确停止在 journal `Reserve`、`DispatchStarted`、`EffectStarted` 或最终 forward guard 出现 `Unavailable` / `CorruptProjection` 时，转入同一真实 owner 通道。`RunCancel` 通过实际 supervisor 激活确认关闭 forward gate；只有 owner 返回 `ForwardGateClosed` 才返回 `SafetyStopRequestedButUnrecorded`，`ScopeMismatch` / `InvalidRequest` / payload conflict 保留拒绝；该回执不声称 cleanup 已完成，也不改写坏尾部。listener 先校验 run 所属 session，拒绝跨 scope 停止。
