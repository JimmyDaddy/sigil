# MCP 配置摘要与工具目录

本页记录 B4 的共享 runtime 工具契约。它补充[核心架构的 MCP 集成](sigil-rust-agent-core-technical-solution.md#11-mcp-插件模型)，Desktop、TUI、自动化 adapter 复用同一 registry，不新增独立权限开关。

当当前 composition 启用 MCP 且存在用户根 `mcp_servers` 配置时，runtime 注册只读 `mcp_catalog`。该工具没有网络或进程启动效果；已注册工具仍通过既有直接调用路径使用，不要求先访问目录，也不默认隐藏完整 schema。

## 配置与操作

`mcp_servers.description` 是可省略的用户说明，缺省为空；stdio 和 Streamable HTTP 均可设置。说明用于发现，不是授权、可信指令或启动条件。示例见 [mcp-safe-defaults.toml](../../docs/examples/config/mcp-safe-defaults.toml)。目录只展示 server name、说明、transport、startup、trust class、当前作用域可见工具数量与 activation 是否可用；不展示 command、args、URL、环境变量和凭据。已配置 server 摘要不是工具可调用性承诺。

`mcp_catalog` 的 typed `operation` 有三种：

- `list_servers`：分页列出配置摘要，既不 initialize，也不探测或启动服务。
- `list_tools`：分页列出当前调用 registry 的 allow/deny scope 内已注册 MCP 工具；可按 exact `server_name` 收窄。每条含说明、工具名和 registration revision。
- `describe_tool`：用目录返回的 exact `tool_name` 和 `revision` 读取完整 `ToolSpec`。缺少、隐藏、已撤销、或注册被替换的工具返回结构化错误；不能靠猜名字获取隐藏 schema。

列表默认每页 20 条，可请求 1–50 条；整个结果最多 24 KiB，说明最多 512 UTF-8 bytes，并保留 `description_truncated`、`next_cursor`、total 与 `ToolResultMeta`。说明先脱敏再裁剪。完整 schema 不裁剪或改写；超出目录输出边界则明确返回 `resource_limit`，已经存在的直接工具契约继续可用。

## 权限与 generation

目录通过 `ToolContext` 中执行入口注入的只读 registry view 获取工具。view 延续该次调用的可见性 scope，不保存注册时的全局 registry，也不提供注册或执行能力。没有实际执行绑定的 context 无法读取目录。

revision 同时绑定独立注册 generation 与完整 spec。同名、同 spec 重新注册也会使旧 revision 失效；它只用于检测详情是否过期，不是 invocation grant。真正调用仍经过现有 registry resolve、权限和审批、lifecycle generation、执行资源校验。`mcp_activate_server` 的启动与 approval 边界保持原有契约。

当前目录只覆盖常规 startup 实际消费的用户根 MCP 配置及可见注册工具。PluginManifest 的解析能力与产品自动激活接线仍按既有 RFC 分开，不能由目录暗中启动未接线扩展。

## 按需 schema 的采用边界

生产继续向模型提供原有可见工具规格。目录是可选发现能力，少量工具不会增加必经模型轮次。是否在大量工具场景改为先目录再提供选中 schema，必须以同工具集、同任务对照验证；不能只依据单次规格 bytes 减少就宣称总 token、成本或耗时改善。

结构消融使用真实 stdio MCP、实际 registry 与目录输出，记录全量规格、摘要、选中规格、累计输入 token 和启动数量；脚本选择的 schema 相等性不是模型选择准确率。在线 spec-only 对照则分别记录服务端 usage、实际模型轮次、选择及参数准确率和耗时；不执行所选业务工具，也不代表完整 agent 任务成功率。离线测试沿用[测试隔离入口](test-isolation.md)，真实 provider 对照只使用显式授权的独立环境。
