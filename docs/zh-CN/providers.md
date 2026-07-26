<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,migrate-a-legacy-configuration,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# 模型服务指南

[文档首页](README.md) · [配置](configuration.md) · [English](../en/providers.md)

先在这里选择模型服务，再为实际使用的账户或端点创建命名 connection。每个 connection 独立拥有
provider 协议、端点、凭据来源和模型目录。保存默认值与运行中的会话都使用
`connection-id/model-id` 复合身份，因此切换 provider 时不会复用其他 connection 的模型兜底。

## 选择模型服务

| 模型服务 | 适合场景 | 图片输入 | 配置值 |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | 快速设置的默认路径，以及 DeepSeek 专用选项 | 不支持 | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | 兼容 Chat Completions 的 `/v1` 网关 | 不支持 | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | 使用 OpenAI Responses 接口 | 识别到的模型 ID | `openai_responses` |
| [Anthropic](provider-anthropic.md) | 通过 Anthropic Messages 使用 Claude | 识别到的 Claude ID | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini 与函数调用 | 识别到的 Gemini ID | `gemini` |

首次使用时，最快的方式是跟随快速设置：依次选择 provider、凭据来源和模型，然后检查并保存。
需要在本机或 CI 中重复使用相同设置时，再改用手写 V2 配置。
在 `/config` → **Provider** 中，对 **Connection** 按 Enter 会打开明确的已保存连接和 Provider
模板选择器；`A` 会直接定位到新增 Provider。普通 macOS 键盘使用 Up/Down 即可，新增操作不会
再猜测“下一家” Provider。
模型选择器只属于当前选中的 connection：它先显示该 provider 自带的默认模型，仅在支持
discovery 时刷新远端列表。只有权威远端/新鲜缓存结果、明确空目录或明确不支持 discovery 时才
提供 `M` 手动输入；网络、认证、TLS、协议、响应格式和过期缓存失败必须先修复或重试。加载中、已认证远端结果、明确空
目录、认证拒绝、离线/TLS 失败、不支持 discovery 和响应格式错误都是不同状态；Sigil 不会拿
另一家模型服务的模型填充当前列表。远端明确返回空目录时会清空候选，并允许用户确认手动 ID。
模型目录成功加载后，离开再进入时会按精确 connection/fingerprint 复用十分钟。更旧的进程内
结果会先按“未验证”继续显示，并在后台刷新，因此菜单往返不会反复把列表替换成阻塞式 loading。

## 迁移旧版配置

Sigil 发现合法的 V1 `[providers]` 配置时，会继续允许旧 route 运行，但升级前必须由用户确认。
迁移完全在本机完成：保留所有可投影 connection、端点、Provider 选项、当前默认模型和 role
route，不加载模型目录，也不访问 Provider。

- Desktop 在打开项目和设置页都会显示**迁移现有 Provider 设置**。检查连接数、密钥数、环境变量
  引用数和默认 route 后，选择**安全迁移**。**暂时继续使用**只让本次启动继续使用兼容 V1
  route；迁移成功前不能新增连接。
- TUI 在 `/config` 的 Provider 第一行显示 **Legacy migration**。按一次 Enter 即会原子迁移
  全部旧连接，不需要 PageUp/PageDown，也不需要再单独保存。如果打开 `/config` 后文件被其他
  程序改过，请关闭并重新打开 `/config`，复核后再试。

V1 内联 key 会从 runtime 已加载的配置直接移入 configured protected credential store，不经过
Desktop renderer 或 TUI 输入框；原有环境变量引用仍保持引用。已有会话和当前 TUI 会话继续使用
已解析 route，迁移后的保存默认值用于新会话。

每次写入待迁移凭据之前，Sigil 都会先在配置旁发布一个有界、typed、无 secret、仅 owner
可读的恢复记录。记录最多只包含 native owner 用于核对或清理的 opaque credential ID，以及写入
时选择的原始凭据存储模式；这些值不会进入 renderer、HTTP response、日志或诊断。复核持有配置
更新锁，并在清理前同时确认配置字节和恢复记录仍是用户刚刚检查的版本；`auto` 模式下系统凭据
存储不可用时不会宣称清理成功。确认发布成功或完整回滚后会删除记录；结果不确定时，这个阻断会跨
Desktop/TUI 重启和项目切换保留。Desktop 的主动作会变为**重新检查配置**；TUI 第一行会变为
**Migration recovery** / **Enter recheck**。请先修复当前配置或凭据来源，再执行这个显式动作。
复核会保留健康 V2 配置实际引用的 ID、删除未引用的 tracked credential；如果回滚后精确的合法
V1 配置仍在，也可以清理完成后恢复为可迁移状态。publication reconciliation 仍必须确认完整、
健康的 V2。恢复记录仍存在而配置缺失或损坏时，TUI 初始化也会保持 fail-closed。Sigil 不会把
该动作变成盲目重试。

## 认证优先级

V2 不会把新输入的 API key 写入 `sigil.toml`。每个 connection 选择一种凭据来源：

| 来源 | 适用场景 | 配置中保存的内容 |
| --- | --- | --- |
| 安全凭据存储 | 普通本机使用 | 仅随机 `source = "stored"` 引用；`auto` 优先系统存储，仅在不可用时回退 |
| 环境变量 | CI 或已由 Shell 管理的 secret | 仅允许列表中的变量名 |
| 无认证 | 显式回环地址的自定义端点 | `source = "none"`；带凭据的远端 HTTP 会被拒绝 |

各 provider 的环境变量依次为 `SIGIL_API_KEY`、`SIGIL_OPENAI_COMPATIBLE_API_KEY`、
`SIGIL_OPENAI_RESPONSES_API_KEY`、`SIGIL_ANTHROPIC_API_KEY` 与
`SIGIL_GEMINI_API_KEY`。`[storage].credential_store` 可选 `auto`、`keyring` 或 `file`。默认
`auto` 会优先使用 macOS Keychain、Windows Credential Manager 或 Linux Secret Service；仅当
系统存储不可用时，才使用 owner-only 的 `~/.sigil/credentials.json`。该专属文件保存的是受权限
保护的明文凭据材料，不是加密。严格 `keyring` 模式在系统存储不可用时 fail closed。任何模式都
不会把新粘贴的 secret 写入 `sigil.toml`、workspace、session、模型缓存、日志、快照或支持报告。
修改凭据后运行 `sigil doctor`；诊断只显示来源和就绪状态，不会打印值或凭据 ID。

## 可复制起点

模板位于 [`docs/examples/config`](../examples/config)。使用前请检查具体模型、基础 URL、凭据来源和权限设置。

## 排障路径

依次检查 `[agent].connection`、`[agent].model`、对应的 `[connections.<id>]` 区块、端点、
凭据来源就绪状态，以及 provider 专项限制。`/config` 会分别显示当前会话 route 与保存默认值。
现有 V1 `[providers]` 配置仍可读取；请按上面的**迁移旧版配置**操作，不要新增重复 connection
或手工编辑 credential ID。排障期间保持 `permission.mode = "manual"`；如果问题并非某个模型
服务特有，再进入
[故障排查](troubleshooting.md)。

<!-- public-doc-cta: open-provider-guide -->
下一步：[设置 DeepSeek，或选择其他模型服务](provider-deepseek.md)。
