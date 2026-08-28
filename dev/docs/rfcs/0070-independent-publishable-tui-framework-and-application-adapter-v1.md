# RFC-0070：Independent Publishable TUI Framework, Presented-Frame Interaction and Application Adapter V1

状态：R70.4 Complete / R70.5 Complete / R70.6 Complete / R70.7 Complete / R70.8 In Progress（R71.8 已在 exact candidate `ec5459d8` 完成 local/five-platform qualification；R70.4 application contract、production ports、five-surface conformance 与 cold-cache gate 已闭合，R70.5 framework/package boundary、R70.6 host ownership、R70.7 preview package qualification 已闭合，R70.8 compatibility retirement 仍待 release-cycle validation）

创建日期：2026-08-23

修订日期：2026-08-23

审查记录：[RFC-0070 architecture review](../../../.repo-local-dev/review/rfc-0070-independent-publishable-tui-framework-review-2026-08-23.md)

依赖：

- [Rust agent core technical solution](../sigil-rust-agent-core-technical-solution.md)
- [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)
- [RFC-0058 Event-driven Worker and Incremental Durable-session Projection V1](0058-event-driven-worker-and-incremental-durable-session-projection-v1.md)
- [RFC-0065 Cache-stable Runtime Contracts V1](0065-cache-stable-runtime-contracts-v1.md)
- [RFC-0067 Single Execution Spine and Monotonic Plan-to-Task Adoption V1](0067-single-execution-spine-and-monotonic-plan-to-task-adoption-v1.md)
- [RFC-0068 Durable Recovery Spine and Effect-Scoped Retry V1](0068-durable-recovery-spine-and-effect-scoped-retry-v1.md)
- [RFC-0069 Recoverability Boundaries, Plan Direct Execution and Workspace Concurrency V1](0069-recoverability-boundaries-plan-materialization-and-workspace-concurrency-v1.md)
- [RFC-0071 Unified Resource Authority, Execution Sandbox and Lifecycle Recovery V1](0071-unified-resource-authority-and-sandbox-lifecycle-v1.md)：**hard implementation prerequisite**；只有R71.8在同一release candidate完成资格化后才允许启动R70.0。

实施前置：两份RFC严格串行。RFC-0071先完整实施R71.0-R71.8；RFC-0070随后从post-R71稳定基线执行R70.0-R70.8。RFC-0071实施期间不得并行推进任何R70 slice；本RFC不得要求RFC-0071预先创建`sigil-application`或拆分TUI package。

## 1. 摘要

当前 `sigil-tui` 不是一个可复用的终端 UI library，而是 renderer、交互状态、terminal host、Sigil
application adapter 与完整 runtime worker 的集合。鼠标事件还会在已绘制帧之外重新从 `AppState`
计算一套 `LayoutSnapshot`。这不仅让点击与用户实际看到的帧可能漂移，也把高频滚动和鼠标输入放大为
重复 projection、布局、命中区域构建与重绘工作。

本 RFC 提议在独立复核通过后冻结以下目标架构：

```text
Public package family, application-neutral
┌────────────────────────────────────────────────────────────────────┐
│ sigil-tui-core                                                     │
│ retained identity / damage / scheduler / input / focus / theme /  │
│ virtualization / CommittedPresentation / HitMap                    │
└───────────────────────────────┬────────────────────────────────────┘
                                │
┌───────────────────────────────▼────────────────────────────────────┐
│ sigil-tui-ratatui                                                  │
│ Ratatui renderer / buffer bridge / optional Crossterm driver /     │
│ headless test host                                                  │
└───────────────────────────────┬────────────────────────────────────┘
                                │
┌───────────────────────────────▼────────────────────────────────────┐
│ sigil-tui                                                          │
│ public facade / standard widgets / themes / extension API          │
└───────────────────────────────┬────────────────────────────────────┘
                                │ presentation model + opaque action
Internal Sigil workspace        ▼
┌────────────────────────────────────────────────────────────────────┐
│ sigil-tui-app                                                       │
│ ApplicationProjection -> SurfaceModel                              │
│ UiAction -> ApplicationCommand / HostRequest                       │
└───────────────────────────────┬────────────────────────────────────┘
                                │ versioned application port
┌───────────────────────────────▼────────────────────────────────────┐
│ sigil-tui-app --consumes--> sigil-application contract             │
│ sigil-runtime --implements--> sigil-application contract           │
│ sigil-application --depends on--> sigil-kernel public contract     │
└────────────────────────────────────────────────────────────────────┘
```

第三方开发者只需要声明一个 `sigil-tui` dependency；其下的 core 与 Ratatui adapter 是锁步发布的实现
package。仓库内 `sigil-tui-app` 与 `sigil-application` 均 `publish = false`，分别承载 Sigil 产品投影和
transport-neutral application contract。

RFC-0071先建立Resource Authority/Sandbox、permission V3、resource/effect receipt、RecoveryBlockerV2及kernel-owned `ResourceRecoverySurfaceContractV1`；R71阶段现有surface可暂经runtime facade消费。RFC-0070不再重做这条authority spine，只把post-R71高层query/command/event/projection收敛到`sigil-application`，并机械删除surface-to-runtime transitional edge。

核心交互不变量是：

> cell surface、hit/text/cursor geometry、event path、modal/focus scope、exact action binding 与 presentation
> obligation 必须由同一次 render transaction 产生，并且只有在 terminal adapter 证明实际 write/flush 完成后
> 才成为新的 `CommittedPresentation`。输入只查询该不可变快照，绝不在 click-time 对尚未呈现的
> application state 重新计算布局。任何可能已部分写出 terminal diff 的错误都会 poison 当前 terminal epoch；
> 此时旧、新快照都不得继续承担 coordinate input 或 presentation acknowledgement。

这不是把 OpenTUI 移植到 Rust。我们继续使用 Ratatui 的 cell buffer 与 diff，复制 OpenTUI 值得保留的
数据流和边界，并避免其 Yoga、JavaScript/Zig FFI、全 retained visual DOM 和matching native artifact分发成本。

## 2. 决策结论

本 RFC 做出以下规范性决策：

1. **保留 Ratatui，暂不迁移 OpenTUI。** 当前问题主要来自应用层重复 projection、click-time layout、
   不完整 invalidation 与职责耦合，不是 Rust 或 Ratatui 无法支持高性能鼠标交互。
2. **不迁移到 `ratatui-kit` 或 `ratatui-interact`。** 二者可提供设计素材，但前者当前事件分发会重建
   handler 并让任意事件触发整树 update/draw；后者只是 flat click-region/component toolkit。
3. **创建可公开发布的 package family。** `sigil-tui-core`、`sigil-tui-ratatui`、`sigil-tui` 的依赖树与
   公共 API 中不得出现任何 Sigil application/domain 类型。
4. **当前产品 crate 改名为 `sigil-tui-app`。** `sigil-tui` 名称留给 application-neutral public facade。
5. **创建内部 `sigil-application` contract。** TUI 产品 adapter 只能依赖该 contract，不能直接依赖
   `sigil-runtime`、`sigil-kernel`、provider、tool、updater 或 session store。
6. **`runner/*` 完整移出 TUI。** provider、tool registry、session lease、agent worker、MCP、compaction、
   updater 与 durable authority 属于 application/runtime host。
7. **采用 hybrid retained architecture。** 保留稳定 node identity、presentation state、focus/event path、
   virtual-list measure cache 与上一呈现帧；不要求首版建立通用 React 式 visual DOM 或 hooks runtime。
8. **采用 render-produced `CommittedPresentation`。** 它原子包含 `PresentedFrame + InteractionSnapshot`；默认
   hit index 是双缓冲 screen-sized dense generational-target grid，click lookup O(1)，z-order、clip、event
   path、modal scope、action binding 与实际 paint 共用同一 generation。
9. **滚动不触发整棵内容重排。** 普通 scroll 只改变 viewport transform/anchor；长 transcript 必须是真
   virtualization，不是只做 paint culling。
10. **事件消费与是否重绘正交。** ignored/no-op input 必须产生零 frame；同一 batch 中多个 damage 合并为
    至多一次 successful present。
11. **主题是版本化公共协议。** 颜色、状态、边框、symbols、density、spacing、motion 和 terminal capability
    fallback 都属于 theme；颜色变化通常仅 `Paint`，宽度/spacing 变化才 `Layout`。
12. **terminal present 是三态协议。** 只有可证明零 I/O 的 `NotStarted` 可以继续使用旧快照；任何
    `IndeterminateAfterIo` 都必须禁用 coordinate input/ACK，并恢复、全量重同步或终止 terminal session。
13. **presentation completion 是 trusted presenter capability。** 它不进入普通 application command catalog，
    不能由普通 in-process、HTTP 或 IPC client 构造、序列化或重放。
14. **snapshot/frontier/event 与 command replay 是 application 规范。** 必须冻结 scope、generation、gap/reset、
    reserve-before-effect、payload conflict 与 uncertain outcome，不能留给各 surface adapter 自行解释。
15. **测试、benchmark、package 与 semver gate 是发布条件，不是后补工作。**
16. **RFC-0071 是硬实施前置。** R70.0只能从R71.8资格化后的稳定基线开始；R70不得重定义Resource Authority、Sandbox、permission V3、resource/recovery durable schema或surface contract canonical hash。

## 3. 背景与当前根因

### 3.1 当前 crate 实际承担五种职责

当前 `crates/sigil-tui/Cargo.toml` 同时依赖 Ratatui/Crossterm、Tokio、clipboard/image/syntax 等 OS/UI
能力，以及 `sigil-kernel`、`sigil-runtime`、`sigil-tools-builtin`、`sigil-updater` 四个 application/runtime
crate。其 `lib.rs` 公开的是 `app`、`launcher`、`runner`、`ui` 等产品模块，而真正可复用的 view model 与
theme 仍是 crate-private。

`AppState` 还同时持有：

- config、workspace、session 绝对路径与 `SigilPaths`；
- runtime session attachment lease、provider/model/task/MCP 状态与 worker command queue；
- updater/background worker/channel；
- timeline、composer、focus、scroll、hover、selection、modal 与 theme preview；
- approval、session browser、setup/config、tool execution correlation；
- terminal size 和 OS effect 状态。

这意味着“只抽出 `ui/`”仍会把 domain、runtime、host 和 presentation 状态复制进新 crate；给 `AppState`
套一层 trait 也只会隐藏耦合，而不会倒转依赖方向。

### 3.2 当前鼠标路径维护第二套布局

当前主循环已经具备 dirty redraw 和最多 64 个 terminal event 的 batching，但每个 mouse event 都执行：

```text
Crossterm Mouse
  -> mouse_layout_snapshot(frame_area, terminal_size, &AppState)
  -> LayoutSnapshot::from_app(...)
  -> 重算 shell / composer / timeline / tool cards / modal / config hit areas
  -> handle_mouse_event
  -> 可能再次 render
```

对应证据包括：

- `launcher.rs:690-693`：每次 mouse event 构造新的 layout snapshot；
- `launcher.rs:736-743`：snapshot 直接从 `AppState` 生成；
- `ui/layout_snapshot.rs:241-280`：从 setup/config 状态开始重算整套 interaction geometry；
- `ui/shell.rs:36-108`：真正 renderer 又独立计算 shell、view model 和各类 modal。

所以当前存在两套必须永远保持一致的 geometry 实现：一套用于用户看到的 frame，一套用于点击。滚动卡顿
不是“为了支持 mouse capture 必须全量 redraw”，而是高频输入触发了本不该存在的第二套 projection/layout。

### 3.3 Ratatui 的边界不是缺陷

Ratatui 明确采用 immediate-mode full-frame 模型：每个 draw callback 填充当前 buffer，`Terminal` 再把它与
上一 buffer 做 cell diff，只输出变化的 cell。Ratatui 也明确不提供 input handling，事件捕获和应用级
routing 由调用者或更上层框架负责。

因此 Ratatui 能减少 terminal I/O，但不会自动减少：

- application projection；
- Markdown/wrap/measure；
- widget construction；
- interaction geometry；
- 不必要的 draw 调用；
- 长列表保留或扫描成本。

这些是本 RFC 要在 Ratatui 之上补齐的职责，不构成迁移到另一语言/runtime 的充分理由。

## 4. 调研范围与证据快照

本 RFC 结合当前 Sigil 工作区、官方互联网资料和以下用户提供的本地源码快照：

| 项目 | 本地路径 | Commit | 工作树 |
|---|---|---|---|
| OpenTUI | `/Users/jimmydaddy/study/rat-tui-comp-repos/opentui` | `eaf1d41e9252505232b1cbeae3ab05c15a55243d` | clean |
| ratatui-kit | `/Users/jimmydaddy/study/rat-tui-comp-repos/ratatui-kit` | `db0bffabb9d1e35609df97b9e1d10888150a2b1c` | clean |
| ratatui-interact | `/Users/jimmydaddy/study/rat-tui-comp-repos/ratatui-interact` | `42f1aab788f910886576fd9756c5ea9f16ad4123` | clean |

互联网资料只使用项目官方文档、官方仓库、Cargo/Rust/Unicode 官方资料或工具自己的 primary repository。
调研日期为 2026-08-23。源码事实以固定 commit 为准，不能把 `main` 分支未来变化反推成本 RFC 已验证事实。

## 5. 竞品源码结论

### 5.1 OpenTUI：应复制数据流，不复制技术栈

OpenTUI 的关键结构如下：

1. `@opentui/core`、React/Solid reconciler与keymap物理分包；testing是`@opentui/core/testing` subpath，
   `packages/native`是private Zig source workspace，公开native分发由core build生成matching platform artifact。
   React/Solid只是同一retained core tree的adapter，不是renderer核心。
2. 每个 `Renderable` 有稳定 ID、parent、Yoga node、缓存 screen coordinate、focus/mouse listener 和多种
   child order；添加、删除或属性变化修改同一保留树。
3. Root 的 frame pipeline 是 Yoga layout、tree/update/render-list、执行 render command 三阶段；Yoga 仅
   dirty 时计算，并在 generation 未变时复用 render command list。
4. `translateX/Y` 绕过 Yoga，ScrollBox 通过负 translate 改变 viewport；这避免普通 wheel scroll 重算
   整棵 layout。
5. 每个已绘制 Renderable 在 paint 后把最终矩形写入 native hit grid。native 维护两个
   `width × height` 的 `u32` grid，后画节点覆盖先画节点，scissor 同步裁剪。
6. `checkHit(x, y)`只是`current[y * width + x]`。native在backend返回非失败后交换grid，所以input不会读取
   半构建grid；但threaded output的queue success不等于physical write/flush completion，不能据此推导更强的
   terminal原子呈现语义。
7. mouse event 支持 target/currentTarget、bubble、stop propagation、prevent default、focus ancestor、
   hover、pointer capture、drag/drop。
8. `requestRender`合并重复invalidation；非continuous状态只响应显式、可合并request，来源包括tree mutation、
   mouse/screen mode、external output、resize、terminal capability、background/debug overlay、palette与host
   state。native renderer再进行row/cell diff，并在cell、cursor、pointer都未变化时写出零字节。
9. ScrollBox 有 viewport culling，但仍长期保留全部 child/Yoga node，并在过滤前更新所有 child layout；
   这不是 10 万条数据所需的真实 virtualization。
10. core 能探测 terminal light/dark/palette，但普通 component 缺少完整 semantic theme provider 协议。

需要避免对 OpenTUI 的过度归因：其 `requestRender`/dirty 主要决定“是否需要一帧”，当前 render 阶段仍遍历
render list；源码也明确记录最坏情况会更新整树。其路线图仍把 v0.x 描述为探索阶段，并计划继续调整 native
layout/render tree、Unicode、accessibility 与 benchmark。OpenTUI 是重要设计证据，不是可以无条件照搬的
稳定 Rust framework specification。

### 5.2 ratatui-kit：借鉴 retained identity、input layer 与 theme protocol

`ratatui-kit` 值得借鉴：

- keyed component identity 与跨帧 hook/state retention；
- state write 通过 waker 唤醒 renderer；
- `InputLayer`、scope、priority、Consumed 与 modal barrier；
- `Palette -> ComponentTheme -> subtree override -> per-call patch`；
- default-empty feature、单独 macros package、extension API、all-feature/rustdoc CI；
- `VirtualList` 作为独立 feature，而不是让普通 ScrollView 假装无限列表。

但不可直接迁移其 runtime：

- `Tree::render` 每轮全树 update + draw；
- 任意 terminal event dispatch 后无条件进入下一轮 render；
- dispatch 为每个 event 创建 active-layer `HashMap`、两个 index `Vec`、排序并 clone event；
- handler table 被 `mem::take` 后丢弃，下一 frame 必须重新注册所有 closure；
- `Tree`与InputRuntime的关键stepwise嵌入点是crate-private；`fullscreen()`提供framework-owned Crossterm loop，
  返回executor-neutral `Future`并使用`futures::select`，问题不是Tokio runtime coupling；
- 普通 ScrollView 为全部 child 建完整 content buffer。

Sigil 当前需要公平地 select terminal、application event、control、panic、deadline 和 animation；不能把这个
host-owned loop 换成“一事件一全树重绘”的 framework-owned loop。

### 5.3 ratatui-interact：借鉴 render-produced click region，不作为完整 runtime

`ratatui-interact` 的 `ClickRegionRegistry` 在 render 时登记真实 `Rect + action`，event 时只查询缓存区域；
`clear()` 保留 `Vec` capacity。这证明在 Ratatui 上不需要 click-time 重算布局。

但它的默认 registry：

- 对每次 click 线性扫描；
- 重叠时 first-registered wins，而不是后画/topmost wins；
- 没有统一 z/layer/clip/pointer-events；
- 没有树级 capture/bubble dispatcher；
- FocusManager 只按 ID 注册序循环，没有真正消费 `can_focus/tab_order`；
- modal 与 click registry 是局部 component state；
- theme 依赖调用方逐 widget 显式应用，漏调会退回硬编码默认样式。

它适合复用组件，不足以成为 Sigil 的 application-neutral renderer/runtime。

### 5.4 能力对比

下表中`R`为flat click region数、`H`为handler数、`L`为input layer数、`D`为committed target path深度；
target lookup复杂度不等于完整dispatch复杂度。

| 能力 | Ratatui | ratatui-interact | ratatui-kit | OpenTUI | RFC-0070 target |
|---|---|---|---|---|---|
| Cell diff | 是 | 复用 Ratatui | 复用 Ratatui | native row/cell diff | 复用 Ratatui，未来 adapter 可替换 |
| Input runtime | 无 | 应用自管 | framework 接管 | core renderer 管理 | host-owned，可嵌入 |
| Retained identity | 无 | component state 为主 | keyed component | Renderable tree | hybrid retained identity |
| Mouse geometry | 无 | render-time flat regions | previous-frame Rect | double-buffer dense grid | atomic `CommittedPresentation` |
| Target lookup | 应用决定 | O(R) | 排序后最坏 O(H) | O(1) | dense O(1)，策略可替换 |
| 完整 mouse dispatch | 应用决定 | O(R) | O(H log H + L) time、O(H + L) temporary | O(1) hit + tree path | O(1) hit + O(D + path handlers) |
| z/clip/bubble | 无 | 不完整 | layer 有，tree bubble 不完整 | 有 | capture → target → bubble |
| Dirty scheduling | 应用决定 | 无 | waker，但一事件一帧 | coalesced request | damage union + at most one frame/batch |
| Long-list virtualization | widget 自行实现 | visible slice，能力有限 | optional VirtualList | culling，不是真 virtualization | stable ID + height index + visible materialization |
| Semantic theme | style primitives | palette，无 provider | 较完整 | terminal palette，component style 分散 | versioned semantic theme |
| Standalone publication | renderer library | 单 crate | runtime + independent macros；testing feature | public core/adapters + core testing subpath + generated platform artifacts | public package family + one facade dep |

## 6. 目标与非目标

### 6.1 目标

1. 让 `sigil-tui` 成为 application-neutral、可独立文档、测试、benchmark、package 和发布的公共 crate。
2. 公共 framework 对所有 `sigil-*` application/domain crate 保持零依赖。
3. Sigil 产品 TUI 只依赖 transport-neutral application port，不直接拥有 runtime 或 durable authority。
4. 删除mouse-event上的第二套layout；所有geometry/path/scope/binding来自single successful
   `CommittedPresentation`，indeterminate terminal没有interaction authority。
5. 保留并增强 mouse click、hover、wheel、drag、selection、focus、modal、keyboard、paste 和 terminal lifecycle。
6. 让 ignored/no-op input 零 redraw，让 scroll/streaming work 与 visible viewport 同阶。
7. 支持 10 万条 variable-height transcript 的真实 virtualization、稳定 scroll anchor 与 width-aware cache。
8. 把当前 semantic palette、built-in themes、custom override、syntax、contrast diagnostics 和 live preview
   迁入清晰的 framework/application 两层协议，不退化主题能力。
9. 保留 approval、cancel、egress disclosure、session recovery 等现有安全/持久化语义。
10. 为第三方提供稳定 extension API、headless tests、完整示例与 SemVer 纪律。
11. 在V1冻结UAX #9 bidirectional layout与logical↔visual interaction mapping，不以grapheme width冒充RTL支持。

### 6.2 非目标

- 不在本 RFC 中迁移到JavaScript runtime、Zig、Yoga或OpenTUI matching native artifact路线。
- 不承诺 Ratatui renderer 永远是唯一 backend；首版只实现并资格化 Ratatui adapter。
- 不把 Sigil-specific transcript/tool/task/agent DTO 放进公共 core。
- 不让 UI 自己决定 approval、cancel、Task terminal、session persistence 或 provider recovery。
- 不在第一阶段引入通用 React-style hooks、proc macro、runtime plugin 或任意 `Any` props 系统。
- 不为发布而一次性大爆炸改写全部 TUI；迁移必须保持可比较的双轨 contract tests。
- 不把 terminal 原生 scrollback 重新引入 full-screen 产品路径。
- 不通过关键词、字符串或 UI label 推断 application command。

## 7. 规范性不变量

### 7.1 公共 crate 不拥有 application authority

可发布 framework 只拥有：

- presentation model；
- ephemeral interaction state；
- retained node identity；
- measure/layout/virtualization cache；
- rendering、cursor、hit map；
- normalized input reduction；
- theme、keymap、widget 与 testing protocol。

它不得：

- 读写 session JSONL 或 config；
- 访问 workspace/filesystem/network/process；
- 构建 provider/tool/MCP/runtime；
- 持有 approval/cancel/session execution authority；
- 把 optimistic presentation state 写成 durable truth；
- 根据产品文案构造 Sigil command。

### 7.2 依赖方向单向

```text
Cargo depends-on edges:

sigil-tui-core
    <- sigil-tui-ratatui
        <- sigil-tui
            <- sigil-tui-app
                <- sigil binary composition root

sigil-tui-app ----------> sigil-application
sigil-http / CLI -------> sigil-application
sigil-runtime ----------> sigil-application
sigil-application ------> sigil-kernel public contract

sigil-runtime --------> sigil-resource-authority / sigil-sandbox
sigil binary composition root -> sigil-runtime + sigil-tui-app
```

