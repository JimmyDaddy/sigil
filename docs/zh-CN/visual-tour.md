# 视觉导览

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/visual-tour.md)

本页通过真实 TUI renderer 生成的 SVG 截图介绍 Sigil 的主要界面。

## 主 TUI 会话

![Sigil TUI 主会话预览](../../site/assets/screenshots/tui-session.svg)

常规流程：

1. 在 workspace 中启动 `sigil`。
2. 在输入框中输入任务。
3. 在会话记录中查看仓库读取、搜索和工具活动。
4. 通过信息栏检查会话、权限、模型、LSP、用量和操作提示。

## 审批检查

![Sigil 工具审批预览](../../site/assets/screenshots/approval-review.svg)

高风险动作运行前，检查：

- 工具摘要；
- 受影响文件；
- diff 预览；
- `allow` 或 `deny` 操作。

如果 diff 不符合预期，选择 `deny`，并要求更窄的改动。

## 配置面板

![Sigil 配置面板预览](../../site/assets/screenshots/config-panel.svg)

使用 `/config` 修改常用的 provider、权限、memory、compaction、code intelligence、终端、Agents、Skills、插件信任和 MCP 设置。低频 provider 细节仍留在 `sigil.toml` 和环境变量中。

## 任务验证

![Sigil 任务 Verification card 预览](../../site/assets/screenshots/verification-card.svg)

Durable task 需要完成证据时，Verification card 会把当前 verdict、推荐检查与证据放在一起。按 `Alt-V` 聚焦，按 `I` 查看 snapshot 与 changeset 细节；如果存在绑定检查，可按 `Enter` 运行。

## Checkpoint 恢复

![Sigil checkpoint 恢复预览](../../site/assets/screenshots/checkpoint-restore.svg)

空闲时按 `Ctrl-R`，Sigil 会从 durable evidence 重建最新受控 checkpoint。Restore 前先 review 精确 reverse diff；如果只想回到较早对话上下文而不改变共享 workspace 文件，可选择 conversation fork。Shell 与远端副作用不在该文件恢复边界内。

## 上下文压缩预览

![Sigil 上下文压缩预览](../../site/assets/screenshots/compaction-preview.svg)

用 `/compact` review 哪些旧消息会被折叠，以及目标请求为何可用或不可用。当前界面只用于只读 review：Context Compaction V2 apply 在修复正确性问题期间仍暂时冻结。
