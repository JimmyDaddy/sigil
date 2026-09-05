# RFC-0072：启动可靠性、Setup 恢复与 Provider-only Safe Mode V1

状态：可靠性改造文档同步（2026-09-01；本 RFC 不代表 RFC-0069 或 RFC-0071 已整体验收完成）

本文是 RFC-0069 的失败隔离与恢复边界、RFC-0071 的 authority bootstrap/journal 约束的增量补充。它固化启动阶段的产品状态映射，以及 authority 故障时可以向用户暴露的最小 provider-only 安全面；不改写 RFC-0069 既有历史内容、执行 ledger 或验收结论。本文描述的是跨 Desktop/TUI/CLI/HTTP 的共同语义；各产品表面可以有不同的布局和提示，但不得改变以下状态与故障边界。

## 1. 目的与边界

本 RFC 解决四个容易互相污染的问题：

1. 配置保存、authority 启动和 worker 启动必须是可观测、可恢复的顺序，而不是用一个模糊的“启动中”状态覆盖全部阶段。
2. authority 的故障必须回到可修复的 Setup/Recovery 面；它不能被伪装成 provider 不可用，也不能用 `Thinking` 占位。
3. provider-only safe mode 只能作为已解析配置在后续 authority 启动失败时的窄诊断逃生面，不能成为绕过 authority、工具或审批的通用运行模式。
4. journal 损坏必须故障隔离并通过显式恢复动作处理；任何自动流程都不得静默删除、截断或重写损坏数据。

本文不引入新的 provider 语义，不把 provider 专项字段加入 kernel 公共契约，也不授权任何 UI 根据自然语言或提示文本推断故障类型。故障分类、恢复动作和重试资格必须来自 typed contract 与可审计证据。

## 2. Setup 状态机

### 2.1 唯一顺序

Setup 的正常启动路径固定为：

```text
Editing → ConfigSaved → AuthorityBooting → WorkerStarting → Ready
```

这是产品状态机的顺序约束，不要求各实现使用同名 enum。任何阶段只允许进入其右侧阶段；未满足前置条件时不得前进，也不得直接显示 `Ready`。

### 2.2 状态定义

| 状态 | 语义 | 允许的副作用与前置条件 |
| --- | --- | --- |
| `Editing` | 用户正在编辑配置，或上一次保存/启动失败后回到可修复表面。 | 只有草稿和校验状态；不启动 worker，不宣称 authority 已建立。 |
| `ConfigSaved` | 配置已通过必要解析/校验，并以明确的持久化结果成为本次启动的输入快照。 | 只接受该次保存得到的精确快照；配置落盘、CAS 或可见性失败时留在 `Editing`，不得覆盖旧的有效配置。 |
| `AuthorityBooting` | authority 正在依据精确配置快照执行当前 epoch 的启动与校验。 | 需校验 root/anchor、generation、lock、journal、process inventory、channel 和 cutover 前置条件；在 authority 完成前不启动可产生外部效果的 worker。 |
| `WorkerStarting` | authority 已完成并交给共享 runtime，worker 正在建立 provider/session/事件投影所需的运行时结构。 | 只能在 authority composition 和 cutover 已经成功后进入；worker 未发出 ready 事实前不能接受普通执行命令。 |
| `Ready` | authority、worker 以及首个可用的 ready/projection 信号均已确认。 | 只有此状态可以进入正常执行路径；provider 的单次请求失败也不能反向改写为 authority 已失败。 |

### 2.3 转移与失败回退

- `Editing → ConfigSaved`：只有在配置解析、校验和持久化结果都明确成功后发生。保存失败、源文件仍有冲突或写入可见性不确定时回到 `Editing`，并保留旧的有效配置与错误证据。
- `ConfigSaved → AuthorityBooting`：必须使用本次保存的精确快照，不得在启动时重新解释路径、隐式替换配置或混用另一 epoch 的状态。
- `AuthorityBooting → WorkerStarting`：只有 authority、journal、resource authority 和必要的 cutover 事实均已确认后发生。任何 anchor、lock、generation、journal、process inventory 或 durability 故障都回到 `Setup/Recovery`。
- `WorkerStarting → Ready`：只有 worker ready 事实已产生并完成最小投影后发生。provider 尚未建立时仍处于 worker 启动/恢复语义，不能拿 `Thinking` 掩盖启动阶段。
- 任意阶段发生未知、矛盾或 durability 不确定的事实：停止向右推进，回到可修复的 `Setup/Recovery`，保留 blocker、frontier 和证据；不得猜测为成功或普通 provider 失败。