禁止 `sigil-tui-core`、`sigil-tui-ratatui`、`sigil-tui` 反向依赖 `sigil-application`。禁止
`sigil-tui-app` 直接依赖 runtime implementation。`sigil-application`不得依赖`sigil-runtime`、
`sigil-resource-authority`或`sigil-sandbox` concrete implementation；runtime实现application port并独占组合
RFC-0071 physical services。上图中public-contract edge允许复用kernel-owned
`ResourceRecoverySurfaceContractV1`，但application facade不得复制其canonical schema/hash或另建recovery authority。
RFC-0070完成态的HTTP/CLI shared application adapter同样只依赖`sigil-application`；binary/server composition root可同时
组装runtime implementation与adapter，但adapter不能借composition重新取得runtime-private或physical API。

### 7.3 Committed presentation 是唯一可交互事实源

`CommittedPresentation` 是以下对象的同 generation、不可变原子快照：

- terminal epoch、viewport 与最终 cell-surface digest；
- hit map、text map、cursor 与可见 obligation marker；
- generational target identity；
- parent/event path、capture/target/bubble policy；
- modal/focus scope、pointer/default policy；
- exact framework action 与 opaque application binding；
- projection scope/frontier 与 binding revision。

规范要求：

1. reconcile 只能修改 pending tree/dispatch table；input 不得读取 pending state。
2. geometry 与 `InteractionSnapshot` 必须在同一 successful present 后原子交换；不能先换 handler/action table、
   后换 hit map。
3. coordinate input 与面向可见 focus target 的 keyboard input只查询最后 committed snapshot。global terminal
   recovery/shutdown shortcut不依赖该 snapshot。
4. runtime node identity必须包含 generation。slot 只有在所有 committed/pending snapshot 都不再引用后才可复用；
   裸 `u32 NodeId` 不能独立作为 action identity。
5. action/binding、modal scope或event path改变属于 `Damage::Interaction`。即使 cell与geometry完全相同，也必须
   经过 presentation barrier后才能提交新的 dispatch snapshot。
6. target 已从 current model 删除时，只能按 committed binding执行并由 application重新校验，或返回
   `StaleTarget`；不得命中新 model 的其他 target。
7. resize 后必须先提交新 viewport generation，或有界丢弃/排队 coordinate event；不得在旧尺寸上猜测。

### 7.4 Terminal present 不假设原子 I/O

Terminal adapter 必须区分：

```rust
pub enum PresentOutcome {
    Presented(TrustedPresentReceipt),
    NotStarted(PresentNotStarted),
    IndeterminateAfterIo(PresentFault),
}
```

- `Presented`：adapter 已确认本次 terminal write/flush completion，并完成自身 buffer/cursor 基线更新；
- `NotStarted`：在任何 backend draw、cursor mutation、buffer swap、write、enqueue 或 flush 之前被明确拒绝，
  可以证明物理 terminal 与 adapter baseline 都没有变化；
- `IndeterminateAfterIo`：可能已经修改 adapter state、写出部分 cell/cursor sequence，或异步 writer只确认入队而
  未确认真实写出。

Ratatui `try_draw` 的任意 `Err` 默认映射为 `IndeterminateAfterIo`，除非 adapter 在调用 Ratatui前通过独立
admission/backpressure gate产生 `NotStarted`。不得根据错误类型猜测“终端仍显示旧帧”。异步 backend 的 queue
acceptance也不是 `Presented`。

进入 `IndeterminateAfterIo` 后必须：

1. 将当前 terminal epoch 标记 `Poisoned`，旧、新 presentation都不再是物理屏幕的可靠描述；
2. 立即拒绝 coordinate input、visible-target keyboard dispatch与所有 presentation ACK；
3. 清除pointer capture、hover、drag sequence与排队coordinate event；迟到/重复的旧epoch receipt或async completion
   typed fail closed；
4. 只允许 owner thread执行 restore/teardown；成功重新初始化后分配新 terminal epoch并进行 full repaint +
   application snapshot reconcile，或直接终止当前 TUI session；
5. 不把 terminal failure改写成 domain Run/Task terminal。

### 7.5 Event propagation 与 damage 正交

事件是否被消费不能隐式等于需要 redraw。所有 handler 必须返回显式 propagation、damage、action 与 wake：

```rust
pub struct UpdateOutcome<A> {
    pub propagation: Propagation,
    pub damage: Damage,
    pub actions: smallvec::SmallVec<[A; 1]>,
    pub next_wake: Option<Instant>,
}
```

概念上的 `smallvec` 不锁定依赖选择；实现也可用内部 inline buffer。规范要求是：

- `Consumed + Damage::None` 合法；
- `Ignored + Damage::Paint` 也可合法，例如外部 state observer；
- no-op mouse move 必须是 `Damage::None`；
- damage 在 scheduler 中做 union，同一 batch 最多一次 successful present。

### 7.6 Durable truth 与 presentation state 分离

- task、plan、approval、session、agent、provider、MCP 状态来自 authoritative application projection；
- focus、hover、scroll、selection、expanded/collapsed、draft、animation 是 ephemeral surface state；
- optimistic command pending 必须带 command id 并与 authoritative receipt 分栏；
- reconnect/resume 使用 scoped snapshot-subscribe cut + versioned event envelope reconcile，不从 UI state重建领域事实；
- application command在 effect 前 durable reserve；相同 identity/different payload fail closed；无法证明 effect
  是否发生时返回 `Uncertain`，不得自动重执行；
- ordinary application client不能构造 presentation completion；trusted presenter capability与普通 command port
  是不同的 authority surface。

## 8. Package 与发布拓扑

### 8.1 `sigil-tui-core`（公开发布）

职责：

- geometry、stable `NodeId/NodeKey`、tree/event path；
- `InputEvent`、focus/modal/pointer capture；
- `Damage`、scheduler state、fake clock contract；
- `CommittedPresentation`、`InteractionSnapshot`、`HitMap`、text map、cursor presentation；
- theme protocol 与 terminal capability model；
- virtual sequence、height index、scroll anchor；
- application-neutral element/component contract。

硬依赖限制：

- 不依赖 Ratatui、Crossterm、Tokio 或任何 `sigil-*`；
- production code 不依赖 filesystem/process/network/clipboard/updater；
- 默认 `#![forbid(unsafe_code)]`；
- public error 使用 typed error，不暴露 `anyhow::Error`；
- public struct 默认 private fields，开放 enum/struct 使用 `#[non_exhaustive]` 或 builder 预留演进空间。

### 8.2 `sigil-tui-ratatui`（公开发布）

职责：

- 把 core scene/layout/style 映射为 Ratatui `Buffer/Frame`；
- 复用 Ratatui `Terminal` double buffer 与 cell diff；
- 维护 pending/committed `CellSurface + InteractionSnapshot`、terminal epoch与 poisoned recovery；
- 可选 Crossterm terminal lifecycle/input decoder；
- headless renderer、test backend 和 frame/style/hit snapshots；
- 通过 bounded scratch buffer提供 Ratatui native-widget adapter，且不得让 raw `Frame/Buffer`、Ratatui类型或
  越界 paint能力泄漏回 core。

首版不实现任意 partial terminal patch。逻辑层可以只 reconcile、measure、materialize visible nodes，但一次
Ratatui draw 仍按其规范填充完整 viewport；屏幕 cell 数远小于 transcript 数据量，先用 benchmark 证明需要
更低层 patch 后再增加 adapter，不在 core 中提前绑定未验证复杂度。

### 8.3 `sigil-tui`（公开 facade）

职责：

- re-export 对齐版本的 core 与 Ratatui adapter；
- application-neutral standard widgets：box/text/stack/scroll/virtual-list/input/button/select/modal/popover/
  status/card/markdown primitives；
- default dark/light/high-contrast themes；
- extension API 与 prelude；
- minimal、todo、chat、dashboard、mouse、theme、virtual-list examples。

第三方默认只添加：

```toml
[dependencies]
sigil-tui = "0.1"
```

建议 feature：

| Feature | Default | 含义 |
|---|---:|---|
| `crossterm` | yes | Crossterm terminal driver/input adapter |
| `serde` | no | theme/keymap/trace schema serialization |
| `syntax` | no | syntax highlighting integration |
| `image` | no | image protocol adapter，不改变 core contract |
| `tracing` | no | structured metrics/tracing hooks |
| `test-util` | no | headless host、manual clock、event injection |

Feature 必须 additive；任意组合不得关闭既有语义。公共 facade 不默认依赖 Tokio，async application 可把
framework 嵌入自己的 runtime。

### 8.4 `sigil-application`（仓库内部，`publish = false`）

职责：

- versioned application query/command/event/projection/receipt；
- 无损复用RFC-0071 kernel-owned `ResourceRecoverySurfaceContractV1`、`ToolPermissionPlanV3/DecisionV3`、resource/effect receipt与`RecoveryBlockerV2` public view；
- bounded renderer-safe DTO；
- transport-neutral application port；
- presentation obligation与独立 trusted presenter capability protocol；
- host-derived control priority；
- command durable reservation/replay/uncertain outcome；
- scoped snapshot-subscribe/frontier/gap/reset/reconcile protocol。

该 crate 可以依赖 `sigil-kernel` 的 provider-neutral public contract，但不得依赖 TUI、Desktop、HTTP 或
某个 provider adapter。现有 `sigil-runtime::application_run` 中的 prepare、public event、cancel、outbox、
delivery receipt 与 session lease 是迁移种子，而不是完整 application port。

`sigil-application`是application protocol/facade owner，不是physical resource或sandbox authority。它不得依赖
`sigil-resource-authority`/`sigil-sandbox` concrete type、持有path/descriptor/lease实现、复制R71 canonical
encoding，或为command reservation、resource reservation与recovery各建一份互相竞争的事实源。R70 command
reservation只保护application command的idempotency/effect admission；R71 resource reservation/journal只保护
physical resource lifecycle。两者通过typed command/effect/resource receipt关联，不能互相替代或双写同一事实。

### 8.5 `sigil-tui-app`（仓库内部，`publish = false`）

职责：

- `ApplicationProjection -> SigilSurfaceModel`；
- `UiAction/ActionRef -> ApplicationCommand`；
- command pending/receipt/event reconciliation；
- Sigil-specific widgets、copy、task/plan/tool/agent/session/setup/config information architecture；
- product themes 与 current config mapping。

本 crate 在本仓库内允许依赖：

- `sigil-tui`；
- `sigil-application`；
- 无 application authority 的纯资源 crate（若未来确有必要，需逐项评审）。

它禁止直接依赖：

- `sigil-runtime`；
- `sigil-kernel`；
- `sigil-tools-builtin`；
- `sigil-updater`；
- provider、MCP、process、session store implementation；
- `sigil-resource-authority`、`sigil-sandbox`及其concrete/physical type。

### 8.6 为什么不在首版拆更多 package

固定源码能证明的只是：OpenTUI使用 core、framework adapter、keymap与平台 artifact 的多包拓扑，且其公开
adapter与core有锁步检查；`ratatui-kit` 则只有主 runtime 与独立 macros package，testing是 feature，版本也不
锁步。Cargo要求被依赖的 package在发布 facade前已经能从 registry解析，但不要求 Sigil 三包永远同版本。

首版固定三个公开 package，是 Sigil 自己基于以下理由作出的 preview policy，而不是竞品架构必然结论：

- core/backend/facade API在 0.x 期间会共同收敛；
- 单版本 compatibility matrix较小，失败时容易停止 release train；
- 不另拆 widgets/testing/macros/crossterm可降低初次发布与文档成本；
- facade仍让第三方只声明一个 dependency。

当公开 API 连续两个 minor release没有跨包破坏、至少两个第三方 consumer需要独立升级 backend/widget，且 CI
已有最低/最高兼容矩阵时，应提交解除锁步的 split/versioning RFC。

只有满足以下至少一项才能提交后续 split RFC：

- feature powerset 导致不可接受的 compile time 或依赖污染；
- testing API 需要不同 MSRV/semver；
- widgets 已有至少两个独立第三方消费者和独立版本压力；
- proc macro 已被两个消费者证明明显减少错误且不要求 unsafe/unchecked props。

### 8.7 机器可检的公开依赖 allowlist

“无application依赖”不能实现成`grep sigil-`。唯一规范allowlist为：

| Root package | 允许出现的实际 `sigil-*` package dependency |
|---|---|
| `sigil-tui-core` | 无 |
| `sigil-tui-ratatui` | `sigil-tui-core` |
| `sigil-tui` | `sigil-tui-core`、`sigil-tui-ratatui` |

CI必须按Cargo package identity解析`cargo metadata`的declared dependency与resolved graph，并检查normal/build/
dev、optional、target-specific edge以及default/no-default/all-supported-feature resolve。dependency rename/alias、
registry/git/path source都不能绕过：任何实际package name匹配`^sigil-`的edge只有表中name/source/version policy
允许；任何其他workspace/private product package无论名称是否以`sigil-`开头都禁止。尤其禁止
`sigil-application/runtime/kernel/tools/updater/provider/mcp/process/desktop/http/...`。Sigil integration tests放在
consumer/adapter侧，不能用dev-dependency把authority偷偷带回public package。
该allowlist应保存为单一machine-readable policy，R70.5、architecture gate与publication gate共同消费。

## 9. Transport-neutral application contract

### 9.1 Contract 形状

`sigil-application` 不复制当前 60+ `WorkerCommand` / 70+ `WorkerMessage` 的扁平枚举，而使用 versioned
envelope 与按领域分组的 typed payload：

```rust
pub struct ApplicationCommandEnvelope {
    pub schema_version: u16,
    pub command_id: ApplicationCommandId,
    pub correlation_id: Option<CorrelationId>,
    pub expected_frontier: ExpectedFrontier,
    pub command: ApplicationCommand,
}

#[non_exhaustive]
pub enum ApplicationCommand {
    Conversation(ConversationCommand),
    Run(RunCommand),
    Approval(ApprovalCommand),
    PlanTask(PlanTaskCommand),
    Agent(AgentCommand),
    UserInput(UserInputCommand),
    Session(SessionCommand),
    Configuration(ConfigurationCommand),
    Provider(ProviderCommand),
    Mcp(McpCommand),
    Maintenance(MaintenanceCommand),
}
```

`client_id`、authenticated principal、session/workspace scope与effective priority不属于 caller-controlled
payload。transport/composition root把它们注入不可伪造的 `CommandAdmissionContext`；application host再按
exhaustive command classifier派生 priority、required scope/frontier与effect settlement class。wire caller最多
提供 bounded scheduling hint，不能把 maintenance/config/stream work提升为 urgent lane。

```rust
pub struct ApplicationCommandRequest {
    pub envelope: ApplicationCommandEnvelope,
}

pub(crate) struct CommandAdmissionContext {
    principal: AuthenticatedPrincipal,
    client_epoch: DurableClientEpoch,
    connection_instance: HostMintedConnectionInstance,
    application_scope: ApplicationScope,
    transport_binding: TransportBinding,
}

struct CommandPolicy {
    lane: HostDerivedLane,
    required_binding: RequiredAuthorityBinding,
    settlement: EffectSettlementClass,
}
```

V1不接受可以提升lane的caller priority；future hint最多允许host降级或延迟。

每个 mutation command必须携带该 command family规定的 exact session/frontier/opaque binding；不能用一个
全局 `Option` 让调用者省略 CAS。`ApplicationCommandReceipt` 是generic transport wrapper，其中必须保留对应
domain receipt，不能把 task id/title/phase/blocker、approval identity或session result压缩成 generic
`accepted`。Presentation completion不属于 `ApplicationCommand`。

### 9.2 Renderer-safe projection

TUI 不再读取 `SessionLogEntry`、runtime object、JSONL 路径或 provider exact payload，而只消费 bounded
projection：

```rust
pub struct ApplicationProjection {
    pub schema_version: u16,
    pub scope: ProjectionScope,
    pub writer_generation: WriterGeneration,
    pub stream_generation: StreamGeneration,
    pub observer_generation: ObserverGeneration,
    pub frontier: ApplicationFrontier,
    pub session: SessionSurfaceProjection,
    pub conversation: ConversationSurfaceProjection,
    pub run: RunSurfaceProjection,
    pub plan_task: PlanTaskSurfaceProjection,
    pub agents: AgentSurfaceProjection,
    pub approval: ApprovalSurfaceProjection,
    pub user_input: UserInputSurfaceProjection,
    pub capabilities: CapabilitySurfaceProjection,
    pub configuration: ConfigurationSurfaceProjection,
    pub attention: AttentionSurfaceProjection,
}
```

约束：

- 所有文字已经过 SafePersist/renderer-safe projection；
- 大 artifact 只暴露 opaque ref、bounded preview 与 allowed actions；
- path authority、credential、signed URL、provider exact args 不进入 projection；
- 每个 action 带 exact opaque binding/generation/hash，UI 只能原样回传；
- projection 可以分页/virtual page，不要求 TUI一次持有完整 transcript；
- snapshot和incremental patch共享同一 reducer；event必须携带base/next frontier与scope/generation，乱序、重复、
  gap和writer restart都有deterministic result。

### 9.3 Command admission、reservation 与 replay

Application service必须为每个 mutation/control command执行以下状态机：

```text
authenticate + bind scope/client
  -> canonicalize command + payload fingerprint
  -> derive priority/effect settlement exhaustively
  -> durable reserve(reservation key, fingerprint, policy)
  -> execute effect at most according to settlement policy
  -> durable terminal domain receipt
  -> replay the exact receipt for the same reservation key
```

Reservation key与fingerprint必须分离：

```text
CommandReservationKey =
  durable application namespace + family-specific durable authority scope
  + authenticated principal + durable client epoch + command id

CommandFingerprint =
  schema version + command kind + exact target/action binding
  + expected writer generation/frontier + canonical payload
  + effect settlement class
```

family-specific authority scope必须从application、workspace、session或其他durable domain scope中穷尽派生，
不能由caller任意省略。Reservation key跨process/writer restart保持稳定；writer/application process generation
只能进入fingerprint/CAS，不能创建新reservation namespace。`DurableClientEpoch`必须能跨response-lost reconnect
恢复；`HostMintedConnectionInstance`只标识当前transport connection，不进入reservation key。若client epoch轮换，
旧epoch必须在所有adapter全局拒绝；不能让同一command id借new epoch绕过旧reservation/tombstone。

强制规则：

1. effect之前必须durable reservation；reservation store unavailable、满载或无法fsync时在首个effect前拒绝。
2. 相同reservation key + 相同fingerprint只重放原receipt或原`Uncertain`；不同fingerprint返回
   `PayloadConflict`并
   fail closed。
3. reservation至少有`Reserved`、`DispatchStarted`、`Settled`、`OutcomeUncertain`状态；跨越effect boundary
   前先durable写`DispatchStarted`。只有reservation key、domain mutation和receipt能在同一append-only authority
   原子提交的family才可省略中间状态；状态只能单调迁移。
4. query不进入mutation journal。每个mutation/control family必须穷尽声明`AtomicDurableMutation`、
   `MonotonicControl`、`IdempotentWithKey`、`ExternalOrWorkspaceEffect`或`NonRepeatable` settlement class及restart
   repair策略；trusted presentation不属于command class。
5. crash/transport loss后不能证明effect未发生时返回`Uncertain`；除非该family有durable reconciliation或
   externally enforced idempotency key，否则不得自动重执行。
6. transport error不等于command未执行。client只能用同一command id重试并读取terminal/uncertain receipt。
7. `Accepted/Settled`只表示domain authority已经durable commit；仅reserve或dispatch不算accepted。并发相同
   reservation key只有一个execution owner，其余等待或得到typed`InFlight/OutcomeUncertain`。
8. `Reserved/DispatchStarted/OutcomeUncertain`不得因TTL或capacity驱逐。terminal receipt在replay horizon后可以
   压缩成fingerprint tombstone，但仍阻止旧command id重新执行；只有owning session被同一authority永久删除，
   或client epoch轮换且旧epoch全局拒绝后，identity才可彻底删除。
9. `Expired`identity不得被当作一条同id的新command；caller必须snapshot/query reconciliation并生成新id。
   store saturated/unavailable必须在effect前失败。
10. urgent lane由host按typed command派生；approval/cancel/pause/shutdown不能被stream、page或maintenance work
   饿死，也不能由caller自行升级priority。

`OutcomeUncertain`只引用现有authority-plane reconciliation：`ObservedApplied`读取/重建durable receipt且不重做
effect；`ObservedNotApplied`结算为明确未发生，但默认要求用户用new command id显式重发；`StillUncertain`或没有
安全只读probe时保持blocked。TUI只看到safe reason与opaque recovery action，不获得effect id/digest/probe
authority。

Startup/restart repair默认表：

| Durable command state | Restart resolution |
|---|---|
| `Reserved`且无dispatch marker | `AbortedBeforeDispatch`，默认不自动执行 |
| `DispatchStarted`且找到exact durable domain receipt | `Settled`并重放receipt |
| `DispatchStarted`且有durable no-effect proof | `ObservedNotApplied`，需要new command id显式重发 |
| `DispatchStarted`的其他情况 | `OutcomeUncertain`并进入既有reconciliation |
| `Settled` | exact receipt replay |
| `OutcomeUncertain` | 保持blocked，直到authoritative reconciliation；禁止重做 |

Family-specific policy只能在比默认表更强的durable/idempotency proof下收紧结果，不能用“进程刚重启”推断effect
未发生。

Generic receipt只封装admission/replay状态与exact domain receipt：

```rust
pub enum ApplicationCommandReceipt<R> {
    Settled(R),
    Replayed(R),
    Rejected(CommandRejection),
    PayloadConflict(CommandConflict),
    Uncertain(UncertainCommandReceipt),
}
```

### 9.4 Scoped snapshot-subscribe contract

`snapshot()` 后再独立调用 `subscribe(from)` 会留下丢事件窗口，因此规范API是一次
`open_projection`：返回同一个 scope/cut 的 snapshot 与 replay feed。实现可以在 durable event log上从cut
回放，也可以在构造snapshot期间有界buffer后续event；buffer溢出只能返回`ResetRequired`，不能静默跳过。

```rust
pub struct ProjectionScope {
    pub application_instance: ApplicationInstanceId,
    pub authenticated_subject: SubjectBinding,
    pub workspace: Option<WorkspaceScopeId>,
    pub session: Option<SessionScopeId>,
    pub run: Option<RunScopeId>,
}

pub struct ApplicationFrontier {
    pub schema_version: u16,
    pub scope: ProjectionScope,
    pub writer_generation: WriterGeneration,
    pub stream_generation: StreamGeneration,
    pub through_sequence: u64,
    pub durable_cursor: OpaqueDurableCursor,
}

pub struct ProjectionSnapshotEnvelope {
    pub schema_version: u16,
    pub scope: ProjectionScope,
    pub writer_generation: WriterGeneration,
    pub stream_generation: StreamGeneration,
    pub observer_generation: ObserverGeneration,
    pub cut: ApplicationFrontier,
    pub projection: ApplicationProjection,
}

pub struct ApplicationEventEnvelope<E> {
    pub schema_version: u16,
    pub scope: ProjectionScope,
    pub writer_generation: WriterGeneration,
    pub stream_generation: StreamGeneration,
    pub observer_generation: ObserverGeneration,
    pub event_id: ApplicationEventId,
    pub base_frontier: ApplicationFrontier,
    pub next_frontier: ApplicationFrontier,
    pub durability: EventDurability,
    pub payload_digest: ContentDigest,
    pub payload: E,
}

pub enum ProjectionFeedItem<E> {
    Event(ApplicationEventEnvelope<E>),
    Gap(ProjectionGap),
    ResetRequired(ResetReason),
    ScopeMismatch(ScopeMismatch),
    Ahead(FrontierAhead),
    Expired(FrontierExpired),
    Closed(ProjectionClosed),
    UnexpectedEof(UnexpectedProjectionEof),
}
```

