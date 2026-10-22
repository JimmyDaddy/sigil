# TUI 提问表单的输入与反馈

2026-10-23。范围为内部 `sigil-tui-host` 的普通 agent/MCP 提问表单，不改变共享问题契约、答案身份、审批权限或持久化流程。

打开的表单独占其粘贴输入：文本题把中文、emoji、空格和换行追加到当前答案；单选题把粘贴内容写入并选中 `Other`，多选题保留已选选项并追加 `Other`。普通键入的其他答案也显式选中 `Other`，避免仍提交之前的建议选项。动作栏没有文本目标，粘贴时显示返回答案的提示；已接受答案的恢复表单只显示不可编辑提示。两者都不会把文字写入背后的 composer。

`Esc` 仅隐藏表单；隐藏后粘贴仍交给 composer，`Shift-Tab` 重开保留表单答案。打开表单期间原 composer 正文、光标和粘贴折叠状态保持独立。

普通文本题的 `Enter` 转下一题，最后一题转到动作栏，不插入换行。`Tab` 前进、字段中的 `Shift-Tab` 返回前一题；动作栏以左右键或 Tab 切换动作，`↑` 返回此前的问题，`Enter` 才执行选定动作。提示随实际焦点与选择状态变化。单选确认后上下键在问题间移动，空格切换建议选项；多选上下键移动选项，空格切换勾选。选项题可直接键入或粘贴其他答案。

提交缺少必填答案或类型不符时，表单在固定动作区显示短错误，焦点返回首个需要修正的问题，并使用 renderer 测得的真实折行位置滚入视野。对应答案修正后清除该错误；提交仍绑定原始 request ID、generation、hash 和 question ID，不自动补造答案。

正文以题号和完整问题为主，内部 question ID 不进入主标题。键位只在表单底部呈现，按终端列宽折行并预留高度，避免与主界面 footer 重复，也保证窄窗中的返回、确认与关闭路径可见。正文继续独立滚动。

PlanRevision 保持专用编辑器：`Enter` 提交，`Shift-Enter` / `Ctrl-J` 换行，保留光标、多行草稿、提交中的禁用和失败行内反馈。

验证应覆盖普通文本、单选/多选 Other、动作/恢复焦点、中文与组合 emoji、多行粘贴、隐藏/重开、校验定位，以及 80×24 与窄短窗口。隔离状态测试和 TestBackend 只证明本地逻辑与布局；真实 Terminal/cua-driver 的候选复验另行记录。

设计参考 [W3C WAI 表单反馈](https://www.w3.org/WAI/tutorials/forms/notifications/) 的简短可执行错误提示原则；输入路由遵循 [Crossterm Event::Paste](https://docs.rs/crossterm/latest/crossterm/event/enum.Event.html#variant.Paste) 独立事件语义。这里借鉴可用性原则，不据此声明终端的读屏或 WCAG 符合。
