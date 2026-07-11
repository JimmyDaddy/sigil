# Sigil MCP 接入指南

[文档首页](README.md) · [配置](configuration.md) · [排障](troubleshooting.md) · [English](../en/mcp.md)

Sigil 可以通过 stdio MCP server 接入外部工具。接入后的 MCP tools、resources 和 prompts 会进入同一个 tool registry，由同一套审批、activity、session control 和 secret egress 规则处理。

建议从保守配置开始：先接入一个 server，保持 `approval_default = "ask"`，运行 `/doctor`，只有在理解该 server 能读取或修改什么之后，再放宽 trust settings。

## 最小配置

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup_timeout_secs = 5
required = true
startup = "eager"
# 只添加这个 server 真正需要的父进程变量。
# inherit_env = ["MY_MCP_API_KEY"]

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

## 进程环境与凭证

本地 stdio MCP 进程不会继承 Sigil 的完整环境。Sigil 会在 spawn 前清空父进程环境，只加入 `PATH`、locale、临时目录和必要 Windows system variables 等固定最小运行基线，然后仅注入用户根配置中显式列出的变量名：

```toml
[[mcp_servers]]
name = "credentialed-search"
command = "/absolute/path/to/search-mcp"
args = ["--stdio"]
inherit_env = ["MY_MCP_API_KEY"]
startup = "lazy"

[mcp_servers.trust]
approval_default = "ask"
allow_secrets = false
```

`inherit_env` 元素必须匹配 `[A-Za-z_][A-Za-z0-9_]*`；Sigil 会去重并排序。server 激活时，每个列出的变量都必须存在。缺失或非 UTF-8 值会在 spawn 前返回 `configuration_invalid`，不会让子进程拿到不完整的凭证集合。

`HOME`、`SSH_AUTH_SOCK`、proxy settings、provider keys 和 cloud credentials 等变量不会自动继承。若 executable 不在基线 `PATH` 中，建议给 `command` 使用绝对路径。

只有用户根 `[[mcp_servers]]` 可以使用 `inherit_env`。Plugin manifest 不能请求环境或凭证 grant；discovery 会以 `plugin_mcp_environment_grant_not_supported` 拒绝该字段。需要凭证的 plugin-declared server 应移到用户根配置。

Sigil 只保存和展示 grant name、source metadata 以及 static/live fingerprint 状态，绝不展示 resolved value。live fingerprint 使用进程随机 key，不能作为离线 secret verifier。grant value 变化或消失时，旧 MCP process binding 会失效，必须重启或 refresh server。

`inherit_env` 与 `allow_secrets` 是两个独立控制。前者只授权 child process 启动时注入变量；后者决定后续 MCP tool/resource/prompt payload 是否可以携带已识别 secret。启用任一项都不会隐式放宽另一项。

## 启动模式

`startup` 支持：

- `eager`：启动时立即启动 server、查询 tools 并注册。
- `lazy`：启动时只记录配置，不启动、不注册伪工具。

`required` 控制失败语义：

- `required = true`：server 启动或 `tools/list` 失败会让严格 registry 构建失败。
- `required = false`：eager server 失败时可以跳过并记录 warning。

在 TUI 中，eager MCP server 会在核心 agent worker 启动后后台激活。某个 MCP server 变慢、缺失或超时不会阻断普通聊天和 `/plan`，这些任务会继续使用内置工具和 code-intelligence tools；失败的 MCP server 会显示为 `failed`，直到修复或刷新。

Lazy server 可以通过 TUI `/config` 的 MCP section 手动 `activate`。`Server` 行与主题选择采用相同的循环交互：按 `Enter` 切换下一个用于查看 lifecycle 的 server，且不会修改配置。按 `Down` 进入 footer，选择 `activate` 后按 `Enter` 启动或刷新该 server。`PageUp/PageDown` 保留为循环切换查看对象的兼容别名。模型也可以调用 `mcp_activate_server` 按需启动指定 lazy server。启动成功后，真实 MCP tools 会加入当前 agent registry。

模型触发的 server activation 会分类为本地 `Execute` 加 `NetworkEffect::Unknown`，并经过完整的工具 permission decision。配置声明的 eager startup、直接 lifecycle activation 与 refresh 保持既有 lifecycle/source 语义，但必须把当前 run-scoped network policy 带到 spawn boundary：网络 `Ask` 缺少显式批准时不能静默启动，网络 `Deny` 下只有所选 backend 能同时证明 network isolation 与 process-tree isolation 时才允许启动。连接建立后，通用 MCP tool call 分类为本地 `Read` 加网络 `Unknown`；resource 和 prompt surface 分类为本地 `Read` 加网络 `Read`。这些本地 access 标签不代表数据不会离开机器。

TUI 会展示生命周期状态：

- `deferred`
- `activating`
- `refreshing`
- `stale <capability>`
- `ready`
- `failed`

