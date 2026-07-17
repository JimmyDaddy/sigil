<!-- public-doc-role: appearance; authority: appearance-guide; sections: start-with-a-built-in-theme,override-a-few-semantic-colors,keep-the-tui-readable; cta: open-appearance-reference -->

# Appearance

[Docs home](README.md) · [Configuration](configuration.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/appearance.md)

Appearance changes only the TUI display. It does not change model requests, approvals, saved conversation, or tool data.

## Start With A Built-in Theme

```toml
[appearance]
info_rail = true
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

`info_rail` controls whether the right rail starts visible when width allows. `F2` changes visibility for the current run; `Shift-F2` switches a visible rail between compact and detail. In `/config` → **Appearance**, preview theme choices with `Enter` and save with `Ctrl-S`.

`syntax_theme = "auto"` follows the TUI theme. `usage_cost_currency` affects display only. The complete accepted values are in [Configuration Reference](configuration-reference.md#appearance).

## Override A Few Semantic Colors

```toml
[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

Overrides use `#RRGGBB` semantic tokens. Start with a few roles rather than restyling every component. `/config` shows the override count and theme preview, but fine-grained tokens are read-only there; add, change, or remove them in `sigil.toml`.

## Keep The TUI Readable

Run `sigil doctor` after color changes. It warns about weak text contrast or indistinct status and diff colors without silently replacing your palette. Keep primary text readable, success/warning/error states distinct, selection visible, and added/removed lines separable from their backgrounds.

<!-- public-doc-cta: open-appearance-reference -->
Next: [Look up exact appearance fields](configuration-reference.md).
