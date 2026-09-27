# 分支谱系与结论回带

用户可以查询当前会话的父分支和直接子分支，并选择某个会话已完成轮中的模型结论回带。此流程复用 `ConversationForked`、当前工作区 session catalog 和原 session writer，没有新建任务、摘要循环或执行 authority。

选择材料是实际已落盘的最终模型回复，不是工具输出、私有推理、未完成回复或另一分支的控制状态。预览和写入分别校验源 session、完成轮 digest、最终 message ID、完整源正文 hash 和选中摘要 hash。摘要按既有 Context V2 的 8 KiB 单 snippet 预算截断，明确显示可能省略细节；长摘要可分页查看。用户看到并选择的是写入的同一文本。

写入 `BranchKnowledgeImportedV1` 控制记录，保持原历史 append-only。确定性 import ID 包含目标 session，同目标精确重试/重启只导入一次，导入其他目标不被错误去重。内容只进入后续请求的 `ExternalUntrusted` 上下文，明确标注结论未验证、不转移任何批准、lease、pending tool、检查通过或执行许可。当前草稿保留，不自动调用模型或发送消息。

TUI 在 `/resume` 选中来源会话后按 `Ctrl-O` 打开操作，再按 `K` 查看父子关系和已完成结论。`Up/Down` 选结论，`PgUp/PgDn` 阅读摘要，`Enter` 明确导入当前会话。失败保留草稿和选择；关闭或目标会话更换后的迟到回执不会污染新会话。此操作不恢复或切换来源会话。

导入复用当前 session owner 和 writer 串行提交，不要求整个目标 frontier 不变；同一目标中的无关追加、后台活动或终端活动不是知识导入许可。当前 owner 无法提供目标 Session、目标 scope 不符、源被替换或所选正文/digest 改变时仍拒绝。读源、追加、重试和 Context V2 投影都使用原有持久化契约。

原始草稿全目标 frontier CAS 的实际消融旧对照已运行：无关目标消息追加会拒绝导入。删除该门禁后的同场景、并发去重及两端产品 gate 仍由主代理串行验证，不能把源码完成表述为测试通过。产品状态与已运行验证以 repo-local B2 执行记录为准。

Desktop 在会话恢复面板的“分支与结论”中显示父分支和直接子分支，可直接导航到对应会话；也可搜索会话目录，选择来源和某条已完成结论，完整阅读有界预览后点击“导入结论”。长预览可滚动，截断会明确提示只导入所见文字。运行中也可导入，供后续请求使用；当前模型请求、草稿和已批准的动作均不被替换。迟到查询/导入回执只更新原目标面板，关闭或更换目标后不会污染新目标。

Desktop native query 只暴露相对 catalog reference 和 durable identity，导入沿现有 recovery command 传递六个精确来源字段，不允许 renderer 指定摘要正文或物理文件路径。receipt 中的 import_id/already_imported 区分首次保存与精确重试，宿主仍逐次验证实际来源。
