# 外观

[文档首页](README.md) · [配置指南](configuration.md) · [权限与沙箱](permissions-and-sandbox.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [English](../en/appearance.md)

Sigil 的外观设置只影响 TUI，不会改变模型请求、批准、会话历史或工具数据。

## 从内置主题开始

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

内置 `theme` 值包括 `sigil_dark`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`nord` 和 `high_contrast_dark`。在 TUI 中打开 `/config`，选择 Appearance，在 Theme 上按 `Enter` 就可以保存前预览不同主题，使用 `Ctrl-S` 保存。

`syntax_theme` 控制 markdown、工具预览和批准摘要里的代码高亮。`auto` 跟随 TUI 主题；也可以显式选择 `catppuccin_mocha`、`catppuccin_latte`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`gruvbox_light`、`nord`、`one_half_dark`、`one_half_light` 或 `monokai`。

`usage_cost_currency` 只改变用量估算的显示格式。`auto` 会在可用时跟随 provider 余额货币，否则显示 USD；`usd` 与 `cny` 可固定显示方式。

## 少量覆盖语义颜色

```toml
[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

颜色覆盖只接受 `#RRGGBB`。请使用 `accent_primary`、`text_muted` 这样的语义角色，而不是尝试为单个组件写样式。TUI 会立即预览 Appearance draft，可以在保存前对比 composer、工具卡片、批准、diff、状态指示和 markdown。

`/config` 的颜色编辑器可以选择 group 与 token、输入或粘贴值、用 `Backspace` 或 `Delete` 清除一个 token、清除一个 group，或用 `Ctrl-R` 清除所有覆盖。

## 保持可读性

修改颜色后运行 `sigil doctor`。它会报告无效值，并警告文字对比度不足、状态颜色难以区分或结构提示较弱的情况。警告不会静默改写你的主题，只会告诉你哪个角色可能难以阅读。

建议从以下原则开始：

- 让 `text_primary` 在主背景、面板、输入区和用户消息上保持可读。
- 让成功、警告、错误与 pending 状态一眼可区分。
- 让选中行和按钮上的选中文字保持可读。
- 让 added 与 removed diff 颜色及其背景可以区分。
- 先覆盖少量 token，再尝试完整自定义主题。

完整 token group 和默认值见[配置字段参考](configuration-reference.md#appearance)。鼠标、复制等终端控制见[高级配置](advanced-configuration.md#终端与模型请求环境变量覆盖)。
