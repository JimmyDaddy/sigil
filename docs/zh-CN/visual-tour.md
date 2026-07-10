# 视觉导览

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/visual-tour.md)

本页通过贴近真实终端布局的 SVG 截图介绍 Sigil 的主要界面。

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
