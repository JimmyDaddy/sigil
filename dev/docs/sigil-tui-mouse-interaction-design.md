# Sigil TUI 鼠标交互功能设计

状态：Implementation Snapshot（2026-06-13）

本文定义 `sigil-tui` 支持鼠标交互时的产品边界、功能范围、事件模型、状态模型、实现分层、测试策略和分阶段交付计划。

## 当前状态

截至 2026-06-13，本设计状态如下：

| 范围 | 状态 | 说明 |
| --- | --- | --- |
| Phase 1 基础点击、滚轮、拖选复制 | 已落地 | live panel、slash、tool card、info rail、composer focus、transcript 文本选择和 OSC52 copy 均复用现有状态路径 |
| Phase 2 approval modal 鼠标交互 | 已落地 | file row、hunk navigation、diff view、metadata、allow/deny action 均有明确 hit area |
| Phase 3 setup/config/session selector | 已落地 | setup/config 字段与 footer action、session row 与确认 action 均支持鼠标选择和确认 |
| 文本选择增强 | 已落地 | 支持按列选择、`Ctrl-C` 复制状态提示、OSC52 兼容开关 |
| Terminal capability / mouse capture 开关 | 已落地 | `[terminal].mouse_capture`、`[terminal].osc52_clipboard`、`[terminal].scroll_sensitivity` 进入配置、`/config` 和 `/doctor` |
| Phase 4 小交互 | 已落地 | composer 点击定位光标、tool card header / hidden-preview 行展开/折叠、tool card hover visual state、可配置滚轮灵敏度 |
| 推迟项 | 明确推迟 | hover tooltip/preview、双击手势、拖拽 resize、右键菜单、直接操作 terminal 原生 scrollback |

相关约束：

- [`../governance/code-standards.md`](../governance/code-standards.md)
- [`../governance/engineering-standards.md`](../governance/engineering-standards.md)
- [`sigil-rust-agent-core-technical-solution.md`](sigil-rust-agent-core-technical-solution.md)

## 1. 背景

`sigil` 的主入口是 TUI，而不是命令集合。当前 `sigil-tui` 已使用 `ratatui` + `crossterm`，TUI 入口可按配置启用 mouse capture；事件循环能收到 `CrosstermEvent::Mouse`，并已通过 `mouse.rs`、`ui/layout_snapshot.rs`、`app/mouse_flow.rs` 建立可维护的坐标命中、区域优先级、点击动作、状态转换、安全约束、提示同步和测试覆盖。

## 2. 设计目标

### 2.1 产品目标

1. 鼠标是键盘交互的补充，不替代键盘主路径。
2. 用户可以用鼠标完成高频、低歧义的 TUI 操作：
   - 滚动当前区域
   - 聚焦 composer
   - 选择 slash command / session / config 候选项
   - 选择或展开 tool card
   - 在 live panel 中拖选可见文本并复制
   - 在 approval modal 内选择文件、切换 diff、滚动 diff、允许或拒绝工具调用
   - 在 info rail 中切换状态卡片或查看 agent detail
3. 鼠标动作必须复用现有 `AppState` 行为与 `AppAction`，不能绕过审批、session 或 worker command 路径。
4. 鼠标交互不能破坏当前 chat/composer-first 心智：
   - composer 默认保持主输入位置
   - stable history 仍优先进入 terminal 原生 scrollback
   - live panel 只承载当前尾部、当前状态和可操作卡片

### 2.2 工程目标

1. 鼠标事件处理只属于 `crates/sigil-tui`。
2. 不向 `sigil-kernel` 暴露 UI 坐标、鼠标事件或 provider 私有语义。
3. 渲染和命中测试共享同一套布局计算，避免“看起来在这里，点起来在别处”。
4. `AppState` 继续作为 façade；鼠标状态、hit-test、layout snapshot 应拆到独立模块，避免 `app.rs` 继续膨胀。
5. 所有会影响审批、session 或 TUI 状态机的行为必须有状态转换测试。

## 3. 非目标

以下能力明确不在当前实现范围内：

