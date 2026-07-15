# RFC-0022 Public Documentation Information Architecture

状态：implemented

创建日期：2026-07-13

基线：

- Related: RFC-0017 Architecture and TUI Productization execution tracking
- Related: RFC-0021 Web Data Tools execution tracking
- Execution plan: repository-local RFC-0022 execution plan (not published)
- Review basis: repository-local comprehensive project review (not published)

## 1. Summary

Sigil 的公开文档已经覆盖了主要产品能力，但导航仍按历史文件顺序平铺，`configuration.md` 同时承担入门、权限说明、外观说明、实现细节和字段字典，导致首次使用者难以判断下一步该读什么。部分公开页面还使用 crate、kernel、adapter、Context V0 和 eval 等实现词汇，模糊了用户承诺与开发者实现的边界。

本 RFC 将公开文档重组为以用户目标为中心的信息架构，并为静态站点增加可重复执行的链接、元数据和可访问性检查。它不改变产品行为、配置兼容性、URL 中已发布页面的含义，且不把公开文档变成新的架构规范副本。

## 2. Goals

1. 将文档侧栏组织为稳定的用户任务分组，而不是单个长列表。
2. 让窄视口的文档导航在首个 HTML 帧即默认折叠；桌面端保持直接可浏览。
3. 将 EN/ZH `configuration.md` 拆为：配置指南、权限与沙箱、外观、Advanced、字段 Reference。
4. 从用户首页、快速开始、工作流、provider、状态、参考等公开页面移除实现层词汇；需要保留的机制说明迁入 `dev/docs`。
5. 让每个生成页面的 `dateModified` 对应其来源文件的实际最后修改日期，而不是构建当天。
6. 把源 Markdown 链接、生成站点链接和可访问性基础规则纳入本地与 Pages CI gate。

## 3. Non-goals

- 不改变 `sigil.toml` 字段、默认值或兼容性。
- 立项时不承诺尚未实现的 `sigil serve`、checkpoint/rewind、session lifecycle 或稳定外部 API；后续能力只有在实现与验收完成后才能进入公开文档。
- 不引入重量级文档平台、服务端搜索或 JavaScript-only 导航。
- 不把开发者架构、Rust crate 边界、协议 DTO 或评估实现复制到公开文档。
- 不以自动翻译替代 EN/ZH 人工对齐。

## 4. Public Information Architecture

侧栏按以下分组渲染；分组名随 locale 本地化，链接仍保持每页现有 slug：

| Group | Pages |
| --- | --- |
| Get started | overview, quickstart, installation, visual tour |
| Use Sigil | workflows, cookbook, user guide, command and key reference |
| Configure Sigil | configuration, permissions and sandbox, appearance, advanced configuration, configuration reference |
| Providers and integrations | provider guide, provider pages, MCP guide, terminal compatibility |
| Safety and troubleshooting | safety, privacy, troubleshooting |
| Project status | status, changelog |

`configuration.md` 是推荐路径和常见设置的入口，不成为所有字段的百科全书。每个拆分页必须在开头给出相邻页链接，EN/ZH 必须一一镜像。

## 5. Configuration Documentation Boundary

### 5.1 Configuration guide

保留配置发现顺序、最小配置、workspace、storage/session 路径、provider 跳转和 Doctor 排障。它只放日常用户需要的设置，并指向主题页。

### 5.2 Permissions and sandbox

说明本地读/写/执行、网络、外部目录、审批和 sandbox 的用户可观察语义。它必须明确安全限制和不保证项，但不得暴露内部 enum、receipt 型号或实现 crate。

### 5.3 Appearance

说明主题、语法高亮、使用成本显示和颜色覆盖，并保留可读性提示。颜色字段的完整枚举放入字段 Reference。

### 5.4 Advanced configuration

放置 task、memory、skills/agents、compaction、code intelligence、terminal、model request environment override、plugin 和 MCP 的高级配置。它强调何时需要编辑文件，而不是描述 runtime 装配过程。

### 5.5 Configuration reference

提供按 TOML section 分组的稳定字段参考：字段、值、默认值、用途和相关指南。它是精确查阅入口，不重复长篇行为说明。

## 6. Public vs Developer Language

公开文档使用用户可以操作或验证的语言，例如“模型服务商”“会话记录”“本地 HTTP 接口（尚未可用）”“检查结果”和“检索上下文”。

下列词汇默认属于开发者文档，不作为普通用户路径的解释方式：

- crate、kernel、runtime registry、adapter、DTO；
- Context V0、provider-neutral chunk、stream framing；
- eval harness、fixture、projection、receipt implementation；
- 具体 Rust module、状态转换和源码职责。

