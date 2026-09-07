# 模块消融与职责收敛

本次重构以删除冗余实现后仍通过行为验证为准。共同使用某个输入或拥有相似签名，
不足以证明两个模块职责重复；不同信任域、不同资源生命周期和不同展示用途分别保留。
设计依据为[代码规范](../governance/code-standards.md)、
[工程规范](../governance/engineering-standards.md)及
[当前架构](sigil-rust-agent-core-technical-solution.md)。

## 收敛后的 owner

| 能力 | 唯一实现位置 | 消费方式 |
| --- | --- | --- |
| provider HTTP client 构造 | `sigil-provider-http` | 五个 provider 构造函数直接调用；CA/TLS 校验继续由传输 owner 执行 |
| code-intel 工具注册 | `sigil-code-intel::register_code_intelligence_tools` | 显式传 workspace trust 与 managed launcher；删除隐式补 Unknown/None 的两层注册包装 |
| 角色工具 scope | runtime `run_options` | 内置 agent profile 与 role registry 使用同一配置解析及默认表；PlanReview 继续执行只读交集和 deny 约束 |
| Task 状态的稳定字符串 | kernel `TaskRunStatus` / `TaskPlanStatus` / `TaskStepStatus::as_str` | runtime、公开事件与 TUI 直接消费；各表面的专用文案与 verification 提示保留 |
| 原始用户消息查询 | kernel `Session::source_user_message` | PlanReview、TaskHandoff 与 runtime coordinator 从同一 session entries 查询 User/ConversationInputPromoted，不从压缩后的模型上下文还原 |
| application snapshot 验证 | `ProjectionReducer::open` | client 先取得已验证 reducer，再检查当前 client 的 scope、observer 和 resume frontier |
| boot composition | runtime authority composition | `application_host` 以同名 re-export 保持 host API，删除没有逻辑的转发函数 |
| 进程 ownership primitive | `sigil-process` | builtin tools 直接导入，删除本地 `process_owner` shim；各平台 cfg 保留 |
| doctor 恢复根生命周期 | resource authority `AuthorityBootstrapRecoveryServiceV1` | 创建 state/cache 与 scratch 目录，返回负责失败清理的 guard；runtime 只协调确认、配置 CAS、恢复执行与 boot |
| package / host 依赖边界 | R70 topology、host ownership、migration gates | R71 删除对已落地 application package 的临时时序禁止，继续验证 physical authority/sandbox 的导入与注册约束 |
| inventory 核对 | `scripts/check-r71-inventories.py` | 一次扫描同时核对 process/producer 两份 manifest；两个领域 shell 入口只选择 kind |

`source_user_message` 是查询，不授予权限。它保持首个匹配记录的行为，只返回当前 session
中的直接 User 或队列提升后的原始用户消息；调用者仍须校验 source session scope、绑定、
hash 和当前 authority。缺失消息与消息存在但没有 text content 仍是不同状态。

snapshot 的内容验证与 client binding 验证分别保留。删除的是同一不可变 envelope 在
`refresh` 与 `ProjectionReducer::open` 中的重复验证；schema、scope、observer、resume
失败均不得 ACK 或替换 client 已提交的 projection。

doctor 的恢复目录必须交给 bootstrap owner，不能归类为普通产品配置写入。
`prepare_replacement_roots` 在分配前检查 scratch 相对路径，创建并收紧新根和目录链的权限；
`PreparedAuthorityBootstrapRootsV1` 在配置发布前失败时自动清理。配置已发布、可能已发布，
或 staged intent 无法退回时，runtime 调用 `retain_for_recovery` 保留目录供重试。
这三处物理创建分别声明 StateAnchor、CacheAnchor、ExecutionTempAnchor 的 exact locator，
root source 为配置平台位置、taint 为 UserConfiguration；不得沿用普通 bootstrap child 的
BootstrapMetadata 分类。执行临时根的显式恢复声明见 RFC 0071 的封闭例外。
`retain_for_recovery` 只关闭临时目录清理，不写配置或 authority state；确认、授权、CAS 和恢复 receipt
校验仍由原有流程完成。

## 删除的非生产路径

- 五个 provider 的私有 `client.rs` 和仅断言包装构造成功的测试。传输安全测试与各 provider
  的协议、streaming、retry、continuation 行为测试保留。
- Desktop 无引用的 `src/contract.ts` 类型别名模块、`LocaleToggle` 和仅供它使用的 `toggleLocale`。
  语言设置继续使用 `SettingsPage` 的 `setLocale`。生成的 HTTP schema、native typed client、
  IPC DTO 与 OpenAPI drift gate 保留。
