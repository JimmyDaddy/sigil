# 命令与键位参考

[文档首页](README.md) · [English](../en/reference.md)

这一页集中列出用户可见命令、键位、路径、共享配置区块、审批结果和恢复事实。Provider 选择与凭据留在 provider 文档中，避免参考页再次复制。

## TUI 键位

| 动作 | 键位 |
| --- | --- |
| 打开帮助 | `F1` |
| 打开 slash command selector | `/` |
| 提交 prompt 或已选 slash command | `Enter` |
| 切换右侧 info rail 精简/详情 | `F2` |
| 滚动 transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| 切换默认 permission mode | `Shift-Tab` |
| 插入 composer 换行 | `Ctrl-J`；terminal keyboard enhancement 已启用且能上报 modifier 时也支持 `Shift-Enter` / `Alt-Enter` |
| 按行或字符移动 composer 光标 | `Ctrl-A/E`、`Ctrl-B/F`、`Left/Right` |
| 按词移动 composer 光标 | `Alt-B/F`、`Ctrl-Left/Right` |
| 删除 composer 文本 | `Backspace/Delete`、`Ctrl-H`、`Ctrl-W`、`Ctrl/Alt-Backspace`、`Ctrl/Alt-Delete` |
| kill/yank composer 行尾 | `Ctrl-K/Y` |
| 恢复最近一次被 Esc 清空的 draft | `Ctrl-Z` |
| 请求协作式取消当前 run | `Ctrl-C` |
| 离开 overlay 或清除 activity focus | `Esc` |
| 聚焦最新 activity | `Ctrl-G` |
| 在 activities 间移动 | `Alt-J` / `Alt-K` |
| 聚焦 task verification | `Alt-V`；随后用 `Enter` 执行精确 action，`I` 查看证据 |
| 打开最新 checkpoint 恢复 | `Ctrl-R` 打开并加载反向 diff 弹窗；`Enter` 恢复受控文件，`F` 在不改文件的情况下 fork 会话，`Esc` 关闭 |
| 切换可见 agent transcript | composer agent 面板（`Down`、`Up/Down`、`Enter`）、`Alt-A`、`Shift-Alt-A` |
| 展开或折叠 thinking / activity | `Ctrl-T` |
| 对 changed source files 运行 code diagnostics | `Alt-D` |
| 取消聚焦的运行中 terminal task | `Alt-X` |
| 在 `/config` 中选择或启动 MCP server | `Enter` 循环切换当前查看的 server；`Down` 进入 footer actions；选择 `activate` 后按 `Enter` 启动或刷新 |

当 composer 聚焦时，`Up/Down` 会优先处理 prompt history 或 multiline input 内的光标移动。如果存在 child agent，输入光标位于 composer 最后一行时按 `Down` 会聚焦 composer agent 面板。`Ctrl-Z` 只恢复被 `Esc` 清空的单个 draft，不是通用 undo 栈。

## Slash Commands

| Command | 作用 |
| --- | --- |
| `/config` | 打开 TUI config panel |
| `/doctor` | 在 transcript 中运行本地 setup diagnostics |
| `/new` | 使用当前 provider 和 model 开始新 session |
| `/resume` | 选择并恢复历史 session |
| `/agent <main|child-id>` | 在 parent session 和 child agent transcript 之间切换主聊天区 |
| `/agent rename <child-id|current> <name>` | 为 child agent transcript 持久化一个短展示名 |
| `/agent cancel <child-id|current>` | 取消仍有 live runtime handle 的运行中后台 child agent |
| `/queue` | 高级 follow-up 控制 |
| `/queue next|interrupt|edit|delete [item]` | 保留 follow-up 到下一轮、interrupt 并立即运行、编辑或取消 |
| `/plan` / `/plan <prompt>` | 运行一次只读 planning prompt；接受 plan card 后创建并运行 durable task |
| `/task <task>` | 创建 durable plan，并分步骤执行任务 |
| `/task continue` | 不带额外 guidance 继续最新 planned task |
| `/model <flash|pro|id>` | 切换下一轮 run 的 model，并开始新 session |
| `/effort <low|medium|high|max>` | 切换下一轮 run 的 reasoning effort |
| `/compact` | 审查 V2 折叠计划；仅本地验证过的 DeepSeek V4 Flash portable checkpoint 可按 Enter 应用 |
| `/quit` | 退出 TUI |

Aliases：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。

Workspace trust 由启动时的 workspace trust gate 处理，不是 slash command。trust decision 会记录进 session audit log；它允许仓库本地 verification 候选检查被提升为 task readiness 可用的检查，也允许精确匹配 workspace 且 `trust_required = true` 的 LSP 启动。它不会单独授予 shell、plugin、MCP 或文件写入权限，LSP 写工具仍需 diff 审批。

`/model`、`/effort`、`/resume`、`/agent` 和 `/queue` 会展示候选项。使用 `Up/Down` 选择，`Tab` 接受，`Enter` 执行。`/agent rename` 会在输入新名字前展示 child agent 候选项。

## CLI Commands

