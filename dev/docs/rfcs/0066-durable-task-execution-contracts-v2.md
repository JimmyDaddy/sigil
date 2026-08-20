# RFC-0066：Durable Task Execution Contracts V2

状态：已实施（2026-08-18）

## 1. 背景

Session `6a999eaa-1d52-4733-bc58-94b1e53de7a2` 暴露的“重复分析”不是单一模型问题，而是执行契约在 planner、orchestrator、child session 和恢复路径之间逐层变薄：

- plan step 只有标题、角色和依赖，目标路径、交付物、验收条件与能力要求未被完整保留；
- child 只能接收宽泛 objective，依赖结果会退化成大段自然语言；
- VCS 事实需要模型自己拼接多条 shell 命令，没有一个固定、可审计的只读入口；
- 同一调用和同一结果前沿可被反复分析，host 没有 durable no-progress 判断；
- 旧的 hosted-tool cache identity 混入了临时授权 UUID，会制造无意义的 cache schema churn；
- plan sidecar、任务进度和恢复证据不是一个明确的版本化提交单元。

因此，本 RFC 不把修复定义为“再加一句 prompt”，而是补齐可执行 Task 的 host-owned contract。

## 2. 目标

1. planner 产生的执行语义无损进入 durable task state。
2. participant 在 provider dispatch 前完成 exact capability admission。
3. child 只接收本步骤所需的目标、依赖摘要和验收条件。
4. 相同语义调用在相同结果前沿上重复时，host 能识别并请求结项。
5. compaction、interrupt、resume 后仍能恢复同一 plan version、step contract 和 participant frontier。
6. cache identity 只覆盖 provider-visible semantic schema，不受进程内授权 identity 干扰。
7. 历史 V1 session 保持可读，不伪造不存在的 V2 contract。

## 3. 非目标

- 不把 DeepSeek 私有字段加入 `sigil-kernel`。
- 不允许模型通过声明 capability 扩大工具权限。
- 不把任意 shell/git 参数重新包装成一个“看似安全”的工具。
- 不把大段 child transcript 复制回 parent session。
- 不承诺 provider cache 一定命中；本地只证明稳定前缀语义。

## 4. TaskStepContract V2

`TaskStepSpec` 保留 V1 的稳定执行图语义；新增 append-only sidecar：

```text
TaskStepContractV2
  target_paths
  required_capabilities
  deliverables
  acceptance_criteria
  check_spec_refs
  risk
  notes
```

每个 sidecar 由 `(task_id, plan_version, step_id)` 绑定。`TaskPlanContractSetCommittedV2` 对完整 contract set 做 canonical SHA-256，并作为原子提交的终止标记。

恢复规则：

- V1：只有 `TaskPlan`，`step_contracts = {}`，`contract_set_committed_v2 = false`；
- V2：只有 plan、所有 sidecar 与 marker 全部匹配时才声明完整；
- 不允许把崩溃留下的部分 sidecar 当作合法 V2 plan；
- replan carry-forward 必须同时比较 step spec 与 V2 contract。

## 5. Capability admission

能力分为两层：

- `TaskCapabilityV2`：plan 需要什么；
- `ToolCapability`：exact scoped registry 实际提供什么。

模型声明只会与 host baseline 做并集，不能删除基线能力。普通 workspace write 的基线是
`workspace_read + workspace_write`；`changeset_only` 只产出待审变更 artifact，因此基线为
`workspace_read`，不会虚构直接 workspace write 权限。participant 启动前对 exact role、exact scope、
exact registry 做集合校验；缺失能力在 thread、worktree 与 provider attempt 之前失败。

`verification_run` 是 host-owned verification lane，不是模型可委派给 participant 的能力。planner
只能通过 `check_spec_refs` / suggested checks 引用待验证内容；把 `verification_run` 写入
`required_capabilities` 或创建 `mode = verify` participant 必须在 plan admission 阶段拒绝，避免生成
一个注定没有执行权限、却会拖垮整个 DAG 的步骤。

每个 task child 还必须携带 process-local、不可 replay 的 `AgentInvocationGrant`，并把安全投影写为 `AgentDelegationAdmitted`。planner、planner discovery、accepted-plan step 和 synthesis 使用不同的 host-owned authority，不能相互冒充。

## 6. 固定 VCS 事实工具

新增 `vcs_inspect`，只允许以下固定操作：

- `status`
- `diff_names`
- `diff_stat`
- `staged_stat`
- `unmerged`

