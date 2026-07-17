# 公开文档与实现文档边界

状态：active maintenance note

创建日期：2026-07-13

关联：

- [RFC-0022 Public Documentation Information Architecture](rfcs/0022-public-documentation-information-architecture.md)
- [RFC-0012 Protocol and App Server Boundary](rfcs/0012-protocol-app-server-boundary.md)
- [RFC-0013 Eval Harness](rfcs/0013-eval-harness.md)
- [Sigil Rust Agent Core Technical Solution](sigil-rust-agent-core-technical-solution.md)

## 目的

公开文档应帮助用户选择 provider、配置权限、理解当前支持范围并完成日常任务。它不承担 Rust crate 边界、协议 DTO、session projection、评估 fixture 或 transport lifecycle 的解释责任。

本说明记录从公开文档迁出的实现语义，避免未来在用户路径中重新混入 `kernel`、`adapter`、`Context V0`、`eval`、`receipt` 等内部名词。

## Provider 边界

`sigil-kernel` 只持有 provider-neutral 的 agent、tool、session、approval 和 event 契约。各 provider crate 解释其 request/response、stream、tool-call、header、continuation 和能力差异。`sigil-runtime` 负责跨 TUI、CLI 和将来入口的装配，入口层不得重新解释 provider 私有字段。

公开页面只需说明：用户如何选择 provider、设置凭据、哪些选项在该 provider 页面上，以及正常的 approval/privacy/session 行为保持一致。

## Context 与验证边界

Context V0、source provenance、projection、verification receipt、workspace snapshot、deterministic eval fixture 和 model-eval policy 都是开发者实现或质量证据术语。它们的语义分别由 RFC-0001、RFC-0003、RFC-0006 和 RFC-0013 维护。

公开页面只需说明用户可观察的效果：Sigil 可以在有边界的情况下使用相关会话、任务与工作区信息；检查结果会在文件变化后过期；并非所有自动化质量实验都构成面向用户的产品能力。

## Local Server 边界

`sigil-http` 是 transport-thin 的本地 HTTP/SSE 入口，它不能复制 agent loop、permission、approval、verification 或 durable session truth。版本化 command envelope、durable replay 和 transient live event 的边界由 RFC-0012 和 RFC-0016 维护。

RFC-0026 已让 `sigil serve` 接入真实共享运行时、loopback bearer listener、retained event replay 与 graceful shutdown。公开文档现在可以说明用户如何启动、认证、读取事件和安全停止，但仍只描述可观察效果；durable command identity、journal、driver ownership、cursor fencing 与内部状态转换继续留在 RFC 和开发者文档中。V1 不得被宣传为 remote、multi-user 或自动启动的 daemon。

## 写作规则

新增公开文档时：

1. 先写用户能做什么、看到什么、如何安全地配置和如何恢复。
2. 如果需要解释限制，描述可验证效果，而不是内部类型或 module 名称。
3. 只有开发者需要维护的契约、版本、结构、测试证据和迁移规则写入 `dev/docs` 或 RFC。
4. 用户页面与开发者页面都必须链接到正确的同类文档；不得让 README 或 quickstart 成为实现细节索引。

## 内容层级

公开内容按职责分为四层，越靠前越短：

1. README、站点首页与 docs hub 只负责定位、差异点和下一步。
2. Quickstart、Visual Tour、Workflows、Cookbook 与 User Guide 负责首次成功和日常任务。
3. Configuration、Safety、Privacy 与 Status 负责选择、风险和支持边界。
4. Installation、Reference、Configuration Reference、provider、MCP 与 terminal 页面负责精确查询。

后一层可以被前一层链接，但前一层不能复制后一层的完整表格、全部安装渠道或字段百科。内容预算是防止职责回流的上限，不是填充目标。

## 单一事实来源

- 安装、更新、卸载与精确发行版本由 Installation 和 Changelog 维护；README、Quickstart 与首页各只保留一个推荐命令。
- 完整键位、slash command、CLI、路径和 machine-output 矩阵由 Reference 维护。
- Provider 凭据名称只在 provider 选择页、对应 provider 页和匹配的配置示例出现。
- 配置字段由 Configuration Reference 维护；流程页只保留完成任务所需的最小配置。
- 审批风险、permission/network/sandbox、数据与 credential 分别由 Safety、Permissions And Sandbox、Privacy 维护。

`dev/docs/public-documentation-content-policy.json` 记录 26 对公开页面、职责、双语标题、双语 CTA 标签与目标、关键安全 topic、hub 路由和搜索权威结果。每个 Markdown source 的首行 role marker 必须与 policy 中的 slug、role、section key 和 CTA key 完全一致，末尾 CTA marker 与正文链接必须精确匹配对应语言的 label 和 target；生成站点会移除这些 marker。Checker 还持有固定的 README、locale、26-page、配置示例和手写 HTML inventory，不能通过缩小 policy 逃避扫描。

`scripts/check-public-doc-content.rb` 消费该文件并验证固定 page/slug/topic/search/authority inventory、职责 marker、双语标题、末尾 CTA 标签与目标、section/topic 正文、内容预算和单一事实来源。`scripts/check-public-doc-parity.rb` 比较每个 EN/ZH section 的链接、图片、代码、topic、CTA、表格和列表结构。`scripts/test-public-doc-content.rb` 在临时 source root 中注入独立负例，证明实现术语、版本、inventory、职责、标题、CTA、安全正文和预算回流都会失败。

## Allowlist 维护

Allowlist 只用于产品入口确实需要的最小重复，例如 README、Quickstart 和首页各一条推荐安装命令，以及 provider 配置示例中的对应环境变量。每条必须同时写明精确文件、规则、行匹配和原因；不能使用目录级或术语级豁免。

新增豁免前先判断是否应改为链接到权威页。确需豁免时，同一变更必须补一个负例，证明匹配范围之外的相同内容仍会被 gate 拒绝。公开内容 gate 的失败格式固定包含文件、行号、规则和权威来源，便于维护者直接迁移内容。