1. 拖动改变面板大小。
2. hover tooltip 或 hover preview。
3. 双击、三击、多指手势。
4. 依赖 GUI 桌面事件的能力。
5. 鼠标直接操作 terminal 原生 scrollback 中的历史消息。

这些能力可以作为后续扩展，但不进入当前实现闭环。

## 4. 用户体验原则

### 4.1 键盘主路径不退化

每个鼠标动作都必须对应一个已有或明确新增的键盘动作。例如：

| 鼠标动作 | 等价键盘路径 |
| --- | --- |
| 点击 composer | 切回 composer focus |
| 点击 slash 候选项 | Up/Down + Enter |
| 点击 tool card | `Ctrl-G` + `Alt-J/K` |
| 点击展开 tool card | `Ctrl-T` |
| 滚动 timeline | Up/Down, PageUp/PageDown, `Ctrl-U/D` |
| 点击 approval allow | `Y` |
| 点击 approval deny | `N` |
| 点击 approval 文件 | `,` / `.` |
| 点击 approval diff view | `V` |
| 点击 metadata toggle | `M` |
| 复制 live panel 选区 | `Ctrl-C` |

如果某个鼠标动作没有合理键盘等价路径，应先补键盘路径或放弃该鼠标动作。

### 4.2 低歧义优先

当前实现只支持用户很容易预期结果的区域：

- 明确按钮
- 明确列表行
- 明确卡片
- 明确输入区
- 明确 diff / preview 滚动区

不支持点击普通文本触发隐藏动作。

### 4.3 审批动作保守

允许或拒绝工具调用属于安全敏感动作：

- 只有点击 footer actions 中明确的 `allow` / `deny` 区域才触发审批。
- 点击 approval 标题、summary、diff、文件行不能隐式 allow 或 deny。
- pending approval 存在时，普通页面区域不响应会改变运行状态的鼠标动作。
- approval decision 仍必须返回 `AppAction::ApprovalDecision`，由现有 worker command 路径执行。

### 4.4 当前区域滚动

滚轮应优先作用于鼠标所在区域，而不是只看全局状态：

1. 鼠标在 approval diff 区：滚动 diff
2. 鼠标在 approval file list 区：移动文件选择或滚动文件列表
3. 鼠标在 slash overlay：移动 slash selection
4. 鼠标在 live panel：滚动 timeline
5. 鼠标在 info rail：滚动或切换 activity/sidebar card
6. 鼠标在 setup/config 列表：移动当前选中项
7. 未命中明确区域：保持现有 fallback，即 approval 优先，否则 timeline

## 5. 功能范围

### 5.1 Phase 1：基础鼠标可用性

Phase 1 目标是让鼠标在主要 TUI 表面可用，但不碰审批安全动作。

必须支持：

1. 区域感知滚轮：
   - live panel 滚动 timeline
   - approval diff 滚动 diff
   - slash overlay 移动候选项
   - info rail 滚动或切换当前 activity card
2. 点击 composer：
   - 切换 `active_pane = PaneFocus::Composer`
   - 点击 input 区域时定位输入光标
   - 点击非 input 区域时只聚焦
3. 点击 slash candidate：
   - 更新 slash selector selection
   - 常规命令单击执行
   - 危险命令需要第二次点击同一候选项确认
4. 点击 tool card：
   - 选择被点击的 tool card
   - 单击只选中
   - 双击不支持
5. 点击 info rail card：
   - 切换 `active_pane = PaneFocus::Activity`
   - 更新 sidebar selected card
   - 点击 agent row 时展示 detail，复用当前 Enter 行为
6. live panel 文本拖选与复制：
   - 左键按下记录可见 timeline 行锚点
   - 拖动更新选区范围
   - `Ctrl-C` 在存在选区时复制选中文本；没有选区时保留取消/退出语义
   - 复制动作返回 app-local `AppAction`，由主循环负责终端剪贴板输出

不支持：

- hover tooltip / preview
- 双击、右键菜单、拖拽 resize
- terminal 原生 scrollback 历史消息直接操作

### 5.2 Phase 2：审批 modal 鼠标支持

Phase 2 目标是让工具审批可被鼠标完整操作，但保持安全保守。

必须支持：

