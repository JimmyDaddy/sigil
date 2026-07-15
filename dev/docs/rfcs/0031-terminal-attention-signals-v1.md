# RFC-0031 Terminal Attention Signals V1

状态：accepted / implemented

创建日期：2026-07-16

基线：

- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)
- Implementation baseline: `9c877f1bd02e88ad20cb0df1471fa4fcabca3034`

## 1. Summary

本 RFC 为 Sigil TUI 增加默认关闭、隐私收紧的终端 attention signals。用户可在 `[terminal.notifications]` 或 TUI `/config` 中选择 `auto`、`osc9`、`osc777`、`bell`，接收长任务完成、工具审批等待、运行失败和 MCP elicitation 等待输入四类提示。

该能力只是 TUI 将既有 worker/run event 投影为 terminal escape sequence 的进程内副作用。它不注册 durable event，不改变 session/control state，不进入 provider context、machine protocol 或恢复语义。

## 2. Goals

1. 长运行完成或 Sigil 等待用户动作时，即使终端窗口不在前台，用户也能获得注意提示。
2. 默认关闭，显式支持 `auto / osc9 / osc777 / bell`，并允许 TUI 内配置。
3. 通知 payload 不包含 prompt、reply、路径、tool/MCP/provider/session/error 详情。
4. focus-aware、bounded、deduplicated，并在写入失败时不影响 agent/TUI 主流程。
5. 保持 notification 为 ephemeral projection，不扩 durable truth。

## 3. Non-goals

- native desktop notification API、custom shell command、webhook、OSC 99 或移动端推送；
- 通知点击回传、按钮、icon、delivery receipt 或 durable history；
- headless CLI/HTTP/serve notifications；
- 动态拼接模型文本、tool name、路径、参数、错误或 MCP server；
- 把 notification 成功解释为 approval/elicitation 已处理。

## 4. Configuration Contract

```toml
[terminal.notifications]
enabled = false
method = "auto"
minimum_run_duration_ms = 10000
```

`method` 只接受 `auto`、`osc9`、`osc777`、`bell`。阈值合法范围为 `1_000..=3_600_000`；非法值在配置校验中报出精确字段，不静默 clamp。旧配置没有该 section 时保持 disabled。

保存 `/config` 后即时读取新配置；是否启用 terminal focus reporting 随配置变化，不要求重启。

## 5. Event Projection

| Existing event | Signal | Gate |
| --- | --- | --- |
| foreground start + matching successful finish | `long_run_completed` | elapsed >= configured threshold |
| `RunEvent::ToolApprovalRequested` | `approval_required` | first unseen call id / cooldown |
| `WorkerMessage::RunFailed` | `run_failed` | one failure terminal, no error text |
| `WorkerMessage::McpElicitationRequest` | `user_input_required` | first unseen request / cooldown |

短回复、普通工具完成、取消、progress、notice、MCP list changed 和 provider refresh 不触发。恢复 session 不补发；缺失 start 不推测 run duration。

payload 使用固定英文文案，以保证 terminal/OS notification 在任意 TUI locale 下稳定可识别并避免泄露。TUI 内现有本地化/详细错误仍是查看细节的唯一表面。

## 6. Terminal Method Policy

- OSC 9：已知 iTerm2/WezTerm/Ghostty/Kitty family；
- OSC 777：已知 VTE/rxvt family；
- BEL：Apple Terminal、VS Code、Windows Terminal、Alacritty 和 unknown；
- explicit method 始终覆盖 auto；
- tmux/screen 下 OSC 使用 DCS passthrough，BEL 不包装；
- OSC 使用 `ST` 结束，BEL 只在 `bell` method 发出。

环境探测只产生 allowlisted family/method decision，不持久化原始环境值。unsupported/unknown 不做危险 capability query，保守回退 BEL。

## 7. Focus, Timing and Dedupe