Setup/Recovery 的“回退”不是清空状态：应保留可复用的配置快照、session identity、已确认的 durable facts 以及不确定效果的 frontier。仅在明确的用户修复或 operator recovery 动作之后，才允许重新从 `ConfigSaved` 或新的 `Editing` 继续。

## 3. Authority 故障的隔离与路由

### 3.1 分类原则

以下故障属于 authority/bootstrap/recovery 域，而不是 provider 域：

- root/anchor、epoch/generation、lock 或 process ownership 校验失败；
- authority channel、resource allocation、journal open/replay 或 durability 失败；
- 旧 authority 尚未 quiesce、恢复证据不足或 cutover 前置条件不满足；
- 配置快照与已绑定的 authority 身份不一致；
- journal corruption、identity drift、hash-chain/payload 校验失败，以及无法确认 durable frontier 的情况。

这类故障必须在 worker 执行普通任务前被捕获，映射到可修复的 Setup/Recovery blocker。它不能伪装为：

- `ProviderUnavailable`：该代码只适用于 authority 已健康、worker 已进入 provider 路径之后的 provider 连接、请求或响应故障；
- `Thinking`：该状态只表示实际 worker 已发起的 provider turn，不是启动等待、authority 等待或未知状态的占位符；
- 普通的 terminal `RunFailed`：不能用普通任务失败掩盖需要修复 authority 或保全证据的系统状态。

如果故障同时影响 provider 和 authority，按 authority 优先隔离：先阻断普通执行并报告 authority blocker，再由恢复流程处理 provider 路径，而不是选择较熟悉的 provider 错误码。

### 3.2 Typed recovery surface

跨表面投影必须保留结构化原因和动作。例如 authority journal 损坏使用 `AuthorityJournalCorrupted` 与 `RepairAuthority`；authority bootstrap 损坏使用 `AuthorityBootstrapCorrupted` 与 `SelectFreshAuthorityEpoch`。展示层可以翻译为中文提示，但不能依靠提示文本重新分类。

恢复事件只需暴露 opaque blocker/frontier、证据摘要和下一步动作，不得向 renderer、普通 TUI 状态或用户提示泄漏 bearer、凭证、原始 journal 内容或不必要的物理路径。恢复动作需要具备权限边界、幂等语义和持久化结果，不能通过“再试一次”隐式创建第二个 authority epoch。

## 4. Provider-only Safe Mode

### 4.1 进入条件和定位

provider-only safe mode 是 authority 修复期间的临时、非持久、provider-only 诊断面。它只有在以下条件同时成立时才可提供：

1. 配置已经成功解析并形成内存中的有效快照；
2. 后续 authority 启动、恢复前置条件或 cutover 失败；
3. 用户明确选择该安全面，或在用户已经明确发起 Start 且配置有效、authority-only boot 失败时由产品自动进入该安全面；产品必须明确标注当前不是正常 `Ready`。

缺失配置、配置语法错误、配置无法解析或快照身份不可信时不得进入 safe mode。safe mode 不是 authority 的替代品、不是权限提升、不是 provider fallback，也不产生新的 session/lease/epoch。

### 4.2 允许的能力

进入 safe mode 后，运行时只允许使用已解析快照中已确定的 provider route，执行有边界的内存 provider 请求、响应和状态展示。输出、错误和诊断信息仅可保留在受限的易失内存中；用户可以据此检查 provider 连接或修复配置，但不能据此宣称 workspace/session/authority 已可用。

### 4.3 明确禁止的副作用

safe mode 的“only provider”是硬边界，至少禁止：