Reducer规则：

- host必须先register/arm observer或durable replay cursor，再取得inclusive snapshot cut；snapshot构造期间产生的
  event进入已arm buffer/log，第一条durable envelope的base必须衔接cut；
- `scope`、writer/stream/observer generation不匹配时不apply；writer restart必须产生new generation并重新
  snapshot，旧observer迟到event直接丢弃；
- exact `base_frontier == committed_frontier` 才apply并推进到`next_frontier`；
- 同event identity/payload digest的duplicate可以幂等忽略；same identity/different payload fail closed；
- ahead、expired、gap、retention loss、buffer overflow都进入显式reset，不尝试拼接不同run/session/writer；
- durable event只在application durable commit后发布；terminal、approval、cancel/control、action binding与
  capability change必须durable；transient wake不能推进durable frontier、创建action/approval/effect authority，
  只能更新可丢弃的provisional UI；
- adapter只有在reducer commit成功后才发送delivery ACK。ACK属于outbox delivery，不等于domain command
  success，也不等于presentation completion；restart时pending outbox继续replay。

Envelope内部还必须交叉校验：

- snapshot envelope的scope/writer/stream/observer generation必须与embedded projection对应字段完全一致，且
  `projection.frontier == cut`；
- event的`base_frontier`、`next_frontier`必须与envelope scope/writer/stream generation一致；
- durable event的next sequence严格单调前进，cursor与sequence共同校验；
- transient event的`base_frontier == next_frontier == current durable frontier`，只能用独立live sequence排序；
- 任一字段自相矛盾都按corruption + `ResetRequired`处理，不能选一个字段“相信”。

`Closed`只有在显式close envelope的scope/generations/final frontier与reducer current完全一致，并且其引用的
authoritative domain terminal已经durable apply时才是clean close。transport EOF、channel drop、terminal event前
close或不匹配close都是`UnexpectedEof/ResetRequired`，必须reopen observation或重新snapshot，不能把提前断流
当作完整terminal。

### 9.5 异步 paged projection

同步`ResidentVirtualSequence`只服务已经resident的framework数据；它不能在`item()`中阻塞、执行I/O或隐式加载完整
transcript。`sigil-application`必须另提供异步range/page contract：

```rust
pub struct ProjectionPageRequest {
    pub request_id: PageRequestId,
    pub scope: ProjectionScope,
    pub source_generation: SourceGeneration,
    pub at_frontier: ApplicationFrontier,
    pub query: PageQueryFingerprint,
    pub anchor: PageAnchor,
    pub direction: PageDirection,
    pub limit: NonZeroUsize,
    pub width_bucket: WidthBucket,
}

pub struct ProjectionPage {
    pub request_id: PageRequestId,
    pub scope: ProjectionScope,
    pub source_generation: SourceGeneration,
    pub at_frontier: ApplicationFrontier,
    pub query: PageQueryFingerprint,
    pub before: Option<StablePageCursor>,
    pub after: Option<StablePageCursor>,
    pub total: TotalCount,
    pub items: Vec<RendererSafeItem>,
}
```

Framework以`RangeNeeded` action请求页面，host异步执行并通过`RangeLoaded/RangeFailed` update回填；render/view
路径不await。必须定义：

- stable item ID与opaque page cursor；cursor只在同一scope/source generation/frontier/query schema内有效，不要求
  预载全部ID或正文；
- page size、并发request、resident page/item/byte、height cache都有硬上限；使用LRU/anchor-aware eviction；
- loading placeholder与estimated height，真实高度到达后做anchor compensation；
- range demand去重、request cancellation、superseded source generation与stale/cancelled response rejection；
- append/live-tail时cursor/frontier的延续或`ResetRequired`；
- selected/focused/pinned item的有界lease，不允许无限阻止eviction。

### 9.6 Application port

Conceptual API：

```rust
pub trait ApplicationPort: Send + Sync {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSession, ApplicationError>>;

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>>;

    fn cancel_page(
        &self,
        request: CancelPageRequest,
    ) -> BoxFuture<'static, Result<PageCancellationReceipt, ApplicationError>>;

    fn execute(
        &self,
        command: ApplicationCommandEnvelope,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>>;
}
```

这段只冻结语义，不锁定`BoxFuture`、`async_trait`或具体channel。实现必须允许in-process runtime、local
authenticated HTTP/IPC、deterministic fake application、disconnect/reset/reopen与host-derived urgent lane。
普通 `ApplicationPort` client永远不能提交presentation completion。

Page cancel是best-effort query lifecycle，不进入mutation command journal：`CancelledBeforeLoad`保证不会apply；
`TooLate/Completed`表示response可能已经在途。无论cancel receipt如何，adapter都必须按request id + scope + source
generation重新校验，已cancel/superseded的迟到response产生0 cache mutation和0 damage。future/drop本身不能作为
唯一cancel协议。

### 9.7 Trusted presentation obligation

现有egress disclosure必须在真实写出且最终compositor确认可见后才允许继续。这里有两个分离对象：

- public framework只看到application-neutral、不可反推出domain identity的`PresentationMarkerId`；
- `sigil-application` broker内部保留字段私有、non-Clone、无`Serialize/Deserialize`、一次消费的
  `TrustedPresenterCapability`；已注册presenter只得到internal crate公开但不可自行构造的session handle，
  capability本身不跨crate。

Capability至少绑定完整application instance、observer generation、operation、route/profile、logical/transport
destination、content digest、required surface、local marker/content revision、presenter session、sink与expiry，并
按值持有/封装现有exact pre-egress disclosure authority。caller supplied fingerprint不构成证明。

```rust
pub(crate) struct TrustedPresenterCapability {
    private: PrivateOneShotPresentationGrant,
}

// sigil-application is publish=false; fields/constructors remain private.
// The session is non-Clone, non-serializable, revocable and Debug-redacted.
pub struct TrustedPresenterSession {
    private: PresenterSessionBinding,
}

// Owned by sigil-application and free of sigil-tui / Ratatui types.
pub struct RendererNeutralPresentationObservation {
    pub marker_nonce: PresenterMarkerNonce,
    pub content_revision: PresenterContentRevision,
    pub frame_nonce: PresenterFrameNonce,
    pub terminal_epoch: PresenterTerminalEpoch,
    pub sink_completion_nonce: PresenterSinkCompletionNonce,
}

// Fields and constructor are private; non-Clone and non-serializable.
pub struct PresenterAttestation {
    private: SessionAuthorizedPresentationAttestation,
}

pub struct PresenterBroker {
    private: DurablePresentationCapabilityStore,
}

mod private {
    pub trait Sealed {}
}

// Public only from the publish=false sigil-application package. A registered
// presenter can call these sealed ports across Sigil crates, but an ordinary
// application client cannot implement them or construct their authority.
pub trait PresenterSessionAttestor: private::Sealed {
    fn attest(
        &mut self,
        observation: RendererNeutralPresentationObservation,
    ) -> Result<PresenterAttestation, PresentationError>;
}

pub trait PresenterCompletionPort: private::Sealed {
    fn complete(
        &self,
        attestation: PresenterAttestation,
    ) -> Result<ConsumedPresentationReceipt, PresentationError>;
}
```

`RendererNeutralPresentationObservation`是`sigil-application`自有的attestation输入，不包含
`sigil-tui-core`、Ratatui或terminal backend类型，也不是authority。它按不可信claim处理；broker必须根据
session binding与其内部capability重新验证application/observer generation、marker/content digest、route/profile、
logical/transport destination、terminal epoch/frame nonce、sink completion、expiry与single-consume状态。真正
authority是不可构造的registered presenter session和broker内one-shot capability。

`TrustedPresenterSession`自身是attestor authority：必须字段与constructor私有、non-Clone、无
`Serialize/Deserialize`、`Debug`脱敏，并绑定presenter principal、application instance、sink、session epoch、
expiry/revocation generation。composition root只把它移动给已注册presenter，不放入普通client registry、DTO、
log或IPC；drop/revoke后所有迟到attestation fail closed。

在TUI路径中，`sigil-tui-app`是唯一类型桥：它消费由`sigil-tui-ratatui` successful write/flush和final compositor共同生成、
constructor私有的committed framework receipt，读取renderer-neutral字段并机械映射为observation，再由其持有的
registered session封装成non-Clone/non-serializable attestation。framework evidence类型永不进入
`sigil-application` API；observation即使被普通client复制，也无法在没有session时兑换receipt。

执行顺序固定为：

```text
application creates durable obligation + one-shot presenter capability
  -> application broker binds capability to local marker + presenter session
  -> final compositor proves marker is visible after clip/overdraw
  -> terminal adapter confirms actual write/flush completion
  -> framework commits terminal epoch + presentation + marker evidence
  -> presenter maps framework receipt to a renderer-neutral observation
  -> registered session seals it into a one-shot attestation
  -> application broker locates and consumes exact capability durably
  -> application releases the bound wire effect
```

普通in-process/HTTP/IPC command client不能构造、序列化、clone或兑换该capability。`sigil serve`的通用bearer
不授予presenter权限；HTTP继续由server-owned durable disclosure journal/presenter完成。Desktop只有受限Tauri
Rust host能从其own sink completion生成证据时才可持有capability，browser“已展示”消息本身不能解锁wire。
若一个surface没有可信presenter，依赖presentation barrier的operation只能等待、取消或typed fail，不能让普通
client回传“已展示”。跨进程实现若确有需要，必须使用server-issued、single-use、client/session/sink-bound
capability并由server durable consume，且仍不得进入普通command catalog。

Capability只证明被审计sink完成规定的呈现，不证明人类已经阅读、理解或同意，也不替代authorization/effect
permit。

prepare、layout、callback返回、queue acceptance、clipped/hidden marker、被后续widget覆盖或
`IndeterminateAfterIo`都不是successful presentation。

一个frame可以提交多个marker，但每条底层disclosure必须有独立capability并分别durable consume；不得用“一帧
一个receipt”覆盖多条egress。测试还必须compile-fail证明capability、session与attestation不能
Clone/Serialize/Deserialize/公开构造，snapshot test证明session `Debug`不泄漏binding，并验证session
drop/revoke/leak attempt、crash、wrong observer generation与aggregate marker都不会产生receipt。

### 9.8 Versioned command/event migration manifest

粗粒度功能矩阵不能证明当前production worker协议已经完全迁移。R70.0必须从现有`WorkerCommand`、
`WorkerMessage`以及其他production surface command/event enum发现variant，并建立source-controlled manifest。
每行至少包含：

```text
schema_version / baseline_commit
source enum + variant + production/test/deprecated classification
disposition: map | merge | retire
target command group / event / projection
typed domain receipt
effect settlement + replay policy + host-derived lane
surface exposure: TUI-keyboard / TUI-mouse / Desktop / HTTP / CLI
NotExposed or retire rationale
contract/failure test id
migration phase
```

Generator可以是xtask或source parser，本RFC不锁定工具；但CI必须比较enum discovery与manifest。新增variant、
unknown target、缺失mapping、无理由`retire/NotExposed`、wildcard classifier或缺test id都阻断。`merge`必须列出
所有source variant并说明信息未丢失。

Runtime只实现一次`sigil-application` service。该service必须直接复用post-R71 kernel public resource/recovery
contract，不能把R71 transitional runtime facade复制成第二个状态源；迁移完成即删除TUI/HTTP/CLI对该transitional
facade的直接依赖。TUI/HTTP/CLI只做mechanical adapter，Desktop继续经typed HTTP schema消费同一service。对于manifest声明为shared的command，在相同authoritative fixture和normalized payload下，
TUI keyboard、TUI mouse、Desktop、HTTP、CLI必须产生相同typed domain receipt、frontier与domain-event序列；
transport metadata/authenticated identity可以不同，但不能改变domain result。确实不适用某surface时必须记录
`NotExposed + rationale`，不能留空。

## 10. Framework object model

### 10.1 Hybrid retained tree

首版不引入完整 React clone。framework 保留对性能和交互有必要的 identity/state：

- stable `NodeKey` 和 runtime `NodeId`；
- parent/children 与 event path；
- layout/style/content revision；
- focus、hover、active、selection、scroll、animation presentation state；
- measure/layout/paint cache；
- pointer behavior、semantic role 与 pending action binding。committed dispatch table只能由successful presentation
  原子替换，不能直接指向mutable retained node。

application data 仍由调用者拥有。一个概念上的声明 API：

```rust
pub trait Surface {
    type Message;
    type Action;

    fn update(
        &mut self,
        message: Self::Message,
        context: &mut UpdateContext<Self::Message, Self::Action>,
    ) -> Damage;

    fn view(&self, context: &mut ViewContext<Self::Message>) -> Element<Self::Message>;
}
```

`Element` 是一轮描述，reconciler 按 `(parent NodeId, NodeKey, NodeKind)` 复用 retained node。公共首版禁止：

- 依赖 component declaration order 作为 identity；
- 通过 unchecked `Any`/transmute 保存 props/context；
- 在 framework hooks 中复制 Sigil domain state；
- render callback 直接执行 application/OS side effect。

如果显式 state struct + builder 已足够，首版不发布 proc macro。

### 10.2 Damage model

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    None,
    Cursor,
    Interaction(NodeId),
    Paint(DamageRegion),
    Layout(NodeId),
    Viewport(NodeId),
    Full,
}
```

Scheduler union 规则：

- `Full` 吸收其他 damage；
- `Layout(parent)` 吸收 descendant 的 layout/paint；
- `Viewport` 只更新 transform、visible range、hit/text map，除非 variable height cache invalid；
- `Paint` 不触发 application projection 或 text measure；
- `Interaction` 更新event path/scope/action snapshot，可以复用cell/hit geometry，但仍需要successful
  presentation barrier后才能commit；
- `Cursor` 只在 cursor presentation 改变时输出；
- theme color generation 产生 Paint；theme metrics/width generation 产生 Layout；
- terminal resize/capability width method change 产生 Full。

### 10.3 Render transaction

```rust
pub struct PreparedRender<A> {
    pub generation: FrameGeneration,
    pub surface: CellSurface,
    pub next_presentation: Arc<CommittedPresentation<A>>,
    pub metrics: FrameMetrics,
}

pub enum PresentationSessionState<A> {
    Synchronized {
        current: Arc<CommittedPresentation<A>>,
    },
    AwaitingPresent {
        current: Arc<CommittedPresentation<A>>,
        generation: FrameGeneration,
        interaction_barrier: bool,
    },
    Presenting {
        current: Arc<CommittedPresentation<A>>,
        attempt: PresentAttemptId,
        generation: FrameGeneration,
    },
    Poisoned {
        last_confirmed: Option<Arc<CommittedPresentation<A>>>,
        fault: PresentFault,
    },
    Recovering { next_epoch: TerminalEpoch },
    Closed,
}
```

状态转换与input policy固定为：

| State/result | Next state | presentation-bound input |
|---|---|---|
| `Synchronized(F0)` + prepare F1 | `AwaitingPresent(F0,F1)` | 无interaction barrier时可继续只读F0 |
| `AwaitingPresent` + begin effect | `Presenting` | 暂停，返回`PresentationPending` |
| `Presenting` + `Presented(F1)` | `Synchronized(F1)` | 原子切换后恢复 |
| `AwaitingPresent` + `NotStarted`，无barrier | `Synchronized(F0)` | F0仍有效 |
| `AwaitingPresent` + `NotStarted`，有barrier | 保留pending/retry | 返回`PresentationPending`，不执行A或B |
| `Presenting` + `IndeterminateAfterIo` | `Poisoned` | 全部fail closed |
| `Poisoned/Recovering` + full resync success | `Synchronized(Fn,new epoch)` | 丢弃旧epoch input后恢复 |
| recovery再次indeterminate/fatal | `Poisoned/Closed` | restore/quit only |

“presentation-bound input”包括coordinate、focused keyboard、approval/modal shortcut、selection/copy target与pointer
capture/drag；host-owned quit/restore以及不依赖可见target的global emergency cancel走独立control path。dispatch开始时
只加载一次`Arc<CommittedPresentation<A>>`，并持有到capture→target→bubble→default全部结束。

Pipeline：

```text
pending model/input updates
  -> union Damage
  -> reconcile retained identities
  -> measure/layout only invalid nodes
  -> compute virtual visible range
  -> cull and paint visible nodes
  -> build next cell surface + immutable InteractionSnapshot + marker evidence
  -> Ratatui/backend draw + actual write/flush completion
  -> Presented: atomically commit CommittedPresentation
  -> NotStarted: follow the explicit interaction-barrier policy; never publish pending dispatch
  -> IndeterminateAfterIo: poison terminal epoch; neither old nor new snapshot remains interactive
```

`PreparedRender`不能被input查询。每个terminal epoch同一时间最多一个pending/presenting attempt。只有
`Presented(TrustedPresentReceipt)`才交换generation；`NotStarted`必须带adapter提供的zero-I/O proof；普通
Ratatui `Err`不能调用“保留旧frame”的abort路径。

### 10.4 Host-owned loop

framework 不创建 Tokio runtime、不拥有 application worker，也不规定唯一 `select!`。它提供：

```rust
pub trait UiRuntimeDriver {
    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<UiAction>;
    fn apply_external(&mut self, update: SurfaceUpdate) -> Damage;
    fn next_wake(&self) -> Option<Instant>;
    fn needs_present(&self) -> bool;
    fn prepare(&mut self, viewport: Viewport) -> Result<PreparedRender, RenderError>;
    fn finish_present(&mut self, outcome: PresentOutcome);
    fn presentation_state(&self) -> &PresentationSessionState;
}
```

Sigil composition root 保留当前 terminal/application/control/panic/deadline 公平多路等待和 event batching。
framework 只要求：

- coordinate event始终绑定读取时的committed terminal epoch/generation；已有layout/viewport/interaction damage时
  可以先present以降低stale率，但不能转而查询pending tree；
- key/application burst 可以继续合并，除非 action 要求 synchronous presentation barrier；
- urgent cancel/approval/pause/shutdown 不因普通 text delta 或 mouse move flood 排队；
- continuous/live frame 由引用计数或 deadline 驱动，idle 时没有固定 tick。
- `Poisoned`状态只接受owner-thread recovery/teardown，所有coordinate/focus-target dispatch与trusted
  presentation completion fail closed。

## 11. Mouse、hit-test 与事件传播

### 11.1 为什么不能在 click-time 重新计算坐标

用户提出的“只在点击时实时计算坐标并适时缓存”看似减少平时工作，实际会破坏 frame consistency：

1. application model 可能已收到新 event，但 terminal 尚未成功 present；click-time 重算会命中用户尚未
   看到的新布局。
2. z-order、nested clip、modal、scroll transform、wide grapheme、selection mapping 都必须与 paint 顺序
   完全一致；独立重算等于维护第二个 renderer。
3. click-time tree traversal 让输入延迟随可见/retained node 数增长，并把最昂贵的工作放在用户等待响应的
   路径上。
4. drag、hover、wheel、pointer capture 不是“偶尔一次 click”，仍会产生高频 coordinate events。
5. cache invalidation 仍需要知道 layout/paint generation，最后会重新发明 presented-frame snapshot。

正确方案是：

> 在已经必须执行的 render transaction 中，零额外布局地登记最终 geometry；input-time 只查 committed
> cache。缓存不是对 click-time 重算结果的补丁，而是 successful presented frame 的正式组成部分。

### 11.2 `CommittedPresentation` 与 `InteractionSnapshot`

```rust
pub struct PresentedFrame {
    generation: FrameGeneration,
    viewport: Viewport,
    surface_digest: CellSurfaceDigest,
    hit_map: HitMap,
    text_maps: TextMapStore,
    cursor: Option<CursorPresentation>,
    presentation_markers: Vec<PresentedMarkerEvidence>,
}

pub struct PresentedMarkerEvidence {
    marker: PresentationMarkerId,
    content_revision: ContentRevision,
    coverage: MarkerCoverage,
}

pub struct CommittedPresentation<A> {
    terminal_epoch: TerminalEpoch,
    frame: PresentedFrame,
    interaction: Arc<InteractionSnapshot<A>>,
}

pub struct InteractionSnapshot<A> {
    generation: FrameGeneration,
    model_scope: OpaqueModelScope,
    model_revision: OpaqueModelRevision,
    focus: FocusPresentation,
    modal_scopes: ModalScopeSnapshot,
    dispatch: FrozenDispatchTable<A>,
}

struct FrozenDispatchEntry<A> {
    target: InteractionTargetId,
    event_path: Box<[InteractionTargetId]>,
    scope: InteractionScopeId,
    pointer_policy: PointerBehavior,
    default_policy: DefaultBehavior,
    action: Option<FrozenAction<A>>,
}
```

`PresentedFrame`保存screen facts；`InteractionSnapshot`保存该screen对应的不可变dispatch facts。两者只作为一个
`CommittedPresentation`交换。它们不持有application model；`FrozenAction<A>`只能产生framework action/local
update，不能直接执行I/O或domain effect。Sigil adapter把exact opaque binding作为generic `A` 的一部分，
application仍会重新验证authority。

`PresentedMarkerEvidence`与committed framework receipt的字段/constructor保持framework-private，只能由final
compositor + trusted terminal completion生成。只读renderer-neutral getter可供debug/test及`sigil-tui-app`机械
映射application-owned observation；该evidence/observation本身都不是authority，不能脱离registered presenter
session兑换application receipt。

`InteractionTargetId`是`{frame generation, target slot}`的逻辑identity。dense grid为保持4-byte cell只存
`TargetSlot(u32)`，并且只能在同一`InteractionSnapshot`的arena中解释；slot不得拿到current retained tree中
重新解析。因此即使pending tree删除/复用node，也不会发生A→B ABA。

### 11.3 Dense hit map baseline

首个资格化实现是双缓冲 dense cell map：

```rust
pub struct DenseCellHitMap {
    width: u16,
    height: u16,
    cells: Vec<TargetSlot>,
}
```

规则：

- `TargetSlot(0)`表示无target；其余slot只索引同generation immutable dispatch arena，不能跨frame复用；
- next grid 在 paint order 中写入，后写覆盖前写，所以视觉 topmost 与 input topmost 一致；
- registration 使用与 paint 相同的 clip/scissor intersection；
- 只 materialize visible/overscan node；被 cull 的 node不能进入 grid；
- node 是否成为 target 由明确 `PointerBehavior` 决定；非交互 child 可以 promote 到最近 semantic ancestor，
  也可以作为 target 后沿 parent bubble，不能由偶然实现决定；
- successful present后与dispatch/text/cursor/marker一起交换；`NotStarted`不交换；`IndeterminateAfterIo`使current
  terminal epoch整体失效；
- hover 只在 pointer cell 或 committed hit generation 改变时复查；
- hit lookup 零分配、O(1)。

双缓冲内存上界为：

```text
2 × width × height × size_of::<u32>()
```

即使 400×120 terminal 也约为 375 KiB，且与 transcript 条目数无关。构建成本可能受 overlapping full-screen
rect 的 overdraw 影响，因此必须测量 `hit_cells_written`，并使用以下优化：

- 只为 pointer-participating semantic nodes 写 grid；
- layout/hit topology 未变的 paint-only frame 复用 committed hit map；
- viewport translate 优先更新 visible rows/dirty tiles，而不是重建 application projection；
- nested full-screen wrapper 默认 `pointer-events: pass-through`；
- benchmark 证明 dense 不适合之前，不增加复杂 spatial tree。

`HitMap` 保持 trait/enum strategy seam。未来 `RowIntervalHitMap` 可以服务超大 viewport，但不能改变
`CommittedPresentation`、paint-order、clip 和 commit 语义。

### 11.4 事件路由

Pointer event 路由固定为：

```text
normalized input
  -> validate terminal epoch + frame generation
  -> PresentedFrame.hit_map.lookup(x, y) -> TargetSlot
  -> resolve FrozenDispatchEntry from the same InteractionSnapshot
  -> apply the committed modal/focus scope
  -> capture over the committed event path
  -> target
  -> bubble over the committed event path
  -> default behavior (focus, selection, scroll) unless prevented
  -> collect action + damage
