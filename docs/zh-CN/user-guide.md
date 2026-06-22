# Sigil TUI 使用指南

[文档首页](README.md) · [快速上手](quickstart.md) · [常见工作流](workflows.md) · [参考](reference.md) · [English](../en/user-guide.md)

本文面向 Sigil 的日常使用者，重点说明 TUI 里能看到和能操作的内容。开发约束、crate 结构和测试规则请看 `dev/governance/*`。

如果你是第一次使用 Sigil，先读 [quickstart.md](quickstart.md)。如果你已经熟悉界面、想看真实任务提示词和流程，读 [workflows.md](workflows.md)。

## 启动

启动 TUI：

```bash
sigil
```

如果没有可用配置，Sigil 会进入 Quick Setup。首配流程只要求确认当前工作区、选择模型并填写认证。完成后会写入 `workspace.root = "."`，表示启动 TUI 时所在目录就是当前工作区。

如果还没有安装，见 [installation.md](installation.md)。在 checkout 内做开发时，`cargo run -p sigil` 等价。

认证方式和环境变量配置见 [configuration.md](configuration.md)。

## 主界面

Sigil 的主界面围绕这几个区域组织：

- Chat / transcript：展示用户消息、assistant 回复、thinking 摘要和工具活动。
- Composer：底部输入区，默认保持可见，发送后会清空并继续可输入。
- Info rail：右侧状态栏，展示 session、权限、模型、LSP、usage 和 controls。
- Activity：工具调用结果，例如读文件、搜索、执行命令、文件修改和 code diagnostics。
- Approval modal：工具需要确认时出现的审批卡片，展示 summary、files、diff 和 actions。

主路径是直接在 composer 输入任务。不要把 Sigil 当成命令集合使用；slash command 只处理少数高频控制动作。

## 高频操作

| 操作 | 快捷键 |
| --- | --- |
| 打开帮助 | `F1` |
| 打开 slash command selector | `/` |
| 回看 transcript | `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` |
| 切换默认权限模式 | `Shift-Tab` |
| 编辑 composer 文本 | `Ctrl-A/E`、`Ctrl-B/F`、`Alt-B/F`、`Ctrl-K/Y`、`Ctrl-Z` |
| 取消当前运行 | `Ctrl-C` |
| 退出当前浮层或清除 activity focus | `Esc` |
| 聚焦最近 activity | `Ctrl-G` |
| 切换 activity | `Alt-J` / `Alt-K` |
| 切换可见 agent transcript | composer agent 面板（`Down`、`Up/Down`、`Enter`），`Alt-A` / `Shift-Alt-A` |
| 展开或收起 thinking / activity | `Ctrl-T` |

Composer 聚焦时，`Up/Down` 会优先处理输入历史或多行输入里的光标移动。`Shift-Enter`、`Alt-Enter` 和 `Ctrl-J` 会插入换行。`Ctrl-Z` 只恢复最近一次被 `Esc` 清空的非空 draft，它不是通用 undo 栈。

终端支持 mouse capture 时，TUI 支持鼠标滚动 transcript、点击 composer 定位光标、操作审批控件、点击 slash 候选、setup/config 行、session 选择、activity 选择，以及点击 tool card header 或 hidden-preview 提示行展开/收起。拖选 transcript 文本时按显示列建立选区，然后按 `Ctrl-C` 通过 OSC52 复制。

可以在 `/config` 的 `Terminal` 区块调整 mouse capture、OSC52 复制和滚轮灵敏度。

终端专项 smoke 检查和 tmux/SSH 建议见 [terminal-compatibility.md](terminal-compatibility.md)。

## Slash Commands

| 命令 | 用途 |
| --- | --- |
| `/config` | 打开 TUI 配置页 |
| `/doctor` | 运行本地环境诊断，显示汇总和修复清单 |
| `/resume` | 选择并恢复历史 session |
| `/agent <main|child-id>` | 在 parent session 和 child agent transcript 之间切换主聊天区 |
| `/agent rename <child-id|current> <name>` | 为 child agent transcript 持久化一个短展示名 |
| `/plan` / `/plan <prompt>` | 进入 plan mode，或运行一次只读 planning prompt |
| `/task <任务>` | 先生成 durable plan，再按步骤执行复杂任务 |
| `/task continue` | 不带额外指引地继续最近一个计划任务 |
| `/model <flash|pro|id>` | 切换下一轮使用的模型，并开启 fresh session |
| `/effort <low|medium|high|max>` | 切换下一轮 reasoning effort |
| `/compact` | 手动压缩当前会话的 provider 可见上下文 |
| `/quit` | 退出 TUI |

`/model`、`/effort`、`/resume` 和 `/agent` 会显示候选项。可以用 `Up/Down` 选中，`Tab` 接受，`Enter` 执行。`/agent rename` 会在输入新名字前展示 child agent 候选项。

## 配置面板

`/config` 面板按 provider、permission、memory、compaction、code intelligence、terminal、appearance、Agents、Skills、Plugins 和 MCP 组织配置。`Appearance` 区块里在 `Theme` 行按 `Enter` 会循环切换内置主题，`Ctrl-S` 保存到 `sigil.toml` 后当前 TUI 会立即应用。`Plugins` 区块会发现工作区里的 `.sigil/plugins/<id>/plugin.toml` manifest。

可以用 `PgUp/PgDn` 在已发现 plugin 之间切换。detail view 会展示当前 trust 状态、manifest 路径、完整 manifest hash、skills、带 args 和 approval mode 的 hook command，以及带 args、startup 和 required 状态的 MCP server command。footer 的 `approve` 会信任当前展示的 manifest hash；`deny` 会禁用这个 hash。写入 review 决策前，Sigil 会先刷新 manifest，并把 review 追加到 session log。

