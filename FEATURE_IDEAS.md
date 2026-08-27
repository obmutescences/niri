# 功能改进清单（基于 fork 自研特性）

基于当前分支 `window-picker`（HEAD `878ac7363`）与最近的特性提交盘点，给出值得做的
功能/改进列表。目标方向：**动画、UI、炫酷特性**。

> **更新（用户反馈）**：post-process shader（恢复 `more_features` 分支）**明确不做**，
> 已从路线图中移除，仅保留在"已排除"记录里。以下所有推荐均不依赖 shader。

## 一、现状盘点（已实现的自研特性）

| 特性 | 代码位置 | 说明 |
| --- | --- | --- |
| 键盘驱动的窗口选择器 | `src/ui/window_picker.rs`、`niri-config/src/window_picker.rs` | Super+Tab 覆盖层；网格预览 + 字母标签（A–Z / 超 26 个用 AA–ZZ）；backdrop 变暗/去饱和/模糊；hover 高亮 + 点击；stagger 动画；open/close 分开时长；选中后预览飞回窗口；屏幕录制中包含 picker；workspace parallax + glass warp |
| 液态玻璃 (liquid glass) | `src/render_helpers/liquid_glass.rs`、`background_effect.rs`、`xray.rs`、`framebuffer_effect.rs`、`shaders/clipped_surface.frag` | 折射、bevel 宽度、色散 (fringing)、interior warp、glow；xray 与非 xray 双路径都支持 |
| 工作区切换缩放 dip | `niri-config/src/appearance.rs` (WorkspaceDip)、`src/layout/...` | 可选缩放下探 + 独立两段 timing |
| 工作区橡皮筋 bounce | `src/layout/monitor.rs` (WorkspaceBounce) | 切到首/尾工作区时的回弹 |
| 聚焦动画 | focus scale flash | 可配置 disable-on-solo / disable-on-floating |
| UI 音效 | `resources/sounds/*.ogg` + `pw-play` | window-open/close、focus-change、workspace-switch；限流防重叠 |

### 已有但容易误判为"缺"的动画基建（别再重复造）

- **spring 动画已有**：`src/animation/spring.rs` + config `animations { kind = "spring" }`，
  可调 `damping-ratio / stiffness / epsilon`，**能过冲**。所以"bounce/弹性"并非缺失，
  只是很多属性没默认配成 spring。
- **窗口移动 / resize / alpha / scale / 焦点动画已有**：`src/layout/tile.rs`
  （`ResizeAnimation / MoveAnimation / AlphaAnimation / ScaleAnimation /
  FocusAnimationState`），open/close 也有（`opening_window.rs / closing_window.rs`）。
- **overview 进度已有**：`src/layout/monitor.rs` 的 `OverviewProgress`。
- **MRU 切换条已有**（上游自带）：`src/ui/mru.rs`。

## 二、关键发现

- **F1（已排除）**：`more_features` 分支删除了 `post-process shader` 相关提交
  （reflog：`40bdf6658`、`47662dc52`、`240ddd61`、`d84d9f19f`）。**用户明确不要**。
- **F2. README 与实现不一致**：README 说 picker "navigable with arrow keys"，
  但 `handle_key()` 只处理 `Escape` / `BackSpace` / 字母。**没有方向键导航**。
  → 用户第一眼就能发现的偏差，也是**第一优先级**。
- **F3. focus glow 加过又被砍**：`d416dce7` 加了 focus glow，`f50a9f6` 又 drop 掉。
  → 可重做成可配置的聚焦外发光。
- **F4. easing 曲线只有 5 种**：`Linear / EaseOutQuad / EaseOutCubic / EaseOutExpo /
  CubicBezier`（`src/animation/mod.rs`）。缺 `ease-in / ease-in-out / ease-out-back`
  （过冲式 easing；过冲目前只能靠 spring 实现）。

## 三、功能列表

### A 档：低成本 / 高回报（不依赖 shader，先做这些）