1. 点击 approval file row：
   - 更新 `approval_selected_file_index`
   - 重置 `approval_selected_hunk_index`
   - 重置 `approval_scroll_back`
2. 点击 hunk marker 或 diff status 中的 hunk navigation 控件：
   - 等价 `[` / `]`
3. 点击 diff view 控件：
   - 等价 `V`
4. 点击 metadata 控件：
   - 等价 `M`
5. 点击 allow / deny action：
   - 返回 `AppAction::ApprovalDecision`
   - 只接受左键 down，和普通可点击行保持一致
6. approval modal 打开时：
   - modal 之外普通页面点击被忽略
   - modal 之外滚轮不滚普通 timeline

### 5.3 Phase 3：表单与列表增强

Phase 3 目标是让 setup/config/session selector 也能被鼠标操作。

必须支持：

1. setup/config：
   - 点击字段行选择字段
   - 点击 footer action 选择 action
   - 点击已选字段的可编辑区域进入编辑
2. session selector：
   - 点击 session row 选择
   - 点击确认 action 切换 session
3. modal：
   - 点击按钮或 footer action
   - 点击 backdrop 不关闭，除非该 modal 明确支持取消

### 5.4 Phase 4：产品化小交互

已落地：

1. 点击 composer input 区域按坐标定位 `input_cursor`。
2. 点击 tool card header 或 collapsed hidden-preview 提示行直接展开或折叠；点击其他 body 区域仍只选中。
3. `[terminal].scroll_sensitivity` 控制 transcript 和 approval diff 的滚轮步长，默认保持 `3`。
4. terminal capability 检测与兼容性开关进入 `sigil doctor`、TUI `/doctor` 和 `/config`。
5. 支持 tool card hover highlight，但只作为视觉反馈，不触发业务动作，不写 session。

仍推迟：

1. hover tooltip 或 hover preview。
2. 双击、右键菜单、拖拽 resize。
3. 鼠标直接操作 terminal 原生 scrollback 中的历史消息。

## 6. 信息架构与区域模型

鼠标支持已把 renderer 内的临时布局计算提取为可复用模型，采用以下模块：

```text
crates/sigil-tui/src/mouse.rs
crates/sigil-tui/src/ui/layout_snapshot.rs
```

### 6.1 LayoutSnapshot

`LayoutSnapshot` 描述当前 frame 的可点击区域：

```rust
pub struct LayoutSnapshot {
    pub screen: Rect,
    pub live_panel: Rect,
    pub composer: Rect,
    pub footer: Rect,
    pub info_rail: Rect,
    pub slash_overlay: Option<SlashOverlayHitAreas>,
    pub approval_modal: Option<ApprovalHitAreas>,
    pub modal: Option<ModalHitAreas>,
    pub setup: Option<SetupHitAreas>,
    pub config: Option<ConfigHitAreas>,
}
```

设计要求：

1. `ui/shell.rs`、`ui/approval.rs`、`ui/slash_overlay.rs` 等 renderer 使用与 `LayoutSnapshot` 相同的布局函数。
2. `LayoutSnapshot` 只描述区域，不持有业务状态。
3. 每个区域使用 `ratatui::layout::Rect`，命中判断统一走 helper。
4. 布局 snapshot 可在每次 render 前或 terminal resize 后更新。

### 6.2 HitTarget

`HitTarget` 是坐标命中后的抽象目标：

```rust
pub enum HitTarget {
    Composer,
    Footer,
    LivePanel,
    InfoRailCard(InfoRailCardTarget),
    InfoRailAgentRow { index: usize },
    ToolCard { timeline_index: usize },
    SlashCandidate { index: usize },
    ApprovalSummary,
    ApprovalFileRow { index: usize },
    ApprovalDiff,
    ApprovalAction(ApprovalMouseAction),
    SetupField(SetupMouseTarget),
    ConfigField(ConfigMouseTarget),
    ModalAction(ModalMouseAction),
    Background,
}
```

命中目标必须表达“用户点了什么”，不能直接表达“系统要改什么状态”。状态变化放在 `AppState` handler 里完成。

### 6.3 命中优先级

命中顺序必须固定：

