<!-- public-doc-role: mcp; authority: mcp-setup-and-use-authority; sections: minimal-config,streamable-http,process-environment-and-credentials,startup-and-refresh,compatibility-and-limits,trust-and-identity,roots-resources-prompts-and-input,troubleshooting; cta: open-troubleshooting -->

# MCP 指南

[文档首页](README.md) · [配置](configuration.md) · [隐私](privacy.md) · [故障排查](troubleshooting.md) · [English](../en/mcp.md)

Sigil 可以连接本机 stdio 和用户根 Streamable HTTP MCP server。先配置一个 server，保持 `approval_default = "ask"`，运行 `/doctor`，并确认它能读取、修改或传输什么。

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

尽量使用绝对 command。暴露的工具名类似 `mcp__filesystem__read_file`；冲突名称会获得稳定 suffix。

## Streamable HTTP

远端 MCP 只允许写在用户根配置中。优先使用 HTTPS；plain HTTP 不能携带环境变量 header、bearer 或 OAuth 凭据，并且只适合受信任的本机环境。

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

静态 bearer token 使用 `bearer_token_env_var`。Sigil 会检查每个目标、不自动跟随 redirect、限制 response size，并显示安全 origin 与凭据来源名称，但不展示值。

### OAuth 认证

OAuth 与静态 Authorization 或 bearer 凭据二选一：

```toml
[mcp_servers.oauth]
# client_id = "sigil-public-client" # 支持动态注册时可省略
scopes = ["mcp:tools"]
```

OAuth 要求 HTTPS，并且只有明确选择 **登录** 后才开始。Eager 或 headless 启动只报告 `authentication required`，不会自动打开浏览器。在 `/config` 中选择 server 并打开 **Authentication**。浮层可以登录、打开或复制授权 URL、在浏览器无法自动返回时粘贴完整 callback URL、刷新、远端 revoke，或显式只清除本机凭据。自动 callback 只监听 IPv4 loopback interface 的随机端口。Callback 文本只在当前操作中保留。Token 存在系统原生 credential store，而不是 TOML；没有明文文件 fallback。

OAuth 可能访问不同的 HTTPS authorization endpoint，并遵守已配置的 Network 控制。Redirect 与自动 retry 均关闭；`401` 会把认证标为 stale，但不会重放请求。

远端退出会尝试 revoke，且不会隐式删除本机凭据。Revoke 失败时，浮层会报告错误并保留凭据；此时可以重试，也可以显式选择**只清除本机**，后者不声称远端 token 已撤销。Revoke 成功，或 server 没有声明 revoke endpoint 时，浮层进入“远端已处理、本机仍保留”状态，仍由你选择清除或继续保留本机凭据。

## 进程环境与凭据

本机 stdio server 从小型运行环境启动，不会继承 Sigil 的完整 parent environment。只在用户根配置中授予必要变量名：

```toml
inherit_env = ["MY_MCP_API_KEY"]
```

Server 启动时，每个变量都必须存在。Provider key、cloud credential、proxy 设置和其他敏感变量不会自动继承。`inherit_env` 控制进程启动；`allow_secrets` 单独控制之后 MCP 调用中的 secret-like 数据。

## 启动与刷新

- `startup = "eager"` 在启动时连接并注册工具。
- `startup = "lazy"` 等待从 `/config` 或获准 activation tool call 启动。
- `required = true` 会让 strict/headless setup 在启动失败时失败；TUI 中的可选 server 失败不会阻止 built-in tool。

TUI 显示 deferred、authentication required、activating、ready、stale 或 failed。修复后使用 `/config` → MCP → **activate** 启动或刷新。OAuth server 会打开 **Authentication**，不会把未认证的零工具连接假装成 ready。

## 兼容性与限制

Stdio server 必须使用 MCP `2025-06-18` 的 newline-delimited JSON；不支持 `Content-Length` framing。启动与调用都有有限 timeout。输入超限、无效或超时会关闭该连接并报告失败。Tool、resource 与 prompt 结果会在提供给 model 前脱敏和缩短。

## Trust 与 Identity

`trust_class` 记录 server 是 official、self-hosted 还是 third-party。`approval_default` 控制常规询问行为。除非可信 server 确实需要敏感数据，否则保持 `allow_secrets = false`。

`pin_version = true` 可以绑定预期 command 和 server 报告的 identity。Pin 缺失或过期会阻止启动。Pinning 有助于发现意外变化，但不能防止同一用户权限下的另一个进程在启动期间替换 executable。

## Roots、Resources、Prompts 与输入

Sigil 只通过 `roots/list` 暴露活动 workspace。Resource 与 prompt 只能通过显式 MCP tool 列出或读取，不会自动注入。Elicitation form 会在 TUI 中显示 server、请求字段和默认值。需要交互输入时，headless use 会报告不支持。Progress update 会刷新 live panel，不会重复刷入 transcript。

## 故障排查

- **Lazy tool 不可见：** 从 `/config` 激活 server。
- **启动失败：** 检查 command path、args、required variable、timeout 和 pin。
- **需要认证：** 打开 **Authentication**；确认 HTTPS、scope 与系统 credential store 可用。
- **Callback 被拒绝：** 粘贴完整 callback URL；取消或重新开始登录后，不要复用旧 tab 或旧 callback。
- **Credential store 不可用：** 解锁或启用平台原生 credential store；Sigil 不会回退到文件。
- **目标被拒绝或预算耗尽：** 检查 Network disclosure 与 Web policy；修正目标或限制后再重试。
- **Secret 被阻止：** 除非理解 server 为什么需要该数据，否则保持阻止。
- **Server stale：** 配置或能力变化后刷新。

症状路径见[故障排查](troubleshooting.md#mcp-server-缺失失败或-deferred)，字段见[配置字段参考](configuration-reference.md#代码智能terminalplugins-与-mcp)。

<!-- public-doc-cta: open-troubleshooting -->
下一步：[使用 MCP 排障路径](troubleshooting.md)。
