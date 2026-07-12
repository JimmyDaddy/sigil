# Appearance

[Docs home](README.md) · [Configuration](configuration.md) · [Permissions and sandbox](permissions-and-sandbox.md) · [Advanced configuration](advanced-configuration.md) · [Field reference](configuration-reference.md) · [简体中文](../zh-CN/appearance.md)

Sigil's appearance settings affect only the TUI. They do not change model requests, approvals, session history, or tool data.

## Start With A Built-in Theme

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

Built-in `theme` values are `sigil_dark`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `nord`, and `high_contrast_dark`. In the TUI, open `/config`, select Appearance, and use `Enter` on Theme to preview the available choices before saving with `Ctrl-S`.

`syntax_theme` controls code highlighting in markdown, tool previews, and approval summaries. `auto` follows the selected TUI theme. You can choose `catppuccin_mocha`, `catppuccin_latte`, `solarized_dark`, `solarized_light`, `gruvbox_dark`, `gruvbox_light`, `nord`, `one_half_dark`, `one_half_light`, or `monokai` explicitly.

`usage_cost_currency` changes only the display format for usage estimates. `auto` follows a provider balance currency when available and otherwise uses USD; `usd` and `cny` force a display choice.

## Override A Few Semantic Colors

```toml
[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

Color overrides accept only `#RRGGBB`. Choose semantic roles such as `accent_primary` or `text_muted`, rather than trying to style one component at a time. The TUI previews an Appearance draft immediately so you can compare the composer, tool cards, approvals, diffs, status indicators, and markdown before saving.

The color editor in `/config` lets you select a group and token, type or paste a value, clear one token with `Backspace` or `Delete`, clear a group, or clear all overrides with `Ctrl-R`.

## Keep The TUI Readable

Run `sigil doctor` after changing colors. It reports invalid values and warns about weak text contrast, indistinct status colors, or unclear structural cues. A warning does not silently change your palette; it tells you which role is likely hard to read.

Good starting rules:

- Keep `text_primary` readable on the main surface, panels, input, and user messages.
- Keep success, warning, error, and pending states visually distinct.
- Make selected text readable on both selected rows and buttons.
- Keep added and removed diff colors distinct from their backgrounds.
- Override a small number of tokens before attempting a full custom palette.

The complete token groups and field defaults are in the [Configuration reference](configuration-reference.md#appearance). For terminal-specific controls such as mouse capture and copy behavior, see [Advanced configuration](advanced-configuration.md#terminal-and-model-request-overrides).