1. active modal
2. approval modal
3. slash overlay
4. setup/config screen
5. composer
6. live panel
7. info rail
8. footer
9. background

理由：

- modal / approval / slash overlay 是覆盖层，必须抢占底层点击。
- composer 与 live panel 同属主列，不能因为边界重叠导致点击输入区滚 timeline。
- info rail 是独立右侧区域，点击后才进入 activity/sidebar 语义。

## 7. 事件模型

当前入口已有：

```rust
CrosstermEvent::Mouse(mouse) => match mouse.kind {
    MouseEventKind::ScrollUp => ...
    MouseEventKind::ScrollDown => ...
    _ => {}
}
```

当前实现采用三层处理：

```text
crossterm MouseEvent
  -> MouseInput
  -> HitTarget
  -> AppMouseOutcome
```

### 7.1 MouseInput

`MouseInput` 是跨 crossterm 的内部事件：

```rust
pub struct MouseInput {
    pub column: u16,
    pub row: u16,
    pub kind: MouseInputKind,
    pub modifiers: MouseModifiers,
}

pub enum MouseInputKind {
    LeftDown,
    LeftUp,
    RightDown,
    ScrollUp,
    ScrollDown,
    Drag,
    Moved,
    Unsupported,
}
```

当前处理：

- `LeftDown` 或 `LeftUp`
- `ScrollUp`
- `ScrollDown`
- `Drag`
- `Moved`

忽略：

- right click
- middle click
- unsupported mouse events

### 7.2 AppMouseOutcome

鼠标 handler 可以产生：

```rust
pub enum AppMouseOutcome {
    Noop,
    Redraw,
    Action(AppAction),
}
```

主循环处理方式：

1. `Noop`：不设置 `needs_render`
2. `Redraw`：设置 `needs_render = true`
3. `Action(action)`：按键盘路径一样交给 worker 或本地 action 分支，再 redraw

### 7.3 AppState handler

已实现入口：

```rust
impl AppState {
    pub fn handle_mouse_event(
        &mut self,
        input: MouseInput,
        layout: &LayoutSnapshot,
    ) -> Result<AppMouseOutcome>;
}
```

职责：

1. 根据 layout hit-test 得到 `HitTarget`
2. 根据当前模式决定是否允许动作
3. 复用已有状态转换方法
4. 必要时返回 `AppAction`

不应在主循环里直接改 `AppState` 字段。

## 8. 状态模型

### 8.1 PaneFocus

当前 `PaneFocus` 只有：

```rust
pub enum PaneFocus {
    Composer,
    Activity,
}
```

Phase 1 可继续使用这两个状态：

- 点击 composer：`Composer`
- 点击 info rail 或 activity card：`Activity`
- 点击 tool card：保持 `Composer`，只更新 selected tool card
- 点击 slash overlay：保持 `Composer`
- pending approval 存在时：不通过 `PaneFocus` 表达 modal focus，继续由 `pending_approval` 抢占输入

后续如果 activity 面板继续扩展，再考虑：

```rust
pub enum PaneFocus {
    Composer,
    Activity,
    ToolCard,
}
```

当前不新增 `ToolCard` 焦点层，避免焦点模型过早变复杂。

### 8.2 鼠标选择状态

可见选择继续复用已有字段：

- `selected_tool_timeline_entry`
- `sidebar_selected_card`
- `sidebar_agent_selected`
- `approval_selected_file_index`
- `approval_selected_hunk_index`
- `slash_selector_index`
- setup/config 内部 selected field/action

hover 只保留 app-local transient state，用于 renderer visual feedback：

- 不写入 session log
- 不产生 control entry
- 不触发业务动作
- 离开可 hover 区域时清除

### 8.3 审计与 session

鼠标交互本身不是 session 内容，不写入 session log。

例外：

- 鼠标触发 approval decision 后，现有 approval / tool execution 路径会产生 session/control 记录。
- 鼠标触发 session switch 后，复用现有 session switch action。
- 鼠标触发 config save 后，复用现有 config save action。

禁止新增“mouse clicked at x,y”这类 session log 记录。

## 9. 具体交互设计

### 9.1 滚轮