| # | 功能 | 工作量 | 为什么值得做 |
| --- | --- | --- | --- |
| A1 | **Picker 方向键 / hjkl 网格导航 + Enter 选中 + Tab 循环** | M | 让 README 名副其实；现在键盘只能靠字母，网格导航是主流操作习惯（KDE/GNOME 风格） |
| A2 | **Picker type-to-filter 标题过滤** | M | 窗口多时字母标签不够用；输入标题子串实时过滤 + 高亮匹配，类似 Spotlight/wofi 的体验 |
| A3 | **Focus glow 重做为可配置外发光/边框辉光** | M | 把 F3 砍掉的效果以配置项形式加回来（强度/颜色/范围），跟随焦点动画 |
| A4 | **全局 reduce-motion 开关** | S | 无障碍友好；一个 `animations { off }` 的精细化版本，尊重系统减弱动态偏好 |

### B 档：动画系统增强

| # | 功能 | 工作量 | 说明 |
| --- | --- | --- | --- |
| B1 | **新增 easing 曲线**：`ease-in / ease-in-out / ease-out-back` | S | 直接扩 `src/animation/mod.rs` 的 `Curve` 枚举 + config 解析；过冲类交给已有的 spring 即可 |
| B2 | **window-open / close 内置动画变体**：scale-in+fade（现状）、slide、glide、from-spawn | M | 现在 open/close 是单一 scale+fade；做成开箱即用的预设变体 |
| B3 | **动态阴影**：焦点窗口阴影更深/更大，可配置 | M | 现有的 shadow 是静态的；焦点联动能显著提升"质感" |
| B4 | **动态渐变边框**：颜色沿边框流动 | M | 在现有 Oklab/Oklch 渐变边框上做动画，配合 liquid glass 风格很搭 |
| B5 | **spring 默认落地**：把焦点/移动/resize 等常用属性默认配成 spring，或提供 `spring` 预设包 | S | 基建已有，只差默认值和文档 |

### C 档：液态玻璃 / 背景效果扩展

| # | 功能 | 工作量 | 说明 |
| --- | --- | --- | --- |
| C1 | **liquid glass 动态折射**（折射点跟随指针/焦点） | M | 现在是静态折射；做成跟随鼠标/焦点的动态玻璃更"物理" |
| C2 | **内置动态背景**（动画渐变 / aurora，shader 驱动，替代外部 mpv） | L | 炫酷大杀器；不依赖 post-process 框架，是独立的背景渲染路径 |
| C3 | **Overview 复用玻璃/parallax** | M | overview（上游已有）还是平铺视图；把 picker 的玻璃、视差效果应用过去 |

### D 档：Picker / 桌面体验

| # | 功能 | 工作量 | 说明 |
| --- | --- | --- | --- |
| D1 | **Picker 按 workspace 分组展示** + 组标题/分隔 + 跳组快捷键 | M | 多工作区下分组更清晰；`WindowPickerSession::collect` 已有窗口数据，按 workspace 归组即可 |
| D2 | **多显示器支持**：picker 跨 output 或按 output 过滤窗口 | M | 现在 `collect()` 用 `active_output` 渲染但窗口列表是全 layout 的；加一个范围选项 |
| D3 | **触摸板手势**：两指上滑打开 picker、滑动选择 | M | 复用 `src/input/swipe_tracker.rs`，与既有手势体系一致 |
| D4 | **Picker 悬停显示窗口标题 tooltip / 底部标题条** | S | 现在只有字母标签，窗口多时不好认 |
| D5 | **Dim inactive windows**（非焦点窗口变暗/去饱和，可配置力度） | M | Hyprland 经典效果，配合聚焦动画观感很好 |
| D6 | **实时缩略图**（live capture，而不是静态快照） | L | 已支持渲染进 screencast；做成每帧更新的 live 预览是顺水推舟，但成本高 |

### N 档：新点子（上一版没有，全部不依赖 shader）