```

公共事件至少支持：

- down/up/click/double-click；
- move/over/out；
- wheel with horizontal/vertical delta；
- drag-start/drag/drag-end/drop；
- pointer capture/release；
- modifier、button、position 与 frame generation；
- `stop_propagation`、`prevent_default`。

同一 cell 连续 move 且目标未变、没有 move subscriber、selection/drag 未激活时必须直接 `Noop`。连续 wheel
可在保留方向和 modifier 的前提下聚合 delta；down/up/click 的顺序事件禁止丢弃或重排。

### 11.5 Stale target 与 resize

- event自带读取时的`TerminalEpoch + FrameGeneration`；epoch/generation不匹配立即`StaleTarget`；
- dispatch只使用committed exact action binding；current/pending handler table永远不参与旧frame事件；
- committed binding被application判定stale时返回typed rejection并请求projection refresh，不能fallback到current
  binding；
- target slot不存在/损坏时fail closed并请求full presentation rebuild，不转而命中新model；
- Crossterm resize 立即使 coordinate frame stale；下一 coordinate event 只能在新 frame 后处理，或在有界
  single-slot queue 中保留最后 move，click/down/up 默认不跨 resize 重放。

## 12. Keyboard、focus、modal、selection 与 input method

### 12.1 Normalized input

`sigil-tui-core::InputEvent` 不暴露 Crossterm 类型：

```rust
#[non_exhaustive]
pub enum InputEvent {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Paste(PasteEvent),
    FocusChanged(bool),
    Resize(Viewport),
    Tick(ClockInstant),
}
```

Crossterm adapter 负责 Kitty keyboard、bracketed paste、mouse encoding、focus change 与 resize 的转换。
未知 sequence 或 capability downgrade 必须产生 typed diagnostic，不能 panic 或把 bytes 当用户文字。

### 12.2 Focus graph

每个 focus node 包含稳定 NodeId、enabled、focusable、tab index、semantic role、spatial rect 与 scope。

规则：

- Tab/Shift-Tab 按 `(scope, tab_index, stable paint order)`；
- disabled/hidden/cull node 自动退出 focus candidates；
- pointer down 是否夺焦由 node policy 决定；
- modal/popover 建立独立 `FocusScope`，打开时保存 previous focus，关闭时优先恢复；
- target 删除时按明确 fallback 选择最近 focusable ancestor、scope default 或无焦点；
- keyboard 与 mouse 必须对所有关键动作保持 parity；鼠标不是唯一入口。

### 12.3 Input layer 与 modal barrier

借鉴 `ratatui-kit` 的 input layer 语义，但不在每个 event 重建 handler table：

- retained node保存pending handler/action description；committed handler/action只存在于immutable
  `InteractionSnapshot`；
- global shortcut、active scope、modal layer、target path 分阶段 dispatch；
- modal 的 `blocks_lower` 在 layer selection 阶段截断背景 shortcut；
- event priority 与视觉 z-order 分离但明确排序；
- registration order 只作为最终稳定 tie-breaker；
- handler mutation在reconcile阶段写pending table，并产生`Damage::Interaction`；只有successful presentation后才
  替换committed table，不以destructive dispatch强制下一frame。

### 12.4 Text mapping 与 selection

宽字符、combining mark、wrapped Markdown 和虚拟列表要求 frame 同时保存 visible cell 到逻辑位置的映射：

```rust
pub struct TextCellMapping {
    pub target: InteractionTargetId,
    pub logical_item: ItemId,
    pub byte_offset: usize,
    pub grapheme_index: usize,
    pub visual_grapheme_index: usize,
    pub bidi_level: u8,
    pub continuation_cell: bool,
}
```

selection、composer cursor、click-to-position 都查询 `TextMapStore`，不重新 wrap 文本。Text segmentation 遵循
Unicode extended grapheme cluster；display width由adapter的versioned `WidthMethod`决定。terminal width method
改变属于Full damage。

### 12.5 Bidirectional text（V1 必选）

因为qualification workload包含RTL，V1不能只用grapheme segmentation冒充bidirectional support。core必须实现
versioned `BidiPolicy`，并以Unicode UAX #9为normative baseline：

```rust
pub enum BidiPolicy {
    AutoParagraph,
    ForceLeftToRight,
    ForceRightToLeft,
    LiteralNoReorder,
}

pub struct VisualLineMap {
    pub paragraph_direction: ParagraphDirection,
    pub embedding_levels: Box<[u8]>,
    pub visual_to_logical: Box<[GraphemeIndex]>,
    pub logical_to_visual: Box<[VisualSpan]>,
}
```

规则：

- logical source order是存储、application DTO和默认copy order；visual order只属于frame-local presentation；
- paragraph先解析embedding/isolate，再按wrapped line执行UAX #9 line reordering；每个visible line保存双向map；
- LRI/RLI/FSI/PDI、embedding/override与zero-width formatting control参与算法但不伪造可点击cell；
- click、cursor、selection、highlight与hit/text map使用visual cell → logical grapheme映射；selection可以产生多个
  visual segment，但copy默认按logical range输出；
- bidi algorithm/version、base direction、width method与wrap width都进入text/measure cache key；
- mixed RTL/LTR数字、nested isolates、directional override、malformed/unbalanced controls和spoofing/security fixture
  必须覆盖；不支持某能力时显式选择`LiteralNoReorder`并诊断，不能声称RTL qualification通过。

Terminal是否自行做bidi不能靠猜测。默认由framework application-reorder；若未来探测到可信terminal bidi模式，
必须以capability generation隔离cache并通过同一logical↔visual contract。

## 13. Layout、滚动与真实 virtualization

### 13.1 Layout strategy

首版继续使用 Rust/Ratatui-friendly layout primitives，不引入 Yoga。core layout 必须支持：

- row/column、fixed/min/max/fill、gap、margin、padding；
- absolute/overlay、z/layer；
- clip/overflow；
- responsive breakpoint；
- content measurement；
- viewport transform。

复杂 CSS parity 不是目标。所有 layout node 都有 revision；measure cache key 至少包括：

```text
NodeId
+ content_revision
+ available_width/height bucket
+ theme_layout_generation
+ width_method_generation
+ widget_measure_options
```

### 13.2 Scroll 不修改 content layout

普通 wheel/PageUp/anchor follow 只更新 viewport offset/transform。只有以下情况允许 content relayout：

- terminal width/height 或 responsive region 改变；
- item content revision 改变；
- theme layout token/width method 改变；
- variable-height item 首次 measure 或 correction；
- widget structural children 改变。

scrollbar、visible range、hit/text map 可以变化，但不得调用 `ApplicationProjection -> SurfaceModel` 全量重建。

### 13.3 Resident virtual sequence

公共virtual-list对完整、已经resident的数据提供同步contract：

```rust
pub trait ResidentVirtualSequence {
    type ItemId: Copy + Eq + Hash;
    type Item;

    fn len(&self) -> usize;
    fn id_at(&self, index: usize) -> Self::ItemId;
    fn item(&self, id: Self::ItemId) -> Option<Self::Item>;
    fn revision(&self, id: Self::ItemId) -> u64;
}
```

实现维护：

- stable item ID；
- visible logical range + bounded overscan；
- width-bucketed variable-height cache；
- prefix-sum/Fenwick tree 或等价 O(log N) height index；
- logical top anchor `{item_id, intra_item_row}`；
- bottom/live-tail anchor；
- height correction 后 anchor compensation；
- visible + overscan node materialization/recycling。

`item()`只能查询内存resident数据，禁止I/O、await、锁住application runtime或隐式复制page。Paged source不
实现这个“全量resident”trait，而使用独立状态：

```rust
pub enum SequenceExtent {
    Known(usize),
    Estimated { lower_bound: usize, estimate: usize },
    Unknown,
}

pub enum VirtualSlot<I, T> {
    Ready { id: I, item: T, revision: u64 },
    Pending { placeholder: PlaceholderId, estimated_height: u32 },
    Failed { placeholder: PlaceholderId, error: PageErrorView },
}

pub trait PagedVirtualSequence {
    type ItemId: Copy + Eq + Hash;
    type Item;

    fn extent(&self) -> SequenceExtent;
    fn resident_slot(
        &self,
        logical: LogicalSlot,
    ) -> Option<VirtualSlot<Self::ItemId, &Self::Item>>;
    fn demand(&mut self, range: LogicalRange) -> Option<RangeNeeded>;
}
```

`PagedVirtualSequence`只暴露resident/pending placeholder和host effect，不要求`id_at()`遍历未知数据；page cache
内部可以为已加载slice复用`ResidentVirtualSequence`算法。

强不变量：10万条transcript时retained UI node数与`visible + overscan + active pinned`同阶，不能与总item数
同阶。framework synthetic benchmark可以使用resident fixture；Sigil端到端gate必须使用9.5的async page source，
不得先加载完整ID/body再宣称virtualization完成。

### 13.4 Async page 与 viewport 集成

- virtual list根据anchor、estimated heights和overscan产生`RangeNeeded`，不直接调用application；
- 同一source generation的重叠range request合并，离开viewport的request可取消；
- page response先校验scope/source generation/frontier/request id，再进入bounded resident cache；
- placeholder本身有stable presentation identity，但不得携带旧page action binding；
- eviction保留height summary和anchor所需索引，不保留item正文、widget、handler closure；
- page到达、eviction或estimate correction只对受影响range产生`Viewport/Layout` damage；
- scroll hot path不能等待page；缺页时继续显示明确loading/unavailable状态并保持keyboard/mouse可预测。

### 13.5 Streaming 与锚点

- live-tail 状态下 append 保持 bottom anchored；
- 用户离开 tail 后，stream delta、new item、height correction、info rail 显隐和 resize 都保持 logical top
  anchor，不把用户拖回 tail；
- streaming item 使用 stable ID + content revision 原位更新 measure cache；
- 多个 delta 在 frame budget 内 coalesce；final/durable boundary 不丢失；
- selected/expanded/focused item 即使暂时离开 overscan也保留轻量 presentation state，不保留整棵 visual node。

### 13.6 Culling 与 virtualization 的区别

Paint culling 只是不绘制 viewport 外的 retained node；它仍可能保留 O(N) node 和 layout bookkeeping。
RFC-0070 的验收要求是真 virtualization。OpenTUI ScrollBox 的 culling 可作为 visible selection 算法参考，
不能作为 100k transcript 完成证明。

## 14. Ratatui renderer 决策

### 14.1 为什么保留 Ratatui

Ratatui 已提供：

- backend-neutral `Terminal<B>`；
- screen-sized double buffer；
- minimal cell diff；
- Crossterm/Termion/Termwiz/TestBackend 等 adapter；
- mature text/style/widget ecosystem；
- full-frame 与 test buffer 的清晰语义。

当前主要热点位于 Ratatui draw 之前：`AppState -> ViewModel`、Markdown/wrap、timeline measure、第二套
LayoutSnapshot 和 input-triggered redraw。先修复这些边界，风险和收益都优于替换渲染栈。

### 14.2 Renderer contract

`sigil-tui-ratatui` 可以在内部拥有 retained `CellSurface`，但每次 Ratatui draw 必须按其规范把完整 viewport
复制/绘制到当前 frame；不能只画 damage rect 后让未画 cell 被清空。

```rust
let prepared = runtime.prepare(viewport)?;
match adapter.try_admit_present(&prepared) {
    PresentAdmission::NotStarted(proof) => {
        runtime.finish_present(PresentOutcome::NotStarted(proof));
    }
    PresentAdmission::Admitted(admission) => {
        match terminal.try_draw(|frame| {
            adapter.paint(&prepared.surface, frame.buffer_mut())
        }) {
            Ok(completed) => {
                match admission.confirm_completed(completed) {
                    Ok(receipt) => {
                        runtime.finish_present(PresentOutcome::Presented(receipt));
                    }
                    Err(fault) => {
                        runtime.finish_present(PresentOutcome::IndeterminateAfterIo(fault));
                        terminal_owner.restore_or_terminate()?;
                    }
                }
            }
            Err(error) => {
                runtime.finish_present(PresentOutcome::IndeterminateAfterIo(
                    admission.into_fault(error),
                ));
                terminal_owner.restore_or_terminate()?;
            }
        }
    }
}
```

具体API以实现期Ratatui版本为准。`try_admit_present`只能检查adapter-owned queue/backpressure且必须在调用
Ratatui前完成；一旦进入`try_draw`，任意error都按`IndeterminateAfterIo`处理。原因是Ratatui可能已经resize
internal buffer、调用backend draw、改变cursor、交换buffer或写出部分diff，错误不能证明物理terminal仍对应旧
frame。

`TrustedPresentReceipt`构造函数仅在adapter内部可见，并绑定terminal epoch、prepared generation与实际
write/flush completion；字段私有、non-Clone、一次消费。异步writer只有收到真实write/flush completion ACK后
才能创建receipt；enqueue success
最多表示admission，不允许commit interaction或obligation。poisoned epoch恢复后必须分配new epoch、清空backend
diff baseline、full repaint并重新open application projection；恢复前不接受coordinate input。

`confirm_completed`若包含任何可能失败的epoch/generation/sink/completion校验，其失败发生在terminal effect之后，
必须显式映射`IndeterminateAfterIo`；禁止用`?`或early return绕过`finish_present`。只有通过typestate证明不可失败
的private constructor才可直接生成receipt。

### 14.3 Future backend seam

如果资格化 benchmark 证明以下任一条件持续成立，才启动新 renderer RFC：

- 在 application projection/layout/hit work 已满足预算后，screen buffer copy/diff仍占 steady frame 超过 40%；
- 120×40 steady scroll p95 仍无法满足 16.7 ms 且 profiler 指向 Ratatui buffer pipeline；
- image/remote/SSH backend 需要 Ratatui 无法表达的原子 frame；
- partial patch 能在至少两种 terminal/backend 上通过 frame consistency tests。

未来 backend 仍必须实现相同 `PreparedRender -> PresentOutcome -> commit CommittedPresentation` contract；不得要求
`sigil-tui-app` 改写。

## 15. Theme、style 与 terminal capability

### 15.1 Theme layering

主题解析链固定为：

```text
TerminalCapabilities
  -> ThemeSpec (base palette + metrics + symbols + motion)
  -> ComponentTheme::from_theme
  -> subtree ThemeProvider override
  -> per-node StylePatch
  -> state slot (normal/hover/focus/active/selected/disabled/error)
  -> resolved backend style
```

所有 standard widget 必须从 theme derive style；禁止在 widget default 中散落 hard-coded product color。

### 15.2 Semantic token

公共 theme 至少覆盖：

- surface：base/panel/elevated/input/overlay/rail；
- text：primary/muted/subtle/disabled/inverse/link；
- border：normal/subtle/strong/focus；
- accent：primary/secondary；
- status：info/success/warning/error/pending/running；
- selection：hover/focus/selected/active；
- diff：added/removed/changed/context；
- syntax base roles；
- spacing/density/padding/gap；
- border glyph/style；
- symbols：checkbox/disclosure/spinner/status/focus marker；
- motion：enabled/reduced、frame interval；
- cursor style。

Sigil-specific tool/plan/task tokens留在 `sigil-tui-app` 的 theme extension namespace，并以通用 semantic tone
作为 fallback。

### 15.3 Capability fallback

`TerminalCapabilities` 至少表达：

- truecolor / ANSI-256 / ANSI-16 / monochrome；
- Unicode/ambiguous-width/emoji width method；
- mouse movement、focus change、bracketed paste、Kitty keyboard；
- OSC52/clipboard；
- image protocol；
- light/dark/unknown background；
- reduced motion / `NO_COLOR` preference。

Theme resolver 必须 deterministic quantize color，保证高对比与键盘 focus 不依赖颜色。能力缺失时可以降低
装饰，不能删除 action、focus indicator、status meaning 或 keyboard parity。

### 15.4 Theme schema 与 invalidation

- `ThemeSpec`、`ThemePatch` 为 `#[non_exhaustive]`；
- serde 置于 optional feature，payload 包含 `schema_version`；
- 未知 token 保留/告警，非法颜色或 width-affecting symbol fail typed validation；
- color-only update增加 `paint_generation`；
- spacing、border width、symbol display width、density增加 `layout_generation`；
- syntax theme有独立 generation，不强制重建非 syntax timeline cache；
- runtime preview使用同一 resolver，保存前后不维护两套主题路径。

### 15.5 现有主题能力迁移

以下当前能力是迁移 parity gate：

- `sigil_dark`；
- `solarized_dark` / `solarized_light`；
- `gruvbox_dark`；
- `nord`；
- `high_contrast_dark`；
- `#RRGGBB` semantic override；
- syntax theme；
- contrast/semantic diagnostics；
- `/config` live preview；
- theme change重建必要 render cache，但不触碰 session/control/provider context。

## 16. Standard widgets 与 extension API

### 16.1 首批 public widgets

- layout：`View`、`Stack`、`Overlay`、`Border`、`Spacer`；
- text：`Text`、`WrappedText`、`RichText`、`CodeBlock`；
- input：single/multiline input、search、completion surface；
- collection：select、multi-select、table、tree、virtual list；
- viewport：scroll view、scrollbar、sticky/pinned region；
- control：button、toggle、tabs、menu；
- overlay：modal、popover、tooltip、toast；
- content primitives：card、status line、markdown block、diff/search/code rails。

Agent、Plan、Task、approval、provider 和 MCP 不是 public core widget identity；Sigil 可以用通用 card/status/input/
virtual-list primitives组合自己的产品组件。

### 16.2 Extension API

发布前必须明确 stable extension surface：

- custom node/widget render只能通过bounded `RenderContext`；
- measure/layout；
- semantic role/style resolution；
- action/event registration；
- virtual sequence；
- test snapshot；
- capability query。

内部 tree/reconciler/scheduler mutation API 默认 sealed 或 doc-hidden。公共 trait 只有在下游实现是正式能力时才
开放；否则使用 builder/newtype，避免把内部 lifetime/ownership 永久冻结。

```rust
pub trait Widget<Message> {
    fn measure(&self, constraints: Constraints, context: &MeasureContext) -> MeasuredSize;
    fn render(&self, context: &mut RenderContext<'_, Message>);
}
```

`RenderContext`只允许在assigned rect与effective scissor内：

- 写cell/style；
- 登记target/event/action；
- 登记text logical↔visual mapping；
- 登记cursor与presentation marker；
- 读取resolved theme/capability，不访问application/terminal host。

Ratatui native widget不得拿到production raw `Frame`或全屏`Buffer`。adapter在local scratch `Buffer`中render后，
按assigned rect/scissor composite回surface；默认pointer pass-through、无semantic action、无obligation。需要交互或
marker时仍必须通过同一个`RenderContext`显式登记。final compositor在所有overlay/custom paint结束后校验marker
要求的cell/region仍可见；clipped、overdrawn或内容digest不匹配的marker不得进入committed evidence。

debug/headless host对越界paint、wide-grapheme截断、未登记interactive visual、protected marker overdraw与clip/hit/
text不一致fail。此API不把同进程恶意Rust代码当sandbox，但必须消除正常extension无意破坏presentation invariant
的能力。

### 16.3 Accessibility 基线

Terminal accessibility有限，但 framework 必须提供：

- keyboard parity 与稳定 focus order；
- visible focus indicator；
- high-contrast、monochrome、reduced-motion；
- semantic role/label/value/state tree 的 headless snapshot；
- copyable plain-text surface；
- status 不只靠颜色/动画；
- future screen-reader adapter seam。

OpenTUI 当前路线图仍把 screen-reader 支持列为后续项，因此不能把采用 OpenTUI 等同于自动获得 accessibility。

## 17. Performance model 与资格化门禁

### 17.1 性能不是单一 FPS

每个 frame 分段记录：

```text
application projection
reconcile
measure
layout
virtual range
paint
hit map
Ratatui buffer apply/diff/flush
input dispatch
present acknowledgement
```

公开 `FrameMetrics`/observer，不强制某个 telemetry backend：

```rust
pub struct FrameMetrics {
    pub generation: FrameGeneration,
    pub damage: DamageSummary,
    pub retained_nodes: usize,
    pub materialized_nodes: usize,
    pub measured_nodes: usize,
    pub painted_nodes: usize,
    pub hit_cells_written: usize,
    pub changed_cells: usize,
    pub cache_hits: CacheHitMetrics,
    pub phase_durations: PhaseDurations,
}
```

Present observer另记录`terminal_epoch`、attempt id、`Presented/NotStarted/IndeterminateAfterIo`、actual write/flush
completion latency、resync attempts、poisoned-input rejection与trusted marker ACK count。queue latency必须与真实
writer completion latency分开，不能把enqueue计为present。

### 17.2 Complexity hard invariants

这些比某台 CI 机器的微秒数更稳定，必须作为单测/benchmark counter 断言：

1. hit lookup：O(1)、warm path 零 allocation；
2. unchanged pointer move：0 application projection、0 layout、0 frame；
3. ignored key/event：0 frame；
4. 一个 event/application burst batch：最多 1 successful present；
5. ordinary scroll：不重新 measure 未失效 item，不全量扫描 transcript；
6. virtual materialized nodes：`O(visible + overscan + pinned)`；
7. height locate/update：O(log N) 或更好；
8. idle 且无 deadline/live request：0 periodic draw；
9. theme color-only change：0 application projection、0 text measure；
10. click：0 layout；
11. `NotStarted`：0 committed generation、0 ACK；`IndeterminateAfterIo`：在new epoch full resync前0
    presentation-bound semantic input、0 ACK；
12. application stream text delta可以 coalesce，但 approval/cancel/user-input/terminal event 不可因 coalesce 丢失。
13. 每次dispatch的hit/path/modal/focus/action/binding/frontier全部来自同一committed snapshot；mixed-generation
    dispatch计数必须为0。

### 17.3 Reference workloads

Benchmark fixture 必须固定 seed、terminal size、theme、width method 和 content distribution：

