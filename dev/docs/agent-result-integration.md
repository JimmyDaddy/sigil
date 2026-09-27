# 子代理结果与变更整合

## 完整执行材料与展示摘要

子代理结果的展示限额不约束合法 changeset 的内容。运行时先保留完整、经过安全处理的执行正文，再独立生成父模型与界面的 bounded summary。前台、手动及后台 changeset-only 路径从完整正文解析 typed proposal，不从展示摘要或结果分页重建 patch。

新 changeset 的 patch 使用现有 immutable content artifact 存储，并将精确引用写入 durable integration facts。后续整合通过该引用读取完整 patch，仍验证 proposal、artifact 与目标文件约束。已经存在的 inline pending review 保留其当前读取方式；新 writer 不再生成这种引用。超过 4,000 字符的展示摘要阈值或结果分页额度不会改变执行输入，也不会自动增加父模型上下文。
