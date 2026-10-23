# Desktop 信息层级与 Markdown 写作体验

本文记录桌面端的呈现约束与本轮改造。Desktop 与 TUI 继续共用执行、权限、审批与恢复语义；此处的界面变化不改变 HTTP、IPC、durable session 或工具行为。

## 工作台信息层级

正文、真实结果和当前决定优先。系统字体、语义字号与留白区分正文、控件及辅助信息，背景采用平面层级；按钮、菜单与字段继续复用内部 `ui` API 和既有 CSS layers/tokens。

会话侧栏默认宽 280px，已保存的用户宽度继续使用。小于 900px 时复用导航抽屉，输入区的模型/权限/推理选项允许换行，不靠把文字压到难以阅读解决空间不足。原生窗口最小尺寸由 native 壳另行约束，本轮目标为 840×560；在 native 候选实测前不视为已经资格化，组件响应式检查不能证明原生窗口已经支持对应尺寸。

上下文容量未知或尚在加载时，不显示“—”占位；已知的使用率保留简短入口，缓存与 token 诊断在同一上下文详情中查看。provider 缓存未命中只使用中性呈现，原始 token 与诊断保留在详情提示中，不添加无可执行动作的警告颜色。这些辅助观察不成为发送门禁。

默认输入区为正文与底部操作两层。模型和权限保持可见；图片入口及“编辑”位于同一底栏，格式、原文开关、展开与发送键提示按需在“编辑选项”中查看。空队列且未暂停时不显示入口；有待发送内容时以“待发送 N”打开列表，运行中的发送按钮继续提交为后续消息，停止按钮独立保留。附件、已选扩展和真实草稿存储错误按实际状态呈现。

低频的扩展、恢复、意图与已完成的代理/验证入口集中在带文字的“会话工具”菜单，活动代理及未通过的验证继续显示上下文入口；所有菜单支持键盘操作，关闭面板后回到实际入口。每条消息的分支入口位于“消息操作”更多菜单，默认不重复显示整句按钮；鼠标悬停、键盘聚焦与触摸均可访问。

向上阅读后保持当前锚点，显示“回到最新”；新正文或状态到达时改为“有新内容”，不计算流式 chunk 为消息数，也不自动抢走阅读位置。点击后跳到最新并恢复跟随；该提示以 workspace/session 为 scope 清理，不接收前一会话的迟到内容。

设置先呈现连接、默认模型、外观及日常偏好，再呈现插件、MCP 导入和排障。分区使用轻分隔与紧凑布局，空插件目录保留刷新入口；真实错误、清理未确认及有待审查的能力保持可见。

## 审批与确认

当前 pending 审批继续显示完整冻结正文、主要风险、固定决定按钮和单一纵向滚动区。缺失精确文件/命令预览仍不可批准，既有 request/hash/source 与决定 guard 不变。

服务端接受决定只是一条紧凑确认，不表示工具执行成功；投递不确定仍采用明确的问题与下一步提示。canonical 的 typed approved 事实使用中性、默认折叠的审计行，详情可展开；拒绝、过期、取消和失败保留准确状态。不得按提示词、工具名称、相邻位置或翻译文案删除记录。

菜单 trigger 的 `aria-controls` 指向真正的 `role=menu`，菜单具有对应的可访问名称；方向键、字首导航与焦点返回仍复用现有 primitive。DOM 交互测试不替代 WebKit/VoiceOver 或原生 AX 验收。

## 工作区定位与旧共享会话

同名工作区只在冲名时增加 native 提供的短父级说明，菜单当前项、打开项、最近项及关闭确认使用同一可访问名称。说明最多包含父/祖父两个 basename（以 ` · ` 分隔），不返回完整路径、可执行路径、内部哈希或新 filesystem capability；根目录、控制符、非 Unicode 或超长组件不伪造定位，旧 DTO 缺字段仍显示原名称。真正相同 canonical root 仍复用原 workspace identity，标签不参与授权或 session 路由。

新 realm 的目录若报告共享历史访问入口，侧栏只显示一条中性定位提示：已打开的访问工作区按 exact workspace ID 切回，已在最近列表中的访问工作区复用现有 native reopen；未列出时打开现有工作区菜单。入口只表示可查看共享历史的位置，不声明旧会话或资源属于该工作区。`unverified` 只如实说明尚未核实，不把空列表称为删除、不推断迁移，也不把观察状态变成新会话权限门禁。不同 workspace 或旧 catalog 的提示不会借用于当前导航。

确认为无会话、无筛选且没有选中会话时，主区提供“开始新会话”，复用现有 create 与 busy 处理；有真实 route recovery 时，新建入口只在其 typed actions 允许 `start_new_session` 时显示，其余仅呈现已有允许的恢复操作。有历史时保留会话导航；空搜索结果提供显式“清除搜索和筛选”，不静默改变跨工作区的搜索 scope。上述源变化由组件与 native DTO/真实 recent store 回归覆盖，原生菜单视觉和 AX 仍以最终候选实测为准。

## 工具输出阅读

长工具输出读取全文后，原预览由单一阅读区替换。正文显示读取范围，搜索显示已提交的查询与本页命中数；零命中、空输出和末尾分别提示。上一页复用已读内容，搜索后可返回原正文页及滚动位置，不重复请求已缓存页面，也不扩大后端读取权限与预算。