工具不接受任意 git 参数；执行有 timeout、cancellation、bounded output，并声明 `WorkspaceRead + VcsRead`、`ParallelReadOnly`、无 mutation tracking。外置 `.git` 默认 fail closed。

## 7. Scoped prompt 与 typed handoff

participant prompt 使用 task 的语义标题和当前 `TaskStepContractV2`，不再复制整份 approved-plan wrapper。

依赖结果只传递：

- output/summary hash；
- bounded summary；
- artifact refs；
- changed paths；
- verification refs。

下游不接收上游完整 transcript。未知或缺失的 durable reference 必须明确失败或降级，而不是重新探索整个仓库。

## 8. Durable checkpoint 与 no-progress

每个 task participant 工具批次在 V3 tool result 全部结算后追加 `TaskStepCheckpointV2`：

- task/plan/step/attempt identity；
- model turn；
- semantic call hash（忽略 call UUID；只对 shell/terminal 这类 bounded observation 归一化命令
  拼写，artifact 的 `start_line` / `max_lines` 等页游标必须保留）；
- result frontier hash（工具状态、模型可见结果摘要的 SHA-256、返回/总计 bytes/lines/matches/
  entries、截断状态、exit code、changed paths 等）；
- consecutive no-progress count。

checkpoint 不保存原始参数或输出。连续两次重复“相同调用 + 相同结果前沿”后，agent loop 禁止再开普通工具回合，注入一次受限 finalization contract。结果前沿变化、文件发生变化或命令输出发生变化都会清零计数。

如果受限 finalization 仍未产生 bounded result，participant 以 `repair_replan_required` 阻塞终态结束，并把当前步骤交给显式 retry、repair 或 replan 控制流；不得继续消耗普通模型工具回合。

Provider 已产生输出后出现的结构化协议拒绝不能在同一个 logical run 内透明重放。若 host 能证明
当前步骤为 `read/review/verify + shared_read_only`、没有 durable side-effect，并把失败 physical attempt
的 exact request material fingerprint 作为恢复证据，则可追加一个新的 durable participant attempt
做有限恢复（当前最多 2 次）。替代 attempt 另外绑定 retry-stable task input hash 与 provider/model route
fingerprint；provider dispatch 前必须同时复核 input 与 route，route 漂移时进入 typed recovery boundary，
不能把旧 schedule 静默迁移到新路由。
恢复预算耗尽时步骤进入 `Blocked`、Task 进入 `Paused`，并保留依赖步骤；只有不可恢复执行失败才进入
`Failed` 并取消依赖。这样既不把不确定副作用重放，也不会把 provider 格式漂移升级成整张 DAG 的
永久失败。

错误恢复属于 projection 语义，而不是新的 JSONL 状态覆盖：工具错误保留为历史证据，并由当前未解决 blocker、readiness、acceptance 和 final answer 重新聚合为 `recovered` 或 `unresolved`。成功替代路径不能让历史 `PermissionDenied` 继续阻塞终态；只有仍未解决的错误才可产生 `blocked`。

可写 task child 的 invocation grant 把 mint 时的 workspace snapshot 保留为不可变 admission/audit
证据，同时维护 process-local、host-owned 的 audited mutation frontier。每个 mutating/unknown tool
仍在 effect 前校验 exact registry、当前 frontier 与 root cancellation；effect 后先结算 RFC-0002 或
unknown-mutation evidence，再以 compare-and-swap 推进 frontier。自身合法写入因此不会自废 grant，
而外部未归因漂移、并发 writer 冲突、只读 child 写入或 post-effect snapshot 不可用仍 fail closed。
frontier 不是 durable grant，也不能从历史 audit record 恢复为新的执行权限。

## 9. Cache semantic identity V2

`CacheLayoutProofV1` 保持历史语义不变。新增 `CacheLayoutProofV2`：

- local tool 只 hash provider-visible name/description/schema；
- hosted tool 只 hash kind/name/limits；
- 排除 per-turn authorization token、request correlation id 和进程内 UUID；
- 普通 provider attempt 与 native compaction 都追加 V2 proof；
- UI cache mutation diagnostic 使用 V2，历史 V1 记录仍可 replay。

Provider adapter 若有额外 wire `type` 版本变化，应另加 provider-owned versioned wire-profile identity，不能把 provider 私有常量塞入 kernel。

## 10. 产品表面

TUI 与 Desktop 读取同一 Task projection：