| Command | 用途 |
| --- | --- |
| `sigil` | 在当前 workspace 打开 TUI |
| `sigil doctor` | 运行本地诊断 |
| `sigil run "<task>"` | 运行非交互自动化任务 |
| `sigil resume [session-id]` | 打开 TUI 并恢复 latest 或指定 session；TUI 退出时会打印可复制的恢复命令 |
| `sigil serve` | 检查本地服务设置；尚不能启动服务 |
| `sigil --version` | 打印安装版本 |
| `sigil --config <path> doctor` | 使用显式 config 文件运行诊断 |

子命令用于自动化、诊断、脚本和设置检查。完整产品表面是 TUI。

## Config 解析顺序

Sigil 按以下顺序解析 config：

1. `--config <path>`
2. 用户可见 Sigil 配置目录下的 `sigil.toml`

默认用户配置路径：

- `~/.sigil/sigil.toml`

## 重要路径

| Path | 含义 |
| --- | --- |
| 用户态 state root `workspaces/<workspace-id>/sessions/` | 默认 append-only session logs |
| 用户态 state root `workspaces/<workspace-id>/input-history.jsonl` | composer input history |
| 用户态 state root `workspaces/<workspace-id>/artifacts/` | terminal 和 changeset artifacts |
| 用户态 cache root `workspaces/<workspace-id>/tmp/` | shell scratch 目录，通过 `$SIGIL_SCRATCH_DIR` 暴露，对模型显示为 `cache/tmp` |
| 用户配置目录 `sigil.toml` | 默认本机配置 |
| `.sigil/agents/`、`.sigil/commands/`、`.sigil/skills/`、`.sigil/plugins/` | 可选 workspace project assets |
| `SIGIL.md` | 稳定 workspace memory file |
| `AGENTS.md` | Sigil 可作为 memory 加载的 agent 协作说明 |
| `SIGIL.local.md` | 本地专用 memory file |

不要提交包含真实 secret 的 `sigil.toml` 或本地 memory 文件。workspace 根目录的 `sigil.toml` 默认不会被读取；如需实验配置，请显式传入 `--config <path>`。

## Provider 设置

支持的 provider value、model 选择和认证优先级见 [Provider 指南](providers.md)。其中链接的各 provider 专页分别维护可复制的配置 block 和完整环境变量清单。共享的模型请求 timeout 覆盖见[高级配置](advanced-configuration.md#终端与模型请求环境变量覆盖)。

## 常见 Config Sections

| Section | 作用 |
| --- | --- |
| `[workspace]` | Workspace root |
| `[agent]` | 共享 agent 设置 |
| `[permission]` | 默认审批策略 |
| `[web]` | Stable search route、网络策略、destination rule 与预算 |
| `[memory]` | Workspace memory loading |
| `[compaction]` | Context compaction 阈值 |
| `[task]` | Planned task 行为和 role settings |
| `[verification]` | 显式用户批准的 verification checks |
| `[code_intelligence]` | LSP 和 code intelligence tools |
| `[terminal]` | Mouse、OSC52 clipboard 和 scroll 行为 |
| `[appearance]` | TUI 主题、usage cost currency 和语义颜色覆盖 |
| `[[mcp_servers]]` | 显式 `stdio` 或用户根 `streamable_http` MCP server 配置 |

示例见 [Sigil 配置指南](configuration.md)。
Provider 与 model 选择、`[providers.*]` block 和认证环境变量统一由 [Provider 指南](providers.md)索引。

## Web Tool 输入

| Tool | 必填输入 | 关键边界 |
| --- | --- | --- |
| `websearch` | `query`；可选 `max_results` | Route 由 provider-hosted、authoritative configured MCP 或 bundled Exa 解析；已选 route 失败后不会静默换 destination。 |
| `webfetch` | `source_id`；可选 `format`（`markdown`/`text`）和 `max_content_bytes` | 只接受 session-local 精确 URL capability，不接受新造的 raw `url` 参数。 |

两个工具都是带独立 `NetworkRead` effect 的 `Read` 操作。即使本地 permission mode 较宽松，`[web].network_mode = "deny"` 仍会阻断；`ask` 必须来自显式交互动作，因此在无法询问的 headless/eager 场景会 fail closed。

## Approval Outcomes

| Outcome | 含义 |
| --- | --- |
| allow | 执行 tool call |
| deny | 向模型返回结构化 denial |
| timeout | 等待过久后自动 deny |
| approval_required | Headless mode 需要决策但无法交互询问 |

## Session Recovery Facts

- Session logs 是 append-only JSONL。
- 重启会恢复可见 session state。
- Started tools 没有 terminal records 时会恢复为 interrupted。
- 取消按 run scope durable 且终态唯一：`cancel requested` 后只能进入清理已确认的 `cancelled`，或清理未确认的 `interrupted`；恢复流程不会把未确认的取消升级成 `cancelled`。
- 恢复不会静默重放未完成工具。
- `/new` 会开始一条新的 append-only session log。
- `/resume` 选择历史 session。
- 退出 TUI 会打印当前 session id 和 `sigil resume <session-id>` 恢复命令。
- 存在未完成 planned task 时，`/task continue` 会继续最新任务。