| Workload | Dataset | 关键观测 |
|---|---|---|
| `scroll-10k` | 10k mixed variable-height items, 120×40 | input-to-present p50/p95/p99、measure/paint nodes |
| `scroll-100k` | 100k synthetic resident IDs/items, 120×40 | framework retained/materialized complexity、总量独立性 |
| `paged-scroll-100k-cold` | fake application 100k extent、cold cache | 未预载ID/body、page/in-flight/bytes bound、cancel/stale、anchor |
| `mouse-move-flood` | 100k same-target moves | hit latency、0-frame count、allocations |
| `wheel-flood` | 10k wheel inputs | coalescing、anchor、frames/input ratio |
| `stream-burst` | 1k deltas + durable terminal | coalescing、terminal latency、final preservation |
| `modal-overlap` | nested clip + z + focus scopes | hit correctness、focus restore |
| `resize-storm` | 80×24 ↔ 240×80 | stale generation、relayout、settle latency |
| `theme-hot-swap` | color-only + metric change | correct damage class、cache invalidation |
| `unicode-selection` | CJK/emoji/combining/UAX #9 mixed RTL samples | logical↔visual cell map、cursor/selection/copy correctness |

### 17.4 Absolute与相对预算

在仓库记录 qualification host（CPU、OS、Rust、profile、terminal backend）后，release build 目标为：

- `scroll-10k` steady input-to-successful-present p95 ≤ 16.7 ms，p99 ≤ 33.3 ms；
- `scroll-100k` p95 不高于 `scroll-10k` 的 1.25 倍；
- `paged-scroll-100k-cold`的render/input hot path I/O count = 0，resident page/item/byte与in-flight request不超过
  fixture声明硬上限；
- same-target no-op mouse dispatch p95 ≤ 250 µs，且 100% 零 frame；
- click target lookup + framework dispatch（不含 application handler）p95 ≤ 1 ms；
- 1k stream delta burst不超过 60 present/s，durable terminal event在两个 frame budget内可见；
- idle 30 秒且无 live/deadline时 draw count = 0；
- 400×120 dense double hit grid容量 ≤ 400 KiB，不含 allocator rounding；
- 任一稳定 benchmark相对已批准 baseline 退化 >10% 时 CI 报警，>20% 阻断；
- 任何 complexity invariant 违反直接阻断，不用微基准噪声豁免。

绝对预算不是对所有机器的产品 SLA；它是 qualification host 的工程 gate。低速 terminal/backend 可以降低
animation/stream present rate，但不能破坏 frame consistency 或 input/control priority。

### 17.5 Profiling决策门

每个性能修复必须先标注主要 phase。禁止用以下方式掩盖热点：

- 无证据增加全局 cache；
- 用 stale geometry换低延迟；
- 删除 mouse/theme/selection capability；
- 降低 correctness test数据量；
- 把 full transcript retain在另一个层；
- 在 UI thread 执行 filesystem/process/network；
- 通过固定 sleep/debounce延迟所有 input。

## 18. Testing strategy

### 18.1 Headless deterministic host

`test-util` feature至少提供：

```rust
TestHost::new(width, height)
    .render_if_dirty()
    .dispatch(input)
    .apply_external(update)
    .advance_time(duration)
    .resize(width, height)
    .frame()
    .metrics();
```

Test host使用 manual monotonic clock、memory terminal、fake clipboard/capabilities，不读取真实环境变量、当前
时间或终端状态。

### 18.2 Snapshot dimensions

测试可以独立断言：

- cell character；
- fg/bg/modifier/style；
- cursor position/style；
- hit map target per cell；
- event path、modal/focus/input-layer snapshot与exact action token；
- opaque binding fingerprint、projection scope/frontier与terminal epoch；
- focus/semantic tree；
- text mapping；
- frame presentation markers（非application authority）；
- damage/phase counters；
- application actions/host requests。

只做字符串 golden 不足以验证鼠标、wide glyph、theme 和 presentation receipts。

### 18.3 Contract 与 property tests

至少覆盖：

1. render-produced cell 与 hit target来自同一 node；
2. overlapping region按 paint order topmost；
3. nested clip对 paint/hit/text map一致；
4. `NotStarted`不交换presentation；`IndeterminateAfterIo`poison epoch并拒绝semantic input/ACK；
5. stale/generational target不命中新布局，不发生slot ABA；
6. capture/target/bubble、stop/default；
7. modal blocks lower shortcuts；
8. focus restore/disabled skip；
9. pointer capture与 drag/drop；
10. viewport translate不触发 content relayout；
11. variable height correction保持 anchor；
12. resize后旧 coordinate event拒绝；
13. theme color/metrics触发正确 damage；
14. wide grapheme continuation cell不产生错误 cursor/selection；
15. action binding、event path、modal/focus scope只来自同一committed snapshot；
16. presentation capability只能由trusted presenter一次消费，ordinary client forged/replay被拒绝；
17. event duplicate/out-of-order/disconnect/gap/writer restart通过scoped snapshot-feed reconcile；
18. command reserve-before-effect、same-id payload conflict、restart uncertain与receipt replay；
19. async page cancel/stale/source-generation/eviction/anchor correction；
20. arbitrary rectangles的dense hit map与慢速reference hit tester结果一致；
21. custom widget overlap/clip/wide grapheme/marker overdraw只能产生一致snapshot或typed failure；
22. UAX #9 mixed direction/isolate/control fixtures的logical↔visual/cursor/selection/copy一致。

### 18.4 Present 与 interaction fault matrix

Ratatui adapter test backend必须能在backend draw第N次write、cursor hide/show/position、internal completion前后、
flush和async writer completion分别注错：

| 注入点 | 预期 outcome | 可用 presentation | 输入与 marker ACK |
|---|---|---|---|
| prepare/preflight、确认零I/O | `NotStarted` | F0仍可证明有效 | 按interaction barrier policy使用F0或pending；0 ACK |
| first/partial diff write | `IndeterminateAfterIo` | 无 | semantic input fail closed；0 ACK |
| cursor mutation | `IndeterminateAfterIo` | 无 | semantic input fail closed；0 ACK |
| backend已swap baseline后flush失败 | `IndeterminateAfterIo` | 无 | semantic input fail closed；0 ACK |
| write/flush成功但receipt completion validation失败 | `IndeterminateAfterIo` | 无 | semantic input fail closed；0 ACK |
| async queue accepted、writer未完成 | 仍为`Presenting` | 不提交F1 | 0 F1 action/ACK |
| async writer失败 | `IndeterminateAfterIo` | 无 | fail closed |
| write/cursor/flush全部完成 | `Presented(F1)` | 原子提交F1 | 只允许F1 action/marker |
| poisoned后full repaint成功 | `Presented(Fn,new epoch)` | Fn | 丢弃所有旧epoch input/receipt |
| full resync再次失败 | `Poisoned/Closed` | 无 | restore/quit only |

Interaction interleaving至少覆盖：

| F0 | pending F1 | present outcome | 允许结果 |
|---|---|---|---|
| approval A、target slot N | 同slot/binding变approval B | `NotStarted` | A、`PresentationPending`或stale；绝不能B |
| approval A | approval B | `IndeterminateAfterIo` | A/B都不能执行 |
| node N/gen1=A | 删除并复用N/gen2=B | `NotStarted` | committed A或stale；绝不能B |
| Root→P1→A | event path变Root→P2→A | `NotStarted` | capture/bubble只走P1 |
| blocking modal A | modal关闭/替换B | `NotStarted` | A scope仍阻断或pending；背景/B不能提前生效 |
| focus A | focus B | `NotStarted` | keyboard只到A或pending |
| cells/hit相同、binding A | 仅binding变B | `Presented`前后 | success前A/pending，success后B |

Property test对任意`reconcile/prepare/Presented/NotStarted/Indeterminate/input`序列断言：发出的action一定属于
dispatch开始时一次加载的`CommittedPresentation`；`Poisoned`状态action与ACK数恒为0。多线程实现需要Loom或
等价原子可见性模型；单线程实现需要确定性交错测试。

### 18.5 Application contract 与跨表面 conformance

Fake application、in-process runtime、HTTP adapter与Desktop typed client使用同一fixture验证：

- terminal/control event分别发生在observer arm前、arm与snapshot cut之间、cut与return之间、return之后都不
  丢失；wrong observer generation、scope mismatch、ahead、expired、gap、buffer overflow与writer restart只能
  reset/reopen，不能拼接；
- snapshot/envelope cross-field mismatch、transient推进durable frontier/创建action authority、stream在authoritative
  terminal前close全部fail closed；只有exact final frontier + already-applied terminal的`Closed`可clean close；
- reducer commit前没有delivery ACK，restart会重放pending outbox；
- reserve前/后、dispatch marker前/后、effect调用前/中/后、receipt fsync前/后crash；store append/fsync/capacity、
  retention expiry、same-key same/different fingerprint、concurrent duplicate与uncertain reconciliation；
- writer/application restart后相同command id、response-lost reconnect后的同durable client epoch、旧epoch全局拒绝；
- legacy HTTP store import中途crash、restart repair与cutover期间single writable reservation authority；
- forged/replayed/wrong sink/wrong digest/wrong route/clipped/hidden/zero-area/overdrawn/partial-I/O presentation
  completion全部拒绝；
- ordinary client即使复制/构造renderer-neutral observation也不能生成session-authorized attestation；
  `sigil-application` public API/dependency graph中没有TUI/Ratatui evidence type；
- presenter session的Clone/serde/constructor compile-fail、Debug redaction、drop/revoke/错误session negative test通过；
- TUI keyboard、TUI mouse、CLI、HTTP与Desktop对同一shared command得到相同domain receipt、frontier和domain
  event；generic transport receipt只做机械封装。

### 18.6 Input integration

Crossterm adapter tests必须注入真实 escape sequence，而不只直接构造 normalized event：

- SGR mouse；
- bracketed paste；
- Kitty keyboard/fallback；
- focus in/out；
- resize；
- malformed/truncated sequence；
- high-frequency mouse + key interleave。

### 18.7 PTY 与 terminal matrix

Headless tests覆盖绝大部分交互；PTY只验证真实 terminal lifecycle：

- alternate screen首帧前进入、退出后恢复；
- raw mode、mouse、paste、keyboard enhancement成对开关；
- background panic不会从非 owner thread teardown；
- resize/SIGWINCH后完整 reflow；
- output中无 CPR依赖或 application transcript泄入 native history；
- tmux/Zellij/SSH/Terminal.app/iTerm2等能力降级不破坏键盘路径；
- OSC52/system clipboard通过 host capability，不从 core直接执行。

## 19. Public API、SemVer 与 crates.io 发布

### 19.1 发布前置

每个公开 package必须具备：

- explicit `version`、`rust-version`、license、description、repository、readme、keywords、categories；
- crate-level docs、public item docs、可运行 examples；
- versioned dependencies，不能只有 workspace path；
- `cargo package --list` 审核内容；
- `cargo publish --dry-run`；
- docs.rs build；
- MSRV build；
- default/no-default/all-feature与支持的 feature powerset；
- `cargo semver-checks`；
- changelog与对应 Git tag；
- dependency/license/advisory/supply-chain review。

Cargo 官方文档明确：crate name 先到先得，published version不能覆盖或删除，只能 yank；所以实际 publish前
必须再次检查/预留名称。2026-08-23 调研时 crates.io API 对 `sigil-tui`、`sigil-tui-core`、
`sigil-tui-ratatui` 均返回不存在；这只是当日观察，不是名称保留证明。

### 19.2 Version policy

- public family从 `0.1.0` preview开始，不复用产品 binary 的 `0.0.1-beta.4` 版本；
- 三个公开package首版采用Sigil自主的锁步preview policy，并由release automation按core → ratatui → facade
  顺序dry-run/publish；Cargo发布顺序要求不等于版本必须锁步；
- facade首版兼容范围至少限制在同一minor series，CI测试minimum/current dependency；
- `unstable`/experimental API放显式 feature/module，不伪装稳定；
- 经过 Sigil + 至少一个独立示例/下游 consumer、两个 minor release与 API audit 后再提 1.0；
- breaking change必须有 migration note与 SemVer bump。

连续两个minor release无跨包break、至少两个真实downstream需要独立升级、minimum/current/semver CI稳定且独立
发布automation验证完成后，必须重新评估并以单独RFC决定是否解除锁步；不得把preview policy永久化而无exit。

### 19.3 Rust API guidelines

公共 surface遵循 Rust API Guidelines，特别是：

- common traits、typed errors、Send/Sync声明；
- object-safe trait只在 dynamic integration确有必要时使用；
- caller控制 allocation/intermediate result；
- public dependencies本身稳定；
- struct private fields、sealed implementation traits、`#[non_exhaustive]`；
- examples展示使用原因、failure/panic/safety明确；
- additive feature；
- Debug不泄漏内容、credential或 opaque application binding。

### 19.4 Repository 与 release topology

首版仍在 Sigil monorepo开发，以便同时验证 product adapter和避免跨仓库原子变更困难。达到 `0.3` 资格后再
评估是否把公开 package镜像/迁至独立 repository。无论 repository位置如何：

- public package不能依赖 monorepo private path；
- package tarball在临时目录、无 workspace其他文件时可编译/测试 examples；
- lockstep release失败时不得发布版本不兼容的 facade；
- crates.io owner至少包含项目团队账户，token最小权限并由 release workflow管理。

## 20. Sigil authority 与安全语义保持

### 20.1 Approval

UI只显示 application projection中的 safe preview与 opaque approval binding。`Approve once/session/family/args`
作为 typed application command发送；application service继续验证 session、call id、request id、plan hash、
execution binding与durable frontier。关闭 modal只改变 presentation state，不能代表 approval成功。

### 20.2 Cancel、pause 与 terminal task

- urgent control command由application host对typed variant穷尽派生独立lane；caller/client不能声明或提升priority；
- UI只显示 requested/accepted/cancelling/terminal projection；
- cancellation owner、effect permit、join/quiescence与 Interrupted/Cancelled判断仍在 runtime；
- framework future drop、channel断开或 modal关闭不能写领域 terminal；
- terminal task control不进入 public TUI crate。

### 20.3 Session 与 config

- TUI不扫描、读取、删除或导出 JSONL；
- TUI不持有 `PathBuf` authority或 session attachment lease；
- catalog/lifecycle/config update全部经 application query/command/receipt；
- renderer-safe projection可显示已收窄 path label，不暴露可执行 path capability；
- setup/config live draft是ephemeral；保存由 application host原子执行。

### 20.4 Tool、artifact 与 external data

- framework只接收 safe bounded text、syntax/diff/search model和 opaque action；
- raw tool body、provider exact args、signed URL、artifact path不进入 SurfaceModel；
- `read more/open/copy`是 action，adapter/application再次做 scope/policy校验；
- Web/MCP disclosure继续遵循durable authorization → trusted presenter capability + successful committed marker →
  exact one-shot receipt → wire permit；普通application command/client不能替代presenter。

### 20.5 Domain terminal 与 delivery failure

RFC-0069 的 `FailureScope × Recoverability × EffectSettlement` 继续是领域事实。TUI adapter、terminal backend、
projection或journal delivery failure不能把已提交 domain terminal改写成另一状态。framework error只表达：

- prepare/render/present/input/capability failure；
- committed frame是否仍有效；
- 是否需要 snapshot reconcile/terminal restore。

它不表达 Task/Run是否成功。

### 20.6 Observation 与 command authority

- application snapshot/event contract绑定scope、writer/stream/observer generation与frontier；TUI framework只看到
  generic model revision，不持有session/outbox authority；
- durable/transient event分离，approval/cancel/action binding/capability change不得通过transient wake建立authority；
- 所有surface复用single application command journal；adapter不能自行降低reserve/replay/conflict/uncertain语义；
- delivery ACK、domain receipt、trusted presentation receipt是三个不同对象，任何一个都不能冒充另一个。

### 20.7 RFC-0071 resource/recovery authority 保持独立

R70实施时，以下post-R71 contract是只消费、不重写的冻结输入：

- `ToolPermissionPlanV3/ToolPermissionDecisionV3`与exact approval/resource binding；
- `ManagedExecutionServiceV1`、`ManagedFileAccessServiceV1`、`ManagedStorageServiceV1`的pathless application-facing ports；
- resource journal、physical effect facts、resource/effect/cleanup receipt；
- `RecoveryBlockerV2`与kernel-owned`ResourceRecoverySurfaceContractV1` canonical schema/hash/action token；
- Resource Authority/Sandbox的generation、lease、reservation、quarantine、reconciliation与single-writer ownership。

`sigil-application`可以将这些fact组合进application projection、command admission和typed domain receipt，但不能
解释host path、重新签action token、从UI文案推断recovery、直接调用physical cleanup，或把R71 runtime transitional
facade fork成长期双入口。application command reservation与resource journal的key、owner、terminal各自独立；
application effect只通过R71 typed receipt/frontier确认resource outcome。若R70设计需要改变任何R71 durable bytes、
canonical hash、authority owner或recovery transition，必须先单独修订RFC-0071，不能在R70 slice内隐式完成。

## 21. 当前源码到目标职责的映射

| 当前模块 | 目标归属 | 迁移说明 |
|---|---|---|
| `ui/geometry`, `ui/text`, `ui/primitives` | public core/ratatui | 移除 `AppState` 与 Sigil DTO |
| `ui/theme/*` | public theme + product extensions | 保留 palette/diagnostics，替换 kernel config依赖 |
| markdown/timeline/tool-card/composer/modal renderer | public widgets + `sigil-tui-app` composition | 通用 primitive公开，产品 DTO留 adapter |
| `ui/layout_snapshot.rs` | 删除 | geometry由 `CommittedPresentation` render transaction产生 |
| `app` pure focus/scroll/selection/expand/draft | public presentation state或 product surface state | 不含 durable truth |
| `AppState` paths/runtime/updater/session fields | application/host | 不进入 renderer |
| `view_model` data types | public/product SurfaceModel | `from_app`移入 adapter |
| `worker_bridge`, `runtime_status` | application adapter/host | event/receipt reducer |
| `runner/*` | `sigil-application` + `sigil-runtime` composition | 从TUI完全移出；runtime调用RFC-0071 managed ports，application不接管physical authority |
| setup/config/session flow | adapter + application service | UI draft与真实 effect分离 |
| clipboard/image/open external | host capability | core只发 request/action |
| `workspace_git`, scratch, child JSONL | application semantic adapter + RFC-0071 managed services | UI不得执行I/O；physical allocation/lease/storage仍归Resource Authority/closed semantic owner |
| updater cache/state | transport-neutral updater owner + application typed port | 保持RFC-0071 `ProductUpdaterState`独立owner，不迁入agent resource authority或TUI |
| launcher terminal lifecycle/input decode | public optional driver | worker/session/effect wiring留 composition root |
| `commands.rs` | generic key binding + product action catalog | label不能成为 command authority |
| UI/app/runner tests | public headless、adapter contract、runtime contract三层 | 不再依赖一体化 fixture |

## 22. 分阶段迁移计划

迁移禁止 big-bang。每阶段都要保留现有功能，先建立可比较 contract，再删除旧路径。

**Cross-RFC serial invariant**：本计划只有在RFC-0071 R71.8完成、同一release candidate资格化并产出post-R71 handoff manifest后才启动。R70.0以前不得预建`sigil-application`、拆public TUI package或把R71 transitional facade当成R70进度；R70实施中也不得修改R71 durable schema/authority来“顺便适配”。

### R70.0：冻结基线、能力清单与 profiler

**Depends**：RFC-0071 R71.8 qualified closure；不能用R71.5 shadow、R71.6 cutover或R71.7 local green代替。

交付：

- 校验R71 handoff manifest与exact post-R71 baseline commit，冻结`ResourceRecoverySurfaceContractV1` schema/hash、permission V3、receipt/blocker/action binding、runtime transitional facade入口、consumer清单与待删除edge；
- 记录当前 scroll/mouse/stream/resize/theme基线；
- 固定 10k/100k mixed transcript fixture；
- 将 `scripts/tui-mouse-smoke.sh` 中 composer、slash、scroll、config、session、approval、tool card、hover、
  selection、OSC52能力转为自动化/半自动化 matrix；
- 增加 phase timing临时 instrumentation；
- 记录 current dirty worktree之外的基线 commit；
- 从production `WorkerCommand`、`WorkerMessage`及surface command/event enum生成versioned migration manifest，
  记录每个variant的target、receipt、effect/replay class、surface exposure、test与phase；R71 public resource/recovery row必须标记`reuse`而非`redefine/migrate`。

退出条件：能量化`AppState projection / LayoutSnapshot / render / flush`各自成本；enum discovery与manifest零差异，
没有缺mapping、无理由retire/NotExposed或wildcard classifier；handoff manifest与workspace current contract完全一致，
没有legacy permission/blocker schema、第二份surface canonical hash或未登记surface-to-runtime edge。

### R70.1：在现有 crate 中先落 `CommittedPresentation`

交付：

- renderer在同一次draw中生成cell/hit/text/cursor/marker与immutable interaction snapshot；
- launcher实现`Presented/NotStarted/IndeterminateAfterIo`、terminal epoch与poison/full-resync状态机；
- mouse、focus keyboard与modal route只查询committed snapshot；
- 删除 mouse路径的 `LayoutSnapshot::from_app` 调用；
- 保持现有 AppState、AppAction、worker行为不变。

退出条件：

- click/hover/selection/scroll全部由render-produced geometry驱动；
- approval A→B failed-present、slot ABA、event path、modal/focus和binding-only barrier tests通过；
- Ratatui draw/cursor/flush/async completion/receipt-validation逐点fault injection通过，indeterminate时0
  semantic input/ACK；
- poisoned terminal能full repaint进入new epoch，或由owner thread安全退出；
- unchanged move零 render；
- current mouse smoke与property tests通过；
- scroll/mouse p95不低于R70.0 gate。

这是性能收益最大、风险最小的第一实现切片。

### R70.2：引入 Damage、normalized input 与 host effects

交付：

- Crossterm event转换为core `InputEvent`；
- `AppMouseOutcome/AppAction`拆分为 local update、opaque action、host request；
- event consumption与 redraw解耦；
- scheduler合并damage并保留当前公平select/batching；
- clipboard/image/open-external改为injected capability。

退出条件：ignored input 0 frame；一个 batch最多一帧；control lane在input flood下仍可用。

### R70.3：Renderer 只消费 SurfaceModel + SurfaceState

交付：

- `UiViewModel::from_app` 与所有 domain mapping移入临时 adapter；
- renderer签名不再接收 `AppState`；
- theme使用framework `ThemeSpec`；
- timeline item改 stable ID；
- 虚拟列表、resident/page bridge与height/anchor cache落地；
- UAX #9 bidi与logical↔visual mapping进入text subsystem；
- custom widget只能通过bounded RenderContext/scratch buffer。

退出条件：production renderer源码禁止import`crate::app::AppState`；framework synthetic 100k complexity、custom
render invariant与Unicode/UAX #9 contract gate通过。此阶段不把预载完整application transcript当作paging完成。

### R70.4：建立完整 `sigil-application` contract

**Depends**：R70.3与R70.0冻结的post-R71 contract；不得针对pre-R71 worker/resource schema实现。

交付：