- 所有 filesystem 读写副作用，包括创建目录、写/改/删/重命名配置、session、journal、artifact、workspace、cache 或 outbox；不得借 safe mode 自动修复或替换 authority 文件；
- 所有 OS process side effects，包括启动、终止、回收或接管子进程/进程树，以及 shell、terminal、sandbox 或外部命令执行。safe mode 中即使存在一个 in-process 控制壳，也不意味着允许任何 child process；
- 所有 MCP side effects，包括启动 stdio MCP、建立 MCP 网络连接、OAuth/token refresh、工具调用和工具结果持久化；
- builtin filesystem/workspace/process 工具、普通 agent tool registry、审批执行、session mutation、application command、resource allocation/lease、authority journal、事件 outbox 和 durable transcript；
- 任何会让 authority 看起来已经启动、让 worker 看起来已经 ready，或把 safe mode 的 provider 输出接入正常任务恢复链的行为。

因此 safe mode 的 worker 只能承载 provider-only 的受限交互，不是正常 worker 的降级副本。若 provider 请求自身失败，按 provider-only 诊断结果显示；若发现 authority blocker，仍必须显示 Setup/Recovery，而不能转换为 `ProviderUnavailable` 或 `Thinking`。退出 safe mode 后必须回到 Setup/Recovery，修复并重新完成完整状态机才可进入 `Ready`。

## 5. Journal Corruption：隔离、恢复与数据保全

### 5.1 检测与分类

以下任一事实都按 journal corruption 或 journal durability failure 处理：格式损坏、截断、未知 schema、header/instance mismatch、hash chain 或 payload 校验失败、重复或非单调序列、重解析后不再是同一个 regular file、已发布状态缺少必要快照、以及 rename/parent sync 等造成的 durability 不确定性。

replay 可以记录已验证的 prefix 作为证据，但不得采纳无法验证的 tail，不得根据“看起来像最后一条”的内容继续发放 authority、lease 或 workspace 效果。该故障的归属是 authority/storage recovery，不是 provider。

### 5.2 正常启动行为

正常 boot 在 journal 完整性和身份未确认前必须 fail closed：

1. 不发布新的 authority composition、resource allocation、lease 或 worker ready；
2. 不继续使用损坏 journal 的推测状态，不把它降级为普通 provider 错误；
3. 如独立的 bootstrap/recovery namespace 可安全写入，只记录失败启动证据、journal identity/digest 和恢复 frontier；该记录不能替代损坏 journal，也不能写回损坏文件；
4. 对外发布 typed blocker 与显式恢复动作，隐藏原始内容、凭证和不必要路径。

如果连独立的失败启动证据也无法持久化，必须把“证据不可持久化”作为更高优先级 blocker 保留在 Setup/Recovery，不能假装已经完成恢复。

### 5.3 恢复动作

恢复必须由显式的 doctor/operator recovery contract 驱动，动作顺序如下：

1. 留在 Setup/Recovery，保留配置身份、session identity、已确认的 durable facts 和损坏文件的证据摘要。
2. 使用独立的 authority bootstrap recovery namespace，读取失败启动证据与 authority inventory；确认旧 epoch/owner 已 quiesce。旧 owner 仍存活时不得抢占或创建新 epoch。
3. 生成绑定本次证据的 operator challenge，要求用户确认明确的修复动作；在确认后选择新的 authority epoch/root，并为新 epoch 创建、校验和持久化全新的有效 journal。
4. 进程在恢复中途崩溃时，优先 reconcile 已经 durable 的 fresh-epoch intent/receipt，保证重试幂等，不得因重复点击再创建第二个 epoch。
5. 如果之前存在未确认的物理效果或资源效果，先以 evidence/physical frontier reconcile；不得盲目 replay 可能已经生效的写操作。
6. 若无法证明安全恢复，保持 `UserConfirmationRequired`、`NeedsOperatorConfirmation` 或等价的 Recovery blocker；只有在有 durable evidence 支持时，才可把所属 Task/Run 标为 `Irrecoverable`。

### 5.4 不得静默删除损坏数据

任何正常 boot、自动重试或后台 recovery 都不得对损坏 journal 执行静默 `truncate`、覆盖、reset、删除或不可恢复的 quarantine。原始损坏字节必须保持可读取、可审计，并与新的 fresh epoch 明确隔离；创建新 journal 不等于修复或抹除旧 journal。

