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
| 读取当前聚焦的已保存工具输出的下一页 | `Alt-N` |
| 在当前聚焦的已保存工具输出中搜索有限长度的字面文本 | `Alt-F` |
| 聚焦任务验证 | `Alt-V`；`Enter` 运行，`I` 查看 |
| 在安全边界暂停当前精确任务 | `Alt-P`；之后用 `/task continue` 恢复 |
| 打开最近一次检查点恢复 | `Ctrl-R`；`Enter` 恢复，`F` 分叉会话，`Esc` 关闭 |
| 打开 Intent Stack 检查 | `Alt-S`；`Up/Down` 选择，`D` 预览 Drop，`Enter` 确认 |
| 打开已保存会话的操作菜单 | 选择 `/resume` 行，再按 `Ctrl-O` 或右键 |
| 切换最近一张 Mermaid 图表源码 | 没有已保存会话、工具卡片或其他更高优先级操作时按 `Ctrl-O` |
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
| `/model <model-id|connection-id/model-id>` | 切换到准确的 ready route 并新建会话；在选择器中按 `D` 只修改保存默认值 |
| `/effort <low|medium|high|max>` | 修改下一轮的推理强度 |
| `/compact` | 生成、校验并激活一个可恢复的上下文 checkpoint |
| `/update [check|refresh|apply]` | 检查当前渠道、跳过检查缓存，或明确应用已准入的更新 |
| `/intents` | 检查 durable intent 状态、artifact、冲突与精确 Drop 预览 |
| `/quit` | 退出 TUI |

别名：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。候选命令使用 `Up/Down`、`Tab` 与 `Enter`。

## CLI 命令

| 命令 | 用途 |
| --- | --- |
| `sigil` | 在当前工作区打开 TUI |
| `sigil doctor [--output text|json]` | 运行本机诊断 |
| `sigil mcp add <名称> -- <命令> [参数...]` | 添加本机 stdio MCP 服务 |
| `sigil mcp add <名称> --url <https-url>` | 添加远端 Streamable HTTP MCP 服务 |
| `sigil mcp list` / `get <名称>` / `remove <名称>` | 检查或移除已配置的 MCP 服务 |
| `sigil run "<task>" [--connection <id> --model <id>] [--output text|json|jsonl]` | 运行非交互任务；connection 与 model 必须同时提供 |
| `sigil resume [session-id]` | 打开 TUI 并恢复会话 |
| `sigil intent --session <session-id> inspect` | 输出某个精确会话的 bounded durable Intent Stack |
| `sigil intent --session <session-id> drop-preview --intent-id <id> --intent-version <n>` | 生成精确、只读的 Drop preview |
| `sigil intent --session <session-id> drop --operation-id <id> --stack-version <n> --preview-digest <digest>` | 确认并执行该精确 preview |
| `sigil serve` | 启动带认证且只监听回环地址的本机服务 |
| `sigil update check [--channel current|stable|beta] [--refresh] [--output text|json]` | 只检查发布版本，不修改当前安装 |
| `sigil update apply --yes [--channel current|stable|beta] [--output text|json]` | 明确安装已准入的独立更新，或显示对应包管理器命令 |
| `sigil --version` | 打印已安装版本 |
| `sigil --config <path> doctor` | 诊断显式配置 |

## 脚本输出与本地服务

`sigil run --output json` 会向 stdout 写入一条结果；`jsonl` 会写入有序事件，最后再写一条结果或错误。供人阅读的进度与安全网络提示保留在 stderr。退出码：`0` 表示成功，`1` 表示执行失败，`2` 表示调用方式或配置无效，`130` 表示已取消。

`sigil intent` 始终只向 stdout 写一条带版本的 JSON result 或安全 typed error。它只从当前
workspace catalog 解析精确 durable session id，不接受 session path，也不接受 client 提交的
permission/approval authority。执行 Drop 前必须生成新的 preview；durable session 仍有前台
run 时，preview 和 execute 都会 fail closed。

使用足够随机的环境令牌启动本机服务：

```bash
export SIGIL_HTTP_TOKEN="$(openssl rand -hex 32)"
sigil serve
```

服务会打印选中的回环地址。`GET /health` 无需认证；OpenAPI、披露记录、会话、运行、事件、取消、审批和历史目录路由都要求 `Authorization: Bearer <token>`。这不是远端或多用户服务，不使用 Cookie 认证或通配符 CORS，并会在按下 `Ctrl-C` 时关闭。

受信任的本机 launcher 可以请求一行不含 secret 的就绪 JSON，并用私有 stdin pipe 绑定子进程生命周期：

```bash
sigil serve --startup-output json --shutdown-on-stdin-close
```

带认证的 `GET /server-info` 使用同一份版本化 schema。每个工作区应运行一个服务；bearer token 只放在子进程环境中，不应进入参数或日志。关闭 owner pipe 会触发与 `Ctrl-C` 相同的优雅收尾；未提供该 flag 时，终端 stdin 不控制服务生命周期。

`GET /sessions` 只列出当前服务进程拥有的实时句柄。需要查询跨重启保留的工作区历史时，使用 `GET /session-catalog?limit=50&q=...&provider=...&pinned=true&state=ready`。历史目录只返回 OpenAPI 白名单中经过安全投影的精简元数据和不透明的 `next_cursor`；存储哈希、记录校验和、当前运行、审批和进度都不属于该响应。如果翻页期间历史发生变化，服务会返回 `409 stale_cursor`，客户端应从第一页重新查询。历史目录只是可从会话日志重建的索引，因此目录故障不会阻止运行或会话记录。

服务重启后，如需继续使用一条 ready 历史记录，将目录返回的相对 `session_ref` 和预期 durable `session_id` 发送给带认证的 `POST /sessions/open`。服务会重新验证会话日志，不会直接信任 SQLite，也不会创建运行或发起模型请求；成功后返回一个进程内会话句柄。同一服务进程内重复打开同一 durable session 会返回同一个句柄。来源缺失、未就绪或 identity 已变化时会 fail closed；客户端应重新查询目录，而不是自行拼接文件路径。

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
| `.sigil/agents`、`.sigil/commands`、`.sigil/skills`、`.sigil/plugins` | Sigil 原生工作区资源 |
| `.agents/skills`、`.codex/agents`、`.opencode/{skills,commands,agents}`、`.claude/{skills,commands,agents}` | 默认发现并继承工作区信任的兼容资源；command 使用斜杠前缀名称，agent 使用 `@名称` |
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