| # | 功能 | 工作量 | 说明 |
| --- | --- | --- | --- |
| N1 | **按键/动作浮层（keybind toast）**：按下 Super+Tab / 切工作区等组合时，屏幕角落短暂显示动作名 | S | 小而独特、很 niri；复用现有 UI 覆盖层动画框架，纯加分项 |
| N2 | **窗口 spawn 光效**：新窗口出现时从位置扩散一圈 glow/涟漪，配合现有 open_animation | S-M | 现有 liquid glass glow 机制可复用；开机启动的那一下非常"炫" |
| N3 | **空闲渐暗 + 唤醒淡入**：DPMS 关屏前平滑暗屏，唤醒后从黑淡入 | S-M | 功能性 + 观感，渲染层已有 alpha 通道，改动小 |
| N4 | **配置热重载过渡动画**：`niri msg action reload-config` 后 appearance 相关项平滑过渡而非瞬切 | M | 非常贴合"动画"主题；`Animation::replace_config` 基建现成 |
| N5 | **拖拽插入位置预览动画**：interactive move 时插入提示（`render_insert_hint_between_workspaces`）动起来 | M | 现在插入提示是静态的；动效化后拖拽体验质感翻倍 |
| N6 | **工作区切换 3D/透视增强**：在现有 dip + bounce 之上加可配置的轻微 rotate/scale/perspective | M-L | **✅ 已实现（`workspace-switch-3d` 配置，见下）** |
| N7 | **启动淡入**：进入桌面后整体短暂淡入 | S | 小彩蛋，成本极低 |

### E 档：锦上添花

| # | 功能 | 工作量 | 说明 |
| --- | --- | --- | --- |
| E1 | **音效扩展**：更多事件（picker-open、window-move、move-to-workspace）+ 音量/静音配置 | S | 复用现有 `sounds` 框架 |
| E2 | **截图 UI 增强**：放大镜、选区动画 | M | 复用 `screenshot_ui.rs` 现有结构 |

## 四、建议路线图（已移除 shader）

1. **本周**：A1 方向键导航（顺带修 README 不一致）→ A2 标题过滤。
2. **短期（体验提升）**：A3 focus glow → A4 reduce-motion → D4 标题 tooltip → N1 keybind toast。
3. **中期（炫酷方向）**：B4 动态渐变边框 → C1 动态玻璃 → D5 dim inactive → N2 spawn 光效 → N4 热重载过渡。
4. **长期（大杀器）**：C2 动态背景 → N6 3D 工作区切换 → D6 live 缩略图 → D1 分组 picker。

## 五、注意事项

- 所有改动保持 `AI_MERGE_NOTES.md` 里记录的 7 条行为不变量（尤其 force-xray、
  liquid glass 双路径、shader uniform 注册）。
- 新增 config 记得同步 `resources/default-config.kdl` 与 `niri-config` 内联快照测试。
- 动画类改动优先走现有 `Animation` / `AnimationState` 基建（spring、easing、
  `replace_config` 已就绪），不要新造轮子。

## 六、N6 实现说明（workspace-switch-3d）

`layout { workspace-switch-3d { ... } }`，默认关闭：

```kdl
workspace-switch-3d {
    on
    depth 0.6        // 0–1：远处工作区整体缩放比（1=不缩放，0=缩没）
    squash 0.8       // 0–1：远处工作区额外垂直压扁（1=不压扁，模拟 rotateX 后仰的 2D 近似）
    radius 1.0       // 0–10000：达到最大效果所需距离（单位：屏高）
    curve-power 1.5  // 0–10000：过渡曲线指数（1=线性，越大越集中在屏幕边缘）
}
```

- 只在工作区切换动画期间生效；overview 打开/动画时自动跳过。
- 变换按**每个工作区中心**锚定，窗口、壁纸（Background 层）、每工作区纯色背景**一起缩放/压扁**，不会错位。
- 与 dip、bounce、spring 叠加。niri 渲染元素只支持 2D 仿射（平移+缩放），
  真正的 rotateX/perspective 需要把工作区合成到离屏纹理再做 3D 投影，属渲染器级大改，暂未做。