如未来提供显式的 operator quarantine，至少必须同时具备：typed action、精确的 journal identity/digest、durable receipt、可恢复的保存位置，以及原始字节仍被保全。本 RFC 不授权任何“为了让启动成功”而丢弃原始数据的实现。

同理，reconcile 不能以容量清理为名静默删除未知或缺失记录：存在活动 holder、无法证明归属或无法确认物理状态时，应保持 pending/quarantined 并要求 operator confirmation。恢复的目标是建立新的可验证 authority，不是让旧的损坏证据消失。

## 6. 跨表面映射

| 事实 | 用户可见归属 | 允许动作 | 禁止伪装 |
| --- | --- | --- | --- |
| 配置缺失、不可解析或保存失败 | `Setup/Editing` | 编辑、校验、重新保存 | provider fallback、`Thinking` |
| 配置有效但 authority boot 失败 | `Setup/Recovery` 或明确标记的 provider-only safe mode | 重试、修复 authority；用户发起 Start 后满足条件时自动进入 provider-only safe mode | `ProviderUnavailable`、`Thinking`、`Ready` |
| authority journal 损坏 | `Setup/Recovery`，typed `AuthorityJournalCorrupted` | `RepairAuthority`，经确认后 `SelectFreshAuthorityEpoch` | 普通 provider 错误、静默删除 journal |
| authority 健康且 worker 已运行，provider 请求失败 | provider recovery surface | provider 重试、切换或回报 provider 错误 | authority corruption、`Ready` |
| 实际 provider turn 正在进行 | worker/provider presentation | 显示 `Thinking` 及真实 turn 状态 | 用于表示 boot、authority wait 或未知状态 |
| safe mode 中请求 filesystem/process/MCP | safe-mode capability denial | 返回受限诊断错误，不产生副作用 | 启动工具、子进程、MCP 或任何持久化 |

Desktop、TUI、CLI 和 HTTP 可以分别选择提示、按钮和日志形态，但同一事实必须落在同一行语义中。特别是 Desktop/TUI 不得把 authority blocker 画成 provider banner；CLI/HTTP 也不得把恢复所需的 typed action 压平为一个可重试的 provider status。

## 7. 验收条件

实现或后续 review 至少需要验证：

- Setup 五态只能按 `Editing → ConfigSaved → AuthorityBooting → WorkerStarting → Ready` 前进；每个失败点都回到明确的 Setup/Recovery，且不产生假 `Ready`。
- authority anchor、lock、generation、journal、process ownership、durability 和 cutover 故障不会生成 `ProviderUnavailable` 或 `Thinking`；provider-only safe mode 的进入条件也不会覆盖 malformed/missing config。
- provider-only safe mode 的文件系统、子进程/进程树和 MCP 尝试均被拒绝且无副作用；safe mode 不写 session、journal、outbox、workspace 或 transcript。
- journal corruption 的原始字节、identity/digest 和失败证据在 recovery 前后可审计；正常启动不自动改写或删除损坏文件。
- fresh epoch 只能经显式、绑定证据的 operator recovery 创建；旧 owner 未 quiesce 时恢复被阻断；恢复中断后重试不会创建重复 epoch。
- Desktop/TUI/CLI/HTTP 的 typed reason/action、错误边界和状态投影保持一致；恢复动作不向普通 renderer 或 provider 路径泄漏凭证、路径或原始 journal 内容。

本次变更只同步本文档，不把以上验收条件表述为当前代码已全部通过；具体实现进度和历史证据仍以 RFC-0069、RFC-0071 及其各自 ledger/review 为准。

## 8. 参考文档

- [RFC-0069：Recoverability Boundaries, Plan Direct Execution and Workspace Concurrency V1](./0069-recoverability-boundaries-plan-materialization-and-workspace-concurrency-v1.md)
- [RFC-0071：Unified Resource Authority and Sandbox Lifecycle V1](./0071-unified-resource-authority-and-sandbox-lifecycle-v1.md)
- [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)
