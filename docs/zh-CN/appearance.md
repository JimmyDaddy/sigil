<!-- public-doc-role: appearance; authority: appearance-guide; sections: start-with-a-built-in-theme,override-a-few-semantic-colors,keep-the-tui-readable; cta: open-appearance-reference -->

# 外观

[文档首页](README.md) · [配置](configuration.md) · [字段参考](configuration-reference.md) · [English](../en/appearance.md)

外观设置只改变 TUI 的显示效果，不会影响模型请求、审批、已保存的对话或工具数据。

## 从内置主题开始

```toml
[appearance]
info_rail = true
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

`info_rail` 控制宽度允许时右侧信息栏是否默认显示。`F2` 改变当前运行的显隐；`F4` 打开可滚动的工作区与会话详情，信息栏隐藏时也可使用（兼容 `Alt-I` / `Shift-F2`）。按 `F3` 保留输入草稿并打开设置，或使用 `/config`，然后进入 **Appearance**，用 `Enter` 预览主题，再用 `Ctrl-S` 保存。

`syntax_theme = "auto"` 跟随 TUI 主题。`usage_cost_currency` 只影响显示。所有可用值见[配置字段参考](configuration-reference.md#外观)。

## 覆盖少量语义颜色

```toml
[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

颜色覆盖使用 `#RRGGBB` 格式的语义标记。建议先调整少量颜色，不要一次重做所有组件。`/config` 会显示覆盖数量和主题预览，但细粒度颜色在面板中只读；新增、修改或删除需要编辑 `sigil.toml`。

## 保持 TUI 可读

修改颜色后运行 `sigil doctor`。它会提示对比度不足、状态难以区分或文件差异配色不清，但不会擅自替换你的颜色。请确保正文清晰可读，成功、警告和错误状态容易区分，选中区域足够明显，同时让新增行和删除行都能从背景中辨认出来。

<!-- public-doc-cta: open-appearance-reference -->
下一步：[查找精确外观字段](configuration-reference.md)。
