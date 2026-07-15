# Sigil 配置指南

[文档首页](README.md) · [权限与沙箱](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [Provider 指南](providers.md) · [English](../en/configuration.md)

这是 Sigil 共享配置的推荐入口。本文说明配置在哪里、最小可用设置、workspace 与存储选择，以及应该去哪里查找具体设置。Provider 凭据和 provider 专项选项由 [Provider 指南](providers.md)维护。

## 选择正确的页面

| 你想要… | 从这里开始 |
| --- | --- |
| 设置 Sigil、选择 workspace 或找到配置文件 | 本文 |
| 修改批准、网络、外部路径或 sandbox | [权限与沙箱](permissions-and-sandbox.md) |
| 修改 TUI 主题、代码高亮或颜色 | [外观](appearance.md) |
| 配置 task、检查、memory、代码智能、终端、plugin 或 MCP | [高级配置](advanced-configuration.md) |
| 查询精确的 `sigil.toml` 字段或值 | [配置字段参考](configuration-reference.md) |
| 选择 model service、endpoint 或凭据 | [Provider 指南](providers.md) |

## 配置查找顺序

Sigil 按以下顺序解析配置：

1. `--config <path>`
2. 用户可见 Sigil 配置目录中的 `sigil.toml`

默认用户配置为：

```text
~/.sigil/sigil.toml
```

Quick Setup 会写入这个用户配置。workspace 中的 `sigil.toml` 不会被自动加载；只有明确想使用本地实验配置时，才传入 `--config <path>`。

## 最小路径

日常交互使用时，在希望工作的项目中启动 Sigil 并完成 Quick Setup：

```bash
cd /path/to/workspace
sigil
```

临时使用或 CI 时，先选择 provider，并在启动前设置对应的 provider 凭据。[Provider 指南](providers.md#认证优先级)提供每项服务正确的变量与可复制示例；不存在一个对所有 provider 通用的 API key 变量。

如果希望手写一份很小的共享配置，可以从这里开始：

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

然后从所选 provider 页面加入 provider 区块。可复制示例位于 [docs/examples/config](../examples/config)。

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` 会解析为启动 `sigil` 时所在的目录，因此一份用户配置可以跟随你打开的仓库。文件工具受限于这个 workspace，会拒绝父路径逃逸、绝对路径以及指向 workspace 外的 symlink。

允许 workspace 外路径或修改本地命令行为之前，请先阅读[权限与沙箱](permissions-and-sandbox.md)。

## Storage 与 Session 路径

```toml
[storage]
state_root = "auto"
cache_root = "auto"

[session]
# log_dir = "sessions"

[session.retention]
max_sessions = 500
max_bytes = 2147483648
expire_older_than_ms = 15552000000 # 180 天
```

`state_root` 保存持久的每用户 Sigil state，例如 session 相关记录和 artifact。`cache_root` 保存可重建的 scratch data。`session.log_dir` 只修改当前 workspace 的 session log 位置，不会取代 state root。

Session retention 只为显式 maintenance preview 与确认提供 policy；普通启动、run、resume 和 `sigil serve` 都不会自动应用。current、active、pinned、unsupported 或发生 drift 的 session 会受保护。TUI 操作见[管理已保存的 session](user-guide.md#管理已保存的-session)。

`SIGIL_STATE_HOME` 与 `SIGIL_CACHE_HOME` 可覆盖对应 root。在 `sigil.toml` 中覆盖时，优先使用绝对路径。仓库内可复用资源固定放在 `.sigil/` 下；这些资源见[高级配置](advanced-configuration.md#memoryskills-与-agents)。

## Setup 出问题时使用 Doctor

运行：

```bash
sigil doctor
```

在 TUI 内使用 `/doctor` 可看到同一份报告。它检查配置加载、workspace 解析、session 位置、provider 与凭据来源、已配置 MCP server、代码智能 readiness 和终端兼容性。它绝不打印 secret 值，并为 warning 与 error 提供修复建议。

使用非默认配置启动时，也要带上同一个覆盖：

```bash
sigil --config ./sigil.toml doctor
```

## 下一步

- 在 [Provider 指南](providers.md)中选择 model service。
- 在[权限与沙箱](permissions-and-sandbox.md)中选择安全的编辑与网络策略。
- 在[外观](appearance.md)中自定义 TUI。
- 在[高级配置](advanced-configuration.md)中设置 task、检查、MCP 或终端行为。
- 在[配置字段参考](configuration-reference.md)中查询字段。
