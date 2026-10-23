# Desktop 既有连接的凭证修复

设置页对使用凭证的已有连接提供更新入口，包括当前可用的连接，便于主动轮换密钥。日常管理操作收入连接行的更多菜单；缺少或无法读取凭证时直接显示修复按钮。无需认证的本机连接不显示密钥操作。用户只填写新的密钥；保存成功后刷新连接状态。保存失败保留当前草稿供重试，取消或成功后清除该表单草稿，并回到实际菜单或修复按钮。

环境变量连接使用“改用保存的 API key”这一明确动作。表单说明保存后该连接不再引用环境变量，提交按钮再次标明这一选择；不会改写环境变量本身。已保存凭证的连接继续使用“更新凭证”。无效连接保留“检查配置”入口，不能用更新密钥掩盖配置问题。

工作区重载后，renderer 按原 durable session identity 重新附着会话，留在当前设置分类并保留会话草稿。重新附着不能走“已经选中此会话”的导航快捷路径；可见列表被筛选时，使用独立的有界目录查找，不把缺少搜索结果视为会话已删除。晚到的重载结果不得覆盖用户随后选择的会话或工作区。重新附着失败保留原恢复身份，并在返回会话后提供既有重试入口。

## 窄接口与不变量

- Renderer 只提交 `{ connectionId, apiKey }`，经 allowlisted `desktop_update_provider_connection_credential` 交给 native shell；不能提交 endpoint、provider、protocol、默认模型或完整配置。
- Native shell 先检查工作区能否安全重启，停止该工作区的流，通过 host-private configuration capsule 保存，并验证 authority receipt 后重启工作区。流程复用已有配置修改边界，不授予会话新的执行权限或重绑定授权。
- HTTP host 从既有有效 V2 配置中查找 exact connection ID，保留该连接的 endpoint、protocol、options、context window、label、所有其他连接和默认模型。它只构造该 ID 的 `ConnectionCredentialUpdate`，复用 runtime 的 credential copy-on-write、回读校验、config CAS 和旧凭证清理。
- 配置解析与 authority 发布前置哈希来自同一次读取的持久源字节；解析不应用进程环境中的临时 model-request overrides，凭证修复不会顺带把运行时超时设置写回配置。
- 不使用“新增连接”向导代替修复。未知 ID 返回 `404 / provider_connection_not_found`；空白或超长密钥返回 `422 / provider_setup_invalid`。配置本身无效或没有默认路由时也明确拒绝，不自动替换配置或选择默认模型。
- 密钥只作为请求输入进入所选 credential backend，不写入普通配置、返回 DTO 或日志。结果复用 secret-free `ProviderSetupSaveResult`，其中默认模型保持原值。
- 若保存及 receipt 验证已成功、后续工作区重载失败，native 返回专用 `credential_saved_workspace_reload_failed`。这表示凭证已提交，不能当作保存失败再轮换密钥；表单清除已提交的密钥并引导重新打开工作区或检查诊断，原 manager 原因只采用既有脱敏投影。

公共受认证 HTTP 路径为 `PUT /settings/provider-connections/credential`，输入使用 snake_case；Desktop 实际使用 `POST /v1/private/borrowed/configuration/provider-connection-credential`，请求包含 schema version、capsule ID 与相同 typed input，返回 result 和可校验 receipt。

## 定向验证

所有 Cargo 测试通过 `scripts/run-isolated-tests.py` 执行。HTTP contract test 覆盖 exact ID、非默认连接更新、siblings/options/default 保留、密钥脱敏、旧凭证清理、未知 ID、空密钥、未认证请求，以及无效配置和缺失默认路由保持原样。Desktop client test 覆盖公开/host-private 路径和 receipt mismatch 拒绝；IPC test 验证只消费 exact ID/key 并忽略无关输入字段。

合约由 `scripts/generate-desktop-contract.sh --write` 更新，并使用 `--check` 检查 OpenAPI 与生成 TypeScript drift。Native 实机资格化单独验证真实表单、保存、工作区重载与重新读取状态，不以 renderer mock 或 HTTP 离线测试替代。
