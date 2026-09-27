# 仓库模块回归样本 v1

这 20 个冻结任务使用 Sigil `9346771d139f15fe5d7c997521e64373ddfdb917` 的真实 Python/TypeScript 模块，向隔离副本注入可重放回归。它是质量、耗时和成本的小样本对照，不是历史 issue 排行榜，也不代表完整大型仓库任务成功率。

覆盖 Cargo 成员映射、跨目录检查、Desktop 发送/审批/取消/恢复状态、Unicode markdown、流式内容、图表 LRU 与主题隔离、provider 目录缓存；最后两个任务在同一会话追加下一轮要求，检查跨模块修改是否保留前轮结果。较长会话、真实 MCP 业务和完整浏览器验收需要各自的场景证据，不能从本集合推断。

`provenance.json` 固定来源提交、文件范围和每项 manifest 摘要。每个 `fixture.toml` 固定 prompt、源文件和检查文件 hash；模型运行前后都验证独立检查文件未被改写。每项已执行故障副本失败、原始实现通过的对照。Python 检查禁用 bytecode cache，避免同一秒内等长修复误用旧 `.pyc`。Node 需要支持 TypeScript 类型擦除的版本（此轮为 22.22.0），无需安装 npm 包。

运行仍使用现有 `sigil-model-eval --case repository-v1/<case>`。新旧 engine 比较必须使用相同的 eval harness：冻结原始 engine 提交，并单独记录评测代码补丁、二进制 hash、模型、权限、工具集合、样本顺序、用量与失败。禁止忽略失败或把保留预算当成真实账单；受控源代码回归的高通过率也不能当作通用 coding 能力分数。

这些文件来自本仓库，沿用仓库 MIT 许可证。