## Stdio 兼容性与 deadline

Sigil 使用 MCP `2025-06-18` 定义的 newline-delimited JSON stdio transport。仍使用 LSP 风格 `Content-Length` header 的 MCP server 与之不兼容，需要升级或通过符合标准的 adapter 包装；Sigil 不会在已建立的 stream 上猜测或切换 framing。

启动过程使用一个覆盖 `initialize`、`initialized` 和首次 `tools/list` 的 absolute budget。每次 tool、resource 或 prompt 调用使用当前 tool timeout。timeout 为零时使用有限的 30 秒项目默认值，过大的配置值会被限制到 24 小时硬上限。timeout、非法 frame 或超限 frame 会永久关闭当前 client generation，尝试终止对应的 process group/tree，并回收直接 child；清理不完整时会明确报告，不能伪装成成功。Windows 上会在执行有界 `taskkill /T /F` 期间保持 stdio connection 打开，避免 server 因 stdin EOF 提前退出而抢在 process-tree cleanup 之前；若 teardown 开始前 leader 已消失，则明确报告 tree cleanup 无法确认。各类超限错误会以 resource-limit 类型返回适用 limit 和 observed lower bound 等结构化字段。

单个 NDJSON frame 上限为 4 MiB；一次 operation 最多消费 256 条 inbound message 和累计 8 MiB frame，MCP stderr hard limit 为 8 MiB。tool、resource 与 prompt content 会在 JSON escaping 前脱敏，并在进入 kernel model-content cap 前限制为 32 KiB 或 2,000 行。截断只通过结构化 metadata 报告，不生成可能重新引入已配置 secret carrier 的文本 marker。

可在 `/config` → MCP 中选择 `activate` 刷新 server。Registry ownership 使用未清洗的 exact server identity 加唯一 process-generation id，绝不以 provider-visible name prefix 作为所有权依据，因此 sanitize/hash collision 不会误回收其他 server。即使 server 为 optional，显式 activation/refresh 也必须得到可调用 replacement。只有 replacement 注册成功且所有不同的旧 generation 都已显式 shutdown 后 refresh 才会返回成功；replacement 启动失败时恢复旧 generation，旧 generation 清理失败时则移除并 shutdown replacement，保持 fail-closed。多 server registration 采用事务语义，同一操作中后续 server 失败时会回滚此前已启动的 generation；重复的 exact server name 会在 launch 前被拒绝。旧 generation 不会复用，因此延迟到达的 response 不会被下一次调用误收。

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
- `pin_version`：为 `true` 时，spawn 前先校验 command/args/environment-grant fingerprint，initialize 后再校验 protocol 与 server identity。对于带凭证的 server，pre-spawn fingerprint 还会绑定 canonical execution base，以及通过隔离 baseline `PATH` 解析出的 executable 文件字节。

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

缺少 pinned identity 或 command fingerprint 已过期时，启动会在 server 收到环境 grant 之前失败，并输出 pre-spawn command fingerprint。该 fingerprint 匹配后，Sigil 才会 initialize server 并校验其余 protocol/name/version 字段。没有 `inherit_env` 的既有 server 保持原 command fingerprint；新增或修改 grant name 会有意要求更新 pin。

对于带 `inherit_env` 的 server，原路径下 executable 被替换后，pre-spawn fingerprint 会变化。command args 只按原始文本进入 binding；Sigil 不解释参数，也不 attest 参数中引用的文件。尤其是 `command = "python3"` 且脚本路径位于 `args` 时，pin 覆盖 Python executable 与参数字符串，不覆盖脚本内容。带凭证的 server 优先使用独立 executable；若必须使用解释器，应另行 review 并保护脚本或 module。

该 fingerprint 检测的是 pre-spawn 校验时观察到的 executable 字节，不是针对同一用户下恶意 host process 的 attestation。Sigil 最终仍按路径启动 executable，因此另一个能够并发改写该文件的进程可能在校验与启动之间竞态替换。带凭证的 MCP executable 及其父目录必须位于不受不可信进程写入的范围；若要消除这一 host-level race，需要后续引入平台专属的 handle-bound execution primitive。

## Roots

Sigil 只把已解析的 workspace root 暴露给 MCP server 的 `roots/list`。不要依赖配置文件所在目录推断 workspace。

如果 `allow_secrets = false`，`roots/list` payload 中包含已解析 secret 或 secret-like 内容时会被阻断。

## Resources

当 server 在 `initialize` 中声明 MCP `resources` capability 时，Sigil 会注册两个本地 access 为 `Read`、网络 effect 为 `Read` 的 provider-visible tools：

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

当 server 在 `initialize` 中声明 MCP `prompts` capability 时，Sigil 会注册两个本地 access 为 `Read`、网络 effect 为 `Read` 的 provider-visible tools：

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
