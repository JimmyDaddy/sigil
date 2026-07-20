# RFC-0047 Desktop Workbench UX Reset V1

状态：active / R47.0-R47.5 complete / R47.6 ready

创建日期：2026-07-20

基线：

- [RFC-0046](0046-desktop-material-derived-design-system-and-theme-preferences-v1.md)
- [RFC-0045](0045-desktop-ui-ux-foundation-v1.md)
- [RFC-0044](0044-desktop-shell-mvp-v1.md)

## 1. Problem statement

RFC-0046 建立了 semantic token、内部 primitive、主题持久化和响应式基础，但真实 dogfood 证明完整工作台仍存在产品级问题：

- 应用 chrome、workspace、session、conversation 与 review 通过多层容器重复表达身份和边界；
- navigation 与 timeline 使用相同色调和卡片权重，主任务不突出；
- 工具结果默认暴露 transport JSON，消息阅读面被边框和常驻 `Copy` 文案打断；
- composer 只有 prompt 和 Run，缺少 model、approval mode、effort 与 context usage；
- 默认窗口和最小窗口没有以“可完成编码任务”为约束；
- fixture catalog 只证明零散组件，不能阻止完整页面层级、密度和滚动结构回退。

因此本 RFC 不继续以局部 CSS 修补 RFC-0046，而是冻结一个以 conversation/composer 为中心的 desktop workbench。

## 2. Product contract

### 2.1 Workbench hierarchy

1. 顶部只保留紧凑的 application toolbar：真实 Sigil mark、workspace selector、new conversation、theme 和上下文 action。
2. 左侧是可折叠 session rail；正常行最多两层主要信息，诊断和筛选按需展开。
3. 中央 conversation 是唯一主阅读面；timeline 不再包在多层同权卡片中。
4. Composer 固定在中央底部，是默认主操作；运行时 Send 变 Stop，状态不能只存在于提示文字。
5. Verification、diff、changes 与 detail 使用按需 supporting drawer；无选中对象时不占宽。

### 2.2 Visual hierarchy

- canvas、surface、outline 与普通文本使用 neutral palette；品牌绿只表达品牌、主要操作和当前焦点。
- success、warning、danger、info 使用独立语义色，不复用 primary。
- assistant 正文默认无卡片边框；user message 使用轻量容器；工具组使用语义摘要而不是每个事件一个警示卡。
- 熟悉且高频的 toolbar action 使用 icon + accessible name + tooltip；危险或语义不明确的动作保留短文本。
- 默认页面不得出现超大 marketing heading、重复主 CTA、`No text payload.` 或默认展开的原始 transport JSON。

### 2.3 Window and adaptive behavior

- 首次窗口目标为 `1280×820`，最小可工作尺寸为 `900×640`。
- 窗口尺寸与位置由 native application store 恢复，并在显示器范围变化后 clamp。
- `< 1100px` 不常驻 supporting pane；`< 900px` session rail 进入 drawer。
- 320px 继续作为 emergency reflow/accessibility probe，但不宣称为完整工作尺寸。

### 2.4 Composer and protocol

Composer 最终必须显示并允许选择当前 model 与 approval mode，显示 context usage，并为 effort/context attachment 保留紧凑入口。
这些值必须来自 typed Tauri/HTTP DTO；renderer 不自行猜测、不读取配置文件，也不获得 bearer、path、process、generic HTTP、
filesystem、shell 或 unrestricted window capability。

Usage 事件必须保留 bounded、provider-neutral 的 token/context facts。Model 和 approval mode 的变更必须作用于精确的新 run request，
不得就地改写 durable session truth 或绕过现有 approval contract。

## 3. Hard invariants

1. RFC-0045 的 transcript、reattach、single-final、approval、cancel、verification、bounded output 和 gap 语义不得回退。
2. Theme、navigation、drawer、model picker 不得 remount active conversation、重复 attach、清空 draft 或破坏 IME composition。
3. Desktop renderer 继续只消费 allowlisted Tauri command/event DTO。
4. Tool success 不等于 verification success；UI 不得用颜色或文案混淆二者。
5. Raw tool payload 只能作为明确的 details/debug 内容；默认摘要来自 bounded semantic projection。
6. 完整应用 fixture 与 viewport evidence 是完成条件；独立 primitive catalog 不能替代产品验收。

## 4. Execution slices

| Slice | Scope | Completion evidence |
| --- | --- | --- |
| R47.0 | Contract、full-app fixture 与 commit freeze | RFC/plan/status 链接、product-rule assertions |
| R47.1 | Chrome、真实 logo、neutral palette、window sizing、theme cycle | frontend/native tests、build、真实 window smoke |
| R47.2 | Session rail hierarchy、diagnostic disclosure、adaptive navigation | 30/100 session fixtures、keyboard/viewport tests |
| R47.3 | Message、Markdown、tool grouping、error/detail hierarchy | reducer/render tests、long transcript fixture |
| R47.4 | Typed run options 与 usage/context projection | Rust/HTTP/OpenAPI/Tauri/frontend contract tests |
| R47.5 | Sticky coding composer、model/mode/context controls | IME/draft/run/stop/reattach tests、real-server smoke |
| R47.6 | Contextual supporting drawer 与 scroll/focus ownership | verification/diff/approval/adaptive tests |
| R47.7 | Full-app visual gate、dogfood 与 completion audit | viewport/theme/zoom matrix、package smoke、no P1/P2 |

Dependency order:

```text
R47.0 -> R47.1 -> R47.2 -> R47.3 -> R47.4 -> R47.5 -> R47.6 -> R47.7
```

## 5. Acceptance gates

- No-workspace 状态只有一个 primary CTA；workspace/session identity 在同一 viewport 不重复。
- 1280×820 下 conversation 与 composer 是第一视觉层级，session rail 不超过主内容宽度的 24%。
- 900×640 下所有主操作可达，没有 document horizontal scroll 或嵌套主滚动。
- Theme 是单个 stateful icon action；显式三选设置可留给后续 Settings，不占主 toolbar。
- Session row 默认不常驻 provider/model；异常诊断不以大块 warning card 挤占列表。
- Assistant 文本和成功 tool 默认不使用绿色容器；失败 tool 不把原始 JSON 作为第一信息。
- Composer 可见当前 model、approval mode、context usage 和 Send/Stop；缺失事实显示 unavailable，不伪造数值。
- 完整应用 fixture 覆盖 no-workspace、30/100 sessions、long transcript、tool error、approval、diff、verification 和 active run，
  并在 1280×820、1024×700、900×640、light/dark、100%/200% zoom 上执行。

## 6. Research basis

- [Apple toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars)：高频 action 紧凑表达，低频 action 渐进披露。
- [Apple windows](https://developer.apple.com/design/human-interface-guidelines/windows)：最小尺寸仍应保持内容可用。
- [Apple launching](https://developer.apple.com/design/human-interface-guidelines/launching)：恢复窗口和应用状态。
- [Material icon buttons](https://m3.material.io/components/icon-buttons/overview)：状态化 icon action 与 accessible label。
- [VS Code chat context](https://code.visualstudio.com/docs/chat/copilot-chat-context)：模型与 context 使用量属于输入区核心状态。

## 7. Non-goals

- 不在本 RFC 中增加 remote daemon、多用户、generic filesystem/window capability 或编辑器内核。
- 不改变 TUI-first 定位，不要求桌面端复制 TUI 的所有低频命令。
- 不以 screenshot diff 代替 keyboard、AX、protocol 和真实运行验证。