## 计划任务

普通 composer 输入始终保持 chat-first，不会因为当前 session 存在未完成任务而自动继续 durable task。需要继续任务时，使用 `/task continue` 或 task UI action。需要编辑前只读规划时，使用 `/plan` 或 `/plan <prompt>`。遇到较大的任务时，可以用 `/task <任务>` 让 Sigil 先拆成 durable steps，再逐步执行。

计划任务会使用不同 role：

- Planner：读取上下文并写入 task plan。
- Executor：执行普通 workspace 变更步骤。
- Subagent read/write：把委派步骤放进 child session 执行，并在 parent task 中记录 child session link。

Task run、plan、step 状态、child-session link 和 subagent approval route 摘要都会写入 append-only control entry。右侧 Info rail 会从 durable state 显示最新 task 状态、plan 版本和当前步骤；存在 child agent 时，composer 输入框下方会显示紧凑 agent 面板，并展示每个 agent 的状态。在 composer 输入光标位于最后一行时按 `Down` 可聚焦这个面板，继续用 `Up/Down` 选择 agent，按 `Enter` 切换主聊天区。`Alt-A` / `Shift-Alt-A` 仍可在 `main` 和具体 child agent 之间循环切换，`/agent` 可精确选择目标。Child agent 展示名优先来自显式 plan metadata，其次由持久化的 `/agent rename` 覆盖；都没有时才退回 `read 1`、`write 1` 这类通用 role 编号。

恢复 session 只会重建可见 task 状态，不会自动继续未完成任务。需要继续时，直接在 composer 输入下一步指引；如果不需要额外指引，也可以输入 `/task continue`。

## 审批和文件变更

读文件和搜索这类只读工具通常可以直接执行。写文件、编辑文件、删除文件、shell 执行和外部 MCP 工具会按权限策略进入审批或拒绝。

审批卡片里重点看：

- Summary：这次工具调用要做什么。
- Files：可能影响哪些文件。
- Diff：写操作的变更预览。
- Actions：选择 allow 或 deny。

审批支持 `Left/Right` 选择动作后 `Enter` 确认，也保留 `Y/N` 快捷确认。长时间不决策会自动 deny，避免后台 worker 一直等待。

文件变更工具执行后，activity 会保留 bounded diff。大 diff 会截断并提示隐藏行数。

## Session 和恢复

默认 session log 写入当前工作区：

```text
.sigil/sessions/
```

Sigil 使用 append-only JSONL 保存 session 和控制状态。对使用者来说，这意味着：

- 重启 TUI 后默认恢复最近一次 session。
- 取消运行后，已经写入的消息和工具结果不会因为内存状态丢失而消失。
- 已开始但未完成的工具执行会在恢复后显示为 interrupted。
- 文件变更 activity 会随 session restore 恢复，仍可回看当时捕获的 diff 摘要。
- `/config` 保存新的默认 provider/model 不会改写当前 session identity；新默认值用于后续新 session。

## 长上下文和压缩

Info rail 会显示上一轮 provider 返回的 prompt token 相对模型 context window 的使用状态。`ctx` 行会标明窗口来自 provider metadata 还是 `fallback_context_window_tokens`，Sigil 也用同一个窗口计算 soft / hard threshold：

- soft threshold：提示上下文压力变高。
- hard threshold：当前 run 回到 idle 后自动压缩，不抢占正在流式执行的任务。
- `/compact`：手动压缩当前 session 的 provider 可见上下文。
- 如果窗口未知，可以配置 `fallback_context_window_tokens`，让 TUI 显示百分比和 threshold 提示。

压缩只追加控制记录，不改写旧历史。

## Code Intelligence

Code intelligence 默认关闭。开启后，Sigil 会注册只读代码工具：

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`
- `code_actions`

同时会注册需要审批 diff 才能写入的 LSP edit 工具：

- `code_action`
- `code_rename`

TUI 里可以用 `Alt-D` 对当前 git changed source files 触发 diagnostics 检查。结果会作为普通 activity 展示，并在 Info rail 的 LSP 区保留摘要。

没有可用 LSP server 时，Rust 项目会尽量回退到 Tree-sitter Rust outline / syntax diagnostics。失败不会阻塞普通 chat 和工具调用。

配置方式见 [configuration.md](configuration.md)。

## 常见问题

### 启动后直接进入 Quick Setup

说明没有找到可用配置，或者配置加载失败。完成 Quick Setup 后再进入主界面。

### API key 要不要写入配置文件

推荐临时或 CI 场景使用 `SIGIL_API_KEY`。如果通过 Quick Setup 或 `/config` 写入本地配置，`api_key` 会以 plaintext 保存；`doctor` 会把这个状态作为 warning 并给出修复建议。不要提交真实 `sigil.toml`。

### 终端鼠标或剪贴板支持不正常怎么办

可以在 `/config` 的 `Terminal` 区块调整，或在 `sigil.toml` 里设置 `[terminal].mouse_capture = false` / `[terminal].osc52_clipboard = false` / `[terminal].scroll_sensitivity = 3`。mouse capture 下一次启动生效；OSC52 剪贴板开关从下一次复制开始生效；scroll sensitivity 用于调整 transcript 和 approval diff 的滚轮步长。

运行 `/doctor` 可以查看检测到的终端 profile、multiplexer / remote 层，以及剪贴板桥接风险提示。

### 为什么子命令很少

直接运行 `sigil` 会打开 TUI。`sigil run`、`sigil doctor` 这类子命令主要用于自动化、脚本和调试，不承载完整产品心智。

### 为什么有些工具需要审批

Sigil 的 permission layer 负责 allow / ask / deny 判断。写文件、执行命令和外部工具默认更保守，目的是让用户在关键变更前看到预览和风险。