- 标题使用 durable semantic task title；
- step 列表显示 completed/running/blocked/pending；
- 内部 task id、projection 等待状态默认不作为主标题；
- terminal task 结项后从 active panel 退出，但仍保留在历史审计中；
- resume 从 durable plan/contract/checkpoint 重建，不依赖旧 viewport 或瞬态 UI 状态。
- Plan step 的短展示名在 draft commit 与 Task promotion 复用同一个 bounded canonicalizer；旧
  session 的过长展示名会被兼容缩短，不能阻断执行。Run 失败追加 `TaskCreationFailed`，原 Plan
  保持可重试，TUI 恢复 workbench 并显示具体原因。
- ingress 只分类一次 `ResumeTask | ApplyTaskGuidance` 并把 typed control 带到 adapter dispatch；恢复和
  dispatch 不得从本地化、脱敏或重建后的 guidance 文本再次推断控制语义。
- `ResumeTask` 只恢复 durable plan，不把“继续/继续执行/resume”等别名保存为指导语；
  `ApplyTaskGuidance` 才会追加任务指导。
- `network: deny` 表示离线执行，默认只在执行详情显示，不等价于权限拒绝；只有最终 policy decision 为 deny 才展示拒绝。

## 11. 验收

最低要求：

1. V1 plan replay 不生成虚假 V2 contract。
2. V2 plan + sidecars + marker 原子落盘并可跨重启 replay。
3. 缺失 `VcsRead`/`WorkspaceWrite` 等能力时 provider attempt 为 0。
4. task child invocation grant 与 exact registry、初始 workspace snapshot、root cancellation 绑定；可写 child 只在 mutation evidence 结算后推进 audited frontier，外部漂移仍拒绝。
5. 相同调用与相同 frontier 重复达到阈值后只允许 finalization。
6. frontier 变化不会触发 no-progress。
7. compaction 后 plan version、step status、contract set 与 checkpoint identity 不变。
8. hosted authorization UUID 轮换不触发 `ToolSchemaChanged`；limits 变化会触发。
9. TUI/Desktop 展示语义标题和步骤进度，不展示内部 plan-task UUID 作为主信息。
10. opt-in real DeepSeek 多轮测试记录 hit/miss、请求字节稳定性、阈值和总成本上限。
11. `PermissionDenied -> alternate path -> final answer` 聚合为 completed with warnings；阻塞原因展示当前 blocker。
12. blocked 前置步骤支持幂等 retry/reconcile，恢复后依赖步骤重新进入 ready；session 重启和 compact 后仍可继续原计划。
13. `.git` 只读探测、受控 scratch heredoc、InvalidInput/retryable 工具错误和离线网络展示均有回归覆盖。
14. artifact 分页游标变化和模型可见输出变化会重置 no-progress；完全相同的 bounded observation 才累计。
15. provider 协议拒绝只在只读、零副作用且失败 attempt 的 exact material fingerprint 已作为证据时创建新的 durable attempt；替代 attempt 的 input/route dispatch 前复核，预算耗尽后 Task 为 `Paused`，依赖步骤不被取消。
16. Task resume 使用 typed control；guidance 文本变化不能把 `ResumeTask` 重新解释为 `ApplyTaskGuidance`。
17. 跨层故障测试必须覆盖 orchestrator、runtime child runner 和 provider physical attempt：首次协议拒绝、第二次恢复成功后，依赖步骤与最终 synthesis 都应完成。
18. 超长 Plan step 展示名在提交和旧 session promotion 两条路径都不会阻断 Task；任意 Run
    promotion 失败均留下 durable failure settlement、保持 Plan actions，并在产品表面显示原因。

## 12. 迁移与回滚

- 所有新 durable 字段/variant 为 additive；V1 记录继续读取。
- V2 marker 缺失时按 legacy/incomplete 处理，不补写推测状态。
- `vcs_inspect` 可从 runtime registry 移除而不影响旧 session replay。
- `CacheLayoutProofV2` 可停止生成，但不能重释或删除 V1 字段。
- no-progress 先切换到一次受限 finalization；finalization 仍为空时进入 `repair_replan_required`，不自动提交、回滚、扩大权限或继续请求，必须由 retry/repair/replan 控制流恢复。
- 损坏或过时的 task projection 通过 append-only repair/reprojection 从已持久化 participant evidence 重建；不得手工改写 JSONL。
- shell 权限按 subject 的读写能力计算；heredoc 正文不参与外部路径解析，hard-safety 只在已证明关键路径修改时拒绝。
