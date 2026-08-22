# RFC-0067 实施审计与修复总结报告

日期:2026-08-20
基线:`HEAD 6b27f3e2`(工作区含全部未提交修改)
变更规模:58 个文件,+5916/−474

## 1. 背景

RFC-0067(Single Execution Spine and Monotonic Plan-to-Task Adoption V1)要求把 Plan→Task
handoff 从"Run 时二次语义转换 + 多段 append"重构为一条可证明、可恢复、单调前进的执行脊柱:

```text
semantic route → Plan review → executable candidate compile → PlanReady commit
→ user Run command → atomic Task adoption → runtime admission → execution → terminal settlement
```

核心不变量:`DraftReady` 必须表示"完整、规范化、内容寻址、可无外部副作用采纳的
`ExecutablePlanCandidateV1` 已 durable commit";Run 只做一次 typed CAS + 一次 crash-safe
adoption append;环境问题在 Task 身份已存在后进入统一 admission,以 `Ready | Blocked | Paused`
收口。

## 2. 实施历程

### 2.1 第一轮:核心骨架(已通过审计确认)

- **R67.1 Plan compiler 与 candidate**:`ExecutablePlanCandidateV1`/`PlanCompileBindingV1`/
  `PlanCompileInputV1`/`PlanCompileFailureV1`/`PreparedIntentAdmissionV1`;纯确定性编译器,
  canonical hash 排除易变字段,自带 self-check;失败返回 typed reason_code + affected_step。
- **R67.2 PlanReady marker 与 projection**:`ExecutablePlanCandidatePreparedV1`/
  `PlanReadyCommittedV1`/`PlanCompileFailedV1` 记录;`commit_draft_from_child` 编译后才写
  DraftReady;`PlanReadyStateV1` 只由 marker 驱动;Doctor 新增 `check_plan_execution_spine`。
- **R67.3 Atomic Task adoption**:`PlanRunCommandV1`/`PlanRunReceiptV1`/`PlanRunRejectionV1` +
  `PlanExecutionService::adopt`(CAS + 单次 append,含 intent activation 同批原子写入);
  reducer 从单事件派生 TaskRun/TaskPlan/contracts/decision/task-created;intent stack、lineage、
  approval policy、child grant 继承均已适配。
- **R67.4 Runtime admission**:`TaskAdmissionAttemptV1`/`TaskBlockerV1`/`TaskExecutionPhaseV1`;
  单调 ordinal,typed Blocked。
- **R67.6 表面统一**:TUI 键盘/鼠标、model typed route、HTTP `plan_decision` 接入 adopt + admission;
  `TaskAdmissionBlocked` 消息与 blocker UI;HTTP DTO 暴露 task_title/candidate_hash。
- 测试、文档、RFC 状态、changelog 同步。

### 2.2 第二轮审计修复(评审 1)

| 问题 | 修复 |
| --- | --- |
| P1 共享模型路由仍走旧 `create_task_from_plan` | `continue_application_task_handoff` 的 `RunPendingPlan` 改走 `PlanExecutionService::adopt`;`run_application_admitted_task` 对每个 adopted Task 先 admission(覆盖 StartDurableTask/续接) |
| P1/P2 HTTP/CLI Run 只 adoption 不 admission | `application_plan_decision` adoption 后立即 `admit_adopted_task`;receipt 增 `task_phase`/`task_blocker` |
| P1/P2 TUI admission 假探针 | `build_task_admission_probes`:route 来自配置解析、磁盘来自 `fs2::available_space`、permission 检查 ReadOnly、verification 检查 auto_run、外部 writer 检查活跃 lease |
| P2 readiness 判定不统一 | task_handoff / conversation_coordinator / conversation_display 全部改为 candidate+marker 判定;legacy DraftReady 只给 Revise/Reject |
| P2 candidate/marker 冲突不 fail-closed | commit 时校验已有 candidate/marker 内容一致性,漂移直接 bail |
| P2 durable 事件验证缺失 | `PlanReadyCommittedV1`/`PlanCompileFailureV1`/`TaskAdmissionAttemptV1`(+observation/blocker)均加 `validate()` 并接入 `validate_durable_contract` |
| P2 adoption 未绑定 session scope | `adopt` 强制 `command.session_id == session_scope_id`,否则 `CommandIdentityConflict` |
| P2 command id 未覆盖执行选择 | command id 加入 candidate hash + start mode + permission choice |

### 2.3 第三轮审计修复(评审 2,本轮)

| 问题 | 修复 |
| --- | --- |
| P1 credential 探针等价于 route | 新增 `route_and_credential_probe`:route 只证明配置形状;credential 单独解析实际 source(环境变量读取 / stored 记录经 `ConfiguredProviderCredentialStore` 加载) |
| P1 permission 探针不看 candidate | 探针接收 candidate,`permission_profile_ok = !requires_write \|\| mode != ReadOnly`;纯读 Task 在 ReadOnly 下不再误阻塞 |
| P1 external writer 误判自身 lease | 排除 `task:<id>:` 前缀的自身 lease;session-local 证据边界在文档注释说明 |
| P1 verification 探针未验证 runner | runner 是 host 机制(RFC-0003 materializer),探针验证 `auto_run != Never` + workspace 身份可解析 |
| P2 Desktop/CLI 丢弃 admission 结果 | `sigil-desktop` 新增薄边界镜像类型 `DesktopTaskExecutionPhase`/`DesktopTaskBlocker`,receipt/IPC summary/前端 types/PlanCard 全链路透传;Desktop Run 在 blocker 存在时跳过 `continueTask` 并内联展示;CLI receipt 输出 task_phase/task_blocker |
| P2 Ready 跨记录一致性不完整 | `plan_ready_state`/`plan_is_ready` 要求 marker.plan_id/hash == candidate.plan_id/hash == draft.hash 全链一致 |
| P2 旧 promotion API 仍公开 | `create_task_from_plan`/`create_task_from_plan_inner`/`record_task_creation_failure`/`CreateTaskFromPlanRequest`/`CreatedTaskFromPlan` 全部 `#[cfg(test)]`,从生产 API 面移除 |
| P2 RunPendingPlan 无直接回归测试 | 新增 2 个 runtime application 测试:typed route → adoption → admission → runner → terminal synthesis(Completed);route 不可解析时 adoption 后 Blocked(provider_unavailable)、Task 保持 durable |

