<!-- public-doc-role: reference; authority: command-key-path-authority; sections: tui-keys,slash-commands,cli-commands,machine-output-and-local-server,config-resolution,important-paths,web-tool-inputs,approval-outcomes,session-recovery-facts; cta: return-user-guide -->

# 命令与键位参考

[文档首页](README.md) · [用户指南](user-guide.md) · [配置字段参考](configuration-reference.md) · [English](../en/reference.md)

本页用于精确查询用户可见命令、键位、路径、输出和恢复行为。

## TUI 键位

| 操作 | 键位 |
| --- | --- |
| 打开帮助 / slash selector | `F1` / `/` |
| 提交 | `Enter` |
| 显示或隐藏信息栏 | `F2` |
| 切换可见信息栏紧凑/详细模式 | `Shift-F2` |
| 滚动 transcript | `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` |
| 切换默认 permission mode | `Shift-Tab` |
| 输入换行 | `Ctrl-J`；终端支持时用 `Shift-Enter` / `Alt-Enter` |
| 移动输入光标 | `Ctrl-A/E`、`Ctrl-B/F`、`Alt-B/F`、方向键 |
| 删除输入内容 | `Backspace/Delete`、`Ctrl-H/W`、modified Backspace/Delete |
| 删除/粘贴行尾 | `Ctrl-K/Y` |
| 恢复最近一次由 `Esc` 清空的 draft | `Ctrl-Z` |
| 复制选中的 transcript 文本 | 有选区时按 `Ctrl-C` |
| 复制选区；没有选区时复制最新 assistant 回复 | `Ctrl-L`；不包含信息栏 |
| 取消当前运行 / 关闭浮层 | 无选区时按 `Ctrl-C` / `Esc` |
| 聚焦并切换活动 | `Ctrl-G`、`Alt-J` / `Alt-K` |
| 聚焦任务验证 | `Alt-V`；`Enter` 运行，`I` 查看 |
| 打开最近 checkpoint restore | `Ctrl-R`；`Enter` 恢复，`F` fork，`Esc` 关闭 |
| 打开 saved-session actions | 选择 `/resume` 行，再按 `Ctrl-O` 或右键 |
| 切换可见 agent transcript | Agent panel、`Alt-A`、`Shift-Alt-A` |
| 展开/折叠 thinking 或活动 | `Ctrl-T` |
| 检查已修改源码 | `Alt-D` |
| 取消聚焦的 terminal task | `Alt-X` |

`Up/Down` 优先处理输入历史或多行移动。`Ctrl-Z` 只恢复一次被清空 draft，不是通用 undo stack。

## Slash Commands

| Command | 用途 |
| --- | --- |
| `/config` | 打开配置 |
| `/doctor` | 运行诊断 |
| `/feedback` | 预览并导出本机支持报告 |
| `/new` | 新建 session |
| `/resume` | 选择已保存 session |
| `/agent <main|child-id>` | 切换可见 transcript |
| `/agent rename <child-id|current> <name>` | 命名 child transcript |
| `/agent cancel <child-id|current>` | 取消仍有 live handle 的 child |
| `/queue` | 显示高级 follow-up 控制 |
| `/queue next|interrupt|edit|delete [item]` | 调整顺序、中断后执行、编辑或删除 follow-up |
| `/plan [prompt]` | 运行只读计划；接受 card 后开始 task |
| `/task <任务>` | 开始多步骤执行 |
| `/task continue` | 继续最新未完成 task |
| `/model <flash|pro|id>` | 为下一轮切换 model，并新建 session |
| `/effort <low|medium|high|max>` | 修改下一轮 reasoning effort |
| `/compact` | Review 上下文精简方案，并在 ready 时应用一次 |
| `/quit` | 退出 TUI |

Alias：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。候选命令使用 `Up/Down`、`Tab` 与 `Enter`。

## CLI Commands

| Command | 用途 |
| --- | --- |
| `sigil` | 在当前 workspace 打开 TUI |
| `sigil doctor [--output text|json]` | 运行本机诊断 |
| `sigil run "<task>" [--output text|json|jsonl]` | 运行非交互任务 |
| `sigil resume [session-id]` | 打开 TUI 并恢复 session |
| `sigil serve` | 启动带认证且只监听 loopback 的本机服务 |
| `sigil --version` | 打印已安装版本 |
| `sigil --config <path> doctor` | 诊断显式配置 |

## Machine Output 与本地服务

`sigil run --output json` 向 stdout 写入一条结果；`jsonl` 写入有序 event，最后写入一条结果或错误。人类可读进度与安全网络提示留在 stderr。Exit code：`0` 成功、`1` 执行失败、`2` 调用/配置无效、`130` 取消。

使用高熵环境 token 启动本机服务：

```bash
export SIGIL_HTTP_TOKEN="$(openssl rand -hex 32)"
sigil serve
```

服务会打印选中的 loopback 地址。`GET /health` 无需认证；OpenAPI、disclosure、session、run、event、取消和审批 route 都要求 `Authorization: Bearer <token>`。它不是远端或多用户服务，不使用 cookie auth 或 wildcard CORS，并在 `Ctrl-C` 时关闭。

## Config 解析顺序

提供 `--config <path>` 时使用该文件；否则加载 `~/.sigil/sigil.toml`。Workspace root 下的 `sigil.toml` 不会自动加载。

## 重要路径

| 路径 | 含义 |
| --- | --- |
| State root `workspaces/<workspace-id>/sessions/` | Session 日志 |
| State root `workspaces/<workspace-id>/input-history.jsonl` | 输入历史 |
| State root `workspaces/<workspace-id>/artifacts/` | Terminal 与变更 artifact |
| Cache root `workspaces/<workspace-id>/tmp/` | `$SIGIL_SCRATCH_DIR` |
| 用户配置 `~/.sigil/sigil.toml` | 默认本机配置 |
| `.sigil/agents`、`.sigil/commands`、`.sigil/skills`、`.sigil/plugins` | Workspace 资源 |
| `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` | Workspace 指令 |

不要在 config 或本机指令文件中提交真实 secret。

## Web Tool 输入

| Tool | 输入 | 边界 |
| --- | --- | --- |
| `websearch` | `query`；可选 `max_results` | 使用选中的 provider-hosted、configured MCP 或 bundled route。 |
| `webfetch` | 已观察到的 `source_id`；可选 `format`、`max_content_bytes` | 只打开当前 session 已观察到的 URL。 |

两者还遵守 `[web].network_mode`。`deny` 会阻止；未解决的 `ask` 无法在 headless 中继续。

## Approval Outcomes

| Outcome | 含义 |
| --- | --- |
| `allow` | 运行动作 |
| `deny` | 拒绝动作 |
| `timeout` | 长时间无决定后拒绝 |
| `approval_required` | 非交互运行需要但无法请求决定 |

## Session Recovery Facts

- 重启会恢复受支持的可见 session 与 task state。
- 未完成工具恢复为 interrupted，不会静默重跑。
- `/new` 新建 session；`/resume` 选择旧 session。
- Saved-session action 包括恢复、conversation fork、安全 export、pin/unpin 与经过检查的删除。
- Retention cleanup 需要 `/config` → **Storage** 下的显式预览与确认。
- 退出会打印 session id 和 `sigil resume <session-id>`。
- 存在未完成 task 时，`/task continue` 会继续最新一项。

Provider 凭据见 [Provider 指南](providers.md)，配置字段见[配置字段参考](configuration-reference.md)。

<!-- public-doc-cta: return-user-guide -->
下一步：[返回用户指南](user-guide.md)。
