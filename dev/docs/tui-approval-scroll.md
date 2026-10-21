# TUI 审批正文滚动

长写入 diff 与完整命令预览的 End、PageDown、鼠标滚动必须停在最后一个可见正文窗口。
End 后 PageUp 应立即向上移动；Home 返回开头。审批身份、原始 preview 与允许/拒绝操作不受滚动影响。

host approval renderer 以当前实际边框、padding、状态栏、summary 与正文 viewport 计算布局，
使用既有保留 style/span 与终端字素宽度的 `wrap_terminal_lines` 后按可见行切片。状态更新与
渲染共用同一布局及内容计算；仅在宽度或高度实际变化时重新 clamp，避免每帧重复展开全文。
滚动值全程保留 `usize`，不先缩窄为 u16，
也不以固定行数代替真实内容上界。public TUI framework 无 Sigil approval 依赖。

同域测试实际渲染 80×24、166×48 与 resize 后的窗口，检查末行、diff 新增颜色、命令末行、
超过 u16 行数的尾部，以及 End → PageUp → 重复 PageDown → committed mouse overscroll →
Home 的像素内容和 frozen guard。原有滚动路由测试使用真正超过 viewport 的长 diff，避免
把短内容的无效偏移当作成功。上述结构化渲染证据仍需另配原生窗口复验。
