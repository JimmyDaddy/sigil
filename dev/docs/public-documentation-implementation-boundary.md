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

在 `sigil serve` 接入真实共享运行时之前，公开文档只能描述它会检查本地服务设置、尚不启动服务。不要将 preflight 输出或 library-level listener smoke 宣传为可供用户使用的 server。

## 写作规则

新增公开文档时：

1. 先写用户能做什么、看到什么、如何安全地配置和如何恢复。
2. 如果需要解释限制，描述可验证效果，而不是内部类型或 module 名称。
3. 只有开发者需要维护的契约、版本、结构、测试证据和迁移规则写入 `dev/docs` 或 RFC。
4. 用户页面与开发者页面都必须链接到正确的同类文档；不得让 README 或 quickstart 成为实现细节索引。