| 命中目标 | ScrollUp | ScrollDown |
| --- | --- | --- |
| ApprovalDiff | `approval_scroll_back -= delta` | `approval_scroll_back += delta` |
| ApprovalFileList | approval modal up | approval modal down |
| SlashOverlay | previous candidate | next candidate |
| LivePanel | timeline up | timeline down |
| InfoRail | sidebar previous | sidebar next |
| Setup/Config list | previous field | next field |
| Background | current fallback | current fallback |

滚动 delta：

- timeline：读取 `[terminal].scroll_sensitivity`，默认 `3`
- approval diff：读取 `[terminal].scroll_sensitivity`，默认 `3`
- list selection：每次移动 1 行
- Page-sized scroll 仍由键盘负责，不绑定滚轮

### 9.2 点击 composer

行为：

1. 设置 `active_pane = PaneFocus::Composer`
2. 点击 input 区域时按可见行/列定位 `input_cursor`
3. 点击 header、gutter 或 composer 空白区域时只聚焦，不提交、不清空
4. 清除 transcript 文本选择

### 9.3 点击 slash overlay

行为：

1. 命中候选行时，更新 `slash_selector_index`
2. 常规命令单击执行，复用现有 slash command action
3. 危险命令第一次点击只进入确认态，第二次点击同一候选项才执行
4. Enter 仍是键盘执行路径

### 9.4 点击 tool card

行为：

1. 命中普通 tool card body 时，设置 `selected_tool_timeline_entry`，不展开/折叠。
2. 命中 tool card header 或 collapsed hidden-preview 提示行时，先选中，再复用 `Ctrl-T`/tool view 路径展开或折叠。
3. header/body 点击都不改变 composer 文本。
4. pending approval 存在时，普通 tool card 点击不改变状态。

### 9.5 点击 info rail

行为：

1. 点击 permission card：
   - `active_pane = PaneFocus::Activity`
   - `sidebar_selected_card = Permission`
2. 点击 agents card：
   - `active_pane = PaneFocus::Activity`
   - `sidebar_selected_card = Agents`
3. 点击具体 agent row：
   - 更新 `sidebar_agent_selected`
   - 展示 detail，复用当前 Enter 行为
4. 点击 usage card：
   - `active_pane = PaneFocus::Activity`
   - `sidebar_selected_card = Usage`

当前不支持点击 permission card 直接切换写权限。写权限切换属于风险动作，保留 `BackTab` 或未来明确按钮后再做。

### 9.6 点击 approval modal

Phase 2 行为：

| 区域 | 行为 |
| --- | --- |
| Summary | 无状态变化 |
| File row | 切换 selected file，重置 hunk 和 scroll |
| Diff body | 只聚焦/保留，滚动靠滚轮 |
| View action | 等价 `V` |
| Metadata action | 等价 `M` |
| Hunk previous/next | 等价 `[` / `]` |
| Allow action | 返回 approved `AppAction::ApprovalDecision` |
| Deny action | 返回 denied `AppAction::ApprovalDecision` |
| Modal backdrop | Noop |

allow / deny 触发规则：

1. 只响应左键单击。
2. 当前统一使用 `LeftDown` 触发，与其他可点击区域保持一致。
3. action 区域必须是 footer 中明确 badge 或按钮所在 rect。

### 9.7 setup/config/session

Phase 3 行为：

1. 点击字段行：未选中时选择字段。
2. 点击已选字段：进入编辑或执行当前字段 Enter 行为。
3. 点击 footer action：执行明确 action；保存、关闭、确认等 guard 仍走现有状态机。
4. 直接点击保存必须经过现有 dirty、validation 和 busy guard。

## 10. 实现分层

### 10.1 已采用模块

```text
crates/sigil-tui/src/mouse.rs
crates/sigil-tui/src/ui/layout_snapshot.rs
```

`mouse.rs` 负责：

- `MouseInput`
- `MouseInputKind`
- crossterm mouse event 转换
- `HitTarget`
- `AppMouseOutcome`
- hit-test helper

`ui/layout_snapshot.rs` 负责：

- 顶层 shell 区域计算
- approval modal hit areas
- slash overlay hit areas
- setup/config/modal hit areas