launcher 仅在 notifications enabled 时启用 focus-change reporting。收到过 focus event 后，focused 状态才被视为可靠并抑制通知；从未收到 focus event 时不假装支持，继续发送。

controller 以进程内 `Instant` 记录 foreground run；main/plan/skill/follow-up、agent 和 task 使用不同 key。相同 attention key 20 秒内最多发送一次；resolved/terminal state 清理 key。所有 timer、focus 和 dedupe state 随进程退出丢弃。

## 8. Ownership

- `sigil-kernel`：纯配置 schema/default/validation；不增加事件或 session 字段。
- `sigil-tui::attention`：signal、timer、focus、dedupe、terminal family/method、codec。
- `sigil-tui::launcher`：唯一 terminal owner，观察 borrowed worker message 并写/flush bytes。
- `sigil-tui` config panel：Terminal 配置 UI。
- `sigil-runtime` doctor：粗粒度 enabled/method/threshold 检查，不显示 payload/env。

## 9. Security and Privacy Invariants

1. notification content 来自四个固定枚举值；任何 prompt/path/tool/error/MCP canary 都不得进入输出 bytes。
2. 不运行 command，不访问网络/clipboard/file，不写 workspace/state/cache/session。
3. codec 拒绝控制字符、换行和协议 delimiter，输出有硬上限。
4. 写入失败只能 debug log，不能失败 run 或退出 TUI。
5. notification 不参与 durable replay、approval authority、MCP response、machine protocol 或 provider request。

## 10. Implementation Slices

1. R31.0：RFC、技术方案、execution plan、status。
2. R31.1：kernel config/validation、TUI config、runtime doctor 和 contract tests。
3. R31.2：attention controller/codec、launcher/focus integration、worker/run event projection 和测试。
4. R31.3：real PTY acceptance、EN/ZH docs/site、workspace gates 和完整度审查。

## 11. Acceptance Criteria

- 默认配置和旧配置不会写 notification byte；
- 四种 method 精确编码，auto/unknown/tmux/screen 行为有测试；
- 短 run 不通知，长 run 只通知一次，缺失 start 不通知；
- approval、failure、elicitation 各触发一次，focus/cooldown 抑制可证明；
- canary prompt/path/tool/error/server 不进入 notification frame 或 support bundle；session JSONL 只保留既有正常事实，不新增 notification event，也不复制固定通知正文（正常用户 prompt 仍按既有会话契约持久化）；
- notification write failure 不改变 AppState 既有处理结果；
- TUI `/config`、doctor、EN/ZH docs/site 与真实实现一致；
- real binary PTY 验证 default-off 和 explicit BEL；
- targeted tests、workspace fmt/check/test/Clippy、docs/site 和 diff gates 通过。

## 12. Validation

```bash
cargo test -p sigil-kernel terminal_notification
cargo test -p sigil-runtime terminal
cargo test -p sigil-tui attention
cargo test -p sigil-tui config
./scripts/tui-attention-signals-pty-acceptance.py
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
./scripts/check-pages-site.sh
git diff --check
```

## 13. Progress

- R31.0 complete：技术方案、正式 RFC、执行台账和 commit/gate 边界已冻结于 commit `ce44f8bc`。
- R31.1 complete：kernel 默认值与边界校验、TUI Terminal 配置入口、保存回读和 runtime doctor 投影已完成；受影响 crate 全量测试、Clippy、格式与 docs gate 通过。
- R31.2 complete：纯 attention controller/codec、focus telemetry、launcher 动态启停与 cleanup 已完成；固定 payload、cooldown、threshold、multiplexer passthrough 和 non-fatal write failure 均有测试覆盖。
- R31.3 complete：真实 binary PTY 已证明 default-off 零通知字节、explicit BEL 恰好一次、focus reporting cleanup 对称，且 notification frame、session/state/cache 均满足隐私与 durable boundary；EN/ZH 文档、site、workspace fmt/check/test/strict Clippy、docs/Pages/diff gates 和两轮实现审查全部通过。
