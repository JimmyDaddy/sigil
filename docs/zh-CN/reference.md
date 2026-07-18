<!-- public-doc-role: reference; authority: command-key-path-authority; sections: tui-keys,slash-commands,cli-commands,machine-output-and-local-server,config-resolution,important-paths,web-tool-inputs,approval-outcomes,session-recovery-facts; cta: return-user-guide -->

# 命令与键位参考

[文档首页](README.md) · [用户指南](user-guide.md) · [配置字段参考](configuration-reference.md) · [English](../en/reference.md)

本页用于精确查询用户可见命令、键位、路径、输出和恢复行为。

## TUI 键位

| 操作 | 键位 |
| --- | --- |
| 打开帮助或斜杠命令列表 | `F1` / `/` |
| 提交 | `Enter` |
| 显示或隐藏信息栏 | `F2` |
| 切换可见信息栏紧凑/详细模式 | `Shift-F2` |
| 滚动会话记录 | `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` |
| 切换默认权限模式 | `Shift-Tab` |
| 输入换行 | `Ctrl-J`；终端支持时用 `Shift-Enter` / `Alt-Enter` |
| 移动输入光标 | `Ctrl-A/E`、`Ctrl-B/F`、`Alt-B/F`、方向键 |
| 删除输入内容 | `Backspace/Delete`、`Ctrl-H/W`、终端支持的修饰键加 Backspace/Delete |
| 删除/粘贴行尾 | `Ctrl-K/Y` |
| 恢复最近一次由 `Esc` 清空的草稿 | `Ctrl-Z` |
| 复制选中的会话文本 | 有选区时按 `Ctrl-C` |
| 复制选区；没有选区时复制最新助手回复 | `Ctrl-L`；不包含信息栏 |
| 取消当前运行 / 关闭浮层 | 无选区时按 `Ctrl-C` / `Esc` |
| 聚焦并切换活动 | `Ctrl-G`、`Alt-J` / `Alt-K` |
| 聚焦任务验证 | `Alt-V`；`Enter` 运行，`I` 查看 |
| 打开最近一次检查点恢复 | `Ctrl-R`；`Enter` 恢复，`F` 分叉会话，`Esc` 关闭 |
| 打开已保存会话的操作菜单 | 选择 `/resume` 行，再按 `Ctrl-O` 或右键 |
| 切换可见的子智能体会话 | 子智能体面板、`Alt-A`、`Shift-Alt-A` |
| 展开或折叠推理过程与活动 | `Ctrl-T` |
| 检查已修改源码 | `Alt-D` |
| 取消当前聚焦的终端任务 | `Alt-X` |

`Up/Down` 会优先处理输入历史或多行移动。`Ctrl-Z` 只能恢复最近一次被清空的草稿，不是通用的撤销功能。

## 斜杠命令

| 命令 | 用途 |
| --- | --- |
| `/config` | 打开配置 |
| `/doctor` | 运行诊断 |
| `/feedback` | 预览并导出本机支持报告 |
| `/new` | 新建会话 |
| `/resume` | 选择已保存会话 |
| `/agent <main|child-id>` | 切换可见的会话记录 |
| `/agent rename <child-id|current> <name>` | 命名子智能体会话 |
| `/agent cancel <child-id|current>` | 取消仍在运行的子智能体 |
| `/queue` | 显示高级后续输入控制 |
| `/queue next|interrupt|edit|delete [item]` | 调整顺序、中断后执行、编辑或删除后续输入 |
| `/plan [prompt]` | 运行只读计划；接受计划后开始任务 |
| `/task <任务>` | 开始多步骤执行 |
| `/task continue` | 继续最近的未完成任务 |
| `/model <flash|pro|id>` | 为下一轮切换模型，并新建会话 |
| `/effort <low|medium|high|max>` | 修改下一轮的推理强度 |
| `/compact` | 检查上下文精简方案，并在就绪时应用一次 |
| `/quit` | 退出 TUI |

别名：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。候选命令使用 `Up/Down`、`Tab` 与 `Enter`。

## CLI 命令

| 命令 | 用途 |
| --- | --- |
| `sigil` | 在当前工作区打开 TUI |
| `sigil doctor [--output text|json]` | 运行本机诊断 |
| `sigil run "<task>" [--output text|json|jsonl]` | 运行非交互任务 |
| `sigil resume [session-id]` | 打开 TUI 并恢复会话 |
| `sigil serve` | 启动带认证且只监听回环地址的本机服务 |
| `sigil --version` | 打印已安装版本 |
| `sigil --config <path> doctor` | 诊断显式配置 |

