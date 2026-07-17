<!-- public-doc-role: privacy; authority: data-and-credential-authority; sections: what-can-leave-your-machine,what-stays-local,api-keys,session-logs,mcp-and-web-data,doctor-and-feedback-output,before-sharing-logs-or-reports; cta: review-safety -->

# 隐私与数据处理

[文档首页](README.md) · [安全](safety.md) · [权限](permissions-and-sandbox.md) · [English](../en/privacy.md)

Sigil 在本机运行，但 model provider、Web route、MCP server 和获准命令可能接收数据。在敏感仓库中使用前，请检查这些目标。

## 什么可能离开本机

<!-- public-doc-topic: data-egress -->

根据配置与审批，外发数据可能包含 prompt、选中的对话上下文、`AGENTS.md` 或 `SIGIL.md` 等 workspace 指令、文件片段、搜索结果、诊断、命令输出、Web query、MCP input 和已接受的 elicitation response。启用的 workspace 指令会成为模型请求的一部分。Sigil 不会自行发布仓库，但获准的 provider、tool 或 command 可以传输你交给它的内容。

模型请求遵守对应 provider 的政策；外部工具遵守所选 Web 或 MCP 服务的政策。不需要的 route 应关闭，目标不明确时应拒绝动作。

## 什么默认留在本机

用户配置、输入历史、session 日志、变更 artifact、cache、workspace 指令文件的磁盘副本和 `/feedback` export 默认存放在本机；但其中的内容仍可能通过模型上下文或其他获准动作离开本机。Memory 启用时，workspace 指令会发送给已配置的 model provider。

## API Keys

<!-- public-doc-topic: credentials-plaintext -->

优先使用 [Provider 指南](providers.md#认证优先级)列出的 provider 专项环境变量。通过 Quick Setup 或 `/config` 保存的 key 会以明文写入用户态 `sigil.toml`；不要提交或分享真实配置。`sigil doctor` 会报告凭据来源，但不打印值。

为远端 MCP server 配置的 OAuth 凭据存放在系统原生 credential store，而不是 TOML。登录、退出与本机清除行为见 [MCP](mcp.md)。

## Session 日志

<!-- public-doc-topic: session-log-local -->

本机 session 日志可能包含 prompt、assistant 回复、工具摘要与预览、审批决定、中断活动、任务状态和上下文管理记录。即使源仓库公开，也应把日志视为敏感内容。分享前检查任何 export。

## MCP 与 Web 数据

MCP server 是外部工具提供方。除非 server 确实需要且目标可信，否则保持 secret access 关闭。Web search 会把 query 发送到选中的 provider-hosted、configured MCP 或 bundled route；网络服务可以观察 query 与连接 metadata。返回内容仍属于外部不可信输入。Route 与关闭方式见[权限与沙箱](permissions-and-sandbox.md#网络与-web-工具)。

## Doctor 与 Feedback 输出

Doctor 输出经过脱敏，但仍可能包含路径、provider label 和本机环境事实。`/feedback` 会先预览包含的类别，只写入一个本机 JSON 报告，不会自动上传。附加到 issue 前请检查 export JSON。

## 分享日志或报告前

删除凭据、私有路径、专有源码、敏感 prompt 或工具预览、内部 endpoint，以及与私密使用相关的 identifier。无法判断时，在不敏感的 workspace 中复现问题，再分享该输出。

<!-- public-doc-cta: review-safety -->
下一步：[查看安全指南](safety.md)。
