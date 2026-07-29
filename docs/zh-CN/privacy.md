<!-- public-doc-role: privacy; authority: data-and-credential-authority; sections: what-can-leave-your-machine,what-stays-local,api-keys,session-logs,mcp-and-web-data,doctor-and-feedback-output,before-sharing-logs-or-reports; cta: review-safety -->

# 隐私与数据处理

[文档首页](README.md) · [安全](safety.md) · [权限](permissions-and-sandbox.md) · [English](../en/privacy.md)

Sigil 在本机运行，但模型服务、Web 路由、MCP 服务端和获准执行的命令仍可能接收数据。在敏感仓库中使用前，请先确认数据会发送到哪里。

## 什么可能离开本机

<!-- public-doc-topic: data-egress -->

根据配置与审批，发往外部的数据可能包含提示词、选中的对话上下文、`AGENTS.md` 或 `SIGIL.md` 等工作区指令、文件片段、搜索结果、诊断信息、命令输出、Web 查询、MCP 输入，以及你在补充信息请求中提交的回答。启用的工作区指令会成为模型请求的一部分。Sigil 不会自行发布仓库，但获得允许的模型服务、工具或命令可以传输你交给它的内容。

模型请求受对应服务商的政策约束；外部工具则受所选 Web 或 MCP 服务的政策约束。不需要的路由应该关闭；无法确认目标时，应拒绝操作。

## 什么默认留在本机

用户配置、输入历史、会话日志、变更记录、缓存、工作区指令文件的磁盘副本，以及 `/feedback` 导出的报告，默认都存放在本机；但其中的内容仍可能通过模型上下文或其他获准操作离开本机。启用记忆功能时，工作区指令会发送给已经配置的模型服务。

## API 密钥

<!-- public-doc-topic: credentials-plaintext -->

可以使用[模型服务指南](providers.md#认证优先级)列出的专用环境变量，也可以通过快速设置或 `/config` 把 secret 保存到受保护凭据存储。这里并不是“密钥在任何地方都不落盘”。默认 `file` 与非交互 `auto` 都只使用 owner-only 的 `~/.sigil/credentials.json`；这个专属文件是受权限保护的明文，不是加密。`auto` 不会查询旧的原生系统记录。只有明确需要原生系统存储及其潜在认证界面时才选择严格 `keyring`。V2 配置只保存允许列表中的环境变量名或不透明 stored credential ID。Sigil 永远不会把新输入的 key 写入 `sigil.toml`、workspace、模型缓存、会话日志、快照、普通日志或诊断输出。旧 V1 文件可能已经含有明文；请在 `/config` 中检查并显式迁移，再删除仍含旧值的备份。`sigil doctor` 只报告凭据来源和就绪状态。

远端 MCP 服务的 OAuth 凭据存放在系统原生凭据存储中，而不是 TOML 文件里。登录、退出与清除本机凭据的行为见 [MCP 指南](mcp.md)。

## 会话日志

<!-- public-doc-topic: session-log-local -->

本机会话日志可能包含提示词、助手回复、工具摘要与预览、审批决定、中断的活动、任务状态和上下文管理记录。即使源仓库是公开的，也应该把日志视为敏感内容。分享任何导出文件前都要先检查。

## MCP 与 Web 数据

MCP 服务端是外部工具提供方。除非服务确实需要且目标可信，否则应保持敏感凭据访问关闭。Web 搜索会把查询发送到选中的模型服务、已配置的 MCP 服务或内置路由；网络服务可以看到查询内容和连接元数据。返回结果仍属于外部不可信输入。路由与关闭方式见[权限与沙箱](permissions-and-sandbox.md#网络与-web-工具)。

## Doctor 与支持报告输出

Doctor 输出经过脱敏，但仍可能包含路径、模型服务名称和本机环境信息。`/feedback` 会先预览报告中包含的类别，只在本机写入一个 JSON 文件，不会自动上传。把报告附加到问题单前，请先检查导出的 JSON。

## 分享日志或报告前

删除凭据、私有路径、专有源码、敏感提示词或工具预览、内部端点，以及能够关联私密使用场景的标识符。无法判断时，请在不敏感的工作区中复现问题，再分享相应输出。

<!-- public-doc-cta: review-safety -->
下一步：[查看安全指南](safety.md)。