renderer 应逐步改为读取共享 layout helper，而不是重复计算 rect。

### 10.2 主循环变化

`crates/sigil-tui/src/launcher.rs` 的主循环应从直接匹配 scroll 改为：

```text
read mouse event
  -> convert to MouseInput
  -> get latest LayoutSnapshot
  -> app.handle_mouse_event(input, &layout)
  -> process AppMouseOutcome
```

需要注意：

- `LayoutSnapshot` 必须对应当前 terminal size。
- resize 后必须刷新 snapshot。
- render 前后 snapshot 不能和当前 frame 尺寸不一致。

### 10.3 renderer 变化

现有 renderer 中的这些布局应抽成共享 helper：

- shell horizontal split
- main vertical split
- composer area
- live panel area
- footer area
- info rail area
- slash overlay rect
- approval modal area/body/footer/diff/file list

第一阶段可以只抽 shell + slash + approval 需要的部分，不必一次性重写所有 renderer。

## 11. 安全与边界

### 11.1 Provider / kernel 边界

鼠标交互不得进入：

- `sigil-kernel`
- provider crates
- runtime provider/tool registry

允许进入：

- `sigil-tui` 状态模型
- `sigil-tui` renderer layout helper
- `sigil-tui` tests

### 11.2 Approval 边界

禁止：

- 点击 diff 内容直接 allow
- 点击 modal 背景 deny
- 点击文件行后自动 allow
- 绕过 `AppAction::ApprovalDecision`

必须：

- 保留键盘 `Y/N`
- 鼠标 approval decision 走同一 worker command 路径
- 对 allow/deny 命中区域写测试

### 11.3 Session 边界

鼠标 UI 状态不进入 session log。只有被鼠标触发的业务动作按现有路径进入 session/control。

## 12. 测试策略

### 12.1 单元测试

优先补状态转换测试：

1. `mouse_scroll_live_panel_moves_timeline`
2. `mouse_scroll_approval_diff_moves_approval_scroll`
3. `mouse_scroll_uses_terminal_scroll_sensitivity`
4. `mouse_click_composer_focuses_and_positions_cursor`
5. `mouse_click_slash_candidate_selects_entry`
6. `mouse_click_tool_card_body_selects_without_toggling`
7. `mouse_click_tool_card_header_toggles_card`
8. `mouse_move_tool_card_updates_hover_visual_state`
9. `mouse_click_info_rail_agent_row_shows_detail`
10. `mouse_click_approval_file_selects_file`
11. `mouse_click_approval_allow_returns_approval_action`
12. `mouse_click_approval_deny_returns_approval_action`
13. `mouse_click_behind_approval_modal_is_noop`

### 12.2 Hit-test 测试

必须覆盖：

1. desktop 宽屏布局
2. 窄屏布局
3. approval modal 覆盖普通区域
4. slash overlay 覆盖 composer 上方区域
5. 边界坐标：
   - 左上角
   - 右下角
   - rect 外一格
   - zero-width / zero-height 防御

### 12.3 Renderer / snapshot 一致性测试

如果抽出 `LayoutSnapshot`，应测试：

1. shell snapshot 与 renderer split 结果一致
2. approval snapshot 与 approval renderer 使用同一 helper
3. slash overlay snapshot 与 overlay renderer 使用同一 helper

### 12.4 手工冒烟

每个阶段至少做一次真实 TUI 冒烟：

1. 启动 TUI
2. 输入 prompt
3. 用滚轮滚 live panel
4. 打开 slash selector 并点击候选项
5. 触发 tool preview 并点击 tool card
6. 触发 approval 后滚动 diff
7. Phase 2 后用鼠标 allow / deny 一次写工具

## 13. 文档同步要求

实现鼠标交互时需要同步：

1. `README.md`
   - TUI 交互说明
   - approval 操作说明
2. `dev/docs/sigil-rust-agent-core-technical-solution.md`
   - 如果最终 mouse support 改变第一代 TUI 信息架构，需要回填简要说明
3. `dev/governance/*`
   - 只有新增工程约束时才更新
