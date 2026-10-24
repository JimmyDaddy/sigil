# MCP 配置预览导入

TUI 在 `/config` 的 MCP 页按 Ctrl-N 输入用户明确选择的 JSON 文件，使用共享 `sigil_runtime::mcp_import` parser 投影安全摘要。上下键移动，空格选择，Enter 把所选条目追加到当前设置草稿；Ctrl-S 继续走原有 `ConfigurationSaveRequest` 和配置 CAS。命令/参数不会显示在摘要中，但会随所选声明保存，预览明确提醒用户先检查秘密；内联 env/header 值始终不复制。同名条目不覆盖。

文件读取在有 owner 和 JoinHandle 的短期线程中完成。用户明确选择的 symlink 目标可读；实际打开的 FD 必须是 regular file，unix 使用 O_NONBLOCK 避免 FIFO 等待，元数据和实际读取共用既有 1 MiB 文档预算。没有扫描其它产品目录、自动执行或凭证探测。关闭预览不会脱离 reader owner；完成后 join，迟到结果仅能更新相同 request 和同一设置面板。选择时以当前草稿重新检测名称冲突；保存时保持原 config CAS。

预览和加入草稿不要求模型凭证。导入的服务沿共享 parser 的 lazy、非 required 与现有审批配置，保存本身不激活导入条目。未知/不合法条目保留诊断而不可勾选；未选条目不进入草稿。

激活失败时，拒绝消息区分网络策略、权限设置、工具自身策略及同时阻止的情况，不根据服务器名称或 third-party 标签猜测原因。网络阻止不表示导入失败：未选择 Web 的 `core` 组合会关闭运行时网络；无法证明禁网的本地 stdio 服务也受此约束。只有用户明确允许联网后，才在配置文件为原 `composition.enhancements` 加入 `web`，并设置 `[web] enabled = true`、`network_mode = "ask"`；保存、重启工作区后新建会话，继续通过原手动审批。已有 Web 选择的 TUI 可在 `/config` → Web 调整 Network mode；Desktop 当前需编辑配置文件。无需搜索能力时可保持 `search_route = "disabled"` 和 `[web.bundled_search] enabled = false`。导入本身不会修改这些网络设置，普通权限模式或工具 allow 规则也不会覆盖网络 deny。

验证覆盖共享 file reader 的 symlink、FIFO、目录及超预算，以及真实 TUI path→preview→selection→settings save、缺少环境凭证、重复名称、并发配置 CAS、关闭/更换面板、非法与未选条目。测试通过统一隔离入口运行；通过情况见 repo-local 执行记录，不把编写测试表述为通过。