如果公开文档必须表达限制，应写可验证的产品效果，例如“V1 本地服务只接受 loopback 并要求 bearer token”，而不是解释 adapter 的内部阶段。实现与协议细节应迁移到对应 RFC、技术方案或 `dev/docs` 专题文档，并从公开页链接到用户可执行的替代路径，而非链接到源码。

## 7. Navigation and Progressive Enhancement

生成 HTML 的文档导航不在源码中预置 `open` 属性。这样在 JavaScript 尚未运行或禁用时，窄视口的 `<details>` 仍安全折叠，正文从首屏开始可读。

CSS 在桌面宽度让侧栏内容保持可见；客户端脚本只在大视口添加 `open` 以同步 disclosure 状态。相同原则适用于主导航：移动端闭合优先、桌面端直接可用。站点检查必须在 390px 视口验证默认折叠，在桌面验证导航可见。

每个分组使用语义标题和分组 `nav`，当前页仍有唯一的 `aria-current="page"`。搜索表单保持在文档导航顶部，并在移动端随导航一起折叠。

## 8. Metadata, Links and Accessibility Gates

### 8.1 Modified dates

生成器为每个 source Markdown 文件读取 Git 中最近一次影响该文件的提交日期，并将日期写入其 JSON-LD 和 sitemap URL。工作树或浅克隆无法提供 Git 日期时，构建可以回退到 source mtime，但必须在输出中使用该 source 的日期，不能用全站的构建日期伪装内容更新。

### 8.2 Source and artifact links

源文档检查必须验证：

- 本地 Markdown/asset 链接存在；
- 带 fragment 的本地 Markdown 链接指向真实 heading；
- EN/ZH 文档集合和语言切换链接一一对应。

生成产物检查必须验证：

- 每个本地 `href` / `src` 与 fragment 都可解析；
- sitemap 和 JSON-LD 有效且每页 `dateModified` 与 source 日期一致；
- 新增配置页出现在生成页、搜索索引、侧栏和 sitemap 中。

### 8.3 Accessibility baseline

不引入无法在 CI 重现的在线扫描服务。仓库内 gate 至少检查：语言、标题、描述、单一主内容地标、skip link、唯一 id、唯一 H1、具名导航、表单控件标签、图片替代文本、可展开元素的 summary，以及焦点/键盘可达的导航语义。现有真实浏览器视口检查继续覆盖窄屏无横向溢出、导航默认状态和正文首屏位置。

这是基础合规 gate，不等价于完整的人工辅助技术审计；高风险视觉改动仍需要人工检查。

## 9. Migration and Compatibility

1. 既有 `configuration/` URL 保持为配置指南。
2. 新页使用新增 slug，不删除或重定向现有用户 URL。
3. 语言切换必须落到同 slug 的对等页。
4. 文档内旧链接在同一变更中替换，构建后检查不得留下断链。
5. `docs/en` 与 `docs/zh-CN` 的文件集合保持镜像；文案可本地化，但页面职责不可漂移。

## 10. Implementation Slices

| Slice | Scope | Depends on |
| --- | --- | --- |
| D22.1 | 侧栏分组、默认折叠、生成页/搜索/sitemap 接线 | none |
| D22.2 | EN/ZH 五页配置文档拆分与旧链接迁移 | D22.1 |
| D22.3 | 公开页开发者术语清理及 `dev/docs` 迁移 | D22.2 |
| D22.4 | source/artifact link、dateModified、accessibility gate 与 Pages CI | D22.1-D22.3 |

完整验收命令、文件归属和结果记录见 repo-local execution plan。D22.4 完成后，RFC 状态改为 `implemented`，但后续文档行为变更仍必须遵守本 RFC 的边界。

## 11. Acceptance Criteria

- EN/ZH 侧栏按一致分组展示，移动端 HTML 初始状态为折叠，桌面端可直接浏览。
- 两种语言均具备五个职责清晰且相互链接的配置页面。
- 用户主路径页面不再以 crate/kernel/Context V0/adapter/eval/runtime internals 解释产品能力。
- 每个生成页的 `dateModified` 可追溯到对应 source 文档，而非构建当天。
- `scripts/check-docs.sh` 与 `scripts/check-pages-site.sh` 覆盖 source link、artifact link、metadata 和 accessibility baseline，并在 Pages CI 中运行。
- 现有链接、语言镜像、搜索索引、sitemap 和窄屏视口检查均通过。

## 12. Validation

最小验证：

```bash
scripts/check-docs.sh
scripts/check-pages-site.sh
```

文档与站点变更不要求全量 Rust gate；若同步修改 Rust 用户可见行为，按工程规范对相应 crate 运行 tiered gate。
