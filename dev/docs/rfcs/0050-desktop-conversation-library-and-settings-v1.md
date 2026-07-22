# RFC-0050 Desktop Conversation Library and Settings V1

状态：active / implementation and automated gates complete; restarted native dogfood pending

创建日期：2026-07-21

依赖：

- [RFC-0047](0047-desktop-workbench-ux-reset-v1.md)
- [RFC-0027](0027-local-session-lifecycle-v1.md)
- [RFC-0048](0048-desktop-composer-and-transcript-v2.md)

## 1. Problem statement

当前 desktop 左侧会话 rail 同时承担高频切换、搜索、异常来源恢复和删除等管理职责。它适合快速导航，但不适合批量选择、危险操作预检和结果核对；底部菜单还会受到滚动容器与窗口边界影响。应用也缺少稳定的设置入口，外观和语言被拆成顶栏快捷按钮，`/config` 仍显示为没有 desktop route。

本 RFC 新增三个一级应用表面：

1. **Conversation Library**：面向低频会话治理，提供筛选、当前已加载集合的多选、服务端批量预检、单次确认和逐项 receipt。
2. **Settings**：只承载真实可持久化、可立即验证的桌面偏好；provider、model、permission、effort 等会话/运行事实不伪装为全局偏好。
3. **Support & diagnostics**：展示服务端生成的脱敏健康检查，并由原生层在用户明确选择的位置保存私有支持报告。

页面完整性审计没有把所有命令都升级为一级页面：`/compact` 与 verification 属于当前会话，skills/agents 属于现有 workbench，task/queue 尚无 desktop-owned runtime owner。它们继续保留在上下文弹窗、drawer 或 composer 中。Support 是唯一新增一级页面，因为 `/doctor`、`/feedback` 已有稳定的 runtime privacy projection 和独立任务闭环。

## 2. Research and interaction baseline