## 脚本输出与本地服务

`sigil run --output json` 会向 stdout 写入一条结果；`jsonl` 会写入有序事件，最后再写一条结果或错误。供人阅读的进度与安全网络提示保留在 stderr。退出码：`0` 表示成功，`1` 表示执行失败，`2` 表示调用方式或配置无效，`130` 表示已取消。

使用足够随机的环境令牌启动本机服务：

```bash
export SIGIL_HTTP_TOKEN="$(openssl rand -hex 32)"
sigil serve
```

服务会打印选中的回环地址。`GET /health` 无需认证；OpenAPI、披露记录、会话、运行、事件、取消、审批和历史目录路由都要求 `Authorization: Bearer <token>`。这不是远端或多用户服务，不使用 Cookie 认证或通配符 CORS，并会在按下 `Ctrl-C` 时关闭。

`GET /sessions` 只列出当前服务进程拥有的实时句柄。需要查询跨重启保留的工作区历史时，使用 `GET /session-catalog?limit=50&q=...&provider=...&pinned=true&state=ready`。历史目录只返回 OpenAPI 白名单中经过安全投影的精简元数据和不透明的 `next_cursor`；存储哈希、记录校验和、当前运行、审批和进度都不属于该响应。如果翻页期间历史发生变化，服务会返回 `409 stale_cursor`，客户端应从第一页重新查询。历史目录只是可从会话日志重建的索引，因此目录故障不会阻止运行或会话记录。

## 配置解析顺序

提供 `--config <path>` 时使用该文件；否则加载 `~/.sigil/sigil.toml`。工作区根目录下的 `sigil.toml` 不会自动加载。

## 重要路径

| 路径 | 含义 |
| --- | --- |
| 状态根目录 `workspaces/<workspace-id>/sessions/` | 会话日志 |
| 状态根目录 `workspaces/<workspace-id>/input-history.jsonl` | 输入历史 |
| 状态根目录 `workspaces/<workspace-id>/artifacts/` | 终端任务与变更记录 |
| 缓存根目录 `workspaces/<workspace-id>/tmp/` | `$SIGIL_SCRATCH_DIR` |
| 用户配置 `~/.sigil/sigil.toml` | 默认本机配置 |
| `.sigil/agents`、`.sigil/commands`、`.sigil/skills`、`.sigil/plugins` | 工作区资源 |
| `SIGIL.md`、`AGENTS.md`、`SIGIL.local.md` | 工作区指令 |

不要在配置或本机指令文件中提交真实密钥。

## Web 工具输入

| 工具 | 输入 | 边界 |
| --- | --- | --- |
| `websearch` | `query`；可选 `max_results` | 使用选中的模型服务、已配置 MCP 或内置路由。 |
| `webfetch` | 已观察到的 `source_id`；可选 `format`、`max_content_bytes` | 只打开当前会话已经观察到的 URL。 |

两者还遵守 `[web].network_mode`。`deny` 会阻止请求；未解决的 `ask` 无法在非交互模式中继续。

## 审批结果

| 结果 | 含义 |
| --- | --- |
| `allow` | 运行动作 |
| `deny` | 拒绝动作 |
| `timeout` | 长时间无决定后拒绝 |
| `approval_required` | 非交互运行需要但无法请求决定 |

## 会话恢复要点

- 重启后会恢复受支持的可见会话与任务状态。
- 未完成的工具会恢复为“已中断”，不会静默重跑。
- `/new` 新建会话；`/resume` 选择以前的会话。
- 已保存会话的操作包括恢复、会话分叉、安全导出、固定或取消固定，以及经过检查的删除。
- 保留期限清理需要在 `/config` → **Storage** 中明确预览并确认。
- 退出时会显示会话 ID 和 `sigil resume <session-id>`。
- 存在未完成任务时，`/task continue` 会继续最近的一项。

模型服务凭据见[模型服务指南](providers.md)，配置字段见[配置字段参考](configuration-reference.md)。

<!-- public-doc-cta: return-user-guide -->
下一步：[返回用户指南](user-guide.md)。