搜索框默认收起，点击搜索入口才展开；Escape 收起输入并返回触发按钮，保留查询。结果标题持续显示正在阅读的内容类型，避免收起输入后失去搜索上下文。切换工具或 artifact identity 会清理对应阅读状态。

## D09：字号、留白与分隔层级

辅助文字统一使用 11px caption token，控件标签至少 12px，紧凑正文采用 13px detail token，主要会话正文仍为 15px。来源状态、时间、工具读取范围与任务元信息不再使用 7–10px 小字。计划步骤、风险内容、工具正文与错误说明使用更清楚的正文或标签层级；caption 与 detail 分别提供统一行高。

会话、工具输出与设置沿用现有间距 token。工具卡与计划卡使用较轻的中性外框，代码输入预览、计划步骤、嵌套思考内容和审查事实减少重复描边；报告回执改为轻分隔，设置内的新增连接表单不再叠加完整卡片。输入区域、选中项、真实审批、失败及风险继续保留各自的状态强调和焦点标记。

操作文字增大时保留既有点击区域，页头返回按钮与正文使用相同水平留白。会话库紧凑列在窗口宽度不大于 960px 时启用，覆盖 840px 原生最小宽度，避免只在不可达的更窄尺寸生效。设计参考 [W3C 目标尺寸说明](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html) 关于目标大小及相邻间距的要求；11px/12px 是本产品的呈现选择，并非 WCAG 字号标准。静态 token/主题检查不构成完整无障碍合规声明。

会话库紧凑列中的来源、活动和时间横向排列，并在空间不足时换行，避免每项辅助信息独占一行，把会话名称挤在五行元信息中。

最终原生候选仍需检查侧栏长标题与来源标签、窄窗设置和会话库、工具全文与搜索、计划及审批长正文的换行、滚动与固定操作区。此节记录实现约束，不替代这些实机证据。

## 单一区域 Markdown 即时编辑

默认在唯一 CodeMirror 编辑区直接排版 Markdown，输入完成的粗体、斜体、标题、行内代码、列表、引用与链接在原位呈现，不再并排显示原文和预览。原始 Markdown string 始终是唯一正文；HTML、图片和代码 fence 保留惰性 source，不生成远程图片或可导航链接，也不把 DOM/HTML 转回文本。

排版消费官方 CommonMark/GFM 语法树，通过 mark/line/replace decorations 改变展示，不使用自写正则 parser。光标进入局部节点或选择对应内容时揭示该处标记，闭合标记之后继续输入立即排版；工具菜单聚焦期间保留所选语法，避免浏览器选区塌缩。“编辑选项”的“显示 Markdown 原文”只切换同一 editor 的呈现，不增加第二 pane，不重置 selection、滚动、附件、草稿或撤销历史。

格式操作仍提供粗体、斜体、行内代码、代码块、列表和链接，修改同一原始字符串。Cmd/Ctrl-B、I、K 保留；基础 Cmd/Ctrl-Z 与重做只负责误删或格式修改撤回，不增加记录列表或历史按钮。主题、原文模式和展开不重挂 editor。展开只提高长文上限，短文按实际内容高度显示：普通上限 min(176px,24dvh)，展开 min(280px,38dvh)，当前审批时 88px，保留会话正文空间；实际窄窗几何与输入法另行原生确认。

Enter 发送（运行中入队），Shift+Enter 换行；代码 fence、原文模式和展开态规则一致。组合输入先于建议、格式与发送，期间冻结装饰并映射原范围，浏览器提交后再排版。编辑弹层打开时，Escape 优先取消输入法候选，不关闭弹层或移动编辑焦点；随后普通 Escape 才关闭弹层。普通 Markdown 连同空白、CRLF、中文和代码 fence 直接发送，不 trim。异步发送只清理原 revision；后续编辑、composition、附件与迟到 durable draft identity 均保留既有范围。256 KiB 草稿预算或存储失败保留文字和之前已保存值，不阻止编辑或发送。

样式由官方 scoped theme adapter 消费应用语义 token，不手工改 contentDOM，不添加 `!important`。依赖边界见[供应链台账](../governance/dependency-supply-chain.md#markdown-原位编辑desktop-polish-b3-d012026-10-23-更新)。组件与引擎测试覆盖完成语法即时排版、原文 toggle 不重挂、selection/scroll/history、合成 composition 事件、精确提交、late draft identity、附件和危险内容惰性；jsdom 不代表真实输入法或 native 布局资格化。

官方依据：[Decorations](https://codemirror.net/examples/decoration/)、[StateField / direct decorations](https://codemirror.net/docs/ref/#view.EditorView.decorations)、[Markdown 配置](https://codemirror.net/docs/ref/#lang-markdown.markdown)、[Lezer Markdown](https://code.haverbeke.berlin/lezer/markdown)，访问日期 2026-10-23。

设计依据：[Apple Layout](https://developer.apple.com/design/human-interface-guidelines/layout)、[Typography](https://developer.apple.com/design/human-interface-guidelines/typography)、[Feedback](https://developer.apple.com/design/human-interface-guidelines/feedback)、[W3C Menu Button](https://www.w3.org/WAI/ARIA/apg/patterns/menu-button/) 与 [VS Code Markdown](https://code.visualstudio.com/docs/languages/markdown)，访问日期 2026-10-21。完整原生基线和研究证据保留在本次 repo-local 调研目录，不能代替最终源的真实体验验收。
