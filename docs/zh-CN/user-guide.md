<!-- public-doc-role: user-guide; authority: tui-daily-use-authority; sections: start,headless-and-local-api-workflows,main-screen,common-controls,image-attachments,slash-commands,config-panel,web-search-and-fetch,planned-tasks,approvals-and-file-changes,sessions-and-recovery,long-context-and-compaction,code-intelligence; cta: open-reference -->

# Sigil TUI 用户指南

[文档首页](README.md) · [快速开始](quickstart.md) · [常见工作流](workflows.md) · [参考](reference.md) · [English](../en/user-guide.md)

本指南介绍每天使用的 TUI。完整命令和键位表只在[参考](reference.md)维护。

## 启动

在要处理的仓库中运行 `sigil`。缺少配置时，Quick Setup 会要求确认 workspace、provider、model 和认证。找不到命令时见[安装](installation.md)，可重复设置见[配置](configuration.md)。

## Headless 与本地 API 工作流

TUI 是常规用户界面。`sigil run` 为脚本提供文本、JSON 或 JSONL 输出；未解决的审批会失败，不会打开浮层。`sigil serve` 是面向受信任本机 client 的高级接口，只监听 loopback 并要求认证。命令、认证、输出和 exit behavior 见[机器输出与本地服务](reference.md#machine-output-与本地服务)。

## 主界面

- **会话记录：** 用户消息、assistant 回复和工具活动。
- **输入框：** 底部任务输入区域。
- **信息栏：** 宽度允许时显示 session、权限、model、用量、code intelligence 与操作状态。
- **活动：** 文件读取、搜索、命令、编辑、诊断和结果。
- **审批浮层：** 高风险工具调用的操作、受影响文件、预览与决策。

普通任务直接在输入框中提出；slash command 只承担少量控制动作。

## 高频操作

按 `F1` 或 `/` 打开帮助与命令；`F2` 显示或隐藏信息栏，`Shift-F2` 切换详细程度。`Ctrl-G` 聚焦活动，`Alt-V` 聚焦任务验证，`Ctrl-R` 打开最近一次受控恢复，`Ctrl-T` 展开或折叠 thinking 与活动。未选中文本时，`Ctrl-C` 取消运行；`Esc` 关闭当前浮层。完整键位矩阵见[参考](reference.md#tui-键位)。

终端宽度足够时，信息栏默认显示。`F2` 只改变当前运行。要修改启动默认值，打开 `/config`，选择 **Appearance**，切换 **Info rail**，再用 `Ctrl-S` 保存；窄终端仍会自动收起。

拖选会话文本后按 `Ctrl-C`，可以在剪贴板集成可用时复制选区。`Ctrl-L` 会优先复制现有选区；没有选区时才复制最新 assistant 回复。这两条路径都使用会话内容，因此不会包含信息栏。没有选区时，`Ctrl-C` 保持原有取消或退出语义。

鼠标还支持滚动、输入框定位、审批控件、菜单、session 行、活动和工具卡片展开。终端复制、键盘、鼠标、tmux 与 SSH 检查见[终端兼容性](terminal-compatibility.md)。

## 图片附件

在空闲输入框中粘贴本机 PNG、JPEG 或 WebP 路径，或在剪贴板有图片时按 `Ctrl-V`。发送前检查 metadata chip；用 `Up` 选中 chip，`Left/Right` 切换，`Backspace` 或 `Delete` 删除。

每轮最多 4 张图片，每张 8 MiB、总计 24 MiB，并限制尺寸。图片不能排队，也不能附加到 plan、command、skill、task 或 agent 输入。只有明确支持图片的 OpenAI Responses、Anthropic 和 Gemini model 可以接收。恢复的 session 如果缺少本机图片，请重新粘贴原图，或从不需要该图片的对话继续。

## Slash Commands

常用控制命令：

- `/config` — 修改常用设置。
- `/doctor` — 诊断 setup、认证、集成和终端支持。
- `/resume` — 选择已保存 session。
- `/plan <prompt>` — 执行前请求只读计划。
- `/task <任务>` 与 `/task continue` — 启动或继续多步骤工作。
- `/compact` — 检查上下文精简方案。
- `/feedback` — 预览并保存本机支持报告。
- `/quit` — 关闭 TUI。

Model、agent、follow-up 和其他所有命令形式见[参考](reference.md#slash-commands)。

运行进行中时，普通输入会成为可见 follow-up，通常在当前 turn 结束后执行。按 `Tab` 聚焦 follow-up 面板；只有明确要中断时才选择对应动作。交付状态不确定时，Sigil 不会自动重发 follow-up。

## 配置面板

`/config` 汇总常用 provider、权限、Web、memory、上下文、code intelligence、terminal、appearance、agent、skill、plugin 和 MCP 设置。Theme 修改会立即预览；按 `Ctrl-S` 保存。精确字段和默认值只在[配置字段参考](configuration-reference.md)维护。

为 Streamable HTTP MCP server 配置 OAuth 后，打开详情并选择 **Authentication**。浮层可以显示状态、开始登录、打开或复制授权 URL、接收临时 callback URL、刷新、退出登录，或清除保留的本机凭据。连接 server 前请阅读 [MCP](mcp.md)。

## Web Search 与 Fetch

启用后，search 与 fetch 活动会显示数据发往哪里。搜索结果属于外部不可信内容。Fetch 只打开当前 session 已观察到的 URL，并重新应用网络限制。Route、关闭方式和目标规则见[权限与沙箱](permissions-and-sandbox.md#网络与-web-工具)。

## 计划任务

使用 `/plan` 获取只读计划；只有需要开始执行时才接受 Plan ready card。已经确定需要多步骤执行时使用 `/task`。普通 chat 保持 chat-first，不会自行继续未完成任务。

任务界面显示步骤、当前状态、child agent 工作，并在需要检查时显示 Verification card。按 `Alt-V` 聚焦。恢复 session 只显示已保存任务状态，不会自动继续。

## 审批和文件变更

只读文件与搜索工具通常直接运行。写入、删除、命令、网络和外部工具遵守配置的权限策略。

允许高风险动作前，检查：

- 将要执行什么；
- 涉及哪些文件或目标；
- 可见 diff 或请求预览；
- **allow**、**allow for this session** 或 **deny** 是否符合意图。

活动视图可能缩短大型 diff；提交前仍要检查仓库最终 diff。

## Session 和恢复

Session 日志保存在 Sigil 用户态状态目录。重启后，Sigil 可以恢复最新受支持的 session，包括可见消息、任务状态、已完成活动摘要和中断工具结果。中断工具不会被静默重跑。退出时会打印 session id 和 `sigil resume <session-id>` 命令。

取消操作会停止接收新工作，并短暂等待活动工作结束。**Cancelled** 表示清理完成；**Interrupted** 表示在限制时间内无法确认。已经保存的消息和结果仍会保留。

### 管理已保存的 Session

打开 `/resume` 并选择一行。`Enter` 恢复；`Ctrl-O` 或右键打开操作，可以 fork 对话、导出安全 transcript、pin session 或检查删除。删除需要二次确认，并且只作用于已经检查的非活动文件。Retention cleanup 是 `/config` → **Storage** 下的显式操作；普通启动不会自动删除 session。

### 受控 Checkpoint 与会话 Fork

最近完成的 turn 包含受支持文件编辑时，按 `Ctrl-R` 检查 reverse diff。`Enter` 恢复已检查文件；`F` 只 fork 对话，不修改共享文件。文件已变化或预览过期会阻止恢复。Shell 命令、远端服务、目录、rename、symlink 和其他外部效果不会被撤销。成功恢复后要重新运行验证。

## 长上下文和压缩

信息栏显示已报告的上下文用量，并在 model window 接近上限时提醒。`/compact` 打开只读预览，显示哪些内容会精简、哪些会保留；只在界面显示 ready 时应用。上下文大小未知时，可设置 `fallback_context_window_tokens`。设置与恢复方式见[高级配置](advanced-configuration.md)。

## Code Intelligence

启用后，Sigil 可以利用仓库结构和可用 language server 提供符号、定义、引用、诊断、code action 与 rename preview。`Alt-D` 检查已修改源码。编辑动作仍需要 diff 审批。Language server 不可用时，普通 chat 和文件工具继续工作。见[高级配置](advanced-configuration.md#compaction-与代码智能)。

Setup、凭据告警、终端问题或集成失败请进入[故障排查](troubleshooting.md)。

<!-- public-doc-cta: open-reference -->
下一步：[在参考中查找精确操作](reference.md)。
