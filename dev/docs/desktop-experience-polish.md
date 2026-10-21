# Desktop 信息层级与 Markdown 写作体验

本文记录桌面端的呈现约束与本轮改造。Desktop 与 TUI 继续共用执行、权限、审批与恢复语义；此处的界面变化不改变 HTTP、IPC、durable session 或工具行为。

## 工作台信息层级

正文、真实结果和当前决定优先。系统字体、语义字号与留白区分正文、控件及辅助信息，背景采用平面层级；按钮、菜单与字段继续复用内部 `ui` API 和既有 CSS layers/tokens。

会话侧栏默认宽 280px，已保存的用户宽度继续使用。小于 900px 时复用导航抽屉，输入区的模型/权限/推理选项允许换行，不靠把文字压到难以阅读解决空间不足。原生窗口最小尺寸由 native 壳另行约束，本轮目标为 840×560；在 native 候选实测前不视为已经资格化，组件响应式检查不能证明原生窗口已经支持对应尺寸。

上下文容量未知或尚在加载时，不显示“—”占位；已知的使用率保留简短入口，缓存与 token 诊断在同一上下文详情中查看。这些辅助观察不成为发送门禁。

设置先呈现连接、默认模型、外观及日常偏好，再呈现插件、MCP 导入和排障。分区使用轻分隔与紧凑布局，空插件目录保留刷新入口；真实错误、清理未确认及有待审查的能力保持可见。

## 审批与确认

当前 pending 审批继续显示完整冻结正文、主要风险、固定决定按钮和单一纵向滚动区。缺失精确文件/命令预览仍不可批准，既有 request/hash/source 与决定 guard 不变。

服务端接受决定只是一条紧凑确认，不表示工具执行成功；投递不确定仍采用明确的问题与下一步提示。canonical 的 typed approved 事实使用中性、默认折叠的审计行，详情可展开；拒绝、过期、取消和失败保留准确状态。不得按提示词、工具名称、相邻位置或翻译文案删除记录。

菜单 trigger 的 `aria-controls` 指向真正的 `role=menu`，菜单具有对应的可访问名称；方向键、字首导航与焦点返回仍复用现有 primitive。DOM 交互测试不替代 WebKit/VoiceOver 或原生 AX 验收。

## Markdown 编辑与实时预览

用户选择 Markdown 原文编辑与实时排版预览。输入区使用 CodeMirror 6 的单一原文 editor；不提供 WYSIWYG，也没有隐藏 textarea 或 HTML 正文。`格式`菜单提供粗体、斜体、行内代码、代码块、列表和链接，修改原始字符串并使用同一 transaction history。Cmd/Ctrl-B、I、K 分别插入粗体、斜体和链接；Cmd/Ctrl-Z 撤销，macOS Shift-Cmd-Z、其他平台 Ctrl-Y 重做。真实 WebView 键盘与 IME 仍以最终候选实机为准。

`预览`是输入区可见的开关，默认收起以保留正文空间；选择格式会打开它。打开后随编辑实时更新，复用 SafeMarkdown 的 CommonMark/GFM、安全 HTTPS、sanitize、数学与 Mermaid 边界，不创建远程图片元素或新增网络/IPC能力。预览可单独滚动；主题、预览和展开不重挂同一 editor，不清选区、撤销历史、附件或草稿 scope。展开用于较长内容，空间受视口限制；当前审批出现时进一步限制 source/preview 高度并保留审批的精确正文和固定决定区。

Enter 始终发送（运行中入队），Shift+Enter 换行，代码块和展开态遵循相同规则，输入区显示该提示。中文组合输入的 guard 先于建议选择、格式快捷键和发送；不会阻止浏览器提交组合文字。slash、skill、agent 继续使用原有显式语法，普通 Markdown 连同前后空白、CRLF 和代码 fence 直接发送，不 trim 或把预览 HTML 转回正文。

Markdown string 是唯一发送与草稿正文。异步发送只清理原提交 revision；用户后续编辑、开始新的 composition、增删附件都会使该 revision 失效。迟到 durable draft identity 仅迁移存储 key，不重建 editor；新 key 保存失败时保留旧 key。256 KiB 草稿预算或存储失败只提示当前文字尚未保存，保留之前已保存文本，不静默删掉原草稿，不阻止用户编辑或发送。附件继续通过既有受限 native ingress 与 scope 释放机制，不把路径混入正文。

CodeMirror 自带的运行时样式不在应用 CSS layer 中。`ui/editor` 仅使用其官方 scoped theme adapter 覆盖编辑器默认的光标、焦点、字体和选区，值均来自应用语义 token；工作台与编辑器容器布局仍在原有 layers 中。不全局覆盖第三方样式，也不增加 `!important`。

依赖与审计边界见[依赖供应链台账](../governance/dependency-supply-chain.md#markdown-原文编辑desktop-polish-b32026-10-21)。组件/引擎测试覆盖格式与 undo/redo、preview toggle 不重挂、真实危险 Markdown 不产生 active HTML/remote image、IME 建议、原文发送、晚 identity/异步发送/附件和存储失败。jsdom 不具备实际排版及原生输入法，840×560、WebKit Cmd-Z / Shift-Cmd-Z 与真实中文输入法仍由后续 native 验收记录。

设计依据：[Apple Layout](https://developer.apple.com/design/human-interface-guidelines/layout)、[Typography](https://developer.apple.com/design/human-interface-guidelines/typography)、[Feedback](https://developer.apple.com/design/human-interface-guidelines/feedback)、[W3C Menu Button](https://www.w3.org/WAI/ARIA/apg/patterns/menu-button/) 与 [VS Code Markdown](https://code.visualstudio.com/docs/languages/markdown)，访问日期 2026-10-21。完整原生基线和研究证据保留在本次 repo-local 调研目录，不能代替最终源的真实体验验收。
