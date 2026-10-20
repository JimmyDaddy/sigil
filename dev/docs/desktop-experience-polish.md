# Desktop 信息层级与 Markdown 写作体验

本文记录桌面端的呈现约束与本轮改造。Desktop 与 TUI 继续共用执行、权限、审批与恢复语义；此处的界面变化不改变 HTTP、IPC、durable session 或工具行为。

## 工作台信息层级

正文、真实结果和当前决定优先。系统字体、语义字号与留白区分正文、控件及辅助信息，背景采用平面层级；按钮、菜单与字段继续复用内部 `ui` API 和既有 CSS layers/tokens。

会话侧栏默认宽 280px，已保存的用户宽度继续使用。小于 900px 时复用导航抽屉，输入区的模型/权限/推理选项允许换行，不靠把文字压到难以阅读解决空间不足。原生窗口最小尺寸由 native 壳另行约束，组件响应式检查不能证明原生窗口已经支持对应尺寸。

上下文容量未知或尚在加载时，不显示“—”占位；已知的使用率保留简短入口，缓存与 token 诊断在同一上下文详情中查看。这些辅助观察不成为发送门禁。

设置先呈现连接、默认模型、外观及日常偏好，再呈现插件、MCP 导入和排障。分区使用轻分隔与紧凑布局，空插件目录保留刷新入口；真实错误、清理未确认及有待审查的能力保持可见。

## 审批与确认

当前 pending 审批继续显示完整冻结正文、主要风险、固定决定按钮和单一纵向滚动区。缺失精确文件/命令预览仍不可批准，既有 request/hash/source 与决定 guard 不变。

服务端接受决定只是一条紧凑确认，不表示工具执行成功；投递不确定仍采用明确的问题与下一步提示。canonical 的 typed approved 事实使用中性、默认折叠的审计行，详情可展开；拒绝、过期、取消和失败保留准确状态。不得按提示词、工具名称、相邻位置或翻译文案删除记录。

菜单 trigger 的 `aria-controls` 指向真正的 `role=menu`，菜单具有对应的可访问名称；方向键、字首导航与焦点返回仍复用现有 primitive。DOM 交互测试不替代 WebKit/VoiceOver 或原生 AX 验收。

## Markdown 编辑与实时预览

用户选择 Markdown 原文编辑与实时排版预览。本轮工作台批次不宣称已实现编辑器；后续独立批次必须共同交付格式辅助、长文展开、实时安全预览、可靠撤销/重做与组合输入保护。

Markdown string 是唯一发送与草稿正文；预览为派生展示，不能把 HTML/DOM/编辑器 JSON 变成第二份 durable truth。既有安全 HTTPS、HTML sanitize、图片附件、数学和 Mermaid 边界继续使用。预览、主题和展开不能重挂同会话编辑实例或丢失焦点、选区、IME、草稿和图片 scope。

格式操作不能假定 `setRangeText` 或 React 状态替换自然进入 WebKit 原生撤销栈。组合输入 guard 必须先于建议选择、快捷键和发送逻辑；异步发送仍按提交 revision 清理，迟到 durable draft identity 不得造成丢字或重现已发送草稿。

设计依据：[Apple Layout](https://developer.apple.com/design/human-interface-guidelines/layout)、[Typography](https://developer.apple.com/design/human-interface-guidelines/typography)、[Feedback](https://developer.apple.com/design/human-interface-guidelines/feedback)、[W3C Menu Button](https://www.w3.org/WAI/ARIA/apg/patterns/menu-button/) 与 [VS Code Markdown](https://code.visualstudio.com/docs/languages/markdown)，访问日期 2026-10-21。完整原生基线和研究证据保留在本次 repo-local 调研目录，不能代替最终源的真实体验验收。
