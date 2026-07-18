<!-- public-doc-role: safety; authority: risk-model-authority; sections: risk-model,review-an-approval,hard-limits-to-remember; cta: configure-permissions -->

# 安全

[文档首页](README.md) · [权限与沙箱](permissions-and-sandbox.md) · [隐私](privacy.md) · [故障排查](troubleshooting.md) · [English](../en/safety.md)

Sigil 把安全落实为一套清晰的决策过程：先理解准备执行的操作，再检查相关预览，并且只授予当前任务真正需要的权限。

## 风险模型

<!-- public-doc-topic: approval-risk-model -->

读取仓库通常风险较低。写入、删除、执行命令、访问外部路径或网络、调用 MCP、使用语言工具编辑代码，以及发送可能包含密钥的请求，都需要更仔细地检查。配置决定操作会直接运行、询问用户还是被拒绝；操作获得允许，并不代表结果一定正确。

## 检查审批

允许动作前确认：

1. 目标与请求一致。
2. 文件、命令、服务端或网络目标符合预期。
3. 文件差异或请求预览范围足够小。
4. 单次允许是否已经足够；只有确实需要重复访问时，才在整个会话中授予权限。
5. 你知道如何验证结果。

预览异常或范围过宽时，选择拒绝并重新说明范围。

## 必须记住的硬限制

- 非交互式 `run` 无法向用户发起询问；仍需审批的操作会失败。
- 权限策略不等于沙箱。默认的本机命令执行方式不提供操作系统级隔离。
- 外部目录、网络和沙箱需要分别配置，任何一项都不能单独提供全面保护。
- 文件恢复不会撤销 Shell 命令、远端服务、MCP 调用或其他外部变更。
- 中断工具在恢复后仍显示为中断，不会静默重跑。
- `sigil serve` 只面向受信任的本机客户端：服务仅监听回环地址，特权路由需要认证。
- 通过快速设置或 `/config` 保存凭据，会写入明文本机配置。

控制项见[权限与沙箱](permissions-and-sandbox.md)，数据与凭据见[隐私](privacy.md)，外部服务的信任设置见 [MCP 指南](mcp.md)，本地服务细节见[参考](reference.md)。

<!-- public-doc-cta: configure-permissions -->
下一步：[配置权限与沙箱限制](permissions-and-sandbox.md)。
