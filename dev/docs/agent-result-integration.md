# 子代理结果与变更整合

## 完整执行材料与展示摘要

子代理结果的展示限额不约束合法 changeset 的内容。运行时先保留完整、经过安全处理的执行正文，再独立生成父模型与界面的 bounded summary。前台、手动及后台 changeset-only 路径从完整正文解析 typed proposal，不从展示摘要或结果分页重建 patch。

新 changeset 的 patch 使用现有 immutable content artifact 存储，并将精确引用写入 durable integration facts。后续整合通过该引用读取完整 patch，仍验证 proposal、artifact 与目标文件约束。已经存在的 inline pending review 保留其当前读取方式；新 writer 不再生成这种引用。超过 4,000 字符的展示摘要阈值或结果分页额度不会改变执行输入，也不会自动增加父模型上下文。

## 合并只校验实际目标的来源

新建 merge review 保存 host 捕获的目标来源绑定：workspace identity、完整目标集合、每个文件的原内容哈希或明确不存在，以及完整 patch 的哈希。changeset-only 在子代理开始前通过 Resource Authority 的目录句柄观察源文件，逐级拒绝别名并限制扫描/读取资源；默认排除项沿用 verification scope。未读到、被排除或超预算的目标保持 unknown，不能伪造为空文件或不存在；无关目标的 unknown 不影响已观察的变更来源。

子代理运行期间或 review 等待期间，无关文件的变更不会取消结果。目标内容变化、目标新建冲突、patch 不匹配、工作区身份变化仍拒绝整合；实际目标来源读取继续受既有 `MAX_WORKSPACE_SNAPSHOT_FILE_BYTES` 上限约束，包含读取中增长的情况；实际写入、删除、新建都在 mutation prepare/commit 边界执行 expected-content CAS，不能只依赖提前检查。

带来源绑定的 review 使用目标范围 manifest identity；对应事件明确标记 `merge_review_targets` 范围。完成后的整仓观察允许未知，不把已成功写入转成观察失败。没有来源绑定的既有 review 继续遵守它原本记录的整仓快照条件，不从模型提供的哈希补造 host 证明。worktree 的初始完整基线冻结保留，合并来源取自 host 对基线树的提取结果。