- 会话管理首版使用原生语义 table + checkbox，而不是直接声明 ARIA `grid`。WAI-ARIA APG 明确指出 `grid` 是复合控件，需要应用自己实现方向键和 roving focus；普通 table 会保留更自然的 Tab 顺序。多选状态由每行 checkbox 明确表达，避免焦点与选择耦合。参考：[WAI-ARIA Grid Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/grid/)、[WAI-ARIA Table Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/table/)。
- 设置只暴露用户确实需要控制的全局体验选项。Apple HIG 建议应用设置用于界面风格、保存/恢复行为等整体体验偏好，并保持稳定、可查找的设置结构。参考：[Apple HIG Settings](https://developer.apple.com/design/human-interface-guidelines/settings)。
- 普通页面状态不弹 toast；只有需要关注的失败、危险操作完成或需要用户继续处理的结果进入通知中心。筛选、选中、预检加载等留在原位反馈。

## 3. Product contract

### 3.1 Application navigation

- 顶栏保留 icon-only 的 New conversation、Conversation Library、Settings；每个 icon 都必须有 tooltip 与 accessible name。
- Conversation 是默认工作表面；Library 与 Settings 使用主内容区，不挤压 conversation timeline。
- 页面路由区分一级目的地与嵌套目的地：Conversation、Library、Settings 是由全局应用栏切换的同级一级目的地，Support 是 Settings 的二级目的地。
- 一级目的地通过全局应用栏的 `aria-current="page"` 与可见选中态表达当前位置；Library 与 Settings 同时保留显式返回 Conversation 的 escape route。品牌标识仍始终返回 Conversation。
- 非 Conversation 页面使用共享页面容器提供独立、固定高度的 page toolbar：返回 icon、当前页标题和尾部操作位于同一栏，完整返回目的地通过 tooltip 与 accessible name 表达。返回控件不得放进内容标题行，也不得通过标题左内边距侵入或挤压正文内容轴。
- Library、Settings、Support 复用同一个 application-page 容器，统一 max width、滚动 ownership、标题层级、metadata/action slot；Conversation workbench 保留其专用 timeline/composer 框架，不被管理页容器重包裹。
- `/config` 是 typed `OpenSettings` client action，必须打开 Settings，不能插入 prompt 或返回“没有 route”。
- 返回 conversation 时保留当前 workspace、已选会话和草稿。

### 3.2 Conversation Library

- 默认每页最多 100 条，支持 title 搜索、provider、source state、pinned 筛选和继续加载。
- 表格最少展示：选择、名称、状态、provider/model、activity、更新时间；无 absolute path。
- 筛选与批量操作属于常驻 command surface；长列表只滚动数据区域，不能让批量操作随列表滚出可见区域。
- “全选”只选择**当前已加载且可见**的行，文案和 accessibility label 不得暗示选择了所有匹配结果。
- 支持三种批量 action：删除 ready session、隔离 invalid source、永久删除 invalid source。
- 每个 action 必须先调用 authenticated server `plan`：返回 exact catalog generation、plan id、executable/blocked 数量和逐项 reason。
- 用户确认后调用 `execute`，服务端重新生成计划并比较 plan id；catalog、identity、pin 或 active-run 状态漂移时 fail closed。
- 执行是明确的 best-effort batch，不声称跨多个文件原子提交；每个 item 返回 `completed`、`failed` 或 `skipped` receipt。ready session 删除仍使用现有 append-only lifecycle delete；invalid source 仍使用 source fingerprint revalidation。
- active、pinned、changed、not-ready 项在 preview 中显示 blocked reason，不由 renderer 猜测。

### 3.3 Settings

V1 提供四组真实偏好：

1. New conversation model：从当前 workspace 已加载的 provider-validated `RunContext` 读取可选模型，允许选择“使用工作区配置”或仅覆盖之后由 desktop 新建的会话；已有会话的 provider/model identity 不变。
2. Appearance：System / Light / Dark，复用 native-owned appearance store。
3. Language：English / 简体中文，复用 renderer presentation preference 并立即更新 `lang`。
4. Startup：是否自动恢复最近工作区；关闭后仍保留 recent workspace 记录，只是不自动启动。

明确不在 V1 提供：

- provider credential、endpoint 或 tool policy 编辑器；这些属于受约束配置文件和 runtime restart 边界。
- provider 全局 model、permission 或 effort 配置。desktop 的 model 偏好只参与新建会话请求；model/effort 的 exact admission 与 permission 的每次 run 选择继续由 runtime/composer 管理。
- 虚假的“保存”按钮。每项变更在其 owner 成功后立即生效；失败保留旧值并给出 error toast。

### 3.4 Support & diagnostics

- `/doctor` 与 `/feedback` 共享 typed `OpenSupport` client action；从 Settings 也能进入同一页面。
- 页面只读取 authenticated `GET /support/doctor` 的 path-free projection，不读取配置文件、环境变量或本地日志。
- “保存私有报告”调用 `POST /support/bundle`，bounded JSON 只进入 native desktop client。renderer 只收到取消状态和文件名，不收到报告内容或 absolute destination path。
- 原生保存使用用户明确选择的目的地、私有文件权限并拒绝 symbolic-link target；取消保存不产生通知。
- 普通健康状态原位展示。只有明确保存成功或失败才使用通知，不为页面加载或诊断完成弹 toast。

## 4. Architecture and ownership

- `sigil-runtime`：继续拥有 lifecycle mutation 与 projection truth；新增批量 plan 所需的 exact catalog classification，不引入 desktop 文案。
- `sigil-http`：拥有 authenticated batch plan/execute、plan digest、active adapter-session blocker、OpenAPI 和逐项 receipt。
- `sigil-desktop` 与 Tauri：只传递 allowlisted typed DTO，不返回本地 absolute path、bearer 或 generic filesystem capability。
- `apps/desktop`：拥有页面路由、筛选/选中 presentation state 和设置交互；不得循环调用单项 mutation 冒充一次批量事务。
- appearance 继续由 native store 持久化；locale、startup 和 navigation width 是 renderer presentation preferences，读取失败时使用安全默认值。

## 5. Hard invariants

1. 批量 execute 必须绑定同一 request、同一 catalog generation 和服务端重算结果；renderer 提交的 blocked/executable 结论不可信。
2. active foreground run、verification、pinned session 和漂移 source 在执行前 fail closed。
3. 任何单项失败都不能吞掉；receipt 必须能定位到 `session_ref`，但 UI 默认展示 bounded title/ref，不输出 absolute path。
4. 删除 ready session 的 durable audit、invalid source 的 metadata revalidation、projection rebuild 语义不得被批量层绕过。
5. Settings 页面不能直接读写 `~/.sigil`、环境变量或 provider secret。
6. Library/Settings 不能引入第二套 workspace/session/runtime state machine。
7. Support report 必须通过 runtime frozen privacy projection；renderer 不得接收 bundle JSON 或 native save path。

## 6. Execution slices

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| R50.0 | Contract、research、ownership 和 commit boundary freeze | RFC/plan/status、decomposition audit |
| R50.1 | Batch plan/execute durable contract | runtime/HTTP/OpenAPI/registry tests |
| R50.2 | Native client、Tauri IPC 与 frontend bridge | DTO drift、allowlist command tests |
| R50.3 | Conversation Library table、filter、multiselect、preview、receipt | frontend interaction/AX/100-row tests |
| R50.4 | Settings page、typed `/config` route、startup preference | runtime catalog、frontend persistence tests |
| R50.5 | Real-server dogfood and completion audit | real desktop smoke、theme/locale/restart/batch drift matrix |
| R50.6 | Support & diagnostics page and typed `/doctor`/`/feedback` route | authenticated redaction tests、native private-save tests、frontend interaction tests |
| R50.7 | Shared page routing、scroll ownership、field/pagination primitives、new-session model preference and active-session identity | frontend route/field tests、1280×480 scroll dogfood、same-session no-reload test |

## 7. Acceptance gates

- 100 条已加载 row 的筛选和多选不阻塞 conversation；键盘可操作且选择状态可感知。
- batch preview 能区分 executable 与 pinned/active/changed/not-ready；stale plan 不执行任何新 mutation。
- best-effort execute 返回逐项 receipt，页面能保留失败项并刷新已完成项。
- Library 的筛选与批量操作常驻在列表滚动区上方；表头、数据行和“加载更多”属于同一个列表滚动容器，底部行、菜单和 confirmation 不被 rail/viewport 截断。
- `/config`、顶栏 Settings 均打开同一设置表面；theme、locale 和 startup 重启后行为与保存值一致。
- 一级页面通过全局应用栏直接切换，替换 Conversation 的页面也存在明确返回路径；低窗口高度下主页面只产生一个可操作的纵向滚动容器，input/select 必须经共享 field primitive 渲染。
- 当前会话在 rail 中具有 `aria-current` 与可见选中态；重复点击不得重取 transcript/run context，也不得留下 loading overlay。
- 新会话 model 偏好只接受当前 provider capability 投影中的模型，并在创建下一条 desktop 会话时显式传入；清空后恢复 workspace 配置。
- OpenAPI、native DTO、Tauri IPC、renderer types 无 drift；CSP/capability 未扩张。
- Support 页面不包含 absolute path、credential 或会话内容；取消原生保存无 toast，保存只返回文件名。

## 8. Non-goals

- 不做云同步、账号级偏好、跨 workspace 批量删除或远程 session 管理。
- 不做 “select every result across all pages”；V1 只选择当前已加载集合。
- 不提供 session 文件浏览器、JSONL 编辑器或 quarantine 恢复编辑器。
- 不在本 RFC 改变 retention policy、session export/fork/rewind 语义。

2026-07-22 page-frame follow-up：Library、Settings、Support 已迁移到共享 application-page 框架。Conversation、Library、
Settings 仍作为同级一级目的地在全局应用栏表达 active state；Library 与 Settings 额外保留显式返回 Conversation 的
escape route，Support 返回 Settings。所有返回入口都由独立 page toolbar 承载，不进入内容标题轴。Library 的筛选和
批量操作保持常驻，表头、数据行和分页控件由同一个列表滚动容器负责。该框架统一页面 max width、滚动 ownership、
标题层级和 navigation slot，避免返回按钮通过 padding 改变标题、筛选区和表格的共同左对齐轴。
