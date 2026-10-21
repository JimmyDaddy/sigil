# 默认主题的文字对比度

默认 Desktop 浅色、深色与 TUI Sigil-dark 的弱文字仍承载可用信息：字段 placeholder、选中会话的 metadata、主题说明、审批字段标签与键位提示。本次只调整三个文字 token，保留正文与次正文的层级。

| 文字 token | 原值 → 新值 | 已核对的最差实际背景 | 比值前 → 后 |
|---|---|---|---:|
| Desktop dark subtle | `#858e94` → `#8e979d` | 选中主题说明 `#123432` | 4.0270 → 4.5194 |
| Desktop light subtle | `#687178` → `#626b72` | 选中主题说明 `#d9f1ef` | 4.2111 → 4.5995 |
| TUI Sigil-dark muted | `#636d7e` → `#7b8596` | input `#191d25` | 3.2314 → 4.5326 |

Desktop 的 token 定义在 [`reference.css`](../../apps/desktop/src/ui/foundations/reference.css)；TUI 定义在 [`builtin.rs`](../../crates/sigil-tui/src/ui/theme/builtin.rs)。TUI 审批键位说明在 `#12171a` 背景上的比值由 3.4554 提高至 4.8467。默认正文、次正文、disabled 和其它主题不随本次调整改变。

计算使用 [WCAG 2.2 的 sRGB 相对亮度](https://www.w3.org/TR/WCAG22/#dfn-relative-luminance)与[对比度公式](https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio)，以未舍入结果比较普通文字的 4.5:1 参考值。表格只为阅读保留四位小数；颜色选择按保持通道差值的整数平移检查，浅色还覆盖额外 error-container 的 4.5601:1。

这只证明已列明的不透明文字/背景组合。终端 ANSI profile 色、透明混合、hover filter、字体与抗锯齿仍需要各自验证；数值达标不等于全应用合规或原生 UI 已通过。后续主题变更应同时检查 active 提示与正文，不把仍有操作意义的弱文字当作 disabled 豁免。
