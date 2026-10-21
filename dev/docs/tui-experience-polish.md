# TUI 信息层级与审阅体验

2026-10-21。本批仅调整 `sigil-tui-host` 的展示与同一帧的交互几何，不改变 kernel/runtime 的审批、权限、取消或持久化语义。

## 默认主界面

- 对话正文优先。总宽不足 124 列时自动收起信息栏；足够宽时信息栏固定 32 列，避免宽屏不断扩大辅助信息。F2 显隐、Shift-F2 精简/详情继续有效；字体功能键如何到达应用取决于终端和系统设置。
- composer 默认显示当前模式、写权限与模型，普通 provider、推理强度和 `agent: main` 不常驻。非默认 agent 仍明确显示；完整 provider、模型、推理强度在信息栏详情中保留。
- 底部状态行优先显示当前可用操作，默认最多三个完整操作片段分配宽度，完整键位在 F1 帮助。空闲为发送、可用的换行键和 F1 帮助；忙碌时先显示 Ctrl-C 停止，再是 Enter 添加跟进。git 与上下文仅使用剩余空间，不能挤掉主要动作。
- 普通栏目使用中性标题；颜色用于焦点、真实风险和错误。用户正文正常字重，思考内容继续默认折叠，Ctrl-T 可逆展开。主题配置与用户颜色覆盖保持兼容。
- 首次设置的复核区只重复确认 provider、模型和实际认证来源；自定义 provider 额外保留协议。内部编排策略、推荐清单来源及相同路由的重复说明不进入默认复核。真正的配置错误、凭证缺失和信任文件夹动作继续显示。

## 工具与审批

- 普通成功 read/search/list 调用默认显示动作、对象和有界摘要；完整正文由现有 Ctrl-O 展开。每个调用保留独立 identity，不合并跨 turn 的同名调用。命令、写操作、错误、截断保存和不可用 artifact 的必要事实继续显示。
- 审批保留一层主框，内部使用轻分隔；决定区域固定在底部。单文件使用整宽 diff；多文件在可用宽度足够时分栏，窄区使用现有 `,` / `.` 文件切换。多 hunk 才显示 hunk 导航。
- 允许一次、会话允许、命令族允许和拒绝维持原范围与默认选择。当前选择有明确结构标记；减少未选按钮的彩色实心背景不会改变批准能力。可信正文缺失仍不能批准。
- 恢复投影只在同一 call 的 durable `ToolExecutionStatus::Cancelled` 证据存在时优先显示中性 `CANCELLED`。模型 error envelope 的 Internal 或含“取消”字样的文本不能代替证据；V3 facts 的 error kind 在没有 ToolExecution 记录的审批中断路径也继续传到展示，避免把确证 Interrupted 压成泛化 ERROR。Interrupted、执行失败和未知结果保留警示及原始诊断。

## 键盘与布局约束

运行中在 composer 按 Enter 添加跟进；已有队列时 Tab 聚焦队列。队列内上下选择项目、左右选择操作、Enter 执行、Backspace/Delete 删除、Alt-上下排序，Tab/Shift-Tab/Esc 返回输入。Shift-Enter 仅在键盘增强实际启用时提示，否则使用 Ctrl-J。系统粘贴仍按 bracketed paste 插入而不发送。

布局、鼠标 hit area、输入列宽和历史重排使用相同辅助栏宽度规则；移除装饰不允许点击区域与实际按钮偏离。所有文本仍走现有终端转义清理、Unicode 列宽与 Markdown 安全展示路径。

## 验证与资料

本批需验证 80×24、100×30、124×32 和 140×44 下的空闲、运行、长草稿、单/多文件审批及恢复；重排后历史锚点、光标、工具展开、信任继续/退出与取消后新请求都应实际走查。TestBackend 和隔离状态测试证明局部逻辑，不能代替真实 Terminal/cua-driver，也不能据此声明跨终端、读屏或 WCAG 符合。

设计依据为 [Ratatui 布局](https://ratatui.rs/concepts/layout/)、[Helix 状态栏](https://docs.helix-editor.com/editor.html#editorstatusline-section)、[Lazygit 官方配置](https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md)、[Crossterm 粘贴](https://docs.rs/crossterm/latest/crossterm/event/struct.EnableBracketedPaste.html)，以及 W3C 的[非颜色唯一提示](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html)与[焦点可见](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html)原则。124/32 列是本产品的空间预算，应以真实窗口验证为准。


## 旧共享历史导航

新工作区的会话选择器只列出当前工作区的有效会话。如果 authority 明确记录旧共享历史的访问位置，`/resume` 会显示可查看这些会话的工作区名称与打开提示；观察状态为 `Unverified` 时明确显示“待核实”，不声明会话已丢失。提示不携带内部路径，不阻止新建或恢复当前工作区会话，紧凑终端优先保留可选择的一行。


工具卡之间的空白只用于分隔，不属于工具正文的鼠标区域。折叠成功读取仍可点标题展开；有可见正文时，按下选择、释放切换与拖动选字分别保持原交互。
