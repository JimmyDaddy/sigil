# 用户 Prompt 语义路由全仓审计

审计日期：2026-08-20

## 1. 规则

本审计执行 [`code-standards.md`](code-standards.md) 2.6：production host 不得通过固定中英文
短语、关键词、正则或 prompt alias 推断用户意图并选择功能。语义理解属于模型；模型通过受限
typed tool / schema 表达决定，host 只验证结构、durable authority、安全、资源和幂等前置条件。

以下不属于违规：显式 `/command` / `@agent` 语法、协议与配置 enum、用户已明确调用搜索工具后
的 literal match、路径/标识符检索、shell/tool DSL 解析、secret/credential/PII 防护、provider
错误/markup 解析、纯展示格式化以及测试断言。

## 2. 审计范围与方法

- production Rust：`crates/*/src/**/*.rs`
- Desktop production TypeScript / Rust：`apps/desktop/src/**/*`、`apps/desktop/src-tauri/src/**/*`
- 排除 physical tests、generated contract 和 fixture；测试中的短语只用于证明 host 不读取文案
- 搜索 prompt / guidance / user message 数据流上的 `contains`、prefix/suffix、case-fold、regex、
  equality、Rust `match`、TypeScript `switch`、phrase table、intent-hint helper，并人工复核命中点
  的输入来源和功能影响
- 运行 `scripts/check-no-prompt-phrase-routing.py`；其 detector 另有独立自测

## 3. 已发现并修复

### `sigil-runtime` 自动 Context V2 查询画像

原实现位于 `crates/sigil-runtime/src/context.rs`，存在同一违规簇：

1. 用 `rust/source/function/definition/源码/函数/模块` 等固定词判断 `source_intent`；
2. 用 `diagnostic/error/warning/报错/诊断` 判断 `diagnostic_intent`；
3. 用 `reference/usage/调用/引用` 判断 `reference_intent`；
4. 用固定英文自然语言 stop-word 表决定哪些 prompt token 可以影响源码检索。

修复后不再生成 host-owned semantic intent。Context V2 只做统一的显式路径、代码形态标识符和
path/symbol-gated 词法匹配；LSP symbol / diagnostic / reference 只有在实际路径、名称、消息或
preview 与查询 term 相交时才进入候选。需要进一步理解“查诊断”“找引用”“读取源码”时，由模型
调用对应 typed code-intel tool。

## 4. 全仓复核分类

| 类别 | 结论 |
|---|---|
| Conversation / Plan / Task route 与 continuation | 已使用模型 typed choice + host-owned Plan/Task identity；未发现残留 prompt phrase fallback |
| TUI / Desktop `/command`、`@agent`、selector | 显式用户语法或列表过滤，合规 |
| read/grep/web/code-intel/session catalog 查询 | 用户已选择的 literal search，不推断功能，合规 |
| shell、permission、workspace/path confinement | 解析 tool DSL / effect / capability，不读取用户 prompt 意图，合规 |
| request-user-input 与 persistence secret 检测 | 安全拒绝/脱敏，属于必须独立于模型的防护，合规 |
| provider/MCP/HTTP/config/error/markup parser | 协议或稳定字段解析，合规 |
| TUI 状态色、标题、notice 与 tool-card parser | 纯展示，不改变执行语义，合规 |
| tests/evals/acceptance fixtures | 仅验证行为或模拟输入，不属于 production route，合规 |

## 5. 回归证据

- `context_source_symbol_candidates_do_not_use_natural_language_as_hidden_intent`：自然语言噪声不再
  作为 source intent 或 content-only 入选依据
- `safe_context_sources_do_not_turn_natural_language_into_lsp_switches`：diagnostic/reference/source
  词本身不能开启 LSP 行为
- `auto_routing_exposes_model_handoff_without_classifying_prompt_text`：普通会话只向模型曝光 typed
  handoff，不由 host 分类 prompt
- `draft_ready_plan_replaces_ordinary_route_surface_with_typed_decisions`：存在待确认 Plan 时冻结成
  `run_pending_plan` / `keep_pending_plan` 二选一，而不是匹配“继续/执行”
- `typed_resume_receipt_recovers_without_prompt_matching`：恢复消费 typed receipt 与 durable identity

## 6. 当前结论

静态 gate 与人工数据流复核后，当前未发现剩余 production 用户 prompt 短语路由。静态检查是
最低门禁而不是形式化信息流证明；后续新增自然语言入口时，review 仍必须确认局部变量重命名、
helper 封装或跨函数传递没有把 phrase matching 重新带回功能选择。
