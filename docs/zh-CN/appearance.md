<!-- public-doc-role: appearance; authority: appearance-guide; sections: start-with-a-built-in-theme,override-a-few-semantic-colors,keep-the-tui-readable; cta: open-appearance-reference -->

# 外观

[文档首页](README.md) · [配置](configuration.md) · [字段参考](configuration-reference.md) · [English](../en/appearance.md)

外观只改变 TUI 显示，不会改变 model request、审批、已保存对话或工具数据。

## 从内置主题开始

```toml
[appearance]
info_rail = true
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

`info_rail` 控制宽度允许时右侧信息栏是否默认显示。`F2` 改变当前运行的显隐；`Shift-F2` 在紧凑和详细内容间切换。进入 `/config` → **Appearance**，用 `Enter` 预览主题，再用 `Ctrl-S` 保存。

`syntax_theme = "auto"` 跟随 TUI 主题。`usage_cost_currency` 只影响显示。所有可用值见[配置字段参考](configuration-reference.md#appearance)。

## 覆盖少量语义颜色

```toml
[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

Override 使用 `#RRGGBB` 语义 token。先调整少量角色，不要一次重做所有组件。`/config` 会显示 override 数量和主题预览，但细粒度 token 在面板中只读；新增、修改或删除需编辑 `sigil.toml`。

## 保持 TUI 可读

修改颜色后运行 `sigil doctor`。它会提示弱对比度、难以区分的状态或 diff 颜色，但不会静默替换 palette。保持主文本可读、成功/警告/错误状态可区分、selection 清晰，并确保新增/删除行与背景分离。

<!-- public-doc-cta: open-appearance-reference -->
下一步：[查找精确外观字段](configuration-reference.md)。
