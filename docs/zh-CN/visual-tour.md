# 视觉导览

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/visual-tour.md)

这一页用贴近真实终端布局的 SVG captures 说明 Sigil 的主要界面。

## 主 TUI 会话

![Sigil TUI session preview](../../site/assets/screenshots/tui-session.svg)

常规流程：

1. 在 workspace 中启动 `sigil`。
2. 在 composer 输入任务。
3. 在 transcript 中查看 repository reads、searches 和 tool activity。
4. 通过 info rail 检查 session、permissions、model、LSP、usage 和 controls。

## 审批检查

![Sigil approval review preview](../../site/assets/screenshots/approval-review.svg)

高风险动作运行前，检查：

- tool summary；
- affected files；
- diff preview；
- allow 或 deny action。

如果 diff 不符合意图，deny 并要求更窄的改动。

## 配置面板

![Sigil config panel preview](../../site/assets/screenshots/config-panel.svg)

使用 `/config` 修改常见 provider、permission、memory、compaction、code intelligence、terminal、Agents、Skills、插件信任审查和 MCP settings。低频 provider 细节仍留在 `sigil.toml` 和环境变量中。
