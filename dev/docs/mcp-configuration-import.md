# MCP 配置预览导入

TUI 在 `/config` 的 MCP 页按 Ctrl-N 输入用户明确选择的 JSON 文件，使用共享 `sigil_runtime::mcp_import` parser 投影安全摘要。上下键移动，空格选择，Enter 把所选条目追加到当前设置草稿；Ctrl-S 继续走原有 `ConfigurationSaveRequest` 和配置 CAS。命令/参数不会显示在摘要中，但会随所选声明保存，预览明确提醒用户先检查秘密；内联 env/header 值始终不复制。同名条目不覆盖。

Desktop 的设置导入使用同一转换和精确选择语义，用户确认“导入并启用”后通过原配置 revision 与 authority publisher 保存。对 `core` 配置，明确导入只选择 MCP owner，不切换为 `standard`、不改变 Task 等其它已有选择。已有休眠 MCP 声明须先按选中模块重新解析，再与所选导入合并及检查重名；其它未选模块的原始文档继续休眠保存。TUI 将这一选择留在独立草稿，保留最初的 CAS 快照和其它未完成字段，取消或未勾选不会启用 MCP。

文件读取在有 owner 和 JoinHandle 的短期线程中完成。用户明确选择的 symlink 目标可读；实际打开的 FD 必须是 regular file，unix 使用 O_NONBLOCK 避免 FIFO 等待，元数据和实际读取共用既有 1 MiB 文档预算。没有扫描其它产品目录、自动执行或凭证探测。关闭预览不会脱离 reader owner；完成后 join，迟到结果仅能更新相同 request 和同一设置面板。选择时以当前草稿重新检测名称冲突；保存时保持原 config CAS。

预览和加入草稿不要求模型凭证。导入的服务沿共享 parser 的 lazy、非 required、third-party 与 ask 审批配置，保存及运行时重载本身不激活导入条目。重载后的新会话可发现 MCP catalog 和显式激活工具，启动服务仍经过既有审批。未知/不合法条目保留诊断而不可勾选；未选条目不进入草稿。

激活失败时，拒绝消息区分网络策略、权限设置、工具自身策略及同时阻止的情况，不根据服务器名称或 third-party 标签猜测原因。网络阻止不表示导入失败：未选择 Web 的 `core` 组合会关闭运行时网络；无法证明禁网的本地 stdio 服务也受此约束。只有用户明确允许联网后，才在配置文件为原 `composition.enhancements` 加入 `web`，并设置 `[web] enabled = true`、`network_mode = "ask"`；保存、重启工作区后新建会话，继续通过原手动审批。已有 Web 选择的 TUI 可在 `/config` → Web 调整 Network mode；Desktop 当前需编辑配置文件。无需搜索能力时可保持 `search_route = "disabled"` 和 `[web.bundled_search] enabled = false`。导入本身不会修改这些网络设置，普通权限模式或工具 allow 规则也不会覆盖网络 deny。

MCP 工具调用的审批展示由 host 从本次服务器、原始工具身份和参数生成的有界脱敏预览；该预览不发送 `tools/call`，也不宣称已知文件 diff 或副作用。秘密和本地路径只在展示中隐藏，原始调用与冻结预览仍分别绑定同一审批身份。完整安全展示超过 64 KiB，或参数键含秘密或本地路径、无法原样安全展示时，预览不可用，Desktop 保持不能批准并要求缩小请求。第三方或不完整 annotations 继续按高风险未知影响处理，只能对本次精确调用批准一次；网络 deny、快照和真正文件修改的审阅边界不变。

验证覆盖共享 file reader 的 symlink、FIFO、目录及超预算，以及真实 TUI path→preview→selection→settings save、缺少环境凭证、重复名称、并发配置 CAS、关闭/更换面板、非法与未选条目。测试通过统一隔离入口运行；通过情况见 repo-local 执行记录，不把编写测试表述为通过。
