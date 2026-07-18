<!-- public-doc-role: configuration; authority: configuration-router; sections: choose-the-right-page,resolution-order,minimal-path,workspace,storage-and-session-paths,use-doctor-when-setup-looks-wrong; cta: open-configuration-reference -->

# Sigil 配置指南

[文档首页](README.md) · [权限](permissions-and-sandbox.md) · [外观](appearance.md) · [高级配置](advanced-configuration.md) · [字段参考](configuration-reference.md) · [English](../en/configuration.md)

常规配置从这里开始。模型服务凭据和各服务的专用设置统一在[模型服务指南](providers.md)维护。

## 选择正确的页面

| 目标 | 页面 |
| --- | --- |
| 找到配置、选择工作区或设置存储位置 | 本指南 |
| 修改审批、网络、外部路径或沙箱 | [权限与沙箱](permissions-and-sandbox.md) |
| 修改主题、代码配色或信息栏 | [外观](appearance.md) |
| 配置任务、验证、记忆、子智能体、上下文、终端、插件或 MCP | [高级配置](advanced-configuration.md) |
| 查询精确字段或默认值 | [配置字段参考](configuration-reference.md) |

## 解析顺序

提供 `--config <path>` 时，Sigil 加载该文件；否则使用用户配置：

```text
~/.sigil/sigil.toml
```

快速设置会写入用户配置。工作区中的 `sigil.toml` 不会自动加载；只有明确需要时，才通过参数指定该文件。

## 最小配置路径

进入仓库并运行 `sigil`。快速设置会处理工作区、模型服务、具体模型与认证。最小的手写配置如下：

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[appearance]
info_rail = true
theme = "sigil_dark"
```

然后从所选模型服务的页面加入对应配置区块。可以直接复制的起点位于 [`docs/examples/config`](../examples/config)。

## 工作区

`workspace.root = "."` 表示使用 `sigil` 的启动目录。文件工具会留在这个工作区内；只有明确配置了范围足够小的外部目录规则时才例外。修改前请阅读[权限与沙箱](permissions-and-sandbox.md)。

Shell 选择和终端行为见[终端兼容性](terminal-compatibility.md)；可移植读写优先使用文件工具。

## 存储与会话路径

`[storage].state_root` 存放用户会话和变更记录；`[storage].cache_root` 存放可以重建的数据。`SIGIL_STATE_HOME` 和 `SIGIL_CACHE_HOME` 会覆盖对应的根目录。`[session].log_dir` 只改变当前工作区的会话日志位置。

保留期限只会在 `/config` → **Storage** 中经过预览和确认后应用。普通启动、恢复、运行和 `sigil serve` 不会自动删除会话。见[管理已保存的会话](user-guide.md#管理已保存的会话)。

## 设置异常时使用 Doctor

运行 `sigil doctor`，或在 TUI 中运行 `/doctor`。它会检查配置、工作区、会话位置、模型服务凭据来源、MCP、代码智能和终端支持，但不会打印密钥内容。使用备用配置时，请带上相同的 `--config <path>` 参数。

接下来按需要进入[权限](permissions-and-sandbox.md)、[外观](appearance.md)、[高级配置](advanced-configuration.md)或[字段参考](configuration-reference.md)。

<!-- public-doc-cta: open-configuration-reference -->
下一步：[查找精确配置字段](configuration-reference.md)。