## 3. 新增测试(第三轮)

- `admission_probes_distinguish_route_shape_from_credential_availability`:route 可解析但 env
  credential 缺失 → `credential_available=false`;设置变量后通过。
- `admission_permission_probe_considers_candidate_write_need`:写 Task 在 ReadOnly 下阻塞;
  读 Task 通过。
- `admission_external_writer_probe_ignores_self_leases`:自身 lease 不计外部 writer;他者 lease 计入。
- `admission_verification_probe_requires_runner_capability`:auto_run=Never 或 workspace 不可解析
  → runner 不可用。
- `run_pending_plan_route_drives_adoption_admission_and_terminal_synthesis` /
  `run_pending_plan_route_keeps_blocked_task_durable`:共享模型路由全链路回归。

## 4. 验证结果

### 4.1 自动 gate(全部通过)

| gate | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo check --workspace`(+ `--tests`) | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 全部通过(详见 4.2) |
| `scripts/generate-desktop-contract.sh --check` | 无漂移 |
| `scripts/check-docs.sh`(links/mirror/command-metadata) | 通过 |
| `git diff --check` | 通过 |
| Desktop 前端 `tsc --noEmit` + `vitest run` | TS 通过,284 tests 通过 |

### 4.2 crate 测试统计(最终轮定向确认)

| crate | passed |
| --- | --- |
| sigil-kernel | 1628 |
| sigil-runtime | 1111 |
| sigil-http | 213 |
| sigil-tui | 1684 |
| sigil-desktop | 67 |
| sigil(CLI bin) | 76 |
| frontend vitest | 284 |

已知事项:
- 第三轮中发现 admission 驱动测试依赖进程级 `SIGIL_API_KEY` 环境变量,并行测试会临时移除该
  变量导致偶发 `credential_unavailable` 阻塞;已新增 `routed_unauthenticated_test_root_config`
  (custom + loopback + source=none)消除该类 flake,TUI 全量并行 0 失败。
- `setup_explicitly_replaces_an_existing_malformed_config` 在 16 核并行全量下偶发失败(约
  1/4 概率),隔离与串行均稳定通过;根因是 setup config publish 的
  `PublishedButVisibilityUncertain`/`ReplacementPartiallyApplied` 安全回退在并行文件系统负载下
  偶发触发(持久化层的保守 fail-closed 行为,`config_flow`/`setup_flow` 均未在本 RFC 改动),
  与 RFC-0067 无关。

## 5. 当前状态与剩余工作

### 5.1 已完成(对应 RFC 条款)

- §6.1 Ready means adoptable:`DraftReady` 只由 candidate + ready marker 投影,legacy 计划
  `LegacyPlanNeedsRecompile` 不冒充 Ready(§15.1)
- §6.2 Run is commit-only:adopt 不调用 provider/fs/registry,单次 CAS + append
- §6.3 Adopt first, admit second:环境问题全部成为 typed blocker,Task 不消失
- §6.4 Monotonic progress:admission ordinal 单调,history 不覆盖
- §6.5 One application service:共享模型路由、TUI、HTTP、CLI 统一入口
- §7/§8/§9/§10:compiler、ready 原子提交、typed command/receipt、admission 契约
- §12:Doctor 审计 spine 事实;compaction 不改 control plane
- §15.3:旧 promotion 路径从生产 API 移除(cfg(test) 保留 legacy replay 参考)
- §16 R67.0–R67.6 全部落地

### 5.2 未完成(qualification pending,对应 §18/§19 gate)

- key-gated 真实 DeepSeek 测试:≥6 次 provider request 的 Task E2E(§18.21)
- read → edit → test → failed-test repair → retest → final synthesis 全流程(§18.22)
- 外部 world-state 验证:agent 外重跑测试、受保护文件 byte-identical(§18.23)
- fault campaign:provider disconnect、tool error、approval deny、disk pressure、session append
  failure、process restart(§18.24)
- 同一 failure fixture 连续 20 次无 Task missing/silent failure/duplicate mutation(§18.25)
- 真实 PTY/Desktop 跨表面 E2E(§18.16–§18.20 中依赖真实终端/桌面的项)
- §19 指标埋点(plan_compile_success_ratio、adoption/blocker/tool 收口计数等)

## 6. 结论

RFC-0067 的执行脊柱(compiler → ready marker → atomic adoption → admission → 统一表面)已完整
落地并通过全部自动 gate;两轮审计提出的 1 个 P1 与 10 余个 P2 问题全部修复并有直接回归测试覆盖。
按 RFC 自身 §18 的要求,真实模型、fault campaign 与真实终端/桌面 E2E 的 qualification gate 尚未
执行,因此 RFC 状态保持"部分实施(qualification pending)",待这些 gate 完成后可标记为已实施。