- 直接依赖kernel-owned`ResourceRecoverySurfaceContractV1`并无损组合permission V3、resource/effect receipt、`RecoveryBlockerV2`与exact action envelope；删除任何application-local副本/转换状态机；
- atomic scoped snapshot-feed cut、event envelope、gap/reset/writer restart与delivery ACK；
- async projection page/range contract、bounded cache/cancel/stale response；
- grouped versioned command envelope；
- host-injected identity、exhaustive lane/policy与所有mutation/control typed domain receipt；
- durable reserve-before-effect、payload conflict、settlement class、uncertain/restart repair；
- approval/cancel/session/config/provider/MCP/task/agent/user-input/maintenance projection；
- 独立trusted presenter capability，不进入ordinary command port；
- 现有 `application_run` capability迁入/实现该 contract；R71 runtime transitional facade在迁移期只作为同一service的薄兼容入口，不拥有第二份projection/recovery truth；
- fake application实现；
- 在exclusive lease下幂等导入legacy HTTP command store的identity、unfinished reservation、terminal receipt与
  tombstone；cutover任何时刻只有一个writable reservation authority，并可在中途crash后继续/回滚；
- manifest每个production row映射到single application service；各surface exposure/NotExposed rationale完整。

退出条件：TUI通过port运行；cold-cache`paged-transcript-100k`证明未预载全部ID/body且cache/in-flight有界；
snapshot/gap/reset、reserve/replay/uncertain、forged presenter tests通过；TUI keyboard/mouse、Desktop、HTTP、CLI对
shared commands的domain receipt/frontier/event conformance通过；legacy store crash-safe cutover与single-writer
gate通过；不直接消费内部`RunEvent`或worker oneshot/Arc；application port不依赖RA/Sandbox concrete type，
R71 canonical bytes/hash与authority owner在迁移前后逐项相同。application command reservation与R71 resource
reservation分别只有一个writer，且只通过typed receipt/frontier关联。

### R70.5：物理拆 package 与第二消费者

交付：

- current product package改名 `sigil-tui-app`；
- 创建 `sigil-tui-core`、`sigil-tui-ratatui`、新的 `sigil-tui` facade；
- 迁移 framework modules和tests；
- 提供不依赖Sigil的 todo/chat示例；
- `cargo metadata` dependency gate。

退出条件：fake app与Sigil app无需conditional compilation共享同一公开framework；8.7 machine-readable
allowlist在所有supported feature/target graph通过，除允许的三包内部边外没有来自任何source/alias的disallowed
package-identity edge。

### R70.6：移出 runner 与 side effects

交付：

- `runner/*`从公开/产品 TUI package消失；
- provider/tool/session/MCP/workspace/git/child-session等**semantic orchestration与adapter wiring**由application/runtime host拥有；
- scratch/temp/state/cache/artifact physical allocation、lease、quota、cleanup/recovery继续只由RFC-0071 Resource Authority/Sandbox或RFC-0071 §9.5 closed product owner承担；application/runtime不得接收root path、复制allocator或成为第二cleanup owner；
- updater继续由`ProductUpdaterState` owner管理，application只消费typed updater port；
- `sigil-tui-app`只依赖 `sigil-tui` + `sigil-application`；
- 删除TUI/HTTP/CLI对R71 runtime transitional resource/recovery facade的direct dependency，Desktop generated wire仍消费同一application service；
- binary composition root wiring完成。

退出条件：依赖图与source policy gate全部通过；migration manifest零未映射production row、零无理由
`NotExposed/retire`；approval/cancel/egress/session/recovery cross-surface tests无退化；application/TUI package无
RA/Sandbox concrete/physical type，runtime transitional consumer edge为零，R71 authority/receipt/blocker schema未变化。

### R70.7：发布资格与 preview release

交付：

- README、API docs、examples、changelog；
- MSRV、feature powerset、docs.rs、package dry-run、semver；
- performance report与public benchmark fixtures；
- crates.io name再次确认与owner/release workflow；
- `0.1.0` preview。

退出条件：从生成的 `.crate` tarball在空临时 workspace构建 examples/tests成功；Sigil使用registry-like dependency
也通过完整TUI gates。

### R70.8：删除兼容层

只有在至少一个release cycle与真实用户验证后：

- 删除旧 `AppState -> renderer`；
- 删除旧 WorkerProtocol TUI facade；
- 删除旧 layout snapshot；
- 删除 dual theme/input/command mapping；
- 将旧protocol manifest row标为已验证retire，并保持历史可追溯；
- 删除R71 handoff manifest中登记的runtime transitional surface facade兼容入口；kernel-owned resource/recovery contract与durable history继续保留；
- 更新核心技术方案、README、AGENTS/工程文档中的 crate职责。

## 23. 功能保留矩阵

本表是human-readable摘要，不能替代9.8的versioned command/event manifest。迁移完整性的规范证据是manifest
discovery零差异、production row零未映射以及shared surface conformance。

| 能力 | Framework责任 | Sigil adapter/application责任 | 资格证据 |
|---|---|---|---|
| full-screen/restore | Ratatui/Crossterm driver | composition lifecycle | PTY |
| keyboard/paste/mouse/focus | normalized input + routing | product action mapping | escape injection + PTY |
| click/hover/drag/drop | CommittedPresentation/hit/event path | opaque binding validation | atomic snapshot/ABA property tests |
| transcript scroll | virtual list/anchor/page bridge | async paged safe projection | synthetic + cold-cache 10k/100k benchmark |
| text selection/copy | text map/selection + host request | policy-safe content/clipboard capability | Unicode contract test |
| composer/slash/mention | input/widgets/focus | catalog/action/protocol | product flow tests |
| approval | modal primitives | exact durable command/receipt | stale/replay tests |
| cancel/pause | control action priority | cancellation authority/quiescence | flood + runtime tests |
| plan/task/agent cards | generic card/list/status | authoritative projection | recovery tests |
| sessions/config/setup | generic views/form | application query/effect | restart/concurrency tests |
| tool card/markdown/diff | generic content primitives | safe typed DTO/ref/action | snapshots + SafePersist tests |
| themes/live preview | ThemeSpec/provider/diagnostics | config mapping + product themes | token/contrast tests |
| syntax/image | optional adapter | safe content/capability | feature matrix |
| egress disclosure | marker in committed frame | trusted one-shot presenter capability/wire gate | forged/partial-I/O/failed-present test |
| info rail/status | responsive primitives | bounded projection | size matrix |
| accessibility fallback | focus/semantic/high contrast | product labels/actions | semantic snapshots |

## 24. 被拒绝的替代方案

### 24.1 只给当前 `LayoutSnapshot` 加 cache

可以短期减轻重复计算，但仍保留 renderer外的第二套 geometry、stale invalidation和 AppState耦合；无法成为
第三方framework，也无法保证 successful frame consistency。

### 24.2 Click时实时遍历/重算

把延迟放到用户等待路径，复杂度随node增长，并可能命中尚未呈现的新model。拒绝。

### 24.3 直接依赖 `ratatui-interact`

它的render-time registry值得借鉴，但 flat first-wins、O(R) scan、focus/modal/bubble/theme不足。可以迁移个别
组件或思想，不能成为架构边界。

### 24.4 整体迁移 `ratatui-kit`

会得到retained identity和theme/input layer，但其destructive handler dispatch、一事件一整树render、
framework-owned Crossterm loop、缺少public stepwise embedding surface与普通ScrollView全content buffer不满足
本RFC性能/host边界。`fullscreen()`本身是executor-neutral Future，不把问题错误归因为Tokio coupling。

### 24.5 迁移到 OpenTUI

需要引入JavaScript runtime或跨语言FFI/matching native artifact分发，失去Rust crate直接复用；固定快照除Bun
外还存在Node.js 26.4 ESM + experimental FFI路径。OpenTUI本身仍在v0.x重构，其culling/theme/virtualization
也不完全满足目标。Ratatui可实现同一presented-frame数据流，故拒绝。

### 24.6 只把当前 `sigil-tui` 整块发布

它包含kernel/runtime/tool/updater path dependency、worker、I/O与Sigil专属公共surface；这不是复用，是把整个
应用runtime包装成library。拒绝。

### 24.7 首版建立完整 retained visual DOM + Yoga + hooks + macros

会把Rust ownership/lifetime、incremental consistency、API stability和compile time风险同时前置。首版只保留
performance/interaction所需identity/state，两个consumer证明缺口后再扩展。

### 24.8 首版拆六到八个公共 crate

会扩大Sigil自行承担的feature/release/compatibility矩阵，违反本仓库restrained crate原则。OpenTUI存在
锁步adapter检查，但`ratatui-kit`的runtime/macros并不锁步，Cargo也不要求相同版本号；本选择是Sigil preview
policy。首版三个公开package + facade已经能硬隔离backend-neutral core并让用户声明单一dependency。

## 25. 风险与缓解

### 25.1 只移动文件，不移动 authority

风险：多个crate仍通过internal trait/Arc共享runtime对象。

缓解：cargo metadata + source policy gate；`sigil-tui-app`只能依赖application contract；fake application必须
无需Sigil类型运行。

### 25.2 双状态源

风险：SurfaceState复制Task/session/approval truth。

缓解：projection/domain state只读authoritative；ephemeral state类型单独module；optimistic pending带command id，
receipt/snapshot覆盖。

### 25.3 泛化导致性能下降

风险：每行一个heap component/closure、generic dispatch allocation、reconciler全树扫描。

缓解：virtual materialization、stable handler table、inline action、phase metrics与complexity hard gate；禁止照搬
Kit destructive dispatch。

### 25.4 Dense hit grid overdraw

风险：许多重叠full-screen wrappers导致每帧重复填充cell。

缓解：pass-through default、只写pointer semantic nodes、hit topology generation复用、记录写入数；保留
RowInterval strategy seam。

### 25.5 上一呈现帧与current model短暂不一致

风险：event target、action、modal/focus scope或event path已经更新；Ratatui I/O失败还可能让physical terminal处于
新旧混合状态。

缓解：frame-local generational target与immutable InteractionSnapshot；exact committed binding或typed stale；
绝不重算到新布局。`IndeterminateAfterIo`poison terminal epoch，full resync前0 semantic input/ACK。

### 25.6 Application public projection信息不足

风险：TUI当前消费大量internal RunEvent/worker state，收窄后功能丢失。

缓解：逐功能parity matrix；先建立projection fixture和adapter双轨；缺DTO先补provider-neutral contract，不让TUI
重新依赖internal type。

### 25.7 Control公平性退化

风险：new scheduler/input coalescing饿死cancel/approval/pause。

缓解：host-derived exhaustive priority lane、bounded queue、caller-escalation negative tests与flood tests；保留
current select/batching直到new host有等价证据。

### 25.8 Theme API SemVer压力

风险：新增semantic role破坏下游struct literal或widget override。

缓解：private fields/builders、`#[non_exhaustive]`、ThemePatch、schema version、fallback tone、component theme derive。

### 25.9 Package release train

风险：core/backend/facade registry顺序或版本漂移。

缓解：把lockstep明确限定为Sigil preview policy；pre-publish compatibility check、minimum/current matrix、
dry-run临时registry/目录与前置package可解析后再继续。达到19.2 exit condition后必须另立RFC评估独立version。

### 25.10 Terminal差异

风险：width、mouse、keyboard、palette、OSC与alternate screen在不同terminal不一致。

缓解：versioned capabilities、no-query safe fallback、headless + PTY + terminal matrix、keyboard parity。

### 25.11 迁移期长期双轨

风险：旧/新renderer、theme、action长期漂移。

缓解：每阶段有删除条件、兼容层有owner/expiry、同fixture双render对比；R70.8作为明确终点。

### 25.12 Presenter capability 被普通 client 伪造

风险：可序列化ACK或caller-supplied sink fingerprint绕过egress presentation barrier。

缓解：private affine capability、独立trusted presenter principal、final-compositor marker evidence、durable single
consume与forged/replay/wrong-sink compile/runtime gate；ordinary command port无ACK variant。

### 25.13 Snapshot/feed gap 或 generation 拼接

风险：snapshot与subscribe之间丢terminal/control event，旧session/writer event污染新projection。

缓解：observer先arm、inclusive cut、base/next frontier、scope/writer/stream/observer generation、explicit
gap/reset/ahead/expired与outbox-after-reducer ACK；property test与existing SSE behavior shadow compare。

### 25.14 Command uncertain 被重复执行

风险：effect前没有reservation、response丢失或crash后adapter自动retry，造成duplicate approval/workspace/network
effect。

缓解：single application-owned durable journal、payload conflict、dispatch marker、family settlement、existing effect
reconciliation与non-evicting uncertain record；所有surface共享fault fixture。

### 25.15 Async paging 把I/O带回render hot path

风险：同步`item()`隐式阻塞、预载全部ID/body或无限pin page，表面上virtualized但内存/延迟仍O(N)。

缓解：RangeNeeded host effect、bounded page/in-flight/cache budget、cancel/stale generation、placeholder/estimate与
cold-cache 100k E2E gate。

## 26. 验收标准

### 26.1 架构 gate

- `sigil-tui-core`除root package自身外不依赖Ratatui/Crossterm/Tokio或任何实际package name匹配`^sigil-`的
  dependency；
- 三个公开package在normal/build/dev/optional/target-specific、default/no-default/all-supported-feature graph全部
  通过8.7 package-identity allowlist；alias与registry/git/path source不能绕过；
- 公共API、docs、examples中无provider/agent/session/tool专属contract；
- `sigil-tui-app`不依赖runtime/kernel/tools/updater/provider；
- `sigil-application` presenter port只使用application-owned renderer-neutral type，不依赖任何公开TUI package；
- `sigil-application`可依赖kernel-owned`ResourceRecoverySurfaceContractV1`，但不依赖RA/Sandbox concrete/physical type，也不复制其schema/hash/action signer；
- TUI/HTTP/CLI对R71 runtime transitional resource/recovery facade的direct dependency为零；所有surface只经`sigil-application`或Desktop typed wire消费同一contract；
- Cargo/AST gate证明physical resource allocation/lease/quota/cleanup/recovery仍只有RFC-0071 owner，application/runtime未新增root-path allocator、GC或第二authority；
- renderer源码无 `AppState`；
- public package production源码无filesystem/process/network/session store；
- `runner/*`不在public TUI package；
- fake todo/chat app独立构建、运行、测试。
- command/event migration manifest与production enum discovery零差异。

建议CI机器检查：

```bash
cargo metadata --format-version 1
cargo tree -p sigil-tui-core -e normal
cargo tree -p sigil-tui-ratatui -e normal
cargo tree -p sigil-tui -e normal
```

CI脚本解析JSON/dependency graph做断言，不依赖易碎的文本grep。

### 26.2 Frame/interaction gate

- mouse event路径没有任何layout/projection调用；
- cell、hit、text、cursor、marker、event path、modal/focus scope、action binding来自同一
  `CommittedPresentation`；
- `NotStarted`保留可证明F0；所有Ratatui/async partial effect映射`IndeterminateAfterIo`并poison epoch；new epoch
  full resync前semantic input/ACK恒为0；
- approval A→B、slot generation ABA、binding-only、event-path与modal/focus failed-present property tests通过；
- topmost/clip/custom overdraw/capture/bubble property tests通过；
- resize/stale event行为明确；
- keyboard可以完成所有关键mouse action；
- selection、wide grapheme与UAX #9 logical↔visual mapping正确。

### 26.3 Performance gate

- §17 complexity invariants全部通过；
- reference workload达到absolute/relative budget或有明确、审批过的qualification exception；
- synthetic 100k item时materialized node不随总量线性增长；
- cold-cache application 100k paging不预载全部ID/body，resident bytes/page/in-flight有界，render/input 0 I/O；
- unchanged mouse move 0 frame；
- event batch最多一帧；
- idle 0 periodic draw；
- profiler report随RFC execution ledger提交。

### 26.4 Theme gate

- semantic token完整；
- current six built-in themes与custom override parity；
- high-contrast/NO_COLOR/16/256/truecolor fallback；
- color-only与layout-affecting invalidation分离；
- preview/save使用同一resolver；
- contrast/semantic diagnostics与syntax theme通过。

### 26.5 Application/security gate

- stale approval/action拒绝，旧frame绝不升级成current binding；
- command effect前durable reserve、payload conflict、single owner、typed replay、uncertain/reconciliation、retention
  tombstone全部通过；
- client identity由host/transport注入，priority由typed command穷尽派生，伪造client/urgent hint不能扩权；
- cancel/approval/control flood不饿死；
- ordinary command client无法构造presentation ACK；private one-shot capability的forged/replay/wrong sink/digest/
  observer/clip/overdraw/partial-I/O tests通过；
- observer先arm的snapshot cut、base/next frontier、scope/writer/stream/observer generation、gap/reset/ahead/
  expired、outbox delivery ACK与restart恢复通过；
- TUI keyboard/mouse、Desktop、HTTP、CLI shared command fixture产生相同domain receipt/frontier/event；generic
  receipt没有替代domain receipt；
- post-R71 permission V3、resource/effect receipt、RecoveryBlockerV2、action envelope与canonical hash在application facade迁移前后byte-for-byte/fixture-equal；
- application command reservation与R71 resource reservation各自single-writer、key/terminal不混用，且只通过typed effect/resource receipt和frontier关联；
- forged surface adapter、application-local action token、transport-private recovery enum、第二runtime/application recovery state machine均在effect前拒绝；
- migration manifest零未映射production row、零无理由`NotExposed/retire`；
- SafePersist projection之外的exact内容不进入SurfaceModel；
- renderer error不改写domain terminal。

### 26.6 Publication gate

- package metadata、README、rustdoc、examples完整；
- MSRV/default/no-default/all-feature/powerset通过；
- 8.7依赖allowlist由同一policy文件在package与CI gate消费；
- `cargo package --list`与`cargo publish --dry-run`通过；
- 解包后的独立build通过；
- semver、license、advisory、supply-chain gate通过；
- crates.io name/owner/release order在publish当日复核；
- 至少Sigil与一个non-Sigil consumer通过。

## 27. Definition of Done

RFC-0070 只有同时满足以下条件才能标记 implemented：

1. 三个公开 package已经从生成的crate artifact通过资格化，且至少发布一个preview version；
2. Sigil TUI使用`sigil-tui-app -> sigil-application`，不再拥有runtime worker；
3. current `LayoutSnapshot` click-time路径与旧runner facade已删除；
4. synthetic与cold-cache paged 100k transcript、mouse flood、stream、resize、theme、Unicode/bidi基准和
   interaction contract全部通过；
5. current theme、keyboard、mouse、approval、session、tool-card、selection、terminal lifecycle能力无缺失；
6. `IndeterminateAfterIo` fail closed、new epoch resync、mixed-generation/ABA action为0；
7. command reserve/replay/uncertain、scoped snapshot-feed与trusted presenter capability全部通过fault gate；
8. approval/cancel/egress/session/recovery安全语义通过cross-surface验证；
9. migration manifest零未映射production row、零无理由`NotExposed/retire`；
10. dependency allowlist、独立consumer与crate artifact qualification通过；
11.核心技术方案、crate职责、README与developer docs已同步；
12.旧兼容层、dual state和临时feature flag已清理；
13. RFC-0071 R71.8 qualification与post-R71 handoff manifest先于任何R70 implementation commit，execution ledger不存在重叠slice；
14. `sigil-application`无损复用R71 kernel resource/recovery contract，permission V3、resource journal/receipt、blocker/action canonical bytes与authority owner未被R70改写；
15. `sigil-tui-app`、public TUI packages与application contract均无RA/Sandbox concrete/physical type，R71 transitional surface-to-runtime edge已全部删除；
16. application command reservation与resource lifecycle authority没有双写、互相冒充或第二cleanup/recovery owner。

完成“拆出一个目录”或“能够 `cargo check`”不构成Done。

## 28. 实施记录模板

每个阶段在本RFC末尾追加，不改写原始设计结论：

```text
R70.x
- commit / PR
- moved authority
- deleted legacy path
- behavior parity evidence
- benchmark before/after
- tests/gates run
- remaining deviations / expiry
```

任何偏离以下四条核心不变量的实现必须先修订RFC：

1. public framework零application依赖；
2. input只查询single successful `CommittedPresentation`；indeterminate terminal没有interaction authority；
3. durable authority永远在application/runtime；ordinary client不能伪造presenter、identity或priority；
4. observation与command effect必须遵守scoped frontier和reserve-before-effect。

## 29. 官方互联网资料

