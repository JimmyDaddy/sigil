<!-- public-doc-role: mcp; authority: mcp-setup-and-use-authority; sections: minimal-config,streamable-http,process-environment-and-credentials,startup-and-refresh,compatibility-and-limits,trust-and-identity,roots-resources-prompts-and-input,troubleshooting; cta: open-troubleshooting -->

# MCP 指南

[文档首页](README.md) · [配置](configuration.md) · [隐私](privacy.md) · [故障排查](troubleshooting.md) · [English](../en/mcp.md)

Sigil 可以连接本机 stdio 服务和用户级的 Streamable HTTP MCP 服务。建议先只配置一个服务，保持 `approval_default = "ask"`，运行 `/doctor`，并确认它能够读取、修改或传输哪些数据。

## 最小配置

```toml
[[mcp_servers]]
name = "filesystem"
transport = "stdio"
command = "node"
args = ["/absolute/path/to/server.js"]
startup = "eager"
required = true

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

命令路径尽量使用绝对路径。对模型暴露的工具名类似 `mcp__filesystem__read_file`；名称冲突时，Sigil 会添加稳定的后缀。

## Streamable HTTP

远端 MCP 只能写在用户级配置中。请优先使用 HTTPS；普通 HTTP 不能携带来自环境变量的请求头、Bearer 令牌或 OAuth 凭据，而且只适合受信任的本机环境。

```toml
[[mcp_servers]]
name = "my-search"
transport = "streamable_http"
url = "https://mcp.example.com/mcp"
startup = "lazy"
env_http_headers = { "X-API-Key" = "MY_SEARCH_API_KEY" }
client_capabilities = ["roots", "elicitation"]

[mcp_servers.trust]
trust_class = "third_party"
approval_default = "ask"
allow_secrets = false
```

静态 Bearer 令牌通过 `bearer_token_env_var` 设置。Sigil 会检查每个目标，不会自动跟随重定向，并会限制响应大小。界面只显示经过处理的来源和凭据来源名称，不会展示凭据内容。

### OAuth 认证

OAuth 与静态 Authorization 或 bearer 凭据二选一：

```toml
[mcp_servers.oauth]
# client_id = "sigil-public-client" # 支持动态注册时可省略
scopes = ["mcp:tools"]
```

OAuth 要求 HTTPS，而且只有你明确选择**登录**后才会开始。立即启动或非交互启动只会报告 `authentication required`，不会自动打开浏览器。在 `/config` 中选择服务并打开 **Authentication**。弹窗可以开始登录、打开或复制授权 URL、在浏览器无法自动返回时粘贴完整回调 URL、刷新凭据、在远端撤销授权，或明确选择只清除本机凭据。自动回调只监听 IPv4 回环地址上的随机端口。回调文本只在当前操作期间保留。令牌存放在系统原生凭据存储中，而不是 TOML 文件里；不会回退到明文文件。

OAuth 可能访问不同的 HTTPS 授权端点，并遵守已配置的网络控制。重定向和自动重试均关闭；收到 `401` 后，认证会被标记为已过期，但请求不会自动重放。

退出登录时，Sigil 会尝试在远端撤销授权，但不会顺手删除本机凭据。撤销失败时，弹窗会显示错误并保留凭据；你可以重试，也可以明确选择**只清除本机**。后者并不表示远端令牌已经撤销。远端撤销成功，或服务端没有提供撤销端点时，弹窗会进入“远端已处理、本机仍保留”状态，仍由你决定清除还是继续保留本机凭据。

## 进程环境与凭据

本机 stdio 服务从精简的运行环境启动，不会继承 Sigil 的完整父进程环境。只在用户级配置中授予确实需要的变量名：

```toml
inherit_env = ["MY_MCP_API_KEY"]
```

服务启动时，每个变量都必须存在。模型服务密钥、云凭据、代理设置和其他敏感变量不会自动继承。`inherit_env` 控制进程启动时的环境；`allow_secrets` 则单独控制后续 MCP 调用中疑似敏感的数据。

## 启动与刷新

- `startup = "eager"` 在启动时连接并注册工具。
- `startup = "lazy"` 会等待你从 `/config` 启动，或等待一次获准的激活工具调用。
- `required = true` 会让严格或非交互设置在启动失败时一并失败；TUI 中的可选服务启动失败，不会阻止内置工具继续使用。

TUI 会显示尚未激活、需要认证、正在激活、就绪、已过期或失败等状态。修复后，使用 `/config` → MCP → **activate** 启动或刷新。需要 OAuth 的服务会打开 **Authentication**，不会把尚未认证、没有工具可用的连接显示成就绪。

## 兼容性与限制

Stdio 服务必须使用 MCP `2025-06-18` 规定的逐行 JSON；不支持 `Content-Length` 帧格式。启动与调用都有明确的超时。输入超限、格式无效或超时都会关闭连接并报告失败。工具、资源和提示词的结果在交给模型前会先脱敏并限制长度。

## 信任与身份

`trust_class` 记录服务属于官方、自托管还是第三方。`approval_default` 控制常规询问行为。除非可信服务确实需要敏感数据，否则请保持 `allow_secrets = false`。

`pin_version = true` 可以绑定预期命令和服务端报告的身份。固定信息缺失或过期时，启动会被阻止。固定版本有助于发现意外变化，但无法阻止同一用户权限下的另一个进程在启动期间替换可执行文件。

## 根目录、资源、提示词与输入

Sigil 只通过 `roots/list` 暴露当前工作区。资源和提示词只能通过显式 MCP 工具列出或读取，不会自动注入上下文。补充信息表单会在 TUI 中显示服务端名称、请求字段和默认值。需要交互输入时，非交互模式会明确报告不支持。进度更新只会刷新活动面板，不会反复写入会话记录。

## 故障排查

- **延迟启动的工具不可见：** 从 `/config` 激活服务。
- **启动失败：** 检查命令路径、参数、必需环境变量、超时和固定版本信息。
- **需要认证：** 打开 **Authentication**；确认 HTTPS、权限范围与系统凭据存储可用。
- **回调被拒绝：** 粘贴完整回调 URL；取消或重新开始登录后，不要复用旧标签页或旧回调。
- **凭据存储不可用：** 解锁或启用平台原生凭据存储；Sigil 不会回退到文件。
- **目标被拒绝或预算耗尽：** 检查网络披露信息与 Web 策略；修正目标或限制后再重试。
- **敏感数据被阻止：** 除非理解服务为什么需要该数据，否则保持阻止。
- **服务状态已过期：** 配置或能力变化后刷新。

症状路径见[故障排查](troubleshooting.md#mcp-服务缺失失败或尚未激活)，字段见[配置字段参考](configuration-reference.md#代码智能终端插件与-mcp)。

<!-- public-doc-cta: open-troubleshooting -->
下一步：[使用 MCP 排障路径](troubleshooting.md)。
