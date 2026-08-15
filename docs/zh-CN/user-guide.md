<!-- public-doc-role: user-guide; authority: tui-daily-use-authority; sections: start,headless-and-local-api-workflows,main-screen,common-controls,rich-conversation-content,image-attachments,slash-commands,config-panel,web-search-and-fetch,planned-tasks,approvals-and-file-changes,sessions-and-recovery,long-context-and-compaction,code-intelligence; cta: open-reference -->

# Sigil TUI 用户指南

[文档首页](README.md) · [快速开始](quickstart.md) · [常见工作流](workflows.md) · [参考](reference.md) · [English](../en/user-guide.md)

本指南介绍每天使用的 TUI。完整命令和键位表只在[参考](reference.md)维护。

## 启动

在要处理的仓库中运行 `sigil`。缺少配置时，快速设置会引导你确认工作区、选择模型服务和具体模型，并完成认证。找不到命令时请看[安装](installation.md)；需要可重复使用的配置时请看[配置](configuration.md)。

## 非交互与本地 API 工作流

TUI 是日常使用的主界面。`sigil run` 为脚本提供文本、JSON 或 JSONL 输出；如果某项操作仍需人工审批，命令会直接失败，不会尝试打开弹窗。`sigil serve` 是供受信任本机客户端使用的高级接口，只监听回环地址并要求认证。相关命令、认证方式、输出格式和退出状态见[脚本输出与本地服务](reference.md#脚本输出与本地服务)。

## 主界面

- **会话记录：** 用户消息、助手回复和工具活动。
- **输入框：** 底部任务输入区域。
- **信息栏：** 宽度允许时显示会话、权限、模型、用量、代码智能与当前操作状态。
- **活动：** 文件读取、搜索、命令、编辑、诊断和结果。
- **审批浮层：** 高风险工具调用的操作、受影响文件、预览与决策。

普通任务直接在输入框中提出；斜杠命令只负责少量控制操作。

## 高频操作

按 `F1` 或 `/` 打开帮助与命令；`F2` 显示或隐藏信息栏，`Shift-F2` 切换详细程度。`Ctrl-G` 聚焦活动，`Alt-V` 聚焦任务验证，`Ctrl-R` 打开最近一次受控恢复，`Alt-S` 打开当前 Intent Stack，`Ctrl-T` 展开或折叠推理过程与活动。未选中文本时，`Ctrl-C` 取消运行；`Esc` 关闭当前浮层。完整键位表见[参考](reference.md#tui-键位)。

终端宽度足够时，信息栏默认显示。Git 摘要的第一行只保留分支与总变更数，第二行紧凑展示 staged、modified、untracked、conflict 和 ahead/behind 计数，不再把所有内容塞进一条长句后截断。`F2` 只影响本次运行。要修改启动时的默认状态，请打开 `/config`，选择 **Appearance**，切换 **Info rail**，再用 `Ctrl-S` 保存；终端较窄时，信息栏仍会自动收起。

拖选会话文本并松开鼠标后会立即复制。Sigil 会同时尝试系统剪贴板，以及启用时的 OSC52 终端桥接；任一路径成功即可。复制成功后高亮会清除，失败时选区会保留，可按 `Ctrl-C` 重试。`Ctrl-L` 会优先复制现有选区；没有选区时才复制最新的助手回复。所有复制路径都只读取会话内容，不会把信息栏一起复制。没有选区时，`Ctrl-C` 仍用于取消运行或退出。

鼠标还支持滚动、输入框定位、审批控件、菜单、会话列表、活动和工具卡片展开。终端复制、键盘、鼠标、tmux 与 SSH 检查见[终端兼容性](terminal-compatibility.md)。

## 富对话内容

Sigil 始终把原始 Markdown 作为持久化消息；标题、列表、表格、任务列表、链接、代码、行内公式、块公式和已经闭合的 Mermaid fence 都只是展示投影。异常或仍在流式生成的 fence 只影响实时尾部，不会改写已保存的回复，也不会吞掉后续会话。

桌面端使用 KaTeX 在本机渲染公式，并在有大小限制的本地图表查看器中显示已闭合的 Mermaid。它不会加载远程图表资源，也不会开启 Mermaid 链接、回调、raw HTML 或脚本。公式或图表失败时只对当前块降级，并始终保留可复制源码。

TUI 保持相同的内容顺序，但不会伪装成浏览器排版：公式显示为带标签的 LaTeX 源码，Mermaid 显示为包含类型、状态、摘要和可选源码的图表区块。没有更高优先级的 `Ctrl-O` 操作时，按 `Ctrl-O` 可切换最近一张图表的源码；`Ctrl-L` 仍复制最新助手回复的原始内容。宽表格和代码只在局部内容区域处理，不会撑宽整个会话记录。

## 图片附件

在空闲输入框中粘贴本机 PNG、JPEG 或 WebP 路径，或在剪贴板中有图片时按 `Ctrl-V`。发送前请检查图片信息标签；用 `Up` 选中标签，`Left/Right` 切换，`Backspace` 或 `Delete` 删除。

每轮最多 4 张图片，每张不超过 8 MiB，总计不超过 24 MiB，同时还有尺寸限制。图片不能进入后续输入队列，也不能附加到计划、命令、技能、任务或子智能体输入。只有明确支持图片的 OpenAI Responses、Anthropic 和 Gemini 模型可以接收。恢复会话后，如果原来的本机图片已经找不到，请重新粘贴原图，或从不依赖该图片的位置继续对话。

## 斜杠命令

常用控制命令：

- `/config` — 修改常用设置。
- `/doctor` — 诊断设置、认证、集成和终端支持。
- `/resume` — 选择已保存会话。
- `/plan <prompt>` — 执行前请求只读计划。
- `/task <任务>` 与 `/task continue` — 启动或继续多步骤工作。
- `/compact` — 生成、校验并激活一个可恢复的上下文 checkpoint。
- `/update [check|refresh|apply]` — 检查更新，或明确安装已准入的更新。
- `/feedback` — 预览并保存本机支持报告。
- `/quit` — 关闭 TUI。

模型、子智能体、后续输入和其他命令形式见[参考](reference.md#斜杠命令)。

任务运行时，新输入会显示在“后续输入”区域；排在首位的输入默认已经是“下一条执行”，会在当前回合结束后运行。可以按 `Tab` 聚焦，也可以直接点击某条输入和它的“下一条执行 / 中断 / 编辑 / 删除”操作。新输入仍在保存时按“下一条执行”会立即得到确认；如果选择的是后续项或暂停项，durable queue id 返回后会自动补发重排动作。“下一条执行”也会恢复已暂停的队列；只有确实需要停止当前回合时才使用中断。如果无法确认上一条输入是否已经送达，Sigil 不会自动重发。终端高度较小时，composer 会收缩为三行；后续输入区域消失后，空间会立即还给会话内容，不会留下异常放大的输入框。

## 配置面板

`/config` 汇总常用的模型服务、权限、Web、记忆、上下文、代码智能、终端、外观、子智能体、技能、插件和 MCP 设置。单模型上下文窗口不再要求手填数字，而是在“自动、64K、128K、256K、1M”之间循环；已有的自定义数值会保留到用户主动切换。主题修改会立即预览；按 `Ctrl-S` 保存。精确字段和默认值统一在[配置字段参考](configuration-reference.md)维护。

为 Streamable HTTP MCP 服务配置 OAuth 后，打开详情并选择 **Authentication**。你可以在弹窗中查看状态、开始登录、打开或复制授权 URL、接收临时回调 URL、刷新凭据、退出登录，或清除本机保存的凭据。连接服务前请阅读 [MCP 指南](mcp.md)。

## Web 搜索与页面抓取

启用后，搜索与抓取活动会显示数据将发送到哪里。搜索结果属于外部不可信内容。抓取工具只能打开当前会话已经观察到的 URL，并会重新应用网络限制。可用路径、关闭方式和目标规则见[权限与沙箱](permissions-and-sandbox.md#网络与-web-工具)。

## 计划任务

使用 `/plan` 获取只读计划；确认需要执行且计划内容合适时，再接受“计划就绪”卡片。已经确定需要多步骤执行时使用 `/task`。普通对话不会擅自继续尚未完成的任务。

打开“计划就绪”后，应先在工作台审阅完整、不可变的计划，再选择动作。即使终端很窄或很矮，所有
步骤、依赖、路径、检查项、风险和备注也都可以访问。使用方向键、`PageUp`/`PageDown`、
`Home`/`End` 与 `Tab`/`Shift-Tab` 导航。`Esc` 只关闭工作台，绝不等于拒绝计划；输入可打印字符会
返回 composer 并保留该字符，`Shift-Tab` 可以重新打开待审计划。Run、Save、Revise、Reject 都是绑定
精确 plan id 与 hash 的显式动作。

选择 Revise 后，Sigil 会先询问“希望修改什么”。修改研究期间原计划始终可以审阅；修改失败或取消后，
原计划动作会恢复，不会被空的失败草稿替代。智能体提出的问题也进入同一个 durable attention 区：
Submit、Decline、Cancel run 是三个不同动作，`Esc` 只关闭表单，不会作答。待回答问题没有 wall-clock
超时，`sigil resume` 会恢复它，composer 中按 `Shift-Tab` 可以重新打开。回答被接受后，Sigil 继续精确
的 suspended continuation，不会重放提出问题的 provider turn。

任务界面会显示步骤、当前状态和子智能体的工作；需要你检查时，还会显示验证卡片。按 `Alt-V` 可以直接聚焦。恢复会话只会还原已保存的任务状态，不会自动继续执行。

release 默认值是 `auto / explicit_request_only`：普通输入在 review-first 基线上自动路由
（先自动计划审查，再进入 durable Task）。新安装的 qualified release 还可能在 Quick Setup
与 `sigil doctor` 中显示 direct task execution；该 qualification 与 release 携带的精确
provider route 和 binary build 绑定；已有配置不会改变。要关闭自动路由和 proactive spawn，
同时保留已有 Task history，请设置 `routing_policy = "manual"` 和
`multi_agent_mode = "explicit_request_only"`。

## 审批和文件变更

只读文件与搜索工具通常直接运行。写入、删除、命令、网络和外部工具遵守配置的权限策略。

审批浮层把真正要审核的内容放在中央：命令和工具请求使用独立高对比区域，文件写入使用具体差异。工具类型与风险留在紧凑顶部，策略、影响和隔离信息按 `M` 展开；没有文件差异时不会显示空面板。

允许高风险动作前，检查：

- 将要执行什么；
- 涉及哪些文件或目标；
- 可见的文件差异或请求预览；
- **allow**、**allow for this session** 或 **deny** 是否符合意图。

活动视图可能折叠较长的文件差异；提交前仍要检查仓库中的最终改动。

## 会话与恢复

会话日志保存在 Sigil 的用户状态目录中。直接运行 `sigil` 始终创建 fresh session，即使同一工作区已经打开另一个 Sigil 窗口也不会自动复用最近会话。恢复必须显式进行：`sigil resume` 恢复最近的受支持会话，`sigil resume <session-id>` 恢复精确会话，或用 `/resume` 选择。恢复会带回可见消息、任务状态、已完成活动摘要和中断工具结果，但不会静默重跑中断工具。退出时会显示会话 ID 和精确恢复命令。

同一 session 同时只允许一个可写交互表面 attach。目标 session 已在另一个 TUI 或 Desktop run 中活动时，Sigil 会保留当前 shell，并提供重试、新建会话或返回会话库；退出原 owner 后再重试即可，不要删除 attachment sidecar 或强制接管。

同一可信 origin 内的 Provider endpoint 路径修正会在恢复时自动 rebind。origin、账户/tenant 边界变化、connection 缺失，或旧 session 无法证明 trust binding 时，需要显式确认当前 route 或选择 replacement。请在 `/config`（或 Desktop 设置）检查并保存目标 connection，或选择替代 route；session ID 与可移植对话记录保持不变，旧的 provider 私有 continuation 会被丢弃。

取消操作会停止接收新工作，并短暂等待活动工作结束。**Cancelled** 表示清理完成；**Interrupted** 表示在限制时间内无法确认。已经保存的消息和结果仍会保留。

### 管理已保存的会话

打开 `/resume` 并选择一行。按 `Enter` 恢复；按 `Ctrl-O` 或右键打开操作菜单，可以从当前会话分叉、导出经过处理的会话记录、固定会话，或检查并删除会话。删除需要二次确认，而且只作用于刚刚检查过的非活动会话文件。保留期限清理是 `/config` → **Storage** 下的显式操作；普通启动不会自动删除会话。

### 受控检查点与会话分叉

最近完成的回合包含受支持的文件编辑时，按 `Ctrl-R` 检查反向差异。按 `Enter` 恢复刚刚检查过的文件；按 `F` 只分叉对话，不修改共享文件。文件已经变化或预览过期时，恢复会被阻止。Shell 命令、远端服务、目录、重命名、符号链接和其他外部影响不会被撤销。成功恢复后，请重新运行验证。

### Intent Stack 检查

当前会话具有已接受的 Intent Stack 历史时，按 `Alt-S` 或运行 `/intents`，可以检查每个 intent 的依赖、验证状态、保留 artifact 与冲突。用 `Up/Down` 选择 intent，按 `D` 生成精确 Drop 预览，只有再次按 `Enter` 才会执行刚刚检查过的预览。shared、drifted、unavailable、read-only 或 out-of-scope 的贡献仍可查看，但不能 Drop。Shell、网络、远端服务及其他不支持的副作用永远不会被撤销。当前 session 若没有已接受的 durable intent 历史，会明确显示不可用，不会根据当前文件猜测一个 stack。

## 长上下文和压缩

信息栏会显示上下文用量，并在模型上下文窗口接近上限时提醒。`/compact` 会直接生成、校验并原子激活一个可恢复语义 checkpoint，不再打开确认弹窗；route 准入后会执行一次计费语义摘要请求。Sigil 先展示进度，随后给出已激活回执或精确拒绝原因。摘要、token proof、经济性校验失败，或摘要期间会话发生变化时，当前上下文保持不变。上下文大小未知时，可以设置 `fallback_context_window_tokens`。设置与恢复方式见[高级配置](advanced-configuration.md)。

## 代码智能

启用后，Sigil 可以结合仓库结构和可用的语言服务器，提供符号、定义、引用、诊断、代码操作与重命名预览。按 `Alt-D` 检查已修改的源码。编辑操作仍需要经过差异审批。语言服务器不可用时，普通对话和文件工具仍可继续工作。见[高级配置](advanced-configuration.md#上下文精简与代码智能)。

Setup、凭据告警、终端问题或集成失败请进入[故障排查](troubleshooting.md)。

<!-- public-doc-cta: open-reference -->
下一步：[在参考中查找精确操作](reference.md)。
