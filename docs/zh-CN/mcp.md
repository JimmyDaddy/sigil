# Sigil MCP 接入指南

[English](../en/mcp.md)

Sigil 可以通过 stdio MCP server 接入外部工具。接入后的 MCP tools、resources 和 prompts 会进入同一个 tool registry，由同一套审批、activity、session control 和 secret egress 规则处理。

## 最小配置

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup_timeout_secs = 5
required = true
startup = "eager"

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

远端工具会被清洗成 provider-visible 名称，例如：

```text
mcp__filesystem__read_file
```

名称冲突或超长时会使用稳定 hash 后缀。

## 启动模式

`startup` 支持：

- `eager`：启动时立即启动 server、查询 tools 并注册。
- `lazy`：启动时只记录配置，不启动、不注册伪工具。

`required` 控制失败语义：

- `required = true`：server 启动或 `tools/list` 失败会让严格 registry 构建失败。
- `required = false`：eager server 失败时可以跳过并记录 warning。

在 TUI 中，eager MCP server 会在核心 agent worker 启动后后台激活。某个 MCP server 变慢、缺失或超时不会阻断普通聊天和 `/plan`，这些任务会继续使用内置工具和 code-intelligence tools；失败的 MCP server 会显示为 `failed`，直到修复或刷新。

Lazy server 可以通过 TUI `/config` 的 MCP section 手动 `activate`。模型也可以调用 `mcp_activate_server` 按需启动指定 lazy server。启动成功后，真实 MCP tools 会加入当前 agent registry。

TUI 会展示生命周期状态：

- `deferred`
- `activating`
- `refreshing`
- `stale <capability>`
- `ready`
- `failed`

## Trust Policy

```toml
[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false
```

字段含义：

- `trust_class`：server 信任等级，可选 `official`、`self_hosted`、`third_party`。
- `approval_default`：该 server 工具的默认审批模式，仍可被显式 tool/rule override 覆盖。
- `egress_logging`：审批通过后、执行前，把 server、trust class、remote tool 和参数形状写入 append-only control state。
- `allow_secrets`：为 `false` 时，MCP tool/resource/prompt 参数、`roots/list` payload 或 elicitation response 中包含已解析 secret 或 secret-like 字段会被阻断。
- `pin_version`：为 `true` 时，启动会校验 pinned identity。

MCP tool 的 permission subjects 会包含 `mcp_trust_class:<class>`，可以被 permission rule 匹配。

## Pinned Identity

启用 `pin_version` 时，需要提供 expected identity：

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup = "eager"

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = true

[mcp_servers.trust.pinned]
command_fingerprint = "sha256:..."
protocol_version = "2025-06-18"
server_name = "filesystem"
server_version = "1.0.0"
```

缺少 pinned identity 时，启动会失败并输出 observed pin，供你写回配置。

## Roots

Sigil 只把已解析的 workspace root 暴露给 MCP server 的 `roots/list`。不要依赖配置文件所在目录推断 workspace。

如果 `allow_secrets = false`，`roots/list` payload 中包含已解析 secret 或 secret-like 内容时会被阻断。

## Resources

当 server 在 `initialize` 中声明 MCP `resources` capability 时，Sigil 会注册两个只读 provider-visible tools：

```text
mcp__<server>__resources_list
mcp__<server>__resources_read
```

`resources_list` 调用 MCP `resources/list`，可选参数是用于分页的 `cursor` 字符串。

`resources_read` 调用 MCP `resources/read`，必填参数是 `resources_list` 返回的 `uri` 字符串。

Resource tools 复用同一套 MCP trust policy：

- permission subjects 包含 `mcp_trust_class:<class>`；
- `approval_default` 参与逐调用审批；
- `egress_logging = true` 时只记录安全的参数形状摘要；
- `allow_secrets = false` 时，secret-like resource 参数离开 Sigil 前会被阻断；
- 返回的 resource content 会先在本地脱敏，再展示给模型。

Sigil 不会把 MCP resources 自动注入 system prompt。模型必须通过这些工具显式 list/read resources。

## Prompts

当 server 在 `initialize` 中声明 MCP `prompts` capability 时，Sigil 会注册两个只读 provider-visible tools：

```text
mcp__<server>__prompts_list
mcp__<server>__prompts_get
```

`prompts_list` 调用 MCP `prompts/list`，可选参数是用于分页的 `cursor` 字符串。

`prompts_get` 调用 MCP `prompts/get`，必填参数是 `prompts_list` 返回的 `name`，可选参数是 `arguments` object。

Prompt tools 复用同一套 MCP trust policy、审批默认值、egress logging 和 `allow_secrets = false` 阻断。Sigil 不会把 MCP prompts 自动注入 system prompt；模型必须通过这些工具显式 list/get prompts。

## 输出限额

MCP tool、resource 和 prompt 的返回内容会先在本地脱敏，再进入 model-visible 输出限额。超大输出会被截断，并带上 `truncated`、`limit_bytes`、`limit_lines`、`returned_bytes` 等 metadata，以及 MCP server、远端 tool/surface、trust class、operation 和 observed server identity。

## Elicitation

TUI runtime 会声明并处理 `elicitation/create`。当 MCP server 请求用户输入时，Sigil 会通过 modal 展示 server、请求字段和默认值。

用户动作会映射为：

- accept：只发送 modal 中确认过的 flat primitive object 字段。
- decline：返回 `decline`。
- cancel：返回 `cancel`。

TUI elicitation 决策会写入 append-only control state。审计记录只包含 server、请求 message/schema hash、字段名和 action，不保存用户输入值。

非 TUI 默认 runtime 会明确返回 unsupported，不挂起也不伪造用户输入。

## Progress Notifications

`notifications/progress` 会更新 TUI live panel，不会反复写 timeline。`notifications/tools/list_changed`、`notifications/resources/list_changed` 和 `notifications/prompts/list_changed` 会把 server 标记为 stale，并在 worker 下一个空闲边界安全刷新。

## 常见问题

### 配置了 lazy server 但工具不可见

这是预期行为。`startup = "lazy"` 时启动阶段不会注册伪工具，需要在 TUI `/config` 中 activate，或让模型调用 `mcp_activate_server`。

### Server 启动失败

先确认：

- `command` 是否在 PATH 上，或是否使用了绝对路径。
- `args` 中的 server 路径是否存在。
- 严格/headless registry 构建中，`required` 是否需要设为 `false`，避免可选 server 阻塞主流程。
- `pin_version = true` 时 pinned identity 是否和 observed pin 一致。

在 TUI 中，这不应该停止普通任务。失败 server 会显示为 `failed`，内置工具仍然可用。

### Secret 被阻断

当 `allow_secrets = false` 时，Sigil 会阻断识别到的 secret egress。这是安全策略生效，不是 MCP server 调用失败。确认确实需要发送 secret 后，再显式调整该 server 的 trust policy。
