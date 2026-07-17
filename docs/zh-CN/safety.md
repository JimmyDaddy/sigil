<!-- public-doc-role: safety; authority: risk-model-authority; sections: risk-model,review-an-approval,hard-limits-to-remember; cta: configure-permissions -->

# 安全

[文档首页](README.md) · [权限与沙箱](permissions-and-sandbox.md) · [隐私](privacy.md) · [故障排查](troubleshooting.md) · [English](../en/safety.md)

Sigil 的安全方式是一套决策过程：理解建议动作、检查相关预览，并且只授予当前任务需要的访问。

## 风险模型

<!-- public-doc-topic: approval-risk-model -->

仓库读取通常风险较低。写入、删除、命令、外部路径、网络访问、MCP 调用、language tool 代码编辑和包含 secret 的请求需要更仔细检查。配置决定动作是运行、询问还是拒绝；动作获准不代表结果一定正确。

## 检查审批

允许动作前确认：

1. 目标与请求一致。
2. 文件、命令、server 或目标符合预期。
3. Diff 或请求预览足够窄。
4. 单次决定是否已经足够；只有确实需要重复访问时才使用 session grant。
5. 你知道如何验证结果。

预览异常或范围过宽时，选择拒绝并重新说明范围。

## 必须记住的硬限制

- Headless run 不能交互询问；未解决的审批会失败。
- Permission 不是 sandbox。默认 local command strategy 不提供 OS 隔离。
- External-directory、网络和 sandbox 行为必须分别配置，任何一项都不是全面保证。
- 文件恢复不会撤销 shell command、远端服务、MCP 效果或其他外部变更。
- 中断工具在恢复后仍显示为中断，不会静默重跑。
- `sigil serve` 只面向受信任本机 client：监听 loopback，特权 route 需要认证。
- 通过 Quick Setup 或 `/config` 保存凭据，会写入明文本机配置。

控制项见[权限与沙箱](permissions-and-sandbox.md)，数据与凭据见[隐私](privacy.md)，外部 server trust 见 [MCP](mcp.md)，本地服务细节见[参考](reference.md)。

<!-- public-doc-cta: configure-permissions -->
下一步：[配置权限与沙箱限制](permissions-and-sandbox.md)。