- Ratatui官方说明其不提供input catching，由backend/application负责：
  [Event Handling](https://ratatui.rs/concepts/event-handling/)。
- Ratatui `Terminal` 官方文档说明full-frame callback、double buffer、cell diff与error可能留下partial mutation，
  通常应把该terminal session视为fatal：
  [Terminal rendering pipeline](https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html)。
- Ratatui memory integration backend：
  [TestBackend](https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html)。
- OpenTUI官方renderer文档说明retained root、automatic/request-driven rendering、live mode、mouse/focus与
  terminal capability：
  [OpenTUI Renderer](https://github.com/anomalyco/opentui/blob/eaf1d41e9252505232b1cbeae3ab05c15a55243d/packages/web/src/content/docs/core-concepts/renderer.mdx)。
- OpenTUI retained renderable、三阶段layout/render-list与hit registration：
  [Renderable.ts](https://github.com/anomalyco/opentui/blob/eaf1d41e9252505232b1cbeae3ab05c15a55243d/packages/core/src/Renderable.ts)。
- OpenTUI dense double-buffer hit grid与O(1) lookup：
  [native renderer.zig](https://github.com/anomalyco/opentui/blob/eaf1d41e9252505232b1cbeae3ab05c15a55243d/packages/native/src/renderer.zig)。
- OpenTUI路线图明确v0.x仍在重构layout/render/Unicode/benchmark/accessibility：
  [OpenTUI Roadmap](https://github.com/anomalyco/opentui/issues/821)。
- ratatui-kit官方README说明keyed state retention、waker rendering、input layer、theme、virtual list和feature：
  [ratatui-kit](https://github.com/yexiyue/ratatui-kit/tree/db0bffabb9d1e35609df97b9e1d10888150a2b1c)。
- ratatui-interact官方API展示render-time `ClickRegionRegistry`：
  [ratatui-interact](https://docs.rs/ratatui-interact/0.5.3/ratatui_interact/)。
- Cargo官方发布规范：metadata、`cargo publish --dry-run`、package permanence与name分配：
  [Publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html)。
- Cargo官方feature规范要求feature additive并解释feature unification：
  [Cargo Features](https://doc.rust-lang.org/cargo/reference/features.html)。
- Rust library team维护的公共API设计检查表：
  [Rust API Guidelines Checklist](https://rust-lang.github.io/api-guidelines/checklist.html)。
- Unicode extended grapheme cluster规则：
  [Unicode Text Segmentation, UAX #29](https://unicode.org/reports/tr29/)。
- Unicode bidirectional paragraph/line reordering与logical/visual规则：
  [Unicode Bidirectional Algorithm, UAX #9](https://www.unicode.org/reports/tr9/)。
- SemVer自动检查工具的primary repository及能力限制：
  [cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks)。

## 30. 本地源码关键证据索引

### Sigil current workspace

- `crates/sigil-tui/Cargo.toml:7-29`：terminal/OS/runtime/application混合依赖；
- `crates/sigil-tui/src/app.rs:551-616`：AppState混合paths、runtime、host、presentation；
- `crates/sigil-tui/src/app.rs:619+`：AppAction混合UI/application/OS effects；
- `crates/sigil-tui/src/app/state.rs:32-55`：可抽取的timeline presentation state；
- `crates/sigil-tui/src/app/state.rs:132-190`：不应进入framework的runtime state；
- `crates/sigil-tui/src/launcher.rs:496-667`：current host loop、dirty draw与fair select；
- `crates/sigil-tui/src/launcher.rs:690-743`：mouse-time layout snapshot；
- `crates/sigil-tui/src/ui/shell.rs:36-108`：renderer直接读取AppState并再次布局；
- `crates/sigil-tui/src/ui/layout_snapshot.rs:241-280`：第二套geometry；
- `crates/sigil-tui/src/view_model.rs:24-37`：ViewModel仍由AppState即时构建；
- `crates/sigil-tui/src/runner/protocol.rs:235,616`：worker command/message与runtime resource；
- `crates/sigil-tui/src/app/approval_flow.rs:153-223`：approval exact call/request binding；
- `crates/sigil-kernel/src/egress.rs:290-309,394-407,476-509`：private one-shot disclosure receipt与完整binding校验；
- `crates/sigil-kernel/src/session/active_projection.rs:35-61`：writer/session/offset/cursor frontier；
- `crates/sigil-http/src/sse.rs:727-833,896-949,1390-1421`：scope/ahead/expired/lag cursor语义；
- `crates/sigil-http/src/command_store.rs:28-58,121-205,258-297`：restart sealing、effect前reserve、payload
  conflict与identity fingerprint；
- `crates/sigil-runtime/src/application_run.rs:665-847`：现有application service/event contract种子；
- `crates/sigil-runtime/src/application_run.rs:5830-5897`：durable public-event outbox与delivery receipt replay；
- `crates/sigil-kernel/src/session/effect_reconciliation.rs:13-164`：uncertain effect observation与禁止重做边界。

### Ratatui resolved dependency

- `Cargo.lock:4535-4554`：资格化基线解析到`ratatui 0.30.2`/`ratatui-core 0.1.2`；
- Cargo registry `ratatui-core-0.1.2/src/terminal.rs:136-139`：`try_draw` error可能留下partial diff/cursor
  state，通常应把current terminal session视为fatal；
- Cargo registry `ratatui-core-0.1.2/src/terminal/render.rs:288-319`：backend diff、cursor、buffer swap、backend
  flush的可失败顺序。

### OpenTUI `eaf1d41`

- `packages/core/src/Renderable.ts:136-323`：retained identity/tree/layout/focus/mouse state；
- `packages/core/src/Renderable.ts:512-544`：dirty request与translate绕过layout；
- `packages/core/src/Renderable.ts:1382-1495`：layout/culling/render/hit registration；
- `packages/core/src/Renderable.ts:1597-1605`：mouse bubbling；
- `packages/core/src/Renderable.ts:1791-1849`：三阶段pipeline与render-list cache；
- `packages/core/src/renderer.ts:1537-1579`：render request coalescing；
- `packages/core/src/renderer.ts:3510-3769`：hit lookup、focus、hover、capture、drag/drop；
- `packages/native/src/renderer.zig:255-279`：double-buffer hit grid contract；
- `packages/native/src/renderer.zig:2815-2875`：paint-order fill与O(1) lookup；
- `packages/native/src/renderer.zig:920-959`：backend non-failure后交换；threaded queue success不证明physical flush；
- `packages/native/src/renderer-output.zig:175-201,685-748`：threaded output queue与writer completion/error边界；
- `packages/native/src/renderer.zig:2348-2561`：lazy frame start与row/cell diff；
- `packages/core/src/renderables/ScrollBox.ts:353-380,542-699`：translate scroll与bounded live mode；
- `packages/core/src/lib/objects-in-viewport.ts:40-149`：viewport culling；
- `packages/core/package.json:84-95`：testing是core subpath；
- `packages/native/package.json:1-4`：native source workspace为private；
- `packages/core/src/platform/ffi.ts:181-220`：Bun/Node runtime FFI选择；
- `packages/core/src/testing/*`、`src/benchmark/*`：public testing与benchmark设计。

### ratatui-kit `db0bffa`

- `crates/ratatui-kit/src/component/instantiated_component.rs:64-88`：retained component identity；
- `crates/ratatui-kit/src/render/updater.rs:83-111`：key/type reuse；
- `crates/ratatui-kit/src/render/tree.rs:50-104`：event后整树update/draw；
- `crates/ratatui-kit/src/element/element_ext.rs:58-75`、`render/tree.rs:1-4,78-104`：framework-owned
  Crossterm loop返回executor-neutral Future并使用`futures::select`；
- `crates/ratatui-kit/src/input/mod.rs:105-241`：per-frame registration与per-event allocation/sort/clone；
- `crates/ratatui-kit/src/components/theme/*`：palette/provider/override；
- `crates/ratatui-kit/src/components/virtual_list.rs:44-99,300-325`：optional virtual list；
- `crates/ratatui-kit/Cargo.toml:36-66`：default-empty feature与test-util边界；
- `crates/ratatui-kit/Cargo.toml:1-21`、`crates/ratatui-kit-macros/Cargo.toml:1-23`：runtime/macros独立版本，
  不构成锁步发布证据；
- `EXTENSION_API.md:1-26,105-126`：stable extension surface与macro dependency caveat。

### ratatui-interact `42f1aab`

- `src/traits/clickable.rs:123-195`：render-time reusable click registry；
- `src/traits/clickable.rs:265-275`：overlap first-registered wins；
- `src/components/button.rs:392-476`：actual render area registration；
- `src/state/focus.rs:46-175`：ID-order FocusManager；
- `src/traits/focusable.rs:72-135`：未与manager完整贯通的focus metadata；
- `src/theme.rs:31-231`：semantic palette与显式widget theme；
- `src/components/scrollable_content.rs:212-220,390-400`：visible slice但非variable-height virtualization。

## 31. 实施记录

### R70.0：冻结基线、能力清单与 profiler（2026-08-28）

- implementation commit：`c30cd3846b2dfe05e4287fc2fb98045d0e4146f2`（`rfc-0070(R70.0): establish post-R71 TUI baseline`）；基线提交为
  `c3b3982388e8e97a19ac1adb3576b6f8063d806f`，R71 qualified implementation candidate 为
  `ec5459d829e086fbb73f090dcb3201f649d99d7b`。
- moved authority：没有移动 Resource Authority、Sandbox 或 permission/resource/recovery owner；本 slice
  只冻结其 kernel public contract，并把后续 application migration 的生产 enum 边界登记到
  `dev/governance/r70-command-event-migration-v1.toml`。
- deleted legacy path：没有提前删除 runner、`AppState` 或 click-time layout；这些是 R70.1/R70.6 的明确后续
  删除项。
- behavior parity evidence：`scripts/tui-mouse-smoke.sh` 现在产生稳定 check ID、自动/半自动模式和可审计 Markdown
  matrix；`scripts/check-r70-baseline.sh` 对 R71 handoff、冻结 contract digest、fixture 和生产 enum 做 fail-closed
  exact join。当前 R71 recovery action、Worker/App/Surface protocol 共发现并显式登记 `274/274` variants。
- benchmark before/after：R70.0 临时 profiler 已接入 production TUI present helper；以
  `SIGIL_R70_PHASE_TIMINGS=1 scripts/profile-r70-tui-baseline.sh` 生成 raw logs 与 phase report，当前样本覆盖
  `app_projection`、`layout_snapshot`、`render`、`terminal_present`。该 report 是 R70.1 之前的 baseline，不是
  性能改进声明。
- tests/gates run：`cargo fmt --all --check`、R70 baseline checker、R70 migration unit tests、production present
  timing probe、两个 targeted TUI tests 通过。
- remaining deviations / expiry：R70.0 仍未完成全部长期 Done 条件；`CommittedPresentation`、public package split、
  application contract、100k cold-cache benchmark 和 legacy runner removal 保留给 R70.1-R70.8。临时 phase
  instrumentation 的 expiry 为 R70.1 完成时，届时必须迁移或删除，不能成为永久性能接口。

### R70.1：提交式 presentation 与 terminal fault state（2026-08-28）

- implementation commit：`de9ee32fc5acb2c78067cc8ce3b77acef8afe8f8`（`rfc-0070(R70.1): commit presented-frame interaction state`）；基线为 R70.0 evidence follow-up
  `df34e740f90eab20f4c2ba892d8e18e829b2f12a`。
- moved authority：没有移动 Resource Authority、Sandbox、permission 或 worker/resource owner；本 slice 只在
  `sigil-tui` 内建立 terminal presentation owner。
- delivered：新增 `PresentationSession`，为每个 frame 分配不可复用的 generation/attempt/terminal epoch；同一
  `Terminal::draw` callback 生成 `LayoutSnapshot`、渲染 cell buffer，并只在 backend draw/flush 成功且
  `TrustedPresentReceipt` 匹配后发布 `CommittedPresentation`。`NotStarted` 保留上一个 committed frame，首帧
  no-op 可重试；`IndeterminateAfterIo` poison session，鼠标在无 committed frame 或 poisoned 状态下 fail-closed。
- deleted legacy path：生产 launcher 不再在 mouse event 时间调用 `LayoutSnapshot::from_app` 或读取当前
  `AppState` 重建 geometry；旧 helper 仅保留给既有单元测试，后续 R70.3 renderer/adapter split 时删除。
- behavior parity：`AppState`、`AppAction`、worker protocol 与现有 modal/focus/mouse handling 未改语义；新的
  `handle_committed_mouse_event` 只把 immutable committed layout 传给原有 action handler。
- tests/gates run：`cargo fmt --all --check`、`cargo check -p sigil-tui`、
  `cargo test -p sigil-tui --lib presentation::tests -- --nocapture`（4 passed）、
  `cargo test -p sigil-tui --lib timed_frame_path_uses_the_production_present_helper -- --nocapture`（1 passed）。
- remaining deviations：R70.1 尚未完成 full-resync backend、normalized input、framework package split、
  application contract、renderer decoupling 与旧 runner removal；这些继续由 R70.2-R70.8 交付，不能将本 slice
  解释为 RFC-0070 总体完成。

### R70.2：normalized input、Damage 与 host effects（2026-08-28）

- implementation commit：`8ee8560e89c6070fe7114ae8873380080a95e392`（`rfc-0070(R70.2): normalize input and inject host effects`）；基线为 R70.1 evidence follow-up
  `1dc22eaf50edeea5b1898162c7ba70acc02db2c7`。
- moved authority：没有移动 Resource Authority、Sandbox、permission、worker 或 durable event owner；本 slice
  只收窄 terminal adapter 与 launcher 的输入/host capability 边界。
- delivered：新增不依赖 Crossterm 类型的 `InputEvent`/`InputKeyEvent`/`InputMouseEvent`/`FocusChange`，并在
  launcher 入口完成一次性 Crossterm normalization；增加 `EventEffect` 将 ignored/local update/opaque action/
  host request 分开；增加可合并的 `Damage`，input batch 在同一轮中 union damage，最多在 batch 后触发一次 present。
- host boundary：clipboard text、clipboard image capture、external URL/file opening 通过 launcher 注入的
  `HostEffects` 执行；生产使用 `SystemHostEffects`，测试通过 `TestHostEffects`，normalized input 与 app/action
  层不再直接调用 host process/capability。
- behavior parity：既有 AppAction、AppMouseOutcome、worker command 与反馈文案保持兼容；旧测试 helper 只作为
  test-only wrapper 保留，生产事件循环走 typed effect path；unsupported/repeat/release/focus-only input 不生成
  render damage。
- tests/gates run：`cargo fmt --all --check`、`cargo check -p sigil-tui`、完整
  `cargo test -p sigil-tui --lib -- --test-threads=2`（1714 passed / 3 ignored）、normalized input 3 项定向
  测试、clipboard/external host 行为测试、`git diff --check` 通过。
- remaining deviations：R70.2 尚未交付 renderer-only SurfaceModel、virtualization/UAX #9、`sigil-application`
  contract、public package split、runner 下沉或 release qualification；这些继续由 R70.3-R70.8 完成。

### R70.3：SurfaceModel renderer boundary、bounded virtualization 与 Unicode text（2026-08-28）

- implementation commit：`7c129a02`（`rfc-0070(R70.3): render from owned surface snapshots`）。
- 生产 terminal draw 在一次 `Terminal::draw` 中创建 owned `SurfaceModel`，renderer 入口只消费快照与
  `SurfaceState`；egress disclosure 的 presentation acknowledgement 仍由 launcher 在 draw 成功后执行。
- `surface_adapter` 集中 `AppState -> surface` projection；renderer-facing config/status 只使用 bounded label，
  不把 authority `PathBuf` 带入 SurfaceModel；approval scroll、generation-scoped item ID、UAX #9 bidi map 已
  纳入该边界。
- framework bridge 增加 bounded `RenderContext`/scratch `Buffer`、variable-height prefix index、
  `ViewportAnchor` 与 `ProjectionPageRequest` DTO；100k resident-bound、height lookup、clip 与 logical/visual
  mapping 测试通过。完整 application page source 仍留给 R70.4，未将当前 viewport rows 误报为 cold-cache paging。
- tests/gates：`cargo fmt --all --check`、`cargo check -p sigil-tui`、strict clippy、完整 TUI lib
  `1718 passed / 3 ignored`、surface/UAX #9 targeted tests、`git diff --check`。
- moved authority：没有移动 R71 Resource Authority/Sandbox、permission、worker 或 runtime owner；下一 slice 是
  独立 `sigil-application` contract 与 fake application，R70.4-R70.8 仍未完成。

### R70.4：transport-neutral application contract 与 runtime port（2026-08-28）

- implementation commits：`d9b57da3`（`rfc-0070(R70.4): establish transport-neutral application contract`）、
  `efc7851b`（`rfc-0070(R70.4): consume durable application outbox in projection`）、`b5c0a37e`
  （`rfc-0070(R70.4): route TUI through application port`）与 `8608d6aa`
  （`rfc-0070(R70.4): cut over HTTP reservations to application authority`），以及 `ffc3df4e`
  （`rfc-0070(R70.4): stream bounded transcript pages`），以及 `f44d8c10`
  （`rfc-0070(R70.4): journal application command reservations`），以及本轮新增的 ApplicationClient
  resumable reducer/ACK、durable delivery ACK journal 与 cross-surface contract tests；当前切片补充
  cold-cache 100k qualification。`8b344943`（`rfc-0070(R70.4): persist application delivery acknowledgements`）
  将 TUI production delivery ACK 接入 runtime-owned managed JSONL journal；`d0ab3da7`
  （`rfc-0070(R70.4): bridge HTTP through application port`）新增 HTTP transport-neutral application
  endpoint 与 production client bridge；`cf37303e`（`rfc-0070(R70.4): cut over HTTP cancel commands`）将
  既有 HTTP `/runs/{run_id}/cancel` production route 切换到同一 ApplicationPort durable reservation，
  legacy envelope reservation 仅保留在 `cfg(test)` 合成 driver 兼容路径；`bdcd44a4`
  （`rfc-0070(R70.4): cut over HTTP run starts`）再将 production run-start route 切换到同一
  ApplicationClient 与 durable reservation；`52b7724b`（`rfc-0070(R70.4): cut over HTTP approval decisions`）
  将 production approval decision route 也切换到同一 application reservation 与 typed guard；`7be30de7`
  （`rfc-0070(R70.4): cut over HTTP user-input decisions`）再将 production user-input decision route
  切换到同一 reservation 与 typed kernel decision。
- `5c3b33be`（`rfc-0070(R70.4): cut over HTTP conversation queue`）再将 production conversation queue
  route 切换到同一 host-bound `ApplicationClient` 与 durable reservation；queue-generation CAS、typed
  enqueue/edit/remove/reorder/pause/resume/interrupt action、prompt/material policy 与 foreground-owner guard
  仍由 HTTP driver 的直接 effect seam 在同一 session mutation lock 下执行，避免递归回到旧 command-store
  reservation。application service 内的同步 HTTP driver 调用移出 Tokio worker，避免 production
  `Handle::block_on` re-entrant panic；driver rejection 通过 typed application `Rejected` 返回。
- `b885f198`（`rfc-0070(R70.4): cut over HTTP conversation recovery`）再将 production conversation recovery
  route 切换到同一 host-bound `ApplicationClient` 与 durable reservation；application contract 新增窄化 typed
  recovery action/outcome，完整承载 compaction apply、standalone tool-output shrink、checkpoint restore 与
  conversation fork 的结果，并由 HTTP registry direct effect 保留唯一 driver 执行 owner。
  `PrepareCompaction` 当前在 application boundary 返回 typed preview-required rejection，因为现有 contract 尚
  不能无损携带 process-local preview/review；它没有被降级为 generic uncertain 或伪造 settled。production
  application-client regression 覆盖该拒绝语义。
- `rfc-0070(R70.4): route HTTP compaction preparation through application port`（本轮切片）移除 production HTTP
  bridge 对 `PrepareCompaction` 的 preview-boundary 预拒绝，使该 action 进入既有 typed registry owner；stale
  recovery binding 仍返回 typed rejection。production application regression 与 HTTP application 测试通过。
  该切片不宣称完成四表面 conformance、configuration/session lifecycle 或完整 R70.4 exit gate。
- `rfc-0070(R70.4): route TUI session transitions through application port`（本轮切片）将 `StartNewSession` 与
  `SwitchSession` 送入同一 application reservation/worker edge；application payload 只使用 host-owned opaque
  `SessionItemId`，真实 session path 与可选 attachment-recovery binding 由 TUI adapter 私有 resolver 保留。
  application `6/6`、TUI session `169/169`（`2 ignored`）、package check 与 production-library strict clippy 通过；
  configuration save/reboot、terminal lifecycle、四表面 conformance 与完整 R70.4 exit gate 仍未闭合。
- `rfc-0070(R70.4): route TUI attachments through application port`（本轮切片）新增无损
  `SubmitPromptWithAttachments` application command，复用 kernel image-attachment contract；TUI worker edge
  还原既有 typed attachment command，provider/runtime ownership 不变。application `6/6`、package check、TUI
  定向测试、strict library clippy 与 diff check 通过；configuration、terminal lifecycle、四表面 conformance
  与完整 R70.4 exit gate 仍未闭合。

### R70.5：package topology foundation（本轮切片）

- product implementation package 已改名为 `sigil-tui-app`；workspace 新增 `sigil-tui-core`、
  `sigil-tui-ratatui` 与 public `sigil-tui` facade。`cargo metadata --locked` 已确认 core 无依赖、Ratatui
  adapter 只依赖 core 与 Ratatui、facade 只依赖 core/adapter；CLI 已切换到 app package。
- 该切片只建立真实 package identity 与依赖拓扑，现有 Sigil product modules 尚未全部物理移出 app package，
  因此 R70.5 package-identity/public-source exit gate、R70.4 四表面 conformance 与 R70.6 runner 下沉仍未闭合。
- `rfc-0070(R70.4): route TUI terminal cancellation through application port`（`c593e5c5`）新增
  transport-neutral `ApplicationTerminalTaskIdentity` 与 `RunCommand::CancelTerminalTask`。TUI production adapter
  先通过 application reservation/dispatch，再在 worker 边界还原为私有 `TerminalTaskControlIdentity`；owner
  scope、run/task identity 与 expected generation 均保持不透明且有界。application `7/7`、TUI package check、
  application/TUI strict library clippy 与 diff check 通过；terminal PTY 生命周期、configuration save/reboot、
  四表面 conformance 与完整 R70.4 exit gate 仍未闭合。
- `rfc-0070(R70.4): route TUI session maintenance through application port`（`df28fe0d`）将 session inspect、fork、
  export、pin、delete preview/apply 与 retention apply 收敛为 typed `SessionCommand::Maintain`。application payload
  只携带 opaque `SessionItemId`、request id 与明确 operation，真实路径、fork route、delete/retention preview 仅由
  TUI adapter 私有 resolver 保留。application/TUI package check 与 production-library strict clippy 通过；
  configuration save/reboot、terminal PTY lifecycle、四表面 conformance 与完整 R70.4 exit gate 仍未闭合。
- `rfc-0070(R70.4): route TUI MCP OAuth through application port`（`fa6b3de4`）新增 typed `McpCommand::OAuth` 与
  `ApplicationMcpOAuthAction`；TUI adapter 仅提交 opaque OAuth binding 与 action，manual callback secret 留在
  adapter 私有 map，直到 worker 边界才恢复为 `McpOAuthUserAction`。application/TUI package check、application
  `7/7`、strict library clippy 与 diff check 通过；configuration save/reboot、terminal PTY lifecycle、四表面
  conformance 与完整 R70.4 exit gate 仍未闭合。
- `rfc-0070(R70.4): route session retention preview through application port`（`90f9bf93`）将 retention preview 从
  AppState 直接 enqueue legacy worker 改为 typed `SessionCommand::Maintain` 的 `PreviewRetention` operation；
  retention policy 通过 adapter-owned opaque binding 解析到 worker。session lifecycle 定向测试、
  application/TUI package check、strict library clippy 与 diff check 通过；configuration save/reboot、terminal
  PTY lifecycle、四表面 conformance 与完整 R70.4 exit gate 仍未闭合。
- contract：新增独立 `sigil-application`（`publish = false`），不依赖 TUI、Ratatui、runtime、provider、filesystem、
  sandbox 或 transport；直接复用 kernel-owned `ResourceRecoverySurfaceContractV1`，并定义 grouped versioned
  command envelope、host admission scope/subject/client epoch、derived lane/settlement policy、typed domain
  receipt、payload conflict、in-flight/uncertain 状态、scoped snapshot/reducer、gap/reset、bounded async page、
  cancellation、delivery ACK 与 renderer-safe projection。
- identity/replay：reservation key 为 application instance + authenticated subject + durable client epoch + command
  id；connection instance 不进入 key；canonical fingerprint 与 expected frontier/settlement class 绑定。Fake
  application 覆盖 exact replay、跨 payload conflict、scope/frontier 校验；runtime service 将 reserve → dispatch
  started → executor → terminal settle 编排收敛到注入的 reservation store，executor error fail-closed 为
  `Uncertain`，不能把 transport error 当成 effect 未发生。
- persistence/presentation：生产 reservation store 使用 R71 managed storage writer 的 application-control
  namespace，保存 `Reserved`/`DispatchStarted`/terminal receipt 并在重启时保留未决 identity；trusted presenter
  capability 由 broker arm、绑定 marker/content/terminal epoch、单次 consume，session/broker Debug 脱敏且不允许
  ordinary clone/serialization。runtime projection binding 从现有 durable session query 与完整 durable public
  outbox（按 stream sequence 排序，不受 adapter delivery receipt 影响）生成 bounded、path-free snapshot/page，并
  使用 opaque before cursor；outbox delivery 只表示传输进度，不会从状态历史中删除事件。
- reservation journal：`f44d8c10` 将 application-control reservation 从 whole-file replacement 收敛为可重放
  JSONL journal；每个 reserve、dispatch marker 和 terminal receipt 都在 effect 前按顺序持久化，旧 v1 snapshot
  只在首次重开时迁移，重复记录可安全重放，孤立 transition、指纹冲突和终态改写 fail-closed。dispatch marker
  写入失败时会尽力将已存在的 reservation settlement 为显式 `Uncertain`，避免永久卡在无恢复语义的 `Reserved`。
- delivery ACK：新增 runtime-owned 的独立 application-control named namespace；ACK 在绑定的
  application scope、observer generation、frontier 与 event identity 校验通过后才追加到 durable JSONL journal。
  exact duplicate 可重放，event identity 改写、scope/observer 不一致、partial/corrupt record 与容量超限均
  fail-closed；TUI production bridge 不再使用仅校验内存对象的 ACK adapter。
- TUI adapter：生产 launcher 为每个 worker 绑定 runtime `ApplicationPort`，用同一 application scope/frontier
  刷新 projection；共享 ApplicationClient 统一处理 snapshot、reducer、resumable feed、delivery ACK、page/cancel
  与保留 command id 的 retry；client epoch 由 host-owned application/session scope 稳定派生，reconnect 不会因
  随机 epoch 变化而制造第二个 reservation namespace。prompt、cancel、approval decision、lazy-MCP activate/refresh 已通过 application
  reservation service 进入 worker。worker enqueue 尚未有 domain terminal receipt 时明确返回 `Uncertain`，禁止伪造
  `Settled`；未有无损 V1 payload 的旧动作仍保留在迁移期 adapter，不能把 R70.4 误记为最终闭合。
- `aa177ba3`（`rfc-0070(R70.4): retain TUI application client epoch`）固定上述 client identity：epoch 从
  application/session scope 稳定派生，避免 reconnect retry 因随机 epoch 形成第二个 command reservation namespace。
- HTTP adapter：生产 command store 在 `ApplicationControlLog` 的独占 managed namespace 中完成 legacy
  identity/terminal/unfinished/aborted tombstone 导入；旧 compatibility file 只有在 managed snapshot 成功替换
  后才退役，重启会优先 managed state 并重试旧文件退役。HTTP command registry 的 domain execution 仍需后续
  完整迁移到同一 `ApplicationPort`，不能把本次 reservation cutover 误记为四表面 conformance。
- HTTP application bridge：`d0ab3da7` 新增 `/sessions/{session_id}/application`、`/application/page` 与
  `/application/commands`。请求只携带 bounded command id/typed grouped command，client identity 从 host
  header 注入；production driver 从当前 cutover/managed writer 构造 runtime `ApplicationClient`，使用共享
  projection reducer、bounded page、managed application reservation journal 与 per-session durable delivery
  ACK journal。首个无损 command mapping 为 `Run::Cancel`，绑定必须命中该 HTTP session 的 active run；没有
  无损 host mapping 的 grouped command 返回 typed `Rejected`，不通过 generic string payload 绕过 contract。
- HTTP cancel cutover：`cf37303e` 让既有 `/runs/{run_id}/cancel` 在 production 只经由 host-bound
  ApplicationClient 执行；旧 HTTP command-store reservation 不再进入 shipping path。reason 作为 typed
  `RunCommand::Cancel` 的 bounded optional field 保留，response-lost retry 对 uncertain terminal 返回
  `ReplayedUncertain`，不会再次调用 driver。
- HTTP run-start cutover：`bdcd44a4` 让 production `start_run_command` 通过同一 host-bound
  ApplicationClient、durable reservation journal 和 runtime executor；HTTP DTO 的 permission/model/
  reasoning/skill/agent/task continuation 字段被映射为 typed `RunStartOptions`，不再把 start 命令交给
  legacy HTTP command-store reservation。普通 prompt 与 task continuation 同时存在或同时缺失都会被
  fail-closed 拒绝；真实 production driver application-client regression 覆盖了 start 的 uncertain
  recovery binding。
- HTTP approval cutover：`52b7724b` 让 production approval command route 通过同一 host-bound
  ApplicationClient 与 application reservation；approval request identity、tool/policy hash、expiry、decision、
  family pattern 和 reason 由 provider-neutral `ApplicationApprovalResolution` 承载，执行前重新绑定到
  HTTP registry 的 exact approval guard。旧 HTTP command-store approval reservation 仅保留给 `cfg(test)` 合成
  driver，uncertain delivery 通过 `Uncertain`/`ReplayedUncertain` recovery binding 返回。
- HTTP user-input cutover：`7be30de7` 让 production user-input decision route 通过同一 host-bound
  ApplicationClient 与 durable reservation；request id、generation、request hash、kernel-owned typed decision
  和 permission mode 由 application command 显式承载，执行后仅通过不含答案内容的 opaque recovery binding
  暴露 continuation identity。旧 command-store user-input reservation 仅保留给 `cfg(test)` 合成 driver。
- HTTP queue cutover：`5c3b33be` 让 production conversation queue route 通过同一 host-bound
  ApplicationClient 与 durable reservation；`ConversationCommand::Queue` 明确携带 queue-generation CAS 与
  typed queue actions，driver 保留 exact prompt/material、foreground-owner 与 session mutation guard。
  legacy queue command-store reservation、waiter 与 secret-safe fingerprint helper 仅保留给 `cfg(test)` 合成
  driver；production application executor 的同步 driver 调用移出 Tokio worker，stale/conflict 不会被包装成
  generic uncertain 后的成功。
- snapshot-feed：`OpenProjectionRequest` 可携带 resume frontier；runtime 以 durable session stream sequence 为
  application frontier，在 bounded feed 中逐 record 生成 scope/generation/digest/前后 frontier 一致的
  `ProjectionReplaced` event。outbox projection 保留 durable record order，不再用跨 run 不唯一的 run-local sequence
  排序；缺序、过旧或超出 feed bound 返回 reset/gap，不能拼接不连续状态。transcript page 通过 kernel
  `SessionStreamRecordReader` 逐行验证 V2 envelope/session identity，并只保留有界消息、工具名称和 UTF-8 安全文本；
  不再把整个 durable session 读入 runtime 内存。
- cold-cache qualification：新增 opt-in 的 cold_cache_transcript_page_100k_keeps_the_resident_page_bounded
  fixture/script，实际写入并逐行回放 100,000 条 durable user message，只返回 32 条 bounded page，断言首尾
  ordinal、stable before cursor、完整计数与 bounded content；本地结果为 resident_messages=32、
  elapsed_ms=10352，fixture 全部位于测试 TempDir；cold-cache 100k gate 1 passed（100,000 records / 32 resident
  messages）。
- validation：`cargo fmt --all --check`、`cargo check -p sigil-application -p sigil-runtime`、
  `cargo check -p sigil-tui --tests`、application/runtime strict clippy、application tests `3 passed`、runtime
  projection tests `2 passed`、kernel public-event-outbox tests `2 passed`、normal-dependency `r71_shipping_e2e`
  `2 passed`、TUI launcher regression `1 passed`，以及 runtime application-filtered regression `106 passed`、
  `git diff --check`；`transcript_page` scope/boundary/reasoning/UTF-8 regression `3 passed`；本轮
  `sigil-application` client/reducer/ACK/conformance tests `6 passed`、runtime durable ACK tests `4 passed`，
  以及 TUI production dependency check `cargo check -p sigil-tui`；本轮 `sigil-http --lib` 回归为
  `224 passed`，HTTP application client production 定向测试为 `1 passed`，queue targeted regression 为
  `12 passed`，`cargo check --locked -p sigil-http`
  user-input 定向回归 `4 passed`；与 `cargo clippy --locked -p sigil-http --lib -- -D warnings` 通过；本轮 HTTP cancel/application/runtime
  定向回归与四包 strict clippy 通过，新增 uncertain terminal replay 与 HTTP production run-start 测试通过。
- remaining deviations / exit gate：本记录证明 contract/runtime foundation、TUI 首批 production port bridge、
  HTTP reservation cutover、HTTP application bridge 与 resumable snapshot-feed 基础已分别落地，但不关闭
  R70.4。TUI 尚有未迁移旧动作，HTTP `PrepareCompaction` preview boundary 与其余 command routes 尚未完全
  收敛到同一 application service；HTTP 新 bridge 当前已对 start/cancel/approval/user-input/queue 以及
  recovery apply/shrink/restore/fork 提供无损 typed mapping，preview-boundary 与其余未映射 command 明确拒绝。
  feed 的跨 surface ACK/restart
  conformance、所有 shared command 的四表面 conformance、cold-cache 100k page e2e 与完整 migration manifest
  gate 仍需继续完成。TUI ACK 已进入 durable managed writer，但 HTTP/Desktop/CLI 还未全部复用该 delivery
  journal。R70.5 package split 不得在这些条件未满足前开始。
- TUI queue mutation cutover：`37dbbfd4` 将 application contract 扩展为 queue projection、共享
  queue-generation token 与 typed move action；runtime projection 从 durable conversation queue records 重建
  paused/items/status/dispatchable 状态。TUI production main-thread 的 enqueue、edit、cancel、move、pause/resume
  先通过 generation-bound `ApplicationClient` 与 runtime reservation service，再由 worker executor 发出原有
  typed worker command；未有 worker terminal domain receipt 时保持 `Uncertain`。HTTP 复用相同 generation encoder。
  HTTP 无 anchor 的 Move、TUI 非主线程 target、reorder/interrupt 等不能无损映射的路径维持 typed rejection 或
  明确 legacy adapter 边界。application/runtime/HTTP/TUI 回归与四包 strict clippy 通过（TUI `1720/1720`，3
  ignored）。该 slice 只关闭主线程 queue mutation 子集；TUI task/agent target、promote/send-now、剩余旧动作、
  HTTP compaction preview、四表面 conformance 与完整 migration manifest 仍未闭合。
- queue target follow-up：`f52aecee` 将 application queue contract 扩展为显式的 main-thread、agent-thread、task
  target；target identity 进入 projection 与 queue mutation action，TUI production 的 task/agent queue enqueue、
  promote、send-now 以及 main-thread mutation 通过 generation-bound ApplicationClient 与 typed worker edge。
  TUI enqueue 要求请求 target 与 active target 一致，跨 target request 在 effect 前返回 scope error；HTTP 仍只
  暴露 main-thread queue，TUI-only promote/send-now/move 不被近似映射而是 typed reject。application `6/6`、runtime
  projection `2/2`、HTTP `224/224`、TUI 单线程 `1720/1720`（3 ignored）与四包 strict clippy 通过。该 slice 不关闭
 TUI 其余 command family、HTTP compaction preview、四表面 conformance 或完整 migration manifest。
- user-input follow-up：`c4125bd1` 将 TUI durable request 的 request id、generation、expected request hash 与
  kernel-owned decision 映射为 typed `UserInputCommand::Resolve`；worker 提供 retained command id 时复用为
  application reservation id，否则由 client 生成。worker edge 保留原始 request 字段并以 uncertain delivery
  等待事件 reconciliation。TUI user-input 回归 `9/9`、application `6/6`、runtime service `4/4` 与四包
  strict clippy 通过。permission-mode override 和其他 legacy TUI command family 仍开放，该 slice 不关闭 R70.4。
- plan/task/agent follow-up：`c27fbc47` 将 TUI plan prompt、plan accept/reject/save/revise、task submit/continue/
  pause、agent profile、inline/child-session skill、agent message/close/cancel/background 映射为 typed
  `PlanTaskCommand`/`AgentCommand`；payload 使用 SafeText 或既有 kernel typed request，不把 worker protocol 名称
  或路径带入 application contract。TUI production 经 application reservation service 后再进入现有 worker edge，
  uncertain delivery 等待 durable worker event settlement；HTTP 对这些 TUI-only 操作不虚构 mapping。application
  `6/6`、plan-handoff `19/19`、agent `146/146` 与四包 strict clippy 通过。configuration/permission、verification、
  terminal、recovery preview 与剩余 surface conformance 仍开放。
- permission-control follow-up：`7b27b996` 删除 launcher 对 active-run permission mode 的 direct worker send，新增
  typed `RunCommand::UpdatePermissionMode` 并保留 kernel `PermissionMode` 的原有取值边界；TUI 经 application
  reservation service 后再进入 worker edge，继续用 uncertain delivery 等待 durable worker event。TUI permission
  回归 `20/20`、application `6/6`、四包 strict clippy 与 diff check 通过。persisted configuration CAS、verification、
  terminal、recovery preview 与剩余 surface conformance 仍开放。
- recovery follow-up：`9eddfd6c` 将 TUI compaction start/preview/cancel、checkpoint preview/execute/fork 与 Intent
  Stack load/preview/execute 加入 transport-neutral `ApplicationRecoveryAction`；TUI production 先经过 host-bound
  ApplicationPort reservation，再在 worker edge 还原为既有 typed kernel request，HTTP 对 TUI-only variant 显式
  typed reject。compaction `47/47`、checkpoint `6/6`、Intent Stack `12/12`、application `6/6` 与四包 production
  library strict clippy 通过；all-target strict clippy 仍被本 slice 之外既有
  `sigil-http/src/registry.rs:3344` redundant-closure warning 阻断。该 slice 只关闭 TUI recovery dispatch seam；configuration CAS、verification/maintenance、
  terminal、HTTP compaction preview、四表面 conformance 与完整 migration manifest 仍未闭合。
- verification follow-up：`802c8d00` 将 TUI changed-files diagnostics、mutation-artifact cleanup/delete、verification
  approval/sandbox/rerun 与 integration review/accept 加入 typed `VerificationCommand`；TUI production 先经过
  ApplicationPort reservation，再在 worker edge 还原为既有 typed worker command，application 层不复制 artifact
  lifecycle authority。application `6/6`、verification flow `5/5`、command dispatch `12/12`、worker bridge `104/104`
  与 production-library strict clippy 通过；all-target strict clippy 仍受上述 slice 外的 HTTP test-target warning
  阻断。configuration/session lifecycle、terminal、HTTP compaction preview、四表面 conformance 与完整 migration
  manifest 仍开放。

- TUI configuration-save follow-up：`65562367` 将 production 配置保存拆为 TUI adapter 私有
  `ConfigurationSaveRequest` 与 typed `ConfigurationCommand::Save`。TUI 不再直接调用 credential/config publisher；
  application executor 在 reservation 后消费一次性 draft，完成原有 CAS/config publication，再以 settled receipt
  触发统一的 runtime reboot。敏感 draft 只保存在私有 binding 中，application contract 只传 opaque binding 与
  `config-save-v1` patch marker。测试构建保留隔离 fixture，production build 不再绕过 application port。
  config-flow `118/118`、package check、production-library strict clippy 与 fmt 通过；terminal PTY lifecycle、
  四表面 conformance 与完整 R70.4 exit gate 仍未闭合。

- TUI authority-admission follow-up：`5474f371` 将 `/model` session route、默认模型保存与 permission-mode
  persistence 接入同一 ApplicationPort。provider route 只以 adapter-owned opaque binding 进入
  `ProviderCommand::SelectRoute`，成功 receipt 后由 launcher 重启 worker；配置与权限保存由 typed
  `ConfigurationCommand::Save` 在 application reservation 后执行，busy run 只在 durable config receipt 后提交
  urgent permission override。production session/config actions 不再在 application unavailable 时回退到直接
  worker/path mutation；旧 session fixture 仅保留在测试构建。TUI 全量 lib tests `1720 passed / 3 ignored`，
  migration manifest `276/276`、package check、strict library clippy 与 diff check 通过；HTTP/CLI/Desktop
  四表面 conformance 与完整 R70.4 exit gate仍待完成。

- R70.5 framework contract follow-up：`b5304438` 为 public `sigil-tui` facade 增加 application-neutral 的
  bounded `Surface`/node builder、opaque action hit binding、`App`/`UpdateOutcome` input-update contract 与
  bounded `Text` 类型；新增不依赖 Sigil domain 的 `todo`、`chat` examples 和 independent consumer contract
  tests。新增 `check-r70-package-topology.sh` 使用 `cargo metadata --all-features` 检查 core → ratatui → facade
  的实际 package identity/依赖 allowlist，并扫描 public framework production source 的 domain、filesystem、process
  与 Tokio 依赖；当前 gate 已通过。该 slice 证明了真实第二消费者与 public surface contract，但现有 Sigil product
  modules 尚未全部物理迁出 `sigil-tui-app`，故 R70.5 exit gate、R70.4 四表面 conformance 与 R70.6 runner 下沉
  仍未闭合。

- R70.5 framework module migration follow-up：`26ca4520` 将 `VirtualSequence`、generation-scoped
  `SurfaceItemId`、`HeightIndex`、`ViewportAnchor` 与 `ProjectionPageRequest` 从 Sigil product-private surface
  module 迁入 `sigil-tui-core`；Ratatui-only bounded scratch renderer 迁入 `sigil-tui-ratatui` 并由 facade 暴露。
  product adapter 现在只为其 `Line<'static>` 提供 framework `VirtualSequence` 类型别名，保留 projection ownership
  在 adapter。core、ratatui、framework 与 `sigil-tui-app` 回归、strict clippy、package topology gate 均通过。
  仍未完成全部 public widget/theme/input module 迁移、R70.4 四表面 conformance 或 R70.5 exit gate。

- R70.5 public contract hardening follow-up：`fb13d370` 同步 R70.0 migration manifest 单测到当前冻结的 276 个
  production variants；`SurfaceNode` 与 `VirtualSequence` 不再暴露可绕过构造/校验的可变字段，改为只读访问器，
  并为 virtual sequence generation/identity invariant 增加 core regression。TUI live panel 已迁移到只读 accessor。
  manifest `276/276`、core/ratatui/facade tests、app check 与 diff check 通过；该 slice 仍不关闭 R70.4
  四表面 conformance、R70.5 完整模块迁移或后续 R70.6-R70.8。

- R70.4 surface conformance follow-up：`b6a853f9` 将 application fixture 扩展为 TUI keyboard、TUI mouse、Desktop、
  HTTP、CLI 五个入口；每个入口都从同一 scoped snapshot/frontier 执行 bounded page/cancel，再用相同 command id
  验证 settled/replay 的 domain receipt、settlement 与 frontier 完全一致。application `7/7`、定向 conformance、
  strict clippy 与 diff check 通过。该 fixture 关闭了 contract-level surface parity 缺口；真实 HTTP/Desktop
  transport smoke、legacy cutover、terminal lifecycle 和完整 R70.4 exit gate 仍需继续收口。

- R70.4 terminal projection follow-up：`4a1b6341` 将 kernel durable `TerminalTaskProjection` 重放为有界、无路径的
  `TerminalSurfaceProjection`，纳入 application snapshot/feed；每个 task 只暴露 task id、generation、状态、readiness
  与输出计数/摘要 hash，owner、进程句柄、命令和取消路由仍留在 host/runtime。TUI refresh 保存该 projection，并优先
  用它校验 terminal cancel 的 active 状态与 generation，缺失 projection 时才保留既有 durable-entry fallback。
  application `7/7`、runtime projection `3/3`、TUI app check、fmt 与 diff check 通过；本 slice 不宣称关闭 PTY
  owner lifecycle、四表面真实 transport smoke 或完整 R70.4 exit gate。

- R70.5/R70.6 package-boundary follow-up：当前大而全的产品实现包改为内部 `sigil-tui-host`，并新增独立
  `sigil-tui-app` adapter package；后者只依赖 `sigil-application` 与 public `sigil-tui` facade，持有 bounded
  `ApplicationClient` 和 framework `App` surface，不导入 kernel/runtime/filesystem/process/Tokio。CLI 与 R71
  shipping scripts 改用 host package，避免 facade 与产品 crate 共享同名 Rust library。`check-r70-package-topology.py`
  现同时验证 app adapter 的 allowlist 与 public source markers；metadata、topology、app/host/sigil check 通过。
  这完成 package identity 的可执行基线，但 host 内旧 renderer/runner 尚未全部下沉，R70.5/R70.6 exit gate 与
  R70.4 full gate 仍未关闭。

- R70.5 framework primitives follow-up：`sigil-tui-core` 新增 application-neutral `SemanticTheme`/`ThemeRole`/
  `ThemeColor` 与 `InputEvent::validate` bounded contract，facade/prelude 重导出 theme primitives，独立
  `sigil-tui-app` adapter 在处理输入前执行相同 input validation。core、facade、app tests、fmt、package topology
  与 host check 通过；Sigil-specific Crossterm decoder/Ratatui palette 仍属于 host adapter，R70.5 full module
  migration 和 R70.6 runner extraction 仍需继续。

- R70.4 application exit gate follow-up：新增 `scripts/run-r70-application-gate.sh`，把 R70.4 的退出条件固化为可
  重复执行的 gate：closed migration manifest、public package topology、application contract、runtime
  projection/service、五表面 shared frontier/receipt fixture、production HTTP/TUI adapter tests，以及 100k
  cold-cache transcript test。该 gate 在当前 clean commit 全部通过；R70.4 的 application contract、production
  port、surface conformance 与 cold-cache 条件现已闭合。R70.5 public module completion 与 R70.6-R70.8 仍未闭合。

- R70.5 framework contract completion follow-up：`b5d581bc` 为 public facade 发布 host-owned `UiRuntimeDriver` 生命周期
  契约、renderer-neutral 的 `SurfaceUpdate`/`PreparedRender`/presentation state，以及 bounded standard widget
  declarations（包括 box、text、stack、scroll、virtual list、input、button、select、modal、popover、status、card
  与 markdown 类别）。独立 framework consumer test 覆盖 widget lowering 且不依赖 Sigil domain。该 slice 不引入
  application、filesystem、process 或 Tokio 依赖；R70.5 publication metadata/feature/docs gate 与 R70.6 host
  ownership audit 仍待完成。

- R70.5/R70.6 ownership follow-up：`75bfc35a` 将 historical R71 composition 引用从 TUI/HTTP/CLI production
  surfaces 收敛到 runtime `application_host` composition boundary，并将 host runner 从 public module 降为
  crate-private。新增 `check-r70-host-ownership.py` 与 `run-r70-host-ownership-gate.sh`，验证 app 依赖 allowlist、
  application 无 physical authority marker、ProductUpdaterState 仍在 runtime owner、manifest 276/276 与 host
  全量库测试。该 slice 关闭 R70.5 package/module boundary；R70.6 remaining side-effect extraction、R70.7 release
  qualification 与 R70.8 compatibility deletion 仍未闭合。

- R70.7 preview package follow-up：三个公开 package（`sigil-tui-core`、`sigil-tui-ratatui`、`sigil-tui`）已独立
  声明 `0.1.0`、MSRV `1.85`、repository/docs.rs/README/changelog metadata；新增完整 feature powerset、Cargo
  package verification、unpacked package `--all-targets` tests、docs 与 ordered publish dry-run gate。
  `.github/workflows/sigil-tui-preview.yml` 提供 core → Ratatui adapter → facade 的显式发布顺序，默认只做
  qualification，真实 publish 需由 release operator 显式触发。R70.7 已完成；R70.8 仍需 release-cycle/user
  validation 后执行 compatibility deletion。

- R70.7 metadata correction：Cargo 不接受 `package.changelog` 作为稳定 manifest key；三个公开 package 改为
  使用约定位置的 `CHANGELOG.md`，metadata checker 直接校验文件存在，避免发布 gate 带 warning 或把未生效的
  字段误当成 Cargo 发布元数据。preview package gate 的 package/unpack/docs/feature/dry-run 语义不变。
