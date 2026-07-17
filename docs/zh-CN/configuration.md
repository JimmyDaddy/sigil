<!-- public-doc-role: configuration; authority: configuration-router; sections: choose-the-right-page,resolution-order,minimal-path,workspace,storage-and-session-paths,use-doctor-when-setup-looks-wrong; cta: open-configuration-reference -->

# Sigil 配置指南

[文档首页](README.md) · [权限](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [English](../en/configuration.md)

常规配置从这里开始。Provider 凭据和服务专项设置只在 [Provider 指南](providers.md)维护。

## 选择正确的页面

| 目标 | 页面 |
| --- | --- |
| 找到配置、选择 workspace 或设置存储 | 本指南 |
| 修改审批、网络、外部路径或沙箱 | [权限与沙箱](permissions-and-sandbox.md) |
| 修改主题、代码配色或信息栏 | [外观](appearance.md) |
| 配置任务、检查、memory、agent、上下文、terminal、plugin 或 MCP | [高级配置](advanced-configuration.md) |
| 查询精确字段或默认值 | [配置字段参考](configuration-reference.md) |

## 解析顺序

提供 `--config <path>` 时，Sigil 加载该文件；否则使用用户配置：

```text
~/.sigil/sigil.toml
```

Quick Setup 写入用户配置。Workspace 中的 `sigil.toml` 不会自动加载；只有明确需要时才通过参数指定。

## 最小配置路径

进入仓库并运行 `sigil`。Quick Setup 会处理 workspace、provider、model 与认证。最小手写基础配置为：

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[appearance]
info_rail = true
theme = "sigil_dark"
```

再从所选 provider 页面加入一个 provider block。可复制起点在 [`docs/examples/config`](../examples/config)。

## Workspace

`workspace.root = "."` 跟随 `sigil` 的启动目录。文件工具会留在该 workspace 内；只有显式开启窄范围 external-directory rule 时才例外。修改前请阅读[权限与沙箱](permissions-and-sandbox.md)。

Shell 选择和终端行为见[终端兼容性](terminal-compatibility.md)；可移植读写优先使用文件工具。

## 存储与 Session 路径

`[storage].state_root` 存放用户态 session 与 artifact；`[storage].cache_root` 存放可重建数据。`SIGIL_STATE_HOME` 和 `SIGIL_CACHE_HOME` 会覆盖对应 root。`[session].log_dir` 只改变当前 workspace 的 session 日志位置。

Retention limit 只通过 `/config` → **Storage** 下的显式预览与确认应用。普通启动、恢复、运行和 `sigil serve` 不会自动删除 session。见[管理已保存的 Session](user-guide.md#管理已保存的-session)。

## Setup 异常时使用 Doctor

运行 `sigil doctor`，或在 TUI 中运行 `/doctor`。它会检查配置、workspace、session 位置、provider 凭据来源、MCP、code intelligence 和终端支持，但不会打印 secret value。使用备用配置时，请带上同一个 `--config <path>` 参数。

接下来按需要进入[权限](permissions-and-sandbox.md)、[外观](appearance.md)、[高级配置](advanced-configuration.md)或[字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