- Sandbox 无调用的 `local_effective_enforcement` 与恒空 `local_bind_evidence`。真实 Local
  confinement、requested/effective enforcement 和 managed receipt 路径保留。
- Resource Authority 的 `file_access_stub` 只在测试或显式 `test-support` feature 下导出，
  默认生产构建不再包含这个拒绝所有操作的测试替身。

## 保留的边界

- Provider 协议实现保留在各 provider crate。HTTP client 共用不意味着 Chat Completions、
  Responses、Messages、GenerateContent、DeepSeek reasoning/FIM 可以共享一套协议状态机。
- Resource Authority、Sandbox、Process 与 Process Observer 分别负责资源权威、约束执行、
  OS ownership 和 birth identity 观察。锁内重验、恢复观察与 receipt 验证不因代码相似而删除。
- MCP 的 TERM/grace/KILL 和 zombie-aware liveness 不等于 `sigil-process` 的单次 kill。
  当前没有等价替代，继续保留。
- TUI core、Ratatui adapter、公开 facade、application adapter 与内部 host 具有不同依赖边界。
  Desktop renderer、native IPC、typed client 和 HTTP server 也分别保留各自的身份与数据约束。
- Desktop rail 与 library 的查询、分页、选择状态服务不同展示用途，不合并成额外 controller。
- 各领域 projection 的短 replay 循环保留。通用 reducer trait 不会仅因减少几行循环就降低复杂度。

## 验证责任

测试统一使用[隔离入口](test-isolation.md)。先运行触及模块的行为测试，再执行 workspace
编译、格式、相关 clippy 和 Desktop check；变更门禁实现时补失败路径和调用次数测试。

R71 negative-dependency gate 一次核对两类 inventory。Release qualification 复用该 gate，
不再在它之前重复运行相同 inventory 检查。两份 manifest 的 exact join、非空要求、未分类和
migration-blocker 拒绝继续生效。enforce 同时对照现有 generator 声明表核对 crate、class、
owner、root source、taint、child access 及 admission/resource/lifecycle/receipt contract，
防止手改 manifest 通过覆盖检查。声明表只有一份，generator 与 checker 共用。

这个 checker 证明当前 scanner 识别出的 site 与现有声明一致，不证明 regex scanner 无遗漏，
也不代表 RFC 0071 的完整多字段资格化矩阵或真实平台/发布资格化已经完成。

模块逐项证据、未采用候选、验证结果及独立复核维护在仓库本地
`.repo-local-dev/review/module-ablation-2026-09-05.md`。离线测试结果不代表 live provider、
真实 GUI 或跨平台资格化。

## Resource Authority 专项消融（2026-09-06）

专项实验以当时未提交工作区的源码快照为基线，在独立副本逐项移除实现，并通过统一隔离
入口运行完整 Resource Authority 单测。源码摘要、每个变体的精确替换、原始日志和结果
保存在 `.repo-local-dev/ablation/resource-authority-2026-09-06/`；专项报告位于
`.repo-local-dev/review/sigil-resource-authority-ablation-2026-09-06.md`。

- 删除无调用者的 `provider_registry.rs` 类型骨架。
- `consumer_ports`、`semantic_matrix`、`lease`、`reconcile` 及其 lifecycle re-export 仅在
  `cfg(test)` 下编译。它们的调用者都是本 crate 的契约测试；保留现有 conformance 用例和
  runner filter，但不再将这些模型作为生产 API 或生产保证的证据。
- 删除 `AuthorityStorageGrantTableV1::consumed_capabilities` 恒空字段。真实 namespace
  claim、finalize CAS、quota 与 journal 账本继续持有各自的状态。

生产存储仍通过 `storage::validate_closed_admission_grant` 校验 grant/request/current
authority。测试矩阵退出生产构建不代表 RFC 的完整 owner/kind/source/purpose 矩阵已由
另一实现全面实现；同样，测试 lifecycle/reconcile 模型不代表真实 spawn/recovery 验收。

保护机制对照暴露了三处原有测试覆盖缺口，现已补充真实服务路径回归：进程清单拒绝错误
HMAC 及重算公开 hash 的伪造数据，mutation/restart 拒绝后不改写文件；snapshot 写锁拒绝
另一个文件描述符并在 owner drop 后释放；storage 拒绝不同 epoch 及同 epoch 不同 instance
的 grant，且不创建 namespace claim。保护机制的生产实现继续保留。

本次是离线正确性与编译边界实验，没有测量性能、二进制体积、真实 GUI 或跨平台发布资格。
