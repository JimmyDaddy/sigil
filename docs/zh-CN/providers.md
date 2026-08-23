<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# 模型服务指南

[文档首页](README.md) · [配置](configuration.md) · [English](../en/providers.md)

先在这里选择模型服务，再为实际使用的账户或端点创建命名 connection。每个 connection 独立拥有
provider 协议、端点、凭据来源和模型目录。保存默认值与运行中的会话都使用
`connection-id/model-id` 复合身份，因此切换 provider 时不会复用其他 connection 的模型兜底。

Desktop 输入区会按 connection 分组展示所有已配置且可用连接中的已知模型。选择另一个 Provider
或模型时，Sigil 会为该精确 route 创建新会话，不会原地改写正在查看的会话。在**设置**中选择默认
Provider/模型，会发布与 TUI 共用的 `connection-id/model-id` 默认值。TUI 的 `/model` 会追加可审计
route 边界并在当前会话继续；在精确模型候选上按 `D` 只修改未来会话的默认 route。TUI `/config`
中选择 connection 或模型并保存时，会同时更新保存默认值和当前会话 route。

## 选择模型服务

| 模型服务 | 适合场景 | 图片输入 | 配置值 |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | 快速设置的默认路径，以及 DeepSeek 专用选项 | 仅 `deepseek-v4-flash-vision-exp` | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | 兼容 Chat Completions 的 `/v1` 网关 | 不支持 | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | 使用 OpenAI Responses 接口 | 识别到的模型 ID | `openai_responses` |
| [Anthropic](provider-anthropic.md) | 通过 Anthropic Messages 使用 Claude | 识别到的 Claude ID | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini 与函数调用 | 识别到的 Gemini ID | `gemini` |

首次使用时，最快的方式是跟随快速设置：依次选择 provider、凭据来源和模型，然后检查并保存。
需要在本机或 CI 中重复使用相同设置时，再改用手写的当前 schema 配置。
在 `/config` → **Provider** 中，对 **Connection** 按 Enter 会打开明确的已保存连接和 Provider
模板选择器；`A` 会直接定位到新增 Provider。普通 macOS 键盘使用 Up/Down 即可，新增操作不会
再猜测“下一家” Provider。选中的 connection 就是保存时应用的 route，不需要再做一次“设为默认”；
保存会保留当前 session ID 和对话历史，从下一轮开始使用新 route。
模型选择器只属于当前选中的 connection：它先显示该 provider 自带的默认模型，仅在支持
discovery 时刷新远端列表。模型目录是可选增强：无论站点是否提供 `/models`，`M` 都可以手动输入
当前 connection 的精确模型 ID；网络、认证、TLS、协议或目录格式错误也不会阻止保存本地配置。
这些状态仍会分别显示，方便排障，但真正的 route 可用性由第一次模型请求验证。Sigil 不会拿
另一家模型服务的模型填充当前列表；远端明确返回某模型已不存在时，才会禁用该旧配置候选。
模型目录成功加载后，离开再进入时会按精确 connection/fingerprint 复用十分钟。更旧的进程内
结果会先按“未验证”继续显示，并在后台刷新，因此菜单往返不会反复把列表替换成阻塞式 loading。

## 认证优先级

当前 schema 不会把新输入的 API key 写入 `sigil.toml`。每个 connection 选择一种凭据来源：

| 来源 | 适用场景 | 配置中保存的内容 |
| --- | --- | --- |
| 受保护凭据存储 | 普通本机使用 | 仅随机 `source = "stored"` 引用；`file` 和 `auto` 写入 owner-only 凭据文件 |
| 环境变量 | CI 或已由 Shell 管理的 secret | 仅允许列表中的变量名 |
| 无认证 | 显式回环地址的自定义端点 | `source = "none"`；带凭据的远端 HTTP 会被拒绝 |

各 provider 的环境变量依次为 `SIGIL_API_KEY`、`SIGIL_OPENAI_COMPATIBLE_API_KEY`、
`SIGIL_OPENAI_RESPONSES_API_KEY`、`SIGIL_ANTHROPIC_API_KEY` 与
`SIGIL_GEMINI_API_KEY`。`[storage].credential_store` 可选 `file`、`auto` 或 `keyring`。默认
`file` 与非交互 `auto` 都只使用 owner-only 的 `~/.sigil/credentials.json`。如果该文件没有
当前凭据，请打开 `/config` 重新输入一次 key。严格 `keyring` 模式
才会显式使用 macOS Keychain、Windows Credential Manager 或 Linux Secret Service，并可能显示
平台认证界面。该专属文件保存的是受权限保护的明文凭据材料，不是加密。任何模式都不会把新粘贴的
secret 写入 `sigil.toml`、workspace、session、模型缓存、日志、快照或支持报告。修改凭据后运行
`sigil doctor`；诊断只显示来源和就绪状态，不会打印值或凭据 ID。

## 可复制起点

模板位于 [`docs/examples/config`](../examples/config)。使用前请检查具体模型、基础 URL、凭据来源和权限设置。

## 排障路径

依次检查 `[agent].connection`、`[agent].model`、对应的 `[connections.<id>]` 区块、端点、
凭据来源就绪状态，以及 provider 专项限制。`/config` 会显示待保存 route 与当前会话 route；保存后
两者一致。
不兼容的配置会被拒绝，不会迁移；请直接替换为当前模板，也不要手工编辑 credential ID。
排障期间保持 `permission.mode = "manual"`；如果问题并非某个模型
服务特有，再进入
[故障排查](troubleshooting.md)。

同一 route 上轮换凭据不会使 session 失效。同一可信 origin 内修正 endpoint 路径可以自动 rebind；
origin、账户/tenant、协议变化或 connection 缺失时，需要确认或选择 replacement route。
`sigil doctor` 只报告有界 route 恢复状态，不会打印 endpoint 或 credential identity。

<!-- public-doc-cta: open-provider-guide -->
下一步：[设置 DeepSeek，或选择其他模型服务](provider-deepseek.md)。