4. keyboard help / info rail controls
   - 鼠标不是 keyboard shortcut，但 UI 提示不能继续暗示相关动作只能键盘完成

如果只是新增本设计文档，不需要同步 README。

## 14. 分阶段交付计划

### 14.1 Phase 0：结构准备

交付物：

1. 新增 `mouse.rs`
2. 新增 `ui/layout_snapshot.rs`
3. 主循环保留现有滚轮行为，但通过新 mouse handler 走
4. 现有 `mouse_scroll_moves_transcript` 测试迁移或保留

验证：

```bash
cargo fmt --all --check
cargo test -p sigil-tui mouse_scroll
cargo check -p sigil-tui
```

### 14.2 Phase 1：基础区域与列表

交付物：

1. 区域感知滚轮
2. composer click focus
3. slash candidate click select
4. tool card click select
5. info rail card / agent row click
6. live panel 文本行选区与复制

验证：

```bash
cargo fmt --all --check
cargo test -p sigil-tui mouse_
cargo check -p sigil-tui
```

### 14.3 Phase 2：Approval modal

交付物：

1. approval hit areas
2. file row click
3. diff view / metadata / hunk action click
4. allow / deny click
5. modal backdrop noop

验证：

```bash
cargo fmt --all --check
cargo test -p sigil-tui approval
cargo test -p sigil-tui mouse_
cargo check -p sigil-tui
```

并做一次真实 TUI approval 冒烟。

### 14.4 Phase 3：Setup / Config / Session

交付物：

1. setup/config field hit-test
2. config footer action hit-test
3. session selector row hit-test
4. modal action hit-test

验证：

```bash
cargo fmt --all --check
cargo test -p sigil-tui setup
cargo test -p sigil-tui config
cargo test -p sigil-tui session
cargo test -p sigil-tui mouse_
cargo check -p sigil-tui
```

### 14.5 Phase 4：体验增强

已交付：

1. composer 光标定位
2. tool card header / hidden-preview 行点击展开/折叠
3. terminal capability / config switch（`[terminal].mouse_capture` / `osc52_clipboard` / `scroll_sensitivity`）
4. tool card hover visual state
5. scroll sensitivity 配置进入 `/config` 和 `/doctor`

状态模型仍保持 `PaneFocus::Composer` / `PaneFocus::Activity`，tool card hover 只作为 app-local transient state。

## 15. 决策记录和推迟项

1. 鼠标按钮统一用 `LeftDown` 还是 `LeftUp` 触发？
   - 当前统一用 `LeftDown`
   - 选区用 `LeftDown` 建立锚点，`Drag` 更新范围，`LeftUp` 结束拖动
2. slash candidate 是否支持第二次点击执行？
   - 常规命令单击执行
   - 危险命令第一次点击进入确认态，第二次点击同一候选项执行
3. 点击 tool card 是否直接展开？
   - 点击 body 只选中
   - 点击 header 选中并展开/折叠
4. info rail 的 permission card 是否允许点击切换写权限？
   - 当前推迟；后续若做必须有明确按钮与确认语义
5. 是否需要配置开关禁用 mouse capture？
   - 已补 `[terminal].mouse_capture`
   - 不支持 OSC52 的终端可关闭 `[terminal].osc52_clipboard`
   - 滚轮步长通过 `[terminal].scroll_sensitivity` 调整
6. 仍推迟哪些鼠标能力？
   - hover tooltip/preview、双击手势、拖拽 resize、右键菜单、直接操作 terminal 原生 scrollback

## 16. 当前结论

`sigil-tui` 可以支持类似现代 TUI coding agent 的鼠标辅助体验，但应该按“区域命中 + 状态转换 + 安全审批”的方式做，而不是在主循环里直接按坐标写业务分支。

已采用路线：

1. 先抽 `MouseInput`、`HitTarget`、`LayoutSnapshot`
2. 先落区域感知滚轮和低风险点击
3. 再做 approval modal 鼠标动作
4. 最后扩展 setup/config/session、文本选择、terminal 配置和 Phase 4 小交互

这个顺序能最大化复用当前 `AppState` 行为，同时避免破坏 TUI-first、composer-first 和 approval-safe 的产品边界。
