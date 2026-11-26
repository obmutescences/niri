use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI};
use std::mem;
use std::rc::Rc;
use std::time::Duration;

use anyhow::ensure;
use niri_config::animations::{Kind as ConfigAnimKind, SpringParams as ConfigSpringParams};
use niri_config::{
    Color, Config, CornerRadius, WindowPickerGroupBy, WindowPickerInfo, WindowPickerLabel,
    WindowPickerLayout, WindowPickerMotionStyle, WindowPickerScope,
};
use pango::FontDescription;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::{Color32F, Texture as _};
use smithay::input::keyboard::Keysym;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::{Animation, Clock};
use crate::layout::{Layout, LayoutElement as _, LayoutElementRenderElement};
use crate::niri::Niri;
use crate::niri_render_elements;
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::framebuffer_effect::{
    FramebufferEffect, FramebufferEffectElement, PerspectiveClip,
};
use crate::render_helpers::offscreen::OffscreenBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shader_element::ShaderRenderElement;
use crate::render_helpers::shaders::ProgramType;
use crate::render_helpers::shadow::ShadowRenderElement;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::RenderCtx;
use crate::ui::window_picker_icons::{monogram_color, AppIconCache};
use crate::utils::{output_size, to_physical_precise_round, with_toplevel_role};
use crate::window::mapped::MappedId;
use crate::window::Mapped;

const MAX_WINDOWS: usize = 26 * 26;
const PREFIX_DIM_ALPHA: f32 = 0.24;
/// Zoom applied to the hovered or keyboard-highlighted preview.
const HOVER_SCALE: f64 = 1.035;
/// Subtle zoom applied to the currently focused window card.
const FOCUSED_SCALE: f64 = 1.018;
/// How far the label overlaps the top edge of its preview (fraction of label height).
const LABEL_OVERLAP: f64 = 1. / 3.;
/// Gaussian sigma of the hover/focus glow.
const GLOW_SIGMA: f64 = 22.;
/// Height of the top Liquid Glass HUD bar (in logical pixels).
const HUD_HEIGHT: f64 = 40.;
/// Margin above the HUD bar.
const HUD_TOP_MARGIN: f64 = 18.;
/// Height of workspace/output group header pills.
const GROUP_HEADER_HEIGHT: f64 = 28.;
/// Vertical spacing between groups.
const GROUP_GAP: f64 = 22.;

fn hover_spring_config() -> niri_config::Animation {
    niri_config::Animation {
        off: false,
        kind: ConfigAnimKind::Spring(ConfigSpringParams {
            damping_ratio: 0.88,
            stiffness: 520,
            epsilon: 0.001,
        }),
    }
}

fn filter_spring_config() -> niri_config::Animation {
    niri_config::Animation {
        off: false,
        kind: ConfigAnimKind::Easing(niri_config::animations::EasingParams {
            duration_ms: 140,
            curve: niri_config::animations::Curve::EaseOutCubic,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HighlightKind {
    Normal,
    Focused,
    Hovered,
}

/// Visual style of a label texture: keycaps distinguish normal, focused, and hovered/selected
/// states, and highlight any already-typed two-letter prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LabelStyle {
    highlight: HighlightKind,
    typed_len: u8,
}

type PickerTexture = TextureBuffer<GlesTexture>;

niri_render_elements! {
    PickerThumbnailRenderElement<R> => {
        LayoutElement = LayoutElementRenderElement<R>,
        ClippedSurface = ClippedSurfaceRenderElement<R>,
    }
}

niri_render_elements! {
    WindowPickerUiRenderElement<R> => {
        Thumbnail = RelocateRenderElement<RescaleRenderElement<PickerThumbnailRenderElement<R>>>,
        Label = PrimaryGpuTextureRenderElement,
        Card = ShaderRenderElement,
        SolidColor = SolidColorRenderElement,
        FramebufferEffect = FramebufferEffectElement,
        Shadow = ShadowRenderElement,
    }
}

#[derive(Debug, Clone)]
struct PickerEntry {
    id: MappedId,
    label: String,
    app_id: Option<String>,
    title: Option<String>,
    workspace: String,
    workspace_idx: usize,
    output_name: String,
    is_on_active_output: bool,
    is_on_active_workspace: bool,
    is_floating: bool,
    is_urgent: bool,
    /// Real on-screen logical geometry of the window when the picker opened (for Hero motion).
    origin_rect: Option<Rectangle<f64, Logical>>,
    /// Relative workspace direction (-1 above, 0 current, +1 below) when off-workspace.
    workspace_dir: i8,
    motion: WindowPickerMotionStyle,
}

#[derive(Debug)]
pub struct WindowPickerSession {
    entries: Vec<PickerEntry>,
    output: Output,
    prefix: Option<char>,
    /// The currently focused window when the picker opened, if any.
    focused_id: Option<MappedId>,
    /// Currently keyboard-selected window (via arrows / Tab / search).
    selected_id: Option<MappedId>,
    /// Active window collection scope (switchable live via Ctrl+Tab or HUD click).
    scope: WindowPickerScope,
    /// Live fuzzy search mode (entered via `/`).
    search_mode: bool,
    search_query: String,
    /// Window currently zoomed in Quick Look mode (toggled via `Space`).
    quick_look: Option<MappedId>,
    clock: Clock,
}

impl WindowPickerSession {
    pub fn collect(niri: &Niri) -> Option<Self> {
        let output = niri.layout.active_output()?.clone();
        let active_output_name = output.name();
        let config = niri.config.borrow().window_picker.clone();
        let custom_letters = config.label.letters.clone();

        let mut workspace_info: HashMap<MappedId, (String, usize, String, bool, bool, i8)> =
            HashMap::new();
        let mut origin_rects: HashMap<MappedId, Rectangle<f64, Logical>> = HashMap::new();

        // Collect workspace metadata and real on-screen window geometry for True Hero transitions.
        for (mon_opt, ws_idx, ws) in niri.layout.workspaces() {
            let label = ws
                .name()
                .cloned()
                .unwrap_or_else(|| format!("ws {}", ws_idx + 1));
            let (out_name, on_active_out, on_active_ws, ws_dir) = if let Some(mon) = mon_opt {
                let is_active_out = mon.output() == &output;
                let active_idx = mon.active_workspace_idx();
                let dir = if ws_idx < active_idx {
                    -1
                } else if ws_idx > active_idx {
                    1
                } else {
                    0
                };
                (
                    mon.output().name(),
                    is_active_out,
                    is_active_out && ws_idx == active_idx,
                    dir,
                )
            } else {
                (String::new(), false, false, 0)
            };

            for mapped in ws.windows() {
                workspace_info.insert(
                    mapped.id(),
                    (
                        label.clone(),
                        ws_idx,
                        out_name.clone(),
                        on_active_out,
                        on_active_ws,
                        ws_dir,
                    ),
                );
            }
        }

        if let Some(mon) = niri.layout.monitor_for_output(&output) {
            for (ws, ws_geo) in mon.workspaces_with_render_geo() {
                for (tile, tile_pos, visible) in ws.tiles_with_render_positions() {
                    if !visible {
                        continue;
                    }
                    let win_loc = ws_geo.loc + tile_pos + tile.window_loc();
                    let win_size = tile.window_size();
                    if win_size.w > 0. && win_size.h > 0. {
                        origin_rects.insert(tile.window().id(), Rectangle::new(win_loc, win_size));
                    }
                }
            }
        }

        let mut windows = Vec::new();
        let mut seen = HashSet::new();
        let mut focused_id = None;

        for (_, mapped) in niri.layout.windows() {
            let size = mapped.size();
            if size.w > 0 && size.h > 0 && seen.insert(mapped.id()) {
                if mapped.is_focused() {
                    focused_id = Some(mapped.id());
                }
                let (app_id, title) = with_toplevel_role(mapped.toplevel(), |role| {
                    (role.app_id.clone(), role.title.clone())
                });
                let (
                    workspace,
                    workspace_idx,
                    output_name,
                    is_on_active_output,
                    is_on_active_workspace,
                    workspace_dir,
                ) = workspace_info
                    .get(&mapped.id())
                    .cloned()
                    .unwrap_or_else(|| {
                        (String::new(), 0, active_output_name.clone(), true, true, 0)
                    });
                windows.push((
                    mapped.id(),
                    mapped.get_focus_timestamp(),
                    app_id,
                    title,
                    workspace,
                    workspace_idx,
                    output_name,
                    is_on_active_output,
                    is_on_active_workspace,
                    mapped.is_floating(),
                    mapped.is_urgent(),
                    origin_rects.get(&mapped.id()).copied(),
                    workspace_dir,
                ));
            }
        }

        windows.sort_by(|(_, left, ..), (_, right, ..)| right.cmp(left));
        windows.truncate(MAX_WINDOWS);
        if windows.is_empty() {
            return None;
        }

        if !windows
            .iter()
            .any(|(id, ..)| Some(id) == focused_id.as_ref())
        {
            focused_id = None;
        }

        let labels = make_labels(windows.len(), &custom_letters);
        let entries: Vec<PickerEntry> = windows
            .into_iter()
            .zip(labels)
            .map(
                |(
                    (
                        id,
                        _,
                        app_id,
                        title,
                        workspace,
                        workspace_idx,
                        output_name,
                        is_on_active_output,
                        is_on_active_workspace,
                        is_floating,
                        is_urgent,
                        origin_rect,
                        workspace_dir,
                    ),
                    label,
                )| PickerEntry {
                    id,
                    label,
                    app_id,
                    title,
                    workspace,
                    workspace_idx,
                    output_name,
                    is_on_active_output,
                    is_on_active_workspace,
                    is_floating,
                    is_urgent,
                    origin_rect,
                    workspace_dir,
                    motion: WindowPickerMotionStyle::Slide,
                },
            )
            .collect();

        // Pick an initial scope that actually has windows; if configured scope is empty, fall back
        // to All.
        let mut scope = config.scope;
        if !entries.iter().any(|e| entry_in_scope(e, scope)) {
            scope = WindowPickerScope::All;
        }

        let clock = niri.clock.clone();

        let mut session = Self {
            entries,
            output,
            prefix: None,
            focused_id,
            selected_id: None,
            scope,
            search_mode: false,
            search_query: String::new(),
            quick_look: None,
            clock,
        };
        session.reassign_labels(&custom_letters);
        Some(session)
    }

    fn reassign_labels(&mut self, custom_letters: &str) {
        let in_scope_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| entry_in_scope(e, self.scope).then_some(idx))
            .collect();
        let labels = make_labels(in_scope_indices.len(), custom_letters);
        for (idx, label) in in_scope_indices.into_iter().zip(labels) {
            self.entries[idx].label = label;
        }
    }

    fn visible_entries(&self) -> Vec<&PickerEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                entry_in_scope(entry, self.scope)
                    && (self.search_query.is_empty()
                        || entry_matches_query(entry, &self.search_query))
            })
            .collect()
    }

    fn scope_counts(&self) -> (usize, usize, usize) {
        let all = self.entries.len();
        let ws = self
            .entries
            .iter()
            .filter(|e| e.is_on_active_workspace)
            .count();
        let out = self
            .entries
            .iter()
            .filter(|e| e.is_on_active_output)
            .count();
        (all, ws, out)
    }
}

fn entry_in_scope(entry: &PickerEntry, scope: WindowPickerScope) -> bool {
    match scope {
        WindowPickerScope::All => true,
        WindowPickerScope::Workspace => entry.is_on_active_workspace,
        WindowPickerScope::Output => entry.is_on_active_output,
    }
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let h = haystack.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    if h.contains(&n) {
        return true;
    }
    // Ordered subsequence fuzzy match (e.g. "vsc" matches "Visual Studio Code").
    let mut chars = n.chars();
    let Some(mut target) = chars.next() else {
        return true;
    };
    for c in h.chars() {
        if c == target {
            if let Some(next) = chars.next() {
                target = next;
            } else {
                return true;
            }
        }
    }
    false
}

fn entry_matches_query(entry: &PickerEntry, query: &str) -> bool {
    entry
        .title
        .as_deref()
        .is_some_and(|t| fuzzy_match(t, query))
        || entry
            .app_id
            .as_deref()
            .is_some_and(|a| fuzzy_match(a, query))
        || fuzzy_match(&entry.workspace, query)
}

#[derive(Debug)]
enum WindowPickerState {
    Closed,
    Open {
        session: WindowPickerSession,
        anim: Option<Animation>,
        is_spring: bool,
        step_delay: Duration,
        max_stagger: Duration,
    },
    Closing {
        session: WindowPickerSession,
        anim: Animation,
        /// Animation progress at the moment closing started.
        from_progress: f64,
        is_spring: bool,
    },
}

impl WindowPickerState {
    fn session(&self) -> Option<&WindowPickerSession> {
        match self {
            Self::Open { session, .. } | Self::Closing { session, .. } => Some(session),
            Self::Closed => None,
        }
    }

    fn session_mut(&mut self) -> Option<&mut WindowPickerSession> {
        match self {
            Self::Open { session, .. } | Self::Closing { session, .. } => Some(session),
            Self::Closed => None,
        }
    }

    fn anim(&self) -> Option<&Animation> {
        match self {
            Self::Open { anim, .. } => anim.as_ref(),
            Self::Closing { anim, .. } => Some(anim),
            Self::Closed => None,
        }
    }

    fn is_spring(&self) -> bool {
        match self {
            Self::Open { is_spring, .. } | Self::Closing { is_spring, .. } => *is_spring,
            Self::Closed => false,
        }
    }
}

fn ease_in_cubic(progress: f64) -> f64 {
    progress * progress * progress
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// Piecewise linear-segment interpolation driven by an already-eased or spring `t`
/// (avoids double-easing while still supporting artistic keyframes when not using a spring).
fn keyframe_linear(t: f64, first: f64, mid: f64, last: f64, mid_at: f64) -> f64 {
    if t <= mid_at {
        lerp(first, mid, (t / mid_at.max(1e-4)).clamp(0., 1.))
    } else {
        lerp(mid, last, (t - mid_at) / (1. - mid_at).max(1e-4))
    }
}

/// Computes `(card_duration, step_delay, max_stagger)` for the picker open animation.
/// When `target_total_ms` is `Some(ms)` (from `animation { duration-ms ... }` or explicit
/// `animation-ms-open`), ALL cards including their stagger wave finish at exact `ms` milliseconds
/// while preserving the exact Spring bounce curve or Easing curve.
fn compute_open_timing(
    visible_count: usize,
    stagger_ms: u16,
    target_total_ms: Option<u16>,
    natural_card_dur: Duration,
) -> (Duration, Duration, Duration) {
    let gaps = visible_count.saturating_sub(1);
    if let Some(total_ms) = target_total_ms {
        let total_ms = u64::from(total_ms.max(1));
        let total = Duration::from_millis(total_ms);
        if gaps == 0 || stagger_ms == 0 {
            return (total, Duration::ZERO, Duration::ZERO);
        }
        let raw_stagger_ms = f64::from(stagger_ms) * gaps as f64;
        let max_stagger_ms = (total_ms as f64 * 0.45).min(raw_stagger_ms);
        let step_delay = Duration::from_secs_f64((max_stagger_ms / gaps as f64) / 1000.0);
        let max_stagger = step_delay.saturating_mul(gaps as u32);
        let card_dur = total
            .saturating_sub(max_stagger)
            .max(Duration::from_millis(1));
        (card_dur, step_delay, max_stagger)
    } else {
        if gaps == 0 || stagger_ms == 0 {
            return (natural_card_dur, Duration::ZERO, Duration::ZERO);
        }
        let max_wave_ms = (natural_card_dur.as_secs_f64() * 1000.0 * 0.45).clamp(40.0, 320.0);
        let step_ms = f64::from(stagger_ms).min(max_wave_ms / gaps as f64);
        let step_delay = Duration::from_secs_f64(step_ms / 1000.0);
        let max_stagger = step_delay.saturating_mul(gaps as u32);
        (natural_card_dur, step_delay, max_stagger)
    }
}

#[derive(Debug, Clone, Copy)]
struct CardTransform {
    offset: Point<f64, Logical>,
    scale: f64,
    angle: f64,
    /// True 3D Y-axis flip angle in radians (used by `Flip` motion style).
    flip_y: f64,
    alpha: f64,
}

impl CardTransform {
    fn identity() -> Self {
        Self {
            offset: Point::from((0., 0.)),
            scale: 1.,
            angle: 0.,
            flip_y: 0.,
            alpha: 1.,
        }
    }
}

const MOTION_CYCLE: [WindowPickerMotionStyle; 11] = [
    WindowPickerMotionStyle::Hero,
    WindowPickerMotionStyle::Pop,
    WindowPickerMotionStyle::Slide,
    WindowPickerMotionStyle::Drop,
    WindowPickerMotionStyle::Burst,
    WindowPickerMotionStyle::Vortex,
    WindowPickerMotionStyle::Flip,
    WindowPickerMotionStyle::Cascade,
    WindowPickerMotionStyle::Orbit,
    WindowPickerMotionStyle::Wave,
    WindowPickerMotionStyle::Zoom,
];

fn card_center(card: Rectangle<f64, Logical>) -> Point<f64, Logical> {
    card.loc + card.size.to_point().upscale(0.5)
}

fn scale_rect(
    rect: Rectangle<f64, Logical>,
    pivot: Point<f64, Logical>,
    scale: f64,
    offset: Point<f64, Logical>,
) -> Rectangle<f64, Logical> {
    let center = rect.loc + rect.size.to_point().upscale(0.5);
    let center = pivot + (center - pivot).upscale(scale) + offset;
    let size = rect.size.upscale(scale);
    Rectangle::new(center - size.to_point().upscale(0.5), size)
}

fn rotated_bounds(
    size: Size<f64, Logical>,
    pivot: Point<f64, Logical>,
    angle: f64,
    scale: f64,
    extra_pad: f64,
) -> (Size<f64, Logical>, Point<f64, Logical>) {
    let (sin, cos) = angle.sin_cos();
    let corners = [
        (-pivot.x, -pivot.y),
        (size.w - pivot.x, -pivot.y),
        (size.w - pivot.x, size.h - pivot.y),
        (-pivot.x, size.h - pivot.y),
    ];
    let mut min: Point<f64, Logical> = Point::from((f64::INFINITY, f64::INFINITY));
    let mut max: Point<f64, Logical> = Point::from((f64::NEG_INFINITY, f64::NEG_INFINITY));
    for (px, py) in corners {
        let rx = (px * cos - py * sin) * scale;
        let ry = (px * sin + py * cos) * scale;
        min.x = min.x.min(rx);
        min.y = min.y.min(ry);
        max.x = max.x.max(rx);
        max.y = max.y.max(ry);
    }
    let size = Size::from((
        (max.x - min.x).max(0.) + extra_pad * 2.,
        (max.y - min.y).max(0.) + extra_pad * 2.,
    ));
    let center = Point::from(((min.x + max.x) * 0.5, (min.y + max.y) * 0.5));
    (size, center)
}

fn scatter_transform(
    card: Rectangle<f64, Logical>,
    output: Size<f64, Logical>,
    t: f64,
    index: usize,
) -> CardTransform {
    let t = t.clamp(0., 1.);
    // Gentle quadratic acceleration without fighting the parent easing curve.
    let accel = t * (0.45 + 0.55 * t);
    let target_center = card_center(card);
    let output_center = output.to_point().upscale(0.5);
    let delta = target_center - output_center;
    let mut angle = delta.y.atan2(delta.x);
    if delta.x.abs() < f64::EPSILON && delta.y.abs() < f64::EPSILON {
        angle = if index.is_multiple_of(2) { 0. } else { PI };
    }
    angle += if index.is_multiple_of(2) { -0.15 } else { 0.15 };
    let distance = output.w.hypot(output.h) * 0.55;
    let offset = Point::from((angle.cos(), angle.sin())).upscale(distance * accel);
    let rotation = if index.is_multiple_of(2) { -1. } else { 1. }
        * (5. + (index % 3) as f64 * 2.).to_radians()
        * accel;
    CardTransform {
        offset,
        scale: lerp(1., 0.84, accel),
        angle: rotation,
        flip_y: 0.,
        alpha: 1. - ease_in_cubic(t),
    }
}

fn slide_offset(
    target_center: Point<f64, Logical>,
    output: Size<f64, Logical>,
    progress: f64,
) -> Point<f64, Logical> {
    let output_center = output.to_point().upscale(0.5);
    let delta = target_center - output_center;
    let (x, y) = if delta.x.abs() >= delta.y.abs() {
        let x = if delta.x < 0. {
            -output.w * 0.55
        } else {
            output.w * 0.55
        };
        (x, delta.y * 0.12)
    } else {
        let y = if delta.y < 0. {
            -output.h * 0.55
        } else {
            output.h * 0.55
        };
        (delta.x * 0.12, y)
    };
    Point::from((x, y)).upscale(1. - progress)
}

/// Computes the entry transform from an ALREADY-eased (or unclamped Spring!) `e`.
/// Eliminates the legacy double-easing bug (`1 - (1-t)^9`) and preserves true Spring overshoot.
#[allow(clippy::too_many_arguments)]
fn open_transform(
    style: WindowPickerMotionStyle,
    entry: &PickerEntry,
    placement: &Placement,
    output: Size<f64, Logical>,
    e: f64,
    is_spring: bool,
    index: usize,
) -> CardTransform {
    let alpha_t = e.clamp(0., 1.);
    let target_center = card_center(placement.card);
    let output_center = output.to_point().upscale(0.5);
    let mut tf = CardTransform::identity();

    match style {
        WindowPickerMotionStyle::Hero => {
            if let Some(origin) = entry.origin_rect {
                let origin_center = card_center(origin);
                let start_scale = (origin.size.w / placement.preview.size.w.max(1.))
                    .min(origin.size.h / placement.preview.size.h.max(1.))
                    .clamp(0.35, 3.0);
                tf.offset = (origin_center - target_center).upscale(1. - e);
                tf.scale = lerp(start_scale, 1., e).max(0.05);
                tf.alpha = (alpha_t * 2.2).clamp(0., 1.);
            } else {
                // Window lives on another workspace or output: glide in from its workspace side.
                let dir_y = if entry.workspace_dir != 0 {
                    f64::from(entry.workspace_dir) * output.h * 0.52
                } else {
                    output.h * 0.25
                };
                tf.offset = Point::from((0., dir_y * (1. - e)));
                tf.scale = lerp(0.84, 1., e).max(0.05);
                tf.alpha = alpha_t;
            }
        }
        WindowPickerMotionStyle::Pop => {
            tf.scale = if is_spring {
                lerp(0.74, 1., e).max(0.05)
            } else {
                keyframe_linear(alpha_t, 0.74, 1.035, 1., 0.76)
            };
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Slide => {
            tf.offset = slide_offset(target_center, output, e);
            tf.scale = lerp(0.95, 1., e).max(0.05);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Drop => {
            let y = if is_spring {
                lerp(-output.h * 0.72, 0., e)
            } else {
                keyframe_linear(alpha_t, -output.h * 0.72, 10., 0., 0.78)
            };
            tf.offset = Point::from((0., y));
            tf.scale = lerp(0.95, 1., e).max(0.05);
            tf.angle = lerp(-1.8_f64.to_radians(), 0., e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Burst => {
            let spin = if index.is_multiple_of(2) { -1. } else { 1. }
                * (28. + (index % 4) as f64 * 8.).to_radians();
            tf.offset = (target_center - output_center).upscale(-(1. - e));
            tf.scale = if is_spring {
                lerp(0.28, 1., e).max(0.05)
            } else {
                keyframe_linear(alpha_t, 0.28, 1.04, 1., 0.76)
            };
            tf.angle = lerp(spin, 0., e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Vortex => {
            let spin = if index.is_multiple_of(2) { 1. } else { -1. }
                * (135. + (index % 3) as f64 * 24.).to_radians();
            let delta = target_center - output_center;
            let radius = delta.x.hypot(delta.y);
            let angle = delta.y.atan2(delta.x) + 0.9;
            let mid = Point::from((angle.cos(), angle.sin())).upscale(radius * 0.48);
            let start = Point::from((0., 0.));
            let end = target_center - output_center;
            let u = e.clamp(0., 1.15);
            let first = start + (mid - start).upscale(u);
            let second = mid + (end - mid).upscale(u);
            tf.offset = first + (second - first).upscale(u) - end;
            tf.scale = lerp(0.42, 1., e).max(0.05);
            tf.angle = lerp(spin, 0., e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Flip => {
            // True pseudo-3D Y-axis card flip in picker_card.frag!
            let dir = if index.is_multiple_of(2) { 1. } else { -1. };
            tf.flip_y = dir * (PI * 0.55) * (1. - e);
            tf.scale = if is_spring {
                lerp(0.76, 1., e).max(0.05)
            } else {
                keyframe_linear(alpha_t, 0.76, 1.03, 1., 0.78)
            };
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Cascade => {
            let dir = if index.is_multiple_of(2) { -1. } else { 1. };
            let drop = output.h * (0.42 + (index % 3) as f64 * 0.08);
            tf.offset = Point::from((dir * output.w * 0.07 * (1. - e), -drop * (1. - e)));
            tf.scale = lerp(0.9, 1., e).max(0.05);
            tf.angle = dir * (6. + (index % 3) as f64 * 2.5).to_radians() * (1. - e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Orbit => {
            let dir = if index.is_multiple_of(2) { 1. } else { -1. };
            let target_delta = target_center - output_center;
            let target_angle = target_delta.y.atan2(target_delta.x);
            let target_radius = target_delta.x.hypot(target_delta.y);
            let start_angle = target_angle + dir * (1.5 + (index % 3) as f64 * 0.25);
            let start_radius = output.w.hypot(output.h) * (0.40 + (index % 3) as f64 * 0.05);
            let angle = lerp(start_angle, target_angle, e);
            let radius = lerp(start_radius, target_radius, e);
            let position = output_center + Point::from((angle.cos(), angle.sin())).upscale(radius);
            tf.offset = position - target_center;
            tf.scale = lerp(0.52, 1., e).max(0.05);
            tf.angle = dir * (0.65 + (index % 3) as f64 * 0.14) * (1. - e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Wave => {
            let dir = if index.is_multiple_of(2) { -1. } else { 1. };
            let phase = index as f64 * 0.85;
            let wobble = (phase + alpha_t * 2. * PI).sin() * output.h * 0.04 * (1. - alpha_t);
            tf.offset = Point::from((dir * output.w * 0.58 * (1. - e), wobble));
            tf.scale = lerp(0.94, 1., e).max(0.05);
            tf.angle = dir * (6.5 + (index % 3) as f64 * 2.5).to_radians() * (1. - e);
            tf.alpha = alpha_t;
        }
        WindowPickerMotionStyle::Zoom => {
            tf.scale = if is_spring {
                lerp(1.9, 1., e).max(0.05)
            } else {
                keyframe_linear(alpha_t, 1.9, 0.985, 1., 0.8)
            };
            tf.angle = lerp(0.035, 0., e);
            tf.alpha = alpha_t;
        }
        // Cycle is resolved to a concrete style in `open` before rendering.
        WindowPickerMotionStyle::Cycle => {}
    }

    tf
}

#[derive(Debug, Default)]
struct LabelCache {
    scale: f64,
    config: Option<WindowPickerLabel>,
    focused_color: Option<Color>,
    hover_color: Option<Color>,
    textures: HashMap<(String, LabelStyle), Option<PickerTexture>>,
}

impl LabelCache {
    #[allow(clippy::too_many_arguments)]
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        label: &str,
        style: LabelStyle,
        scale: f64,
        config: &WindowPickerLabel,
        focused_color: Color,
        hover_color: Color,
    ) -> Option<PickerTexture> {
        if self.scale != scale
            || self.config.as_ref() != Some(config)
            || self.focused_color != Some(focused_color)
            || self.hover_color != Some(hover_color)
        {
            self.scale = scale;
            self.config = Some(config.clone());
            self.focused_color = Some(focused_color);
            self.hover_color = Some(hover_color);
            self.textures.clear();
        }

        let accent = match style.highlight {
            HighlightKind::Focused => focused_color,
            HighlightKind::Hovered | HighlightKind::Normal => hover_color,
        };

        self.textures
            .entry((label.to_owned(), style))
            .or_insert_with(|| {
                generate_label_texture(renderer, label, style, scale, config, accent).ok()
            })
            .clone()
    }

    fn logical_size(
        &self,
        label: &str,
        scale: f64,
        config: &WindowPickerLabel,
    ) -> Option<Size<f64, Logical>> {
        if self.scale != scale || self.config.as_ref() != Some(config) {
            return None;
        }

        self.textures
            .iter()
            .find(|((cached, _), _)| cached == label)
            .and_then(|(_, texture)| texture.as_ref())
            .map(PickerTexture::logical_size)
    }

    fn clear(&mut self) {
        self.config = None;
        self.focused_color = None;
        self.hover_color = None;
        self.textures.clear();
    }
}

/// Per-output backdrop state — prevents multi-monitor FBO & texture thrashing.
#[derive(Debug)]
struct BackdropState {
    dim: SolidColorBuffer,
    tint: SolidColorBuffer,
    framebuffer_effect: FramebufferEffect,
}

impl Default for BackdropState {
    fn default() -> Self {
        Self {
            dim: SolidColorBuffer::default(),
            tint: SolidColorBuffer::default(),
            framebuffer_effect: FramebufferEffect::new(),
        }
    }
}

#[derive(Debug, Default)]
struct InfoCache {
    scale: f64,
    config: Option<WindowPickerInfo>,
    show_icons: bool,
    textures: HashMap<InfoKey, Option<PickerTexture>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InfoKey {
    app_id: String,
    title: String,
    workspace: String,
    width: i32,
    height: i32,
    font_scale: i64,
    is_floating: bool,
    is_urgent: bool,
    redacted: bool,
}

impl InfoCache {
    #[allow(clippy::too_many_arguments)]
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        icons: &mut AppIconCache,
        app_id: &str,
        title: &str,
        workspace: &str,
        width: f64,
        height: f64,
        font_scale: f64,
        scale: f64,
        is_floating: bool,
        is_urgent: bool,
        redacted: bool,
        show_icons: bool,
        config: &WindowPickerInfo,
    ) -> Option<PickerTexture> {
        if self.scale != scale
            || self.config.as_ref() != Some(config)
            || self.show_icons != show_icons
        {
            self.scale = scale;
            self.config = Some(config.clone());
            self.show_icons = show_icons;
            self.textures.clear();
        }

        let width = to_physical_precise_round::<i32>(scale, width).max(1);
        let height = to_physical_precise_round::<i32>(scale, height).max(1);
        let key = InfoKey {
            app_id: if redacted {
                String::from("private")
            } else {
                app_id.to_owned()
            },
            title: if redacted {
                String::from("Hidden Window")
            } else {
                truncate_title(title)
            },
            workspace: workspace.to_owned(),
            width,
            height,
            font_scale: (font_scale * 10_000.).round() as i64,
            is_floating,
            is_urgent,
            redacted,
        };
        self.textures
            .entry(key.clone())
            .or_insert_with(|| {
                generate_info_texture(renderer, icons, &key, scale, font_scale, show_icons, config)
                    .ok()
            })
            .clone()
    }

    fn clear(&mut self) {
        self.config = None;
        self.textures.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HudCacheKey {
    width: i32,
    height: i32,
    scale_x100: i32,
    scope: u8,
    counts: (usize, usize, usize),
    search_mode: bool,
    search_query: String,
    prefix: Option<char>,
    active_title: String,
    quick_look: bool,
}

#[derive(Default)]
struct HudCache {
    last_key: Option<HudCacheKey>,
    texture: Option<PickerTexture>,
    glass_elem: Option<ShaderRenderElement>,
    shadow_elem: Option<ShadowRenderElement>,
    blur_elem: Option<(FramebufferEffect, Rectangle<f64, Logical>)>,
}

impl HudCache {
    fn clear(&mut self) {
        self.last_key = None;
        self.texture = None;
        if let Some((effect, last_rect)) = &mut self.blur_elem {
            effect.damage();
            *last_rect = Rectangle::default();
        }
    }
}

#[derive(Default)]
struct GroupHeaderCache {
    scale: f64,
    textures: HashMap<(String, usize, i32, i32), Option<PickerTexture>>,
    glass_elems: HashMap<String, ShaderRenderElement>,
    blur_elems: HashMap<String, (FramebufferEffect, Rectangle<f64, Logical>)>,
}

impl GroupHeaderCache {
    fn clear(&mut self) {
        self.textures.clear();
        for (effect, last_rect) in self.blur_elems.values_mut() {
            effect.damage();
            *last_rect = Rectangle::default();
        }
    }
}

/// Per-card persistent visual & spring state.
/// Each card holds its OWN unique Smithay Element IDs (`card_shadow`, `glow`, `glass_elem`,
/// `rot_elem`, `card_blur`, `privacy_buf`, `offscreen`), preventing OutputDamageTracker ID
/// collisions.
struct CardVisualState {
    hover_anim: Option<Animation>,
    hover_target: f64,
    filter_anim: Option<Animation>,
    filter_target: f64,
    /// Pointer tilt in [-1, 1] x [-1, 1] for subtle 3D perspective & glass glint.
    tilt: Point<f64, Logical>,
    card_shadow: ShadowRenderElement,
    glow: ShadowRenderElement,
    glass_elem: ShaderRenderElement,
    rot_elem: ShaderRenderElement,
    card_blur: FramebufferEffect,
    last_blur_rect: Rectangle<f64, Logical>,
    last_blur_perspective: Option<PerspectiveClip>,
    privacy_buf: SolidColorBuffer,
    offscreen: OffscreenBuffer,
}

impl CardVisualState {
    fn new(_clock: Clock) -> Self {
        Self {
            hover_anim: None,
            hover_target: 0.,
            filter_anim: None,
            filter_target: 1.,
            tilt: Point::from((0., 0.)),
            card_shadow: ShadowRenderElement::empty(),
            glow: ShadowRenderElement::empty(),
            glass_elem: ShaderRenderElement::empty(ProgramType::PickerGlass, Kind::Unspecified),
            rot_elem: ShaderRenderElement::empty(ProgramType::PickerCard, Kind::Unspecified),
            card_blur: FramebufferEffect::new(),
            last_blur_rect: Rectangle::default(),
            last_blur_perspective: None,
            privacy_buf: SolidColorBuffer::new((0., 0.), [0.08, 0.10, 0.14, 0.94]),
            offscreen: OffscreenBuffer::default(),
        }
    }

    fn hover_value(&self) -> f64 {
        self.hover_anim
            .as_ref()
            .map_or(self.hover_target, Animation::value)
    }

    fn filter_value(&self) -> f64 {
        self.filter_anim
            .as_ref()
            .map_or(self.filter_target, Animation::clamped_value)
    }

    fn set_hover_target(&mut self, clock: &Clock, target: f64) {
        if (self.hover_target - target).abs() > 1e-4 {
            let current = self.hover_value();
            self.hover_target = target;
            let anim = Animation::new(clock.clone(), current, target, 0., hover_spring_config());
            self.hover_anim = (!anim.is_done()).then_some(anim);
        }
    }

    fn set_filter_target(&mut self, clock: &Clock, target: f64) {
        if (self.filter_target - target).abs() > 1e-4 {
            let current = self.filter_value();
            self.filter_target = target;
            let anim = Animation::new(clock.clone(), current, target, 0., filter_spring_config());
            self.filter_anim = (!anim.is_clamped_done()).then_some(anim);
        }
    }

    fn advance(&mut self) -> bool {
        let mut settled = false;
        if self.hover_anim.as_ref().is_some_and(Animation::is_done) {
            self.hover_anim = None;
            settled = true;
        }
        if self
            .filter_anim
            .as_ref()
            .is_some_and(Animation::is_clamped_done)
        {
            self.filter_anim = None;
            settled = true;
        }
        settled
    }

    fn is_animating(&self) -> bool {
        self.hover_anim.as_ref().is_some_and(|a| !a.is_done())
            || self
                .filter_anim
                .as_ref()
                .is_some_and(|a| !a.is_clamped_done())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct LayoutCacheKey {
    output_size: (i64, i64),
    scale: i64,
    scope: WindowPickerScope,
    search_query: String,
    window_sigs: Vec<(MappedId, i64, i64, i64, i64)>,
    layout_mode: WindowPickerLayout,
    group_by: WindowPickerGroupBy,
    show_hud: bool,
    area_width: i64,
    area_height: i64,
    max_scale: i64,
    gap: i64,
    card_on: bool,
    info_height: i64,
}

#[derive(Debug, Clone)]
struct GroupHeaderPlacement {
    title: String,
    count: usize,
    rect: Rectangle<f64, Logical>,
}

#[derive(Debug, Clone)]
struct ComputedLayout {
    placements: Vec<(MappedId, Placement)>,
    group_headers: Vec<GroupHeaderPlacement>,
    hud_rect: Option<Rectangle<f64, Logical>>,
    scope_chips: [(WindowPickerScope, Rectangle<f64, Logical>); 3],
}

fn truncate_title(title: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut truncated: String = title.chars().take(MAX_CHARS).collect();
    if title.chars().count() > MAX_CHARS {
        truncated.push('…');
    }
    truncated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPickerKeyResult {
    Handled,
    Selected(MappedId),
    CloseWindow(MappedId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPickerClickResult {
    None,
    SelectWindow(MappedId),
    CloseWindow(MappedId),
    CycleScope,
    ClosePicker,
}

pub struct WindowPickerUi {
    state: WindowPickerState,
    config: Rc<RefCell<Config>>,
    label_cache: RefCell<LabelCache>,
    backdrops: RefCell<HashMap<Output, BackdropState>>,
    /// Window being flown back to its real position during the closing animation.
    fly_back: Option<MappedId>,
    /// Target on-screen rect for `fly_back` (captured from real layout geometry).
    fly_back_target: Option<Rectangle<f64, Logical>>,
    /// Preview the cursor is currently over.
    hover: Option<MappedId>,
    cards: RefCell<HashMap<MappedId, CardVisualState>>,
    info_cache: RefCell<InfoCache>,
    icon_cache: RefCell<AppIconCache>,
    hud_cache: RefCell<HudCache>,
    group_cache: RefCell<GroupHeaderCache>,
    layout_cache: RefCell<Option<(LayoutCacheKey, ComputedLayout)>>,
    /// Next style to use in `cycle` mode. Advances across picker openings so even a
    /// single-window picker changes animation each time.
    motion_cursor: usize,
}

impl WindowPickerUi {
    pub fn new(config: Rc<RefCell<Config>>) -> Self {
        Self {
            state: WindowPickerState::Closed,
            config,
            label_cache: RefCell::new(LabelCache::default()),
            backdrops: RefCell::new(HashMap::new()),
            fly_back: None,
            fly_back_target: None,
            hover: None,
            cards: RefCell::new(HashMap::new()),
            info_cache: RefCell::new(InfoCache::default()),
            icon_cache: RefCell::new(AppIconCache::default()),
            hud_cache: RefCell::new(HudCache::default()),
            group_cache: RefCell::new(GroupHeaderCache::default()),
            layout_cache: RefCell::new(None),
            motion_cursor: 0,
        }
    }

    fn progress(&self) -> f64 {
        match &self.state {
            WindowPickerState::Closed => 0.,
            WindowPickerState::Open { anim, .. } => {
                anim.as_ref().map_or(1., Animation::clamped_value)
            }
            WindowPickerState::Closing { anim, .. } => anim.clamped_value(),
        }
    }

    fn close_from_progress(&self) -> f64 {
        match &self.state {
            WindowPickerState::Closing { from_progress, .. } => *from_progress,
            _ => 0.,
        }
    }

    fn is_closing(&self) -> bool {
        matches!(self.state, WindowPickerState::Closing { .. })
    }

    pub fn is_open(&self) -> bool {
        matches!(&self.state, WindowPickerState::Open { .. })
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, WindowPickerState::Closed)
    }

    pub fn output(&self) -> Option<&Output> {
        self.state.session().map(|session| &session.output)
    }

    #[cfg(test)]
    pub fn visible_window_count(&self) -> usize {
        self.state
            .session()
            .map_or(0, |session| session.visible_entries().len())
    }

    #[cfg(test)]
    pub fn active_motion_styles(&self) -> Vec<WindowPickerMotionStyle> {
        self.state.session().map_or_else(Vec::new, |session| {
            session.entries.iter().map(|e| e.motion).collect()
        })
    }

    pub fn open(&mut self, mut session: WindowPickerSession) {
        let config = self.config.borrow().window_picker.clone();
        let style = config.motion.style;
        let resolved_style = if style == WindowPickerMotionStyle::Cycle {
            let motion = MOTION_CYCLE[self.motion_cursor % MOTION_CYCLE.len()];
            self.motion_cursor = (self.motion_cursor + 1) % MOTION_CYCLE.len();
            motion
        } else {
            style
        };
        for entry in &mut session.entries {
            entry.motion = resolved_style;
        }

        let anim_cfg = config.animation.to_animation(config.animation_ms_open);
        let is_spring = matches!(anim_cfg.kind, ConfigAnimKind::Spring(_));
        let mut anim = Animation::new(session.clock.clone(), 0., 1., 0., anim_cfg);

        self.label_cache.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.hud_cache.get_mut().clear();
        self.group_cache.get_mut().clear();
        *self.layout_cache.get_mut() = None;
        self.cards.get_mut().clear();
        for bd in self.backdrops.get_mut().values_mut() {
            bd.framebuffer_effect.damage();
        }

        // Pre-seed per-card state with unique Element IDs.
        {
            let cards = self.cards.get_mut();
            for entry in &session.entries {
                cards.insert(entry.id, CardVisualState::new(session.clock.clone()));
            }
        }

        let visible_count = session.visible_entries().len();
        let target_total_ms = if is_spring {
            config.target_duration_ms(true)
        } else {
            Some(
                config
                    .animation
                    .explicit_duration_ms
                    .unwrap_or(config.animation_ms_open),
            )
        };
        let (card_dur, step_delay, max_stagger) = compute_open_timing(
            visible_count,
            config.stagger_ms,
            target_total_ms,
            anim.duration(),
        );
        if target_total_ms.is_some() {
            anim = anim.with_duration(card_dur);
        }

        self.fly_back = None;
        self.fly_back_target = None;
        self.hover = None;
        self.state = WindowPickerState::Open {
            session,
            anim: (!anim.is_done_with_delay(max_stagger)).then_some(anim),
            is_spring,
            step_delay,
            max_stagger,
        };
    }

    pub fn select_window(&mut self, id: MappedId) {
        self.fly_back = Some(id);
        if let Some(session) = self.state.session() {
            self.fly_back_target = session
                .entries
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.origin_rect);
        }
        self.close();
    }

    /// Refreshes the fly-back target rectangle after `focus_window` so cross-workspace or
    /// scrolled windows land directly onto their actual live on-screen rectangle.
    pub fn update_fly_back_target(&mut self, layout: &Layout<Mapped>, id: MappedId) {
        if self.fly_back != Some(id) {
            return;
        }
        let Some(output) = self.output().cloned() else {
            return;
        };
        if let Some(mon) = layout.monitor_for_output(&output) {
            for (ws, ws_geo) in mon.workspaces_with_render_geo() {
                for (tile, tile_pos, visible) in ws.tiles_with_render_positions() {
                    if visible && tile.window().id() == id {
                        let rect = Rectangle::new(
                            ws_geo.loc + tile_pos + tile.window_loc(),
                            tile.window_size(),
                        );
                        if rect.size.w > 0. && rect.size.h > 0. {
                            self.fly_back_target = Some(rect);
                            return;
                        }
                    }
                }
            }
        }
    }

    pub fn close(&mut self) -> bool {
        if matches!(self.state, WindowPickerState::Closing { .. }) {
            return true;
        }

        let state = mem::replace(&mut self.state, WindowPickerState::Closed);
        let WindowPickerState::Open {
            mut session, anim, ..
        } = state
        else {
            self.state = state;
            return false;
        };

        let config = self.config.borrow().window_picker.clone();
        session.prefix = None;
        session.search_mode = false;
        session.quick_look = None;

        if config.animation.off
            || (config.animation_ms_close == 0 && config.animation.kind.is_none())
        {
            self.finish_close();
            return true;
        }

        let close_cfg = config.animation.to_animation(config.animation_ms_close);
        let is_spring = matches!(close_cfg.kind, ConfigAnimKind::Spring(_));

        let current = anim
            .as_ref()
            .map_or(1., |a| a.clamped_value().clamp(0., 1.));
        let mut close_anim = Animation::new(session.clock.clone(), current, 0., 0., close_cfg);
        if let Some(target_ms) = config.target_duration_ms(false) {
            close_anim = close_anim
                .with_clamped_duration(Duration::from_millis(u64::from(target_ms.max(1))));
        }
        self.state = WindowPickerState::Closing {
            session,
            anim: close_anim,
            from_progress: current,
            is_spring,
        };
        for bd in self.backdrops.get_mut().values_mut() {
            bd.framebuffer_effect.damage();
        }
        true
    }

    pub fn close_immediately(&mut self) -> bool {
        let was_active = self.is_active();
        if was_active {
            self.state = WindowPickerState::Closed;
            self.finish_close();
        }
        was_active
    }

    fn finish_close(&mut self) {
        self.backdrops.get_mut().clear();
        self.cards.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.hud_cache.get_mut().clear();
        self.group_cache.get_mut().clear();
        *self.layout_cache.get_mut() = None;
        self.fly_back = None;
        self.fly_back_target = None;
        self.hover = None;
    }

    /// Updates hovered card and card-local 3D tilt from pointer motion.
    /// Returns whether a redraw is needed.
    pub fn pointer_motion(
        &mut self,
        layout: &Layout<Mapped>,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> bool {
        let WindowPickerState::Open { session, .. } = &self.state else {
            return false;
        };
        if output != &session.output {
            return self.set_hover(None);
        }

        let config = self.config.borrow().window_picker.clone();
        let layout = self.resolve_layout(layout, output, &config);
        let mut hit_id = None;
        let mut hit_tilt = Point::from((0., 0.));

        if let Some(layout) = layout {
            let cards = self.cards.borrow();
            for (id, placement) in &layout.placements {
                let hover_v = cards.get(id).map_or(0., |c| c.hover_value().clamp(0., 1.2));
                let base_scale = if session.focused_id == Some(*id) {
                    FOCUSED_SCALE
                } else {
                    1.
                };
                let zoom = lerp(base_scale, HOVER_SCALE, hover_v);
                let card = scale_rect(
                    placement.card,
                    card_center(placement.card),
                    zoom,
                    Point::from((0., 0.)),
                );
                if card.contains(pos_within_output) {
                    hit_id = Some(*id);
                    if config.card.tilt && card.size.w > 1. && card.size.h > 1. {
                        let c = card_center(card);
                        hit_tilt = Point::from((
                            ((pos_within_output.x - c.x) / (card.size.w * 0.5)).clamp(-1., 1.),
                            ((pos_within_output.y - c.y) / (card.size.h * 0.5)).clamp(-1., 1.),
                        ));
                    }
                    break;
                }
            }
        }

        let mut changed = self.set_hover(hit_id);

        let mut cards = self.cards.borrow_mut();
        for (id, state) in cards.iter_mut() {
            let target_tilt = if Some(*id) == hit_id {
                hit_tilt
            } else {
                Point::from((0., 0.))
            };
            if (state.tilt.x - target_tilt.x).abs() > 0.008
                || (state.tilt.y - target_tilt.y).abs() > 0.008
            {
                state.tilt = target_tilt;
                changed = true;
            }
        }

        changed
    }

    /// Updates the hovered preview; smoothly animates both hover-in AND hover-out per card.
    pub fn set_hover(&mut self, id: Option<MappedId>) -> bool {
        let changed = self.hover != id;
        if changed {
            let prev = self.hover;
            self.hover = id;
            if let Some(session) = self.state.session() {
                let clock = session.clock.clone();
                let selected = session.selected_id;
                let mut cards = self.cards.borrow_mut();
                if let Some(prev_id) = prev {
                    if let Some(c) = cards.get_mut(&prev_id) {
                        let target = if selected == Some(prev_id) { 1. } else { 0. };
                        c.set_hover_target(&clock, target);
                        if target == 0. {
                            c.tilt = Point::from((0., 0.));
                        }
                    }
                }
                if let Some(new_id) = id {
                    let c = cards
                        .entry(new_id)
                        .or_insert_with(|| CardVisualState::new(clock.clone()));
                    c.set_hover_target(&clock, 1.);
                }
            }
        }
        changed
    }

    pub fn handle_click(
        &mut self,
        layout_state: &Layout<Mapped>,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
        is_middle: bool,
    ) -> WindowPickerClickResult {
        let WindowPickerState::Open { session, .. } = &mut self.state else {
            return WindowPickerClickResult::None;
        };
        if output != &session.output {
            return WindowPickerClickResult::None;
        }

        let config = self.config.borrow().window_picker.clone();

        // In Quick Look mode, left-clicking anywhere confirms the quick-looked window (or backdrop
        // exits).
        if let Some(ql_id) = session.quick_look {
            if !is_middle {
                let out_size = output_size(output);
                let ql_area = Rectangle::new(
                    Point::from((out_size.w * 0.11, out_size.h * 0.11)),
                    out_size.upscale(0.78),
                );
                if ql_area.contains(pos_within_output) {
                    self.select_window(ql_id);
                    return WindowPickerClickResult::SelectWindow(ql_id);
                }
                if let Some(s) = self.state.session_mut() {
                    s.quick_look = None;
                }
                return WindowPickerClickResult::None;
            }
        }

        let layout = self.resolve_layout(layout_state, output, &config);
        if let Some(layout) = layout {
            // Check HUD scope chip clicks first.
            if !is_middle && config.show_hud {
                for (scope, rect) in layout.scope_chips {
                    if rect.contains(pos_within_output) {
                        if let Some(s) = self.state.session_mut() {
                            if s.scope != scope
                                && s.entries.iter().any(|e| entry_in_scope(e, scope))
                            {
                                s.scope = scope;
                                s.prefix = None;
                                s.selected_id = None;
                                s.reassign_labels(&config.label.letters);
                                *self.layout_cache.borrow_mut() = None;
                                return WindowPickerClickResult::CycleScope;
                            }
                        }
                        return WindowPickerClickResult::None;
                    }
                }
            }

            // Check card hit-testing.
            let cards = self.cards.borrow();
            let focused_id = self.state.session().and_then(|s| s.focused_id);
            for (id, placement) in &layout.placements {
                let hover_v = cards.get(id).map_or(0., |c| c.hover_value().clamp(0., 1.2));
                let base_scale = if focused_id == Some(*id) {
                    FOCUSED_SCALE
                } else {
                    1.
                };
                let zoom = lerp(base_scale, HOVER_SCALE, hover_v);
                let card = scale_rect(
                    placement.card,
                    card_center(placement.card),
                    zoom,
                    Point::from((0., 0.)),
                );
                if card.contains(pos_within_output) {
                    let hit = *id;
                    drop(cards);
                    if is_middle {
                        return WindowPickerClickResult::CloseWindow(hit);
                    }
                    self.select_window(hit);
                    return WindowPickerClickResult::SelectWindow(hit);
                }
            }
        }

        if !is_middle && config.click_backdrop_to_close {
            self.close();
            return WindowPickerClickResult::ClosePicker;
        }

        WindowPickerClickResult::None
    }

    pub fn handle_scroll(&mut self, layout_state: &Layout<Mapped>, forward: bool) -> bool {
        let Some(output) = self.output().cloned() else {
            return false;
        };
        let config = self.config.borrow().window_picker.clone();
        let Some(layout) = self.resolve_layout(layout_state, &output, &config) else {
            return false;
        };
        if layout.placements.is_empty() {
            return false;
        }
        let ids: Vec<MappedId> = layout.placements.iter().map(|(id, _)| *id).collect();
        self.step_selection(&ids, forward);
        true
    }

    pub fn advance_animations(&mut self) -> bool {
        let mut changed = false;

        if let WindowPickerState::Open {
            anim, max_stagger, ..
        } = &mut self.state
        {
            if anim
                .as_ref()
                .is_some_and(|a| a.is_done_with_delay(*max_stagger))
            {
                *anim = None;
                changed = true;
            }
        }

        for card in self.cards.get_mut().values_mut() {
            if card.advance() {
                changed = true;
            }
        }

        let finished = matches!(self.state, WindowPickerState::Closing { .. })
            && self.state.anim().is_some_and(Animation::is_clamped_done);
        if finished {
            self.close_immediately();
            changed = true;
        }
        changed
    }

    pub fn update_config(&mut self) {
        self.label_cache.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.icon_cache.get_mut().clear();
        self.hud_cache.get_mut().clear();
        self.group_cache.get_mut().clear();
        *self.layout_cache.get_mut() = None;
        for card in self.cards.get_mut().values_mut() {
            card.card_blur.damage();
            card.last_blur_rect = Rectangle::default();
            card.last_blur_perspective = None;
        }
        for bd in self.backdrops.get_mut().values_mut() {
            bd.framebuffer_effect.damage();
        }
    }

    pub fn retain_windows(&mut self, live_ids: &HashSet<MappedId>) -> bool {
        let Some(session) = self.state.session_mut() else {
            return false;
        };

        let previous_len = session.entries.len();
        session.entries.retain(|entry| live_ids.contains(&entry.id));
        if session.entries.len() == previous_len {
            return false;
        }

        if session
            .selected_id
            .is_some_and(|id| !live_ids.contains(&id))
        {
            session.selected_id = None;
        }
        if session.quick_look.is_some_and(|id| !live_ids.contains(&id)) {
            session.quick_look = None;
        }
        if self.hover.is_some_and(|id| !live_ids.contains(&id)) {
            self.hover = None;
        }

        self.cards.get_mut().retain(|id, _| live_ids.contains(id));
        *self.layout_cache.get_mut() = None;

        let is_empty = session.entries.is_empty();
        if !is_empty {
            session.prefix = None;
            if !session
                .entries
                .iter()
                .any(|e| entry_in_scope(e, session.scope))
            {
                session.scope = WindowPickerScope::All;
            }
            let letters = self.config.borrow().window_picker.label.letters.clone();
            session.reassign_labels(&letters);
        } else {
            self.close_immediately();
        }
        true
    }

    /// Handles keyboard input while the picker is open.
    /// Supports letter jump, arrow/Tab 2D grid navigation, Enter confirm, `/` live fuzzy search,
    /// `Space` Quick Look preview, `Ctrl+Tab` scope cycling, and `Ctrl+W`/`Delete` close window.
    pub fn handle_key(
        &mut self,
        layout_state: &Layout<Mapped>,
        raw: Keysym,
        modified: Option<Keysym>,
        ctrl: bool,
        shift: bool,
    ) -> WindowPickerKeyResult {
        if raw == Keysym::Escape || modified == Some(Keysym::Escape) {
            if let WindowPickerState::Open { session, .. } = &mut self.state {
                if session.quick_look.is_some() {
                    session.quick_look = None;
                    return WindowPickerKeyResult::Handled;
                }
                if session.search_mode {
                    session.search_mode = false;
                    session.search_query.clear();
                    session.selected_id = None;
                    *self.layout_cache.borrow_mut() = None;
                    return WindowPickerKeyResult::Handled;
                }
            }
            self.close();
            return WindowPickerKeyResult::Handled;
        }

        if !matches!(self.state, WindowPickerState::Open { .. }) {
            return WindowPickerKeyResult::Handled;
        }

        let effective = modified.unwrap_or(raw);

        // Ctrl+Tab cycles collection scope (All -> Workspace -> Output).
        if ctrl && (effective == Keysym::Tab || effective == Keysym::ISO_Left_Tab) {
            self.cycle_scope(!shift);
            return WindowPickerKeyResult::Handled;
        }

        // Ctrl+W or Delete closes the highlighted/hovered window directly from the picker.
        let is_close_key =
            (ctrl && matches!(raw, Keysym::w | Keysym::W)) || effective == Keysym::Delete;
        if is_close_key {
            if let Some(target) = self.active_target_id() {
                return WindowPickerKeyResult::CloseWindow(target);
            }
            return WindowPickerKeyResult::Handled;
        }

        // Resolve layout for 2D spatial arrow navigation and Tab cycling.
        let output = self.output().cloned();
        let config = self.config.borrow().window_picker.clone();
        let layout = output
            .as_ref()
            .and_then(|out| self.resolve_layout(layout_state, out, &config));

        // Arrow keys & Tab work in BOTH Keycap mode and Search mode.
        match effective {
            Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down => {
                if let Some(layout) = &layout {
                    let dir = match effective {
                        Keysym::Left => (-1., 0.),
                        Keysym::Right => (1., 0.),
                        Keysym::Up => (0., -1.),
                        Keysym::Down => (0., 1.),
                        _ => (0., 0.),
                    };
                    self.navigate_grid(&layout.placements, dir);
                }
                return WindowPickerKeyResult::Handled;
            }
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                if let Some(layout) = &layout {
                    let ids: Vec<MappedId> = layout.placements.iter().map(|(id, _)| *id).collect();
                    let forward = effective == Keysym::Tab && !shift;
                    self.step_selection(&ids, forward);
                }
                return WindowPickerKeyResult::Handled;
            }
            Keysym::Return | Keysym::KP_Enter => {
                let target = self.active_target_id().or_else(|| {
                    layout
                        .as_ref()
                        .and_then(|l| l.placements.first().map(|(id, _)| *id))
                });
                if let Some(id) = target {
                    self.select_window(id);
                    return WindowPickerKeyResult::Selected(id);
                }
                return WindowPickerKeyResult::Handled;
            }
            _ => {}
        }

        // Search mode handling.
        let in_search = matches!(
            &self.state,
            WindowPickerState::Open { session, .. } if session.search_mode
        );
        if in_search {
            if effective == Keysym::BackSpace {
                if let WindowPickerState::Open { session, .. } = &mut self.state {
                    if session.search_query.pop().is_none() {
                        session.search_mode = false;
                    }
                    session.selected_id = session.visible_entries().first().map(|e| e.id);
                    *self.layout_cache.borrow_mut() = None;
                }
                return WindowPickerKeyResult::Handled;
            }
            if let Some(ch) = effective.key_char() {
                if !ch.is_control() {
                    if let WindowPickerState::Open { session, .. } = &mut self.state {
                        session.search_query.push(ch);
                        session.selected_id = session.visible_entries().first().map(|e| e.id);
                        *self.layout_cache.borrow_mut() = None;
                    }
                }
            }
            return WindowPickerKeyResult::Handled;
        }

        // `/` enters live fuzzy search mode.
        if effective == Keysym::slash {
            if let WindowPickerState::Open { session, .. } = &mut self.state {
                session.search_mode = true;
                session.prefix = None;
                session.quick_look = None;
                session.search_query.clear();
                session.selected_id = session.visible_entries().first().map(|e| e.id);
            }
            return WindowPickerKeyResult::Handled;
        }

        // `Space` toggles Quick Look zoomed preview for the active card.
        if effective == Keysym::space {
            let active = self.active_target_id();
            if let WindowPickerState::Open { session, .. } = &mut self.state {
                if session.quick_look.is_some() {
                    session.quick_look = None;
                } else if let Some(id) = active {
                    session.quick_look = Some(id);
                }
            }
            return WindowPickerKeyResult::Handled;
        }

        // Standard Keycap fast-jump mode.
        let selected = {
            let WindowPickerState::Open { session, .. } = &mut self.state else {
                return WindowPickerKeyResult::Handled;
            };

            if raw == Keysym::BackSpace {
                session.prefix = None;
                return WindowPickerKeyResult::Handled;
            }

            let Some(letter) = letter_from_keysym(raw) else {
                return WindowPickerKeyResult::Handled;
            };

            let single_len = session
                .entries
                .first()
                .is_some_and(|entry| entry.label.len() == 1);

            if single_len {
                session
                    .entries
                    .iter()
                    .find(|entry| {
                        entry_in_scope(entry, session.scope) && entry.label.starts_with(letter)
                    })
                    .map(|entry| entry.id)
            } else if let Some(prefix) = session.prefix {
                session
                    .entries
                    .iter()
                    .find(|entry| {
                        entry_in_scope(entry, session.scope) && {
                            let mut chars = entry.label.chars();
                            chars.next() == Some(prefix) && chars.next() == Some(letter)
                        }
                    })
                    .map(|entry| entry.id)
            } else {
                if session.entries.iter().any(|entry| {
                    entry_in_scope(entry, session.scope) && entry.label.starts_with(letter)
                }) {
                    session.prefix = Some(letter);
                }
                None
            }
        };

        if let Some(id) = selected {
            self.select_window(id);
            return WindowPickerKeyResult::Selected(id);
        }

        WindowPickerKeyResult::Handled
    }

    fn active_target_id(&self) -> Option<MappedId> {
        let session = self.state.session()?;
        session
            .quick_look
            .or(self.hover)
            .or(session.selected_id)
            .or(session.focused_id)
            .or_else(|| session.visible_entries().first().map(|e| e.id))
    }

    fn cycle_scope(&mut self, forward: bool) {
        let WindowPickerState::Open { session, .. } = &mut self.state else {
            return;
        };
        let order = [
            WindowPickerScope::All,
            WindowPickerScope::Workspace,
            WindowPickerScope::Output,
        ];
        let cur_idx = order.iter().position(|s| *s == session.scope).unwrap_or(0);
        for step in 1..=order.len() {
            let next_idx = if forward {
                (cur_idx + step) % order.len()
            } else {
                (cur_idx + order.len() - step) % order.len()
            };
            let candidate = order[next_idx];
            if session.entries.iter().any(|e| entry_in_scope(e, candidate)) {
                session.scope = candidate;
                session.prefix = None;
                session.selected_id = None;
                let letters = self.config.borrow().window_picker.label.letters.clone();
                session.reassign_labels(&letters);
                *self.layout_cache.borrow_mut() = None;
                break;
            }
        }
    }

    fn step_selection(&mut self, ids: &[MappedId], forward: bool) {
        if ids.is_empty() {
            return;
        }
        let prev = self.state.session().and_then(|s| s.selected_id);
        let current = prev
            .or(self.hover)
            .or_else(|| self.state.session().and_then(|s| s.focused_id));
        let next = if let Some(cur) = current.and_then(|id| ids.iter().position(|x| *x == id)) {
            if forward {
                ids[(cur + 1) % ids.len()]
            } else {
                ids[(cur + ids.len() - 1) % ids.len()]
            }
        } else if forward {
            ids[0]
        } else {
            ids[ids.len() - 1]
        };
        self.set_selected(Some(next));
    }

    fn navigate_grid(&mut self, placements: &[(MappedId, Placement)], dir: (f64, f64)) {
        if placements.is_empty() {
            return;
        }
        let current = self
            .state
            .session()
            .and_then(|s| s.selected_id)
            .or(self.hover)
            .or_else(|| self.state.session().and_then(|s| s.focused_id));

        let Some(cur_id) = current else {
            self.set_selected(Some(placements[0].0));
            return;
        };
        let Some((_, cur_pl)) = placements.iter().find(|(id, _)| *id == cur_id) else {
            self.set_selected(Some(placements[0].0));
            return;
        };

        let cur_c = card_center(cur_pl.card);
        let mut best: Option<(MappedId, f64)> = None;

        for (id, pl) in placements {
            if *id == cur_id {
                continue;
            }
            let c = card_center(pl.card);
            let dx = c.x - cur_c.x;
            let dy = c.y - cur_c.y;
            let along = dx * dir.0 + dy * dir.1;
            if along <= 1.0 {
                continue;
            }
            let perp = (dx * dir.1 - dy * dir.0).abs();
            // Favor aligned cards in the same row/column while allowing graceful diagonal jumps.
            let score = along + perp * 2.4;
            if best.is_none_or(|(_, s)| score < s) {
                best = Some((*id, score));
            }
        }

        if let Some((next_id, _)) = best {
            self.set_selected(Some(next_id));
        } else {
            // Wrap around smoothly if at the boundary of the grid.
            let ids: Vec<MappedId> = placements.iter().map(|(id, _)| *id).collect();
            self.step_selection(&ids, dir.0 > 0. || dir.1 > 0.);
        }
    }

    fn set_selected(&mut self, id: Option<MappedId>) {
        let Some(session) = self.state.session_mut() else {
            return;
        };
        let prev = session.selected_id;
        if prev == id {
            return;
        }
        session.selected_id = id;
        let clock = session.clock.clone();
        let hover = self.hover;
        let mut cards = self.cards.borrow_mut();
        if let Some(prev_id) = prev {
            if let Some(c) = cards.get_mut(&prev_id) {
                c.set_hover_target(&clock, if hover == Some(prev_id) { 1. } else { 0. });
            }
        }
        if let Some(new_id) = id {
            let c = cards
                .entry(new_id)
                .or_insert_with(|| CardVisualState::new(clock.clone()));
            c.set_hover_target(&clock, 1.);
        }
    }

    pub fn window_under(
        &self,
        niri: &Niri,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<MappedId> {
        let WindowPickerState::Open { session, .. } = &self.state else {
            return None;
        };
        if output != &session.output {
            return None;
        }

        let config = self.config.borrow().window_picker.clone();
        let layout = self.resolve_layout(&niri.layout, output, &config)?;
        let cards = self.cards.borrow();
        layout.placements.iter().find_map(|(id, placement)| {
            let hover_v = cards.get(id).map_or(0., |c| c.hover_value().clamp(0., 1.2));
            let base_scale = if session.focused_id == Some(*id) {
                FOCUSED_SCALE
            } else {
                1.
            };
            let zoom = lerp(base_scale, HOVER_SCALE, hover_v);
            let card = scale_rect(
                placement.card,
                card_center(placement.card),
                zoom,
                Point::from((0., 0.)),
            );
            card.contains(pos_within_output).then_some(*id)
        })
    }

    /// Returns true ONLY while an animation or spring is actually in motion (or when `glow_pulse`
    /// is explicitly enabled). Guarantees 0 FPS idle power consumption when browsing statically.
    pub fn are_animations_ongoing(&self) -> bool {
        match &self.state {
            WindowPickerState::Open {
                anim: Some(anim),
                max_stagger,
                ..
            } if !anim.is_done_with_delay(*max_stagger) => return true,
            WindowPickerState::Closing { anim, .. } if !anim.is_done() => return true,
            _ => {}
        }

        if self
            .cards
            .borrow()
            .values()
            .any(CardVisualState::is_animating)
        {
            return true;
        }

        let WindowPickerState::Open { session, .. } = &self.state else {
            return false;
        };
        let selection = self.config.borrow().window_picker.selection;
        selection.glow
            && selection.glow_pulse
            && (self.hover.is_some()
                || session.selected_id.is_some()
                || session.focused_id.is_some())
    }

    fn resolve_layout(
        &self,
        layout_state: &Layout<Mapped>,
        output: &Output,
        config: &niri_config::WindowPicker,
    ) -> Option<ComputedLayout> {
        let session = self.state.session()?;
        let out_size = output_size(output);
        let scale = output.current_scale().fractional_scale();
        let mut windows = picker_windows(layout_state, session, &config.label);
        if windows.is_empty() {
            return None;
        }

        let label_cache = self.label_cache.borrow();
        for window in &mut windows {
            if let Some(size) = label_cache.logical_size(&window.entry.label, scale, &config.label)
            {
                window.label_size = size;
            }
        }
        drop(label_cache);

        let key = LayoutCacheKey {
            output_size: (
                (out_size.w * 100.).round() as i64,
                (out_size.h * 100.).round() as i64,
            ),
            scale: (scale * 10_000.).round() as i64,
            scope: session.scope,
            search_query: session.search_query.clone(),
            window_sigs: windows
                .iter()
                .map(|w| {
                    let s = w.mapped.size();
                    (
                        w.entry.id,
                        i64::from(s.w),
                        i64::from(s.h),
                        (w.label_size.w * 10.).round() as i64,
                        (w.label_size.h * 10.).round() as i64,
                    )
                })
                .collect(),
            layout_mode: config.layout,
            group_by: config.group_by,
            show_hud: config.show_hud,
            area_width: (config.area_width * 1000.).round() as i64,
            area_height: (config.area_height * 1000.).round() as i64,
            max_scale: (config.max_scale * 1000.).round() as i64,
            gap: (config.gap * 100.).round() as i64,
            card_on: config.card.on,
            info_height: (config.card.info.height * 100.).round() as i64,
        };

        if let Some((cached_key, cached_layout)) = self.layout_cache.borrow().as_ref() {
            if *cached_key == key {
                return Some(cached_layout.clone());
            }
        }

        let computed = compute_layout(out_size, &windows, config)?;
        *self.layout_cache.borrow_mut() = Some((key, computed.clone()));
        Some(computed)
    }

    pub fn render_output<R: NiriRenderer>(
        &self,
        niri: &Niri,
        output: &Output,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let Some(session) = self.state.session() else {
            return;
        };

        let config = self.config.borrow().window_picker.clone();
        let progress = self.progress();
        let is_spring = self.state.is_spring();
        let closing = self.is_closing();
        let alpha = progress.clamp(0., 1.) as f32;
        let size = output_size(output);
        let scale = output.current_scale().fractional_scale();

        if *output == session.output {
            let selection = config.selection;
            let mut windows = picker_windows(&niri.layout, session, &config.label);
            let mut by_id: HashMap<MappedId, usize> = HashMap::with_capacity(windows.len());

            for (idx, window) in windows.iter_mut().enumerate() {
                by_id.insert(window.entry.id, idx);
                let is_hovered = !closing
                    && (self.hover == Some(window.entry.id)
                        || session.selected_id == Some(window.entry.id));
                let is_focused = !closing && session.focused_id == Some(window.entry.id);
                let highlight = if is_hovered {
                    HighlightKind::Hovered
                } else if is_focused {
                    HighlightKind::Focused
                } else {
                    HighlightKind::Normal
                };
                let typed_len = match session.prefix {
                    Some(prefix) if window.entry.label.starts_with(prefix) => 1,
                    _ => 0,
                };
                let style = LabelStyle {
                    highlight,
                    typed_len,
                };
                let texture = self.label_cache.borrow_mut().get(
                    ctx.as_gles().renderer,
                    &window.entry.label,
                    style,
                    scale,
                    &config.label,
                    selection.focused_color,
                    selection.hover_color,
                );
                if let Some(texture) = &texture {
                    window.label_size = texture.logical_size();
                }
                window.texture = texture;
            }

            if let Some(layout) = self.resolve_layout(&niri.layout, output, &config) {
                // 1. Quick Look zoomed preview overlay (drawn on top of cards when active).
                if let Some(ql_id) = session.quick_look {
                    if let Some(&idx) = by_id.get(&ql_id) {
                        self.render_quick_look(
                            ctx.r(),
                            size,
                            scale,
                            &windows[idx],
                            alpha,
                            &config,
                            push,
                        );
                    }
                }

                // 2. Top Liquid Glass HUD bar (Scope pills + Search / Prefix / Active Title).
                if config.show_hud && !closing {
                    if let Some(hud_rect) = layout.hud_rect {
                        let active_title = self
                            .active_target_id()
                            .and_then(|id| by_id.get(&id).map(|&idx| &windows[idx]))
                            .map(|w| {
                                if ctx.target.should_block_out(w.mapped.rules().block_out_from) {
                                    "Hidden Window".to_owned()
                                } else {
                                    w.entry
                                        .title
                                        .clone()
                                        .filter(|t| !t.is_empty())
                                        .or_else(|| w.entry.app_id.clone())
                                        .unwrap_or_else(|| "Window".to_owned())
                                }
                            })
                            .unwrap_or_default();

                        self.render_hud(
                            ctx.as_gles().renderer,
                            hud_rect,
                            scale,
                            alpha,
                            progress < 1.,
                            session,
                            &active_title,
                            &config,
                            push,
                        );
                    }
                }

                // 3. Group headers (when group-by is workspace / output).
                if !layout.group_headers.is_empty() && alpha > 0.01 {
                    self.render_group_headers(
                        ctx.as_gles().renderer,
                        &layout.group_headers,
                        scale,
                        alpha,
                        progress < 1.,
                        &config,
                        push,
                    );
                }

                let count = layout.placements.len();
                let step_delay = match &self.state {
                    WindowPickerState::Open { step_delay, .. } => *step_delay,
                    _ => Duration::ZERO,
                };
                let close_from = self.close_from_progress();
                let glow_pulse = if selection.glow_pulse {
                    0.82 + 0.18 * (session.clock.now().as_secs_f64() * 2.8).sin()
                } else {
                    1.0
                };

                // Render hovered/selected/fly-back card FIRST (front-to-back order -> drawn on
                // top!).
                let mut draw_order: Vec<usize> = (0..count).collect();
                draw_order.sort_by_key(|&i| {
                    let id = layout.placements[i].0;
                    if closing && self.fly_back == Some(id) {
                        0
                    } else if self.hover == Some(id) || session.selected_id == Some(id) {
                        1
                    } else {
                        2
                    }
                });

                for i in draw_order {
                    let (id, placement) = layout.placements[i];
                    let Some(&win_idx) = by_id.get(&id) else {
                        continue;
                    };
                    let window = &windows[win_idx];

                    // Respect `block-out-from` kind="hide" in screencasts/screen captures!
                    if ctx.target.should_hide(window.mapped.rules().block_out_from) {
                        continue;
                    }
                    let should_block_out = ctx
                        .target
                        .should_block_out(window.mapped.rules().block_out_from);

                    let style = window.entry.motion;
                    let mut tf = if closing {
                        let close_t = if close_from > f64::EPSILON {
                            ((close_from - progress) / close_from).clamp(0., 1.)
                        } else {
                            1.
                        };
                        let start = open_transform(
                            style,
                            window.entry,
                            &placement,
                            size,
                            close_from.clamp(0., 1.),
                            is_spring,
                            i,
                        );
                        let scatter = scatter_transform(placement.card, size, close_t, i);
                        CardTransform {
                            offset: start.offset + (scatter.offset - start.offset).upscale(close_t),
                            scale: lerp(start.scale, scatter.scale, close_t),
                            angle: lerp(start.angle, scatter.angle, close_t),
                            flip_y: lerp(start.flip_y, 0., close_t),
                            alpha: start.alpha * scatter.alpha,
                        }
                    } else {
                        // True time-shifted per-card evaluation: preserves full Spring overshoot
                        // during motion and is guaranteed to settle at exact 1.0 (`identity()`).
                        let card_e = match &self.state {
                            WindowPickerState::Open {
                                anim: Some(anim), ..
                            } => anim.value_with_delay(step_delay.saturating_mul(i as u32)),
                            WindowPickerState::Open { anim: None, .. } => 1.0,
                            _ => progress,
                        };
                        open_transform(style, window.entry, &placement, size, card_e, is_spring, i)
                    };

                    let hovered = self.hover == Some(id) || session.selected_id == Some(id);
                    let focused = session.focused_id == Some(id);
                    let is_fly_back = closing && self.fly_back == Some(id);

                    // Update smooth filter spring for prefix dimming.
                    let prefix_dim = session
                        .prefix
                        .is_some_and(|prefix| !window.entry.label.starts_with(prefix));

                    let (hover_v, filter_v, tilt) = {
                        let mut cards = self.cards.borrow_mut();
                        let card_state = cards
                            .entry(id)
                            .or_insert_with(|| CardVisualState::new(session.clock.clone()));
                        card_state
                            .set_filter_target(&session.clock, if prefix_dim { 0. } else { 1. });
                        (
                            card_state.hover_value().clamp(0., 1.25),
                            card_state.filter_value(),
                            card_state.tilt,
                        )
                    };

                    if is_fly_back {
                        // True Shared-Element fly-back to the target window's real layout rect!
                        let fly_t = (1. - progress / close_from.max(0.001)).clamp(0., 1.);
                        if let Some(target_rect) = self.fly_back_target.or(window.entry.origin_rect)
                        {
                            let from_center = card_center(placement.preview);
                            let to_center = card_center(target_rect);
                            let target_scale = (target_rect.size.w
                                / placement.preview.size.w.max(1.))
                            .min(target_rect.size.h / placement.preview.size.h.max(1.))
                            .clamp(0.4, 3.5);
                            tf.offset = (to_center - from_center).upscale(fly_t);
                            tf.scale = lerp(1., target_scale, fly_t);
                            tf.angle = 0.;
                            tf.flip_y = 0.;
                            tf.alpha = ((1. - fly_t) / 0.25).clamp(0., 1.);
                        } else {
                            tf.offset = Point::from((0., 0.));
                            tf.scale = 1. + 0.45 * fly_t;
                            tf.angle = 0.;
                            tf.flip_y = 0.;
                            tf.alpha = ((progress - 0.1) / 0.3).clamp(0., 1.);
                        }
                    } else if !closing {
                        let base_scale = if focused { FOCUSED_SCALE } else { 1. };
                        let zoom = lerp(base_scale, HOVER_SCALE, hover_v);
                        let dim_scale = lerp(0.96, 1.0, filter_v);
                        tf.scale *= zoom * dim_scale;
                    }

                    let filter_alpha = lerp(f64::from(PREFIX_DIM_ALPHA), 1.0, filter_v) as f32;
                    let quick_look_dim = if session.quick_look.is_some() {
                        0.28
                    } else {
                        1.0
                    };
                    let entry_alpha = if is_fly_back {
                        tf.alpha.clamp(0., 1.) as f32
                    } else {
                        (alpha * tf.alpha.clamp(0., 1.) as f32 * filter_alpha * quick_look_dim)
                            .clamp(0., 1.)
                    };

                    if entry_alpha <= 0.002 {
                        continue;
                    }

                    let highlight_strength = if closing {
                        0.
                    } else if hovered {
                        (0.45 + 0.55 * hover_v.clamp(0., 1.)) as f32
                    } else if focused {
                        0.55
                    } else {
                        0.
                    };
                    let accent_color = if hovered {
                        selection.hover_color
                    } else {
                        selection.focused_color
                    };

                    let glow_spec = if !closing && selection.glow && tf.flip_y.abs() < 0.4 {
                        if hovered {
                            Some((
                                selection.hover_color,
                                glow_pulse as f32 * (0.35 + 0.55 * hover_v.clamp(0., 1.) as f32),
                            ))
                        } else if focused {
                            Some((selection.focused_color, glow_pulse as f32 * 0.52))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let card_blur_enabled = !closing
                        && config.card.on
                        && config.card.blur.on
                        && config.card.blur.passes > 0
                        && config.card.blur.offset > 0.
                        && entry_alpha > 0.01;

                    let has_tilt = !closing
                        && config.card.tilt
                        && (tilt.x.abs() > 0.01 || tilt.y.abs() > 0.01);
                    let needs_composite =
                        tf.angle.abs() > 1e-4 || tf.flip_y.abs() > 1e-4 || has_tilt;

                    if needs_composite {
                        // Composite the whole card (preview + info + glass rim + shadow + glow +
                        // keycap) into its per-card OffscreenBuffer, then render via
                        // `picker_card.frag` and pair it with a perspective-synced `picker_blur`
                        // `FramebufferEffect` sharing the exact same pivot, tilt, angle, flip,
                        // scale, and corner radius.
                        let snapped_base_card = snap_rect_to_px(placement.card, scale);
                        let base_center = card_center(snapped_base_card);
                        let screen_pivot = base_center + tf.offset;
                        let base_max_r =
                            (snapped_base_card.size.w.min(snapped_base_card.size.h) * 0.5) as f32;
                        let base_corner_radius = CornerRadius::from(
                            (config.card.corner_radius.max(0.) as f32).min(base_max_r),
                        );

                        let mut gctx = ctx.as_gles();
                        let mut elems = Vec::new();
                        self.render_card_contents(
                            gctx.r(),
                            scale,
                            window,
                            &placement,
                            CardTransform::identity(),
                            1.,
                            highlight_strength,
                            accent_color,
                            glow_spec,
                            should_block_out,
                            &config,
                            &mut |elem| elems.push(elem),
                        );

                        let rendered = {
                            let mut cards = self.cards.borrow_mut();
                            let card_state = cards
                                .entry(id)
                                .or_insert_with(|| CardVisualState::new(session.clock.clone()));
                            card_state
                                .offscreen
                                .render(gctx.renderer, Scale::from(scale), &elems)
                        };
                        let Ok((elem, _sync, _data)) = rendered else {
                            continue;
                        };
                        let texture = elem.texture().clone();
                        let tex_rect = Rectangle::new(elem.offset(), elem.logical_size());
                        let tex_full_size =
                            texture.size().to_f64().to_logical(scale, Transform::Normal);
                        if tex_rect.size.w <= 0. || tex_rect.size.h <= 0. {
                            continue;
                        }

                        // Exact pivot relative to the offscreen texture's top-left!
                        let pivot = base_center - tex_rect.loc;
                        let extra_pad = if has_tilt || tf.flip_y.abs() > 1e-4 {
                            (snapped_base_card.size.w.max(snapped_base_card.size.h)
                                * tf.scale
                                * 0.16)
                                .max(28.)
                        } else {
                            4.
                        };
                        let (raw_dest_size, raw_dest_center) =
                            rotated_bounds(tex_rect.size, pivot, tf.angle, tf.scale, extra_pad);
                        if raw_dest_size.w <= 0. || raw_dest_size.h <= 0. {
                            continue;
                        }
                        let raw_dest_loc =
                            screen_pivot + raw_dest_center - raw_dest_size.to_point().upscale(0.5);
                        let rot_dest_rect =
                            snap_rect_to_px(Rectangle::new(raw_dest_loc, raw_dest_size), scale);
                        let dest_center = card_center(rot_dest_rect) - screen_pivot;

                        let persp_half_size = [
                            (snapped_base_card.size.w * 0.5 * tf.scale) as f32,
                            (snapped_base_card.size.h * 0.5 * tf.scale) as f32,
                        ];
                        let card_half_size = [
                            (snapped_base_card.size.w * 0.5) as f32,
                            (snapped_base_card.size.h * 0.5) as f32,
                        ];

                        let active_tilt = if has_tilt {
                            [tilt.x as f32, tilt.y as f32]
                        } else {
                            [0.0f32, 0.0f32]
                        };
                        let glint = if has_tilt {
                            (hover_v.clamp(0., 1.) * 0.85) as f32
                        } else {
                            0.0f32
                        };

                        let uniforms = Rc::new([
                            Uniform::new(
                                "src_size",
                                [tex_rect.size.w as f32, tex_rect.size.h as f32],
                            ),
                            Uniform::new(
                                "tex_size",
                                [tex_full_size.w as f32, tex_full_size.h as f32],
                            ),
                            Uniform::new("src_pivot", [pivot.x as f32, pivot.y as f32]),
                            Uniform::new(
                                "dest_size",
                                [rot_dest_rect.size.w as f32, rot_dest_rect.size.h as f32],
                            ),
                            Uniform::new(
                                "dest_center",
                                [dest_center.x as f32, dest_center.y as f32],
                            ),
                            Uniform::new("persp_half_size", persp_half_size),
                            Uniform::new("card_half_size", card_half_size),
                            Uniform::new("angle", tf.angle as f32),
                            Uniform::new("card_scale", tf.scale as f32),
                            Uniform::new("tilt", active_tilt),
                            Uniform::new("flip_y", tf.flip_y as f32),
                            Uniform::new("glint_strength", glint),
                        ]);
                        let textures = HashMap::from([(String::from("tex"), texture)]);

                        let mut cards = self.cards.borrow_mut();
                        let card_state = cards
                            .entry(id)
                            .or_insert_with(|| CardVisualState::new(session.clock.clone()));
                        card_state.rot_elem.update(
                            rot_dest_rect.size,
                            None,
                            scale as f32,
                            entry_alpha,
                            uniforms,
                            textures,
                        );
                        push(WindowPickerUiRenderElement::Card(
                            card_state.rot_elem.clone().with_location(rot_dest_rect.loc),
                        ));

                        if card_blur_enabled {
                            let (raw_blur_size, raw_blur_center) = rotated_bounds(
                                snapped_base_card.size,
                                snapped_base_card.size.to_point().upscale(0.5),
                                tf.angle,
                                tf.scale,
                                extra_pad,
                            );
                            if raw_blur_size.w > 0. && raw_blur_size.h > 0. {
                                let raw_blur_loc = screen_pivot + raw_blur_center
                                    - raw_blur_size.to_point().upscale(0.5);
                                let blur_dest_rect = snap_rect_to_px(
                                    Rectangle::new(raw_blur_loc, raw_blur_size),
                                    scale,
                                );
                                let blur_dest_center = card_center(blur_dest_rect) - screen_pivot;
                                let persp = PerspectiveClip {
                                    card_size: [
                                        snapped_base_card.size.w as f32,
                                        snapped_base_card.size.h as f32,
                                    ],
                                    dest_center: [
                                        blur_dest_center.x as f32,
                                        blur_dest_center.y as f32,
                                    ],
                                    persp_half_size,
                                    angle: tf.angle as f32,
                                    card_scale: tf.scale as f32,
                                    tilt: active_tilt,
                                    flip_y: tf.flip_y as f32,
                                    alpha: entry_alpha,
                                };
                                if card_state.last_blur_rect != blur_dest_rect
                                    || card_state.last_blur_perspective != Some(persp)
                                    || progress < 1.
                                    || entry_alpha < 0.999
                                {
                                    card_state.card_blur.damage();
                                    card_state.last_blur_rect = blur_dest_rect;
                                    card_state.last_blur_perspective = Some(persp);
                                }
                                let params = RenderParams {
                                    geometry: blur_dest_rect,
                                    subregion: None,
                                    clip: Some((blur_dest_rect, base_corner_radius)),
                                    scale,
                                };
                                let blur = BlurOptions {
                                    passes: config.card.blur.passes,
                                    offset: (config.card.blur.offset * f64::from(entry_alpha))
                                        .max(0.25),
                                };
                                let effect = card_state.card_blur.render_perspective(
                                    None,
                                    params,
                                    Some(blur),
                                    persp,
                                );
                                push(WindowPickerUiRenderElement::FramebufferEffect(effect));
                            }
                        }
                    } else {
                        self.render_card_contents(
                            ctx.r(),
                            scale,
                            window,
                            &placement,
                            tf,
                            entry_alpha,
                            highlight_strength,
                            accent_color,
                            glow_spec,
                            should_block_out,
                            &config,
                            push,
                        );

                        if card_blur_enabled {
                            let snapped_card_rect = snap_rect_to_px(
                                scale_rect(
                                    placement.card,
                                    card_center(placement.card),
                                    tf.scale,
                                    tf.offset,
                                ),
                                scale,
                            );
                            let max_r = (snapped_card_rect.size.w.min(snapped_card_rect.size.h)
                                * 0.5) as f32;
                            let card_corner_radius = CornerRadius::from(
                                (config.card.corner_radius.max(0.) as f32 * tf.scale as f32)
                                    .min(max_r),
                            );
                            let mut cards = self.cards.borrow_mut();
                            let card_state = cards
                                .entry(id)
                                .or_insert_with(|| CardVisualState::new(session.clock.clone()));
                            if card_state.last_blur_rect != snapped_card_rect
                                || card_state.last_blur_perspective.is_some()
                                || progress < 1.
                                || entry_alpha < 0.999
                            {
                                card_state.card_blur.damage();
                                card_state.last_blur_rect = snapped_card_rect;
                                card_state.last_blur_perspective = None;
                            }
                            let params = RenderParams {
                                geometry: snapped_card_rect,
                                subregion: None,
                                clip: Some((snapped_card_rect, card_corner_radius)),
                                scale,
                            };
                            let blur = BlurOptions {
                                passes: config.card.blur.passes,
                                offset: (config.card.blur.offset * f64::from(entry_alpha))
                                    .max(0.25),
                            };
                            let effect = card_state.card_blur.render(
                                None,
                                params,
                                Some(blur),
                                0.,
                                1.,
                                None,
                                None,
                            );
                            push(WindowPickerUiRenderElement::FramebufferEffect(effect));
                        }
                    }
                }
            }
        }

        self.render_backdrop(output, size, progress, closing, &config, push);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card_contents<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        scale: f64,
        window: &RenderedWindow<'_>,
        placement: &Placement,
        transform: CardTransform,
        entry_alpha: f32,
        highlight: f32,
        accent_color: Color,
        glow_spec: Option<(Color, f32)>,
        should_block_out: bool,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let id = window.entry.id;
        let pivot = card_center(placement.card);
        let raw_card = scale_rect(placement.card, pivot, transform.scale, transform.offset);
        let raw_preview = scale_rect(placement.preview, pivot, transform.scale, transform.offset);
        let raw_info = scale_rect(placement.info, pivot, transform.scale, transform.offset);
        let label = scale_rect(placement.label, pivot, transform.scale, transform.offset);

        // Snap card, preview, and info to exact physical pixel boundaries so the thumbnail
        // bottom pixel row meets the info strip with zero sub-pixel gap.
        let card = snap_rect_to_px(raw_card, scale);
        let max_r = (card.size.w.min(card.size.h) * 0.5) as f32;
        let card_radius =
            (config.card.corner_radius.max(0.) as f32 * transform.scale as f32).min(max_r);
        let card_top = card.loc.y;
        let card_bottom = card.loc.y + card.size.h;
        let split_y = if config.card.on && raw_info.size.h > 1. {
            let s = if scale > 0. {
                ((raw_preview.loc.y + raw_preview.size.h) * scale).round() / scale
            } else {
                raw_preview.loc.y + raw_preview.size.h
            };
            s.clamp(card_top, card_bottom)
        } else {
            card_bottom
        };
        let preview = Rectangle::new(
            card.loc,
            Size::from((card.size.w, (split_y - card_top).max(1. / scale.max(1.)))),
        );
        let info = Rectangle::new(
            Point::from((card.loc.x, split_y)),
            Size::from((card.size.w, (card_bottom - split_y).max(0.))),
        );

        // 1. Keycap label (topmost element).
        if let Some(texture) = &window.texture {
            let texture = TextureRenderElement::from_texture_buffer(
                texture.clone(),
                label.loc,
                entry_alpha,
                None,
                Some(label.size),
                Kind::Unspecified,
            );
            push(WindowPickerUiRenderElement::Label(
                PrimaryGpuTextureRenderElement(texture),
            ));
        }

        // 2. App metadata strip (title left, pure colored workspace right, app_id bottom-center).
        if config.card.on && info.size.h > 1. {
            let app_id = window.entry.app_id.as_deref().unwrap_or("unknown");
            let title = window
                .entry
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or(app_id);
            let gctx = ctx.as_gles();
            let mut icons = self.icon_cache.borrow_mut();
            let info_texture = self.info_cache.borrow_mut().get(
                gctx.renderer,
                &mut icons,
                app_id,
                title,
                &window.entry.workspace,
                placement.info.size.w,
                placement.info.size.h,
                placement.info_scale,
                scale,
                window.entry.is_floating,
                window.entry.is_urgent,
                should_block_out,
                config.card.show_icons,
                &config.card.info,
            );
            if let Some(texture) = info_texture {
                let texture = TextureRenderElement::from_texture_buffer(
                    texture,
                    info.loc,
                    entry_alpha,
                    None,
                    Some(info.size),
                    Kind::Unspecified,
                );
                push(WindowPickerUiRenderElement::Label(
                    PrimaryGpuTextureRenderElement(texture),
                ));
            }
        }

        // 3. Seamless glass rim border around the unified card (zero background fill, zero divider
        //    line — pure rim over both the window thumbnail and blurred info strip).
        let radius = CornerRadius::from(card_radius);
        if config.card.on {
            let glass_uniforms = Rc::new([
                Uniform::new("geo_size", [card.size.w as f32, card.size.h as f32]),
                Uniform::new("corner_radius", <[f32; 4]>::from(radius)),
                Uniform::new("fill_color", [0.0f32, 0.0, 0.0, 0.0]),
                Uniform::new("border_color", config.card.glass_border.to_array_unpremul()),
                Uniform::new("accent_color", accent_color.to_array_unpremul()),
                Uniform::new("highlight", highlight),
                Uniform::new("divider_y", 0.0f32),
                Uniform::new("scrim_alpha", 0.0f32),
            ]);

            let mut cards = self.cards.borrow_mut();
            if let Some(card_state) = cards.get_mut(&id) {
                card_state.glass_elem.update(
                    card.size,
                    None,
                    scale as f32,
                    entry_alpha,
                    glass_uniforms,
                    HashMap::new(),
                );
                push(WindowPickerUiRenderElement::Card(
                    card_state.glass_elem.clone().with_location(card.loc),
                ));
            }
        }

        // 4. Window live preview (or privacy shield if blocked-out on screencast/capture).
        if should_block_out {
            let mut cards = self.cards.borrow_mut();
            if let Some(card_state) = cards.get_mut(&id) {
                card_state.privacy_buf.update(
                    preview.size,
                    Color32F::new(
                        0.08 * entry_alpha,
                        0.10 * entry_alpha,
                        0.14 * entry_alpha,
                        0.94 * entry_alpha,
                    ),
                );
                push(WindowPickerUiRenderElement::SolidColor(
                    SolidColorRenderElement::from_buffer(
                        &card_state.privacy_buf,
                        preview.loc,
                        1.,
                        Kind::Unspecified,
                    ),
                ));
            }
        } else {
            render_thumbnail(
                ctx.r(),
                scale,
                window.mapped,
                preview,
                entry_alpha,
                thumbnail_clip_radius(
                    window.mapped,
                    preview.size,
                    card_radius,
                    info.size.h > 1.,
                    config,
                ),
                push,
            );
        }

        // 5. Outer card drop shadow (card interior is cut out so it never tints the card).
        if config.card.on && config.card.shadow {
            let mut cards = self.cards.borrow_mut();
            if let Some(card_state) = cards.get_mut(&id) {
                let margin = (GLOW_SIGMA * 2.).ceil();
                let shader_size = card.size + Size::from((margin, margin)).upscale(2.);
                card_state.card_shadow.update(
                    shader_size,
                    Rectangle::new(Point::from((margin, margin)), card.size),
                    Color::from_rgba8_unpremul(0, 0, 0, 255),
                    14. * transform.scale as f32,
                    radius,
                    scale as f32,
                    Rectangle::new(Point::from((margin, margin)), card.size),
                    radius,
                    // Respect entry_alpha so shadows fade seamlessly with cards!
                    0.38 * entry_alpha,
                );
                push(WindowPickerUiRenderElement::Shadow(
                    card_state
                        .card_shadow
                        .clone()
                        .with_location(card.loc - Point::from((margin, margin))),
                ));
            }
        }

        // 6. Outer accent glow (card interior is cut out so it never tints the blurred card, and
        //    compositing inside `render_card_contents` keeps it locked to 3D tilt/flip).
        if let Some((color, base_alpha)) = glow_spec {
            let mut cards = self.cards.borrow_mut();
            if let Some(card_state) = cards.get_mut(&id) {
                let margin = (GLOW_SIGMA * 3.).ceil();
                let shader_size = card.size + Size::from((margin, margin)).upscale(2.);
                let inner_rect = Rectangle::new(Point::from((margin, margin)), card.size);
                card_state.glow.update(
                    shader_size,
                    inner_rect,
                    color,
                    GLOW_SIGMA as f32,
                    radius,
                    scale as f32,
                    inner_rect,
                    radius,
                    base_alpha * entry_alpha,
                );
                push(WindowPickerUiRenderElement::Shadow(
                    card_state
                        .glow
                        .clone()
                        .with_location(card.loc - Point::from((margin, margin))),
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_quick_look<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        output_size: Size<f64, Logical>,
        scale: f64,
        window: &RenderedWindow<'_>,
        alpha: f32,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        if ctx.target.should_hide(window.mapped.rules().block_out_from) {
            return;
        }
        let should_block_out = ctx
            .target
            .should_block_out(window.mapped.rules().block_out_from);
        let win_size = window.mapped.size().to_f64();
        if win_size.w <= 0. || win_size.h <= 0. {
            return;
        }
        let max_w = output_size.w * 0.78;
        let max_h = output_size.h * 0.76;
        let s = (max_w / win_size.w).min(max_h / win_size.h).min(1.0);
        let preview_size = win_size.upscale(s);
        let preview_loc = Point::from((
            (output_size.w - preview_size.w) * 0.5,
            (output_size.h - preview_size.h) * 0.5 + 16.,
        ));
        let preview = Rectangle::new(preview_loc, preview_size);
        let radius = CornerRadius::from(
            (config.card.corner_radius.max(0.) as f32)
                .min((preview.size.w.min(preview.size.h) * 0.5) as f32),
        );

        if !should_block_out {
            let win_r = ((radius.top_left as f64 / s.max(1e-4))
                .min(win_size.w.min(win_size.h) * 0.5)) as f32;
            render_thumbnail(
                ctx.r(),
                scale,
                window.mapped,
                preview,
                alpha,
                CornerRadius::from(win_r),
                push,
            );
        }

        let glass_uniforms = Rc::new([
            Uniform::new("geo_size", [preview.size.w as f32, preview.size.h as f32]),
            Uniform::new("corner_radius", <[f32; 4]>::from(radius)),
            Uniform::new("fill_color", [0.06f32, 0.08, 0.12, 0.85]),
            Uniform::new(
                "border_color",
                config.selection.hover_color.to_array_unpremul(),
            ),
            Uniform::new(
                "accent_color",
                config.selection.hover_color.to_array_unpremul(),
            ),
            Uniform::new("highlight", 0.9f32),
            Uniform::new("divider_y", 0.0f32),
            Uniform::new("scrim_alpha", 0.0f32),
        ]);
        let elem = ShaderRenderElement::new(
            ProgramType::PickerGlass,
            preview.size,
            None,
            scale as f32,
            alpha,
            glass_uniforms,
            HashMap::new(),
            Kind::Unspecified,
        )
        .with_location(preview.loc);
        push(WindowPickerUiRenderElement::Card(elem));
    }

    #[allow(clippy::too_many_arguments)]
    fn render_hud<R: NiriRenderer>(
        &self,
        renderer: &mut GlesRenderer,
        hud_rect: Rectangle<f64, Logical>,
        scale: f64,
        alpha: f32,
        backdrop_animating: bool,
        session: &WindowPickerSession,
        active_title: &str,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let hud_rect = snap_rect_to_px(hud_rect, scale);
        let width_px = to_physical_precise_round::<i32>(scale, hud_rect.size.w).max(1);
        let height_px = to_physical_precise_round::<i32>(scale, hud_rect.size.h).max(1);
        let scope_num = match session.scope {
            WindowPickerScope::All => 0,
            WindowPickerScope::Workspace => 1,
            WindowPickerScope::Output => 2,
        };
        let key = HudCacheKey {
            width: width_px,
            height: height_px,
            scale_x100: (scale * 100.).round() as i32,
            scope: scope_num,
            counts: session.scope_counts(),
            search_mode: session.search_mode,
            search_query: session.search_query.clone(),
            prefix: session.prefix,
            active_title: truncate_title(active_title),
            quick_look: session.quick_look.is_some(),
        };

        let mut hud_cache = self.hud_cache.borrow_mut();
        if hud_cache.last_key.as_ref() != Some(&key) {
            hud_cache.texture = generate_hud_texture(
                renderer,
                &key,
                scale,
                config.selection.hover_color,
                &config.card.info,
            )
            .ok();
            hud_cache.last_key = Some(key);
        }

        if let Some(tex) = &hud_cache.texture {
            let elem = TextureRenderElement::from_texture_buffer(
                tex.clone(),
                hud_rect.loc,
                alpha,
                None,
                Some(hud_rect.size),
                Kind::Unspecified,
            );
            push(WindowPickerUiRenderElement::Label(
                PrimaryGpuTextureRenderElement(elem),
            ));
        }

        let radius = CornerRadius::from((hud_rect.size.h * 0.5) as f32);
        let glass_uniforms = Rc::new([
            Uniform::new("geo_size", [hud_rect.size.w as f32, hud_rect.size.h as f32]),
            Uniform::new("corner_radius", <[f32; 4]>::from(radius)),
            // Pure frosted blur HUD bar — zero background color fill.
            Uniform::new("fill_color", [0.0f32, 0.0, 0.0, 0.0]),
            Uniform::new("border_color", [1.0f32, 1.0, 1.0, 0.20]),
            Uniform::new(
                "accent_color",
                config.selection.hover_color.to_array_unpremul(),
            ),
            Uniform::new(
                "highlight",
                if session.search_mode {
                    0.85f32
                } else {
                    0.15f32
                },
            ),
            Uniform::new("divider_y", 0.0f32),
            Uniform::new("scrim_alpha", 0.0f32),
        ]);
        let glass = hud_cache.glass_elem.get_or_insert_with(|| {
            ShaderRenderElement::empty(ProgramType::PickerGlass, Kind::Unspecified)
        });
        glass.update(
            hud_rect.size,
            None,
            scale as f32,
            alpha,
            glass_uniforms,
            HashMap::new(),
        );
        push(WindowPickerUiRenderElement::Card(
            glass.clone().with_location(hud_rect.loc),
        ));

        let margin = 28.;
        let shadow_size = hud_rect.size + Size::from((margin, margin)).upscale(2.);
        let shadow = hud_cache
            .shadow_elem
            .get_or_insert_with(ShadowRenderElement::empty);
        shadow.update(
            shadow_size,
            Rectangle::new(Point::from((margin, margin)), hud_rect.size),
            Color::from_rgba8_unpremul(0, 0, 0, 255),
            12.,
            radius,
            scale as f32,
            Rectangle::new(Point::from((margin, margin)), hud_rect.size),
            radius,
            0.24 * alpha,
        );
        push(WindowPickerUiRenderElement::Shadow(
            shadow
                .clone()
                .with_location(hud_rect.loc - Point::from((margin, margin))),
        ));

        if alpha > 0.01 {
            let (effect, last_rect) = hud_cache
                .blur_elem
                .get_or_insert_with(|| (FramebufferEffect::new(), Rectangle::default()));
            if *last_rect != hud_rect || backdrop_animating || alpha < 0.999 {
                effect.damage();
                *last_rect = hud_rect;
            }
            let params = RenderParams {
                geometry: hud_rect,
                subregion: None,
                clip: Some((hud_rect, radius)),
                scale,
            };
            let blur = ui_surface_blur_options(config, alpha);
            let elem = effect.render(None, params, Some(blur), 0., 1., None, None);
            push(WindowPickerUiRenderElement::FramebufferEffect(elem));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_group_headers<R: NiriRenderer>(
        &self,
        renderer: &mut GlesRenderer,
        headers: &[GroupHeaderPlacement],
        scale: f64,
        alpha: f32,
        backdrop_animating: bool,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let mut cache = self.group_cache.borrow_mut();
        if (cache.scale - scale).abs() > 1e-4 {
            cache.scale = scale;
            cache.textures.clear();
        }

        for header in headers {
            let rect = snap_rect_to_px(header.rect, scale);
            let width_px = to_physical_precise_round::<i32>(scale, rect.size.w).max(1);
            let height_px = to_physical_precise_round::<i32>(scale, rect.size.h).max(1);
            let key = (header.title.clone(), header.count, width_px, height_px);
            let tex = cache
                .textures
                .entry(key)
                .or_insert_with(|| {
                    generate_group_header_texture(
                        renderer,
                        &header.title,
                        header.count,
                        rect.size,
                        scale,
                        &config.card.info,
                    )
                    .ok()
                })
                .clone();

            if let Some(tex) = tex {
                let elem = TextureRenderElement::from_texture_buffer(
                    tex,
                    rect.loc,
                    alpha,
                    None,
                    Some(rect.size),
                    Kind::Unspecified,
                );
                push(WindowPickerUiRenderElement::Label(
                    PrimaryGpuTextureRenderElement(elem),
                ));
            }

            let radius = CornerRadius::from((rect.size.h * 0.5) as f32);
            let uniforms = Rc::new([
                Uniform::new("geo_size", [rect.size.w as f32, rect.size.h as f32]),
                Uniform::new("corner_radius", <[f32; 4]>::from(radius)),
                // Pure frosted blur group header — zero background color fill.
                Uniform::new("fill_color", [0.0f32, 0.0, 0.0, 0.0]),
                Uniform::new("border_color", [1.0f32, 1.0, 1.0, 0.18]),
                Uniform::new(
                    "accent_color",
                    config.selection.hover_color.to_array_unpremul(),
                ),
                Uniform::new("highlight", 0.0f32),
                Uniform::new("divider_y", 0.0f32),
                Uniform::new("scrim_alpha", 0.0f32),
            ]);
            let glass = cache
                .glass_elems
                .entry(header.title.clone())
                .or_insert_with(|| {
                    ShaderRenderElement::empty(ProgramType::PickerGlass, Kind::Unspecified)
                });
            glass.update(
                rect.size,
                None,
                scale as f32,
                alpha,
                uniforms,
                HashMap::new(),
            );
            push(WindowPickerUiRenderElement::Card(
                glass.clone().with_location(rect.loc),
            ));

            if alpha > 0.01 {
                let (effect, last_rect) = cache
                    .blur_elems
                    .entry(header.title.clone())
                    .or_insert_with(|| (FramebufferEffect::new(), Rectangle::default()));
                if *last_rect != rect || backdrop_animating || alpha < 0.999 {
                    effect.damage();
                    *last_rect = rect;
                }
                let params = RenderParams {
                    geometry: rect,
                    subregion: None,
                    clip: Some((rect, radius)),
                    scale,
                };
                let blur = ui_surface_blur_options(config, alpha);
                let elem = effect.render(None, params, Some(blur), 0., 1., None, None);
                push(WindowPickerUiRenderElement::FramebufferEffect(elem));
            }
        }
    }

    fn render_backdrop<R: NiriRenderer>(
        &self,
        output: &Output,
        size: Size<f64, Logical>,
        progress: f64,
        closing: bool,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let backdrop = config.backdrop;
        let mut backdrops = self.backdrops.borrow_mut();
        let state = backdrops.entry(output.clone()).or_default();

        let progress = if closing {
            let fast = (progress * 1.8).clamp(0., 1.);
            if progress <= 0.02 {
                0.
            } else {
                fast
            }
        } else {
            progress.clamp(0., 1.)
        };

        let dim_alpha = ((1. - backdrop.brightness) * progress).clamp(0., 1.) as f32;
        state.dim.update(size, Color32F::new(0., 0., 0., dim_alpha));
        if dim_alpha > 0. {
            push(WindowPickerUiRenderElement::SolidColor(
                SolidColorRenderElement::from_buffer(
                    &state.dim,
                    Point::new(0., 0.),
                    1.,
                    Kind::Unspecified,
                ),
            ));
        }

        let mut tint = backdrop.color;
        tint.a *= progress as f32;
        state
            .tint
            .update(size, Color32F::from(tint.to_array_premul()));
        if tint.a > 0. {
            push(WindowPickerUiRenderElement::SolidColor(
                SolidColorRenderElement::from_buffer(
                    &state.tint,
                    Point::new(0., 0.),
                    1.,
                    Kind::Unspecified,
                ),
            ));
        }

        let effect_needed = (backdrop.blur.on || backdrop.saturation != 1.) && progress > 0.001;
        if effect_needed {
            if progress < 1. {
                state.framebuffer_effect.damage();
            }
            let params = RenderParams {
                geometry: Rectangle::from_size(size),
                subregion: None,
                clip: Some((Rectangle::from_size(size), CornerRadius::default())),
                scale: output.current_scale().fractional_scale(),
            };
            let blur = backdrop.blur.on.then_some(BlurOptions {
                passes: backdrop.blur.passes,
                offset: (backdrop.blur.offset * progress).max(0.25),
            });
            let saturation = 1. + (backdrop.saturation - 1.) * progress;
            let effect = state.framebuffer_effect.render(
                None,
                params,
                blur,
                0.,
                saturation as f32,
                None,
                None,
            );
            push(WindowPickerUiRenderElement::FramebufferEffect(effect));
        }
    }
}

fn snap_rect_to_px(rect: Rectangle<f64, Logical>, scale: f64) -> Rectangle<f64, Logical> {
    if scale <= 0. {
        return rect;
    }
    let left = (rect.loc.x * scale).round() / scale;
    let top = (rect.loc.y * scale).round() / scale;
    let right = ((rect.loc.x + rect.size.w) * scale).round() / scale;
    let bottom = ((rect.loc.y + rect.size.h) * scale).round() / scale;
    Rectangle::new(
        Point::from((left, top)),
        Size::from((
            (right - left).max(1. / scale),
            (bottom - top).max(1. / scale),
        )),
    )
}

fn ui_surface_blur_options(config: &niri_config::WindowPicker, alpha: f32) -> BlurOptions {
    let (passes, offset) =
        if config.card.blur.on && config.card.blur.passes > 0 && config.card.blur.offset > 0. {
            (config.card.blur.passes, config.card.blur.offset)
        } else if config.backdrop.blur.on
            && config.backdrop.blur.passes > 0
            && config.backdrop.blur.offset > 0.
        {
            (config.backdrop.blur.passes, config.backdrop.blur.offset)
        } else {
            (3, 3.)
        };
    BlurOptions {
        passes,
        offset: (offset * f64::from(alpha)).max(0.25),
    }
}

fn measure_group_header_width(title: &str, count: usize, font_name: &str, max_w: f64) -> f64 {
    let text = format!("{title} · {count}");
    let measured = ImageSurface::create(cairo::Format::ARgb32, 0, 0)
        .ok()
        .and_then(|dummy| cairo::Context::new(&dummy).ok())
        .map(|cr| {
            let mut font = FontDescription::from_string(font_name);
            font.set_weight(pango::Weight::Semibold);
            font.set_absolute_size(11.5 * f64::from(pango::SCALE));
            let layout = pangocairo::functions::create_layout(&cr);
            layout.context().set_round_glyph_positions(false);
            layout.set_single_paragraph_mode(true);
            layout.set_font_description(Some(&font));
            layout.set_text(&text);
            f64::from(layout.pixel_size().0)
        });
    let text_w = measured.unwrap_or_else(|| {
        let title_w: f64 = title
            .chars()
            .map(|c| if c.is_ascii() { 8.5 } else { 13.5 })
            .sum();
        let count_w = count.to_string().len() as f64 * 8.5;
        title_w + count_w + 18.
    });
    // Snug horizontal padding (14px left + 14px right) so the count is never truncated.
    (text_w + 28.).ceil().clamp(68., 360.).min(max_w)
}

struct RenderedWindow<'a> {
    entry: &'a PickerEntry,
    mapped: &'a Mapped,
    texture: Option<PickerTexture>,
    label_size: Size<f64, Logical>,
}

fn picker_windows<'a>(
    layout: &'a Layout<Mapped>,
    session: &'a WindowPickerSession,
    label_config: &WindowPickerLabel,
) -> Vec<RenderedWindow<'a>> {
    let by_id: HashMap<_, _> = layout
        .windows()
        .map(|(_, mapped)| (mapped.id(), mapped))
        .collect();

    session
        .visible_entries()
        .into_iter()
        .filter_map(|entry| {
            let mapped = by_id.get(&entry.id).copied()?;
            (mapped.size().w > 0 && mapped.size().h > 0).then(|| RenderedWindow {
                entry,
                mapped,
                texture: None,
                label_size: estimated_label_size(&entry.label, label_config),
            })
        })
        .collect()
}

fn grid_inputs(windows: &[RenderedWindow<'_>]) -> Vec<GridInput> {
    windows
        .iter()
        .map(|window| GridInput {
            window_size: window.mapped.size().to_f64(),
            label_size: window.label_size,
        })
        .collect()
}

fn thumb_corner_radius(mapped: &Mapped) -> CornerRadius {
    if mapped.sizing_mode().is_normal() {
        mapped.geometry_corner_radius()
    } else {
        CornerRadius::default()
    }
}

fn thumbnail_clip_radius(
    mapped: &Mapped,
    preview_size: Size<f64, Logical>,
    card_radius: f32,
    has_info: bool,
    config: &niri_config::WindowPicker,
) -> CornerRadius {
    if config.card.on {
        // `ClippedSurfaceRenderElement` clips in unscaled window coordinates BEFORE
        // `RescaleRenderElement` scales by `preview_size / win_size`, so scale `card_radius` into
        // window space so the final on-screen corner radius matches the card blur & rim exactly.
        let win_size = mapped.size().to_f64();
        let sx = preview_size.w / win_size.w.max(1.);
        let sy = preview_size.h / win_size.h.max(1.);
        let inv_s = 1.0 / (sx * sy).sqrt().max(1e-4) as f32;
        let max_r = (win_size.w.min(win_size.h) * 0.5) as f32;
        let r = (card_radius * inv_s).min(max_r);
        let bottom_r = if has_info { 0. } else { r };
        CornerRadius {
            top_left: r,
            top_right: r,
            bottom_right: bottom_r,
            bottom_left: bottom_r,
        }
    } else {
        thumb_corner_radius(mapped)
    }
}

fn render_thumbnail<R: NiriRenderer>(
    mut ctx: RenderCtx<R>,
    output_scale: f64,
    mapped: &Mapped,
    preview: Rectangle<f64, Logical>,
    alpha: f32,
    radius: CornerRadius,
    push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
) {
    let geo = Rectangle::from_size(mapped.size().to_f64());

    let scale = Scale::from(output_scale);
    let clip_shader = ClippedSurfaceRenderElement::shader(ctx.renderer).cloned();
    let clip = move |elem| match elem {
        LayoutElementRenderElement::Wayland(elem) => {
            if let Some(shader) = clip_shader.clone() {
                if ClippedSurfaceRenderElement::will_clip(&elem, scale, geo, radius) {
                    return PickerThumbnailRenderElement::ClippedSurface(
                        ClippedSurfaceRenderElement::new(elem, scale, geo, shader, radius),
                    );
                }
            }
            PickerThumbnailRenderElement::LayoutElement(LayoutElementRenderElement::Wayland(elem))
        }
        elem => PickerThumbnailRenderElement::LayoutElement(elem),
    };

    let thumb_scale = Scale {
        x: preview.size.w / geo.size.w,
        y: preview.size.h / geo.size.h,
    };
    mapped.render_normal(ctx.r(), Point::new(0., 0.), scale, alpha, &mut |elem| {
        let elem = RescaleRenderElement::from_element(clip(elem), Point::new(0, 0), thumb_scale);
        let elem = RelocateRenderElement::from_element(
            elem,
            preview.loc.to_physical_precise_round(output_scale),
            Relocate::Relative,
        );
        push(WindowPickerUiRenderElement::Thumbnail(elem));
    });
}

#[derive(Debug, Clone, Copy)]
struct GridInput {
    window_size: Size<f64, Logical>,
    label_size: Size<f64, Logical>,
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    card: Rectangle<f64, Logical>,
    preview: Rectangle<f64, Logical>,
    label: Rectangle<f64, Logical>,
    info: Rectangle<f64, Logical>,
    info_scale: f64,
}

#[derive(Debug)]
struct GridLayout {
    placements: Vec<Placement>,
}

#[derive(Debug)]
struct GridMetrics {
    col_widths: Vec<f64>,
    row_label_heights: Vec<f64>,
    row_card_heights: Vec<f64>,
    size: Size<f64, Logical>,
}

struct HudLayout {
    hud_rect: Option<Rectangle<f64, Logical>>,
    scope_chips: [(WindowPickerScope, Rectangle<f64, Logical>); 3],
    top_reserve: f64,
}

fn hud_layout(output: Size<f64, Logical>, show_hud: bool) -> HudLayout {
    if !show_hud || output.w < 360. || output.h < 240. {
        return HudLayout {
            hud_rect: None,
            scope_chips: [
                (WindowPickerScope::All, Rectangle::zero()),
                (WindowPickerScope::Workspace, Rectangle::zero()),
                (WindowPickerScope::Output, Rectangle::zero()),
            ],
            top_reserve: 0.,
        };
    }

    let hud_w = (output.w * 0.56).clamp(420., 760.).min(output.w - 32.);
    let hud_loc = Point::from(((output.w - hud_w) * 0.5, HUD_TOP_MARGIN));
    let hud_rect = Rectangle::new(hud_loc, Size::from((hud_w, HUD_HEIGHT)));

    let chip_h = HUD_HEIGHT - 12.;
    let chip_y = hud_loc.y + 6.;
    let chip_w = 56.;
    let mut x = hud_loc.x + 8.;
    let c_all = Rectangle::new(Point::from((x, chip_y)), Size::from((chip_w, chip_h)));
    x += chip_w + 4.;
    let c_ws = Rectangle::new(Point::from((x, chip_y)), Size::from((chip_w, chip_h)));
    x += chip_w + 4.;
    let c_out = Rectangle::new(Point::from((x, chip_y)), Size::from((chip_w, chip_h)));

    let top_reserve = HUD_TOP_MARGIN + HUD_HEIGHT + 12.;
    HudLayout {
        hud_rect: Some(hud_rect),
        scope_chips: [
            (WindowPickerScope::All, c_all),
            (WindowPickerScope::Workspace, c_ws),
            (WindowPickerScope::Output, c_out),
        ],
        top_reserve,
    }
}

fn compute_layout(
    output: Size<f64, Logical>,
    windows: &[RenderedWindow<'_>],
    config: &niri_config::WindowPicker,
) -> Option<ComputedLayout> {
    if windows.is_empty() || output.w <= 0. || output.h <= 0. {
        return None;
    }

    let HudLayout {
        hud_rect,
        scope_chips,
        top_reserve: hud_top_reserve,
    } = hud_layout(output, config.show_hud);

    let area_w = output.w * config.area_width.clamp(0., 1.);
    let area_h = (output.h * config.area_height.clamp(0., 1.) - hud_top_reserve * 0.5).max(80.);
    let area_origin_y = if config.show_hud {
        ((output.h - area_h) * 0.5).max(hud_top_reserve)
    } else {
        (output.h - area_h) * 0.5
    };
    let bounds = Rectangle::new(
        Point::from(((output.w - area_w) * 0.5, area_origin_y)),
        Size::from((
            area_w,
            (output.h - area_origin_y - 16.).min(area_h).max(60.),
        )),
    );

    if config.group_by != WindowPickerGroupBy::None {
        return compute_grouped_layout(bounds, windows, config, hud_rect, scope_chips);
    }

    let inputs = grid_inputs(windows);
    let layout = match config.layout {
        WindowPickerLayout::Justified if inputs.len() > 1 => {
            compute_justified_in_rect(bounds, &inputs, config)?
        }
        _ => compute_grid_in_rect(bounds, &inputs, config)?,
    };

    let placements = windows
        .iter()
        .map(|w| w.entry.id)
        .zip(layout.placements)
        .collect();

    Some(ComputedLayout {
        placements,
        group_headers: Vec::new(),
        hud_rect,
        scope_chips,
    })
}

fn compute_grouped_layout(
    bounds: Rectangle<f64, Logical>,
    windows: &[RenderedWindow<'_>],
    config: &niri_config::WindowPicker,
    hud_rect: Option<Rectangle<f64, Logical>>,
    scope_chips: [(WindowPickerScope, Rectangle<f64, Logical>); 3],
) -> Option<ComputedLayout> {
    // Partition windows into deterministically ordered groups (by workspace index or output name).
    let mut groups: HashMap<String, (usize, Vec<usize>)> = HashMap::new();
    for (idx, w) in windows.iter().enumerate() {
        let (key, sort_key) = match config.group_by {
            WindowPickerGroupBy::Workspace => {
                let label = if w.entry.workspace.is_empty() {
                    format!("Workspace {}", w.entry.workspace_idx + 1)
                } else {
                    w.entry.workspace.clone()
                };
                (label, w.entry.workspace_idx)
            }
            WindowPickerGroupBy::Output => {
                let label = if w.entry.output_name.is_empty() {
                    String::from("Output")
                } else {
                    w.entry.output_name.clone()
                };
                (label, 0)
            }
            WindowPickerGroupBy::None => (String::new(), 0),
        };
        groups
            .entry(key)
            .or_insert_with(|| (sort_key, Vec::new()))
            .1
            .push(idx);
    }

    let mut group_order: Vec<(usize, String)> = groups
        .iter()
        .map(|(k, (sort_key, _))| (*sort_key, k.clone()))
        .collect();
    group_order.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let grouped_inputs: Vec<(String, Vec<usize>, Vec<GridInput>)> = group_order
        .into_iter()
        .map(|(_, key)| {
            let indices = groups.remove(&key).map_or_else(Vec::new, |(_, v)| v);
            let inputs = indices
                .iter()
                .map(|&i| GridInput {
                    window_size: windows[i].mapped.size().to_f64(),
                    label_size: windows[i].label_size,
                })
                .collect();
            (key, indices, inputs)
        })
        .filter(|(_, indices, _)| !indices.is_empty())
        .collect();

    let pure_groups: Vec<(String, Vec<GridInput>)> = grouped_inputs
        .iter()
        .map(|(k, _, ins)| (k.clone(), ins.clone()))
        .collect();

    let (group_headers, grouped_placements) =
        compute_grouped_geometry(bounds, &pure_groups, config)?;
    let mut placements = Vec::with_capacity(windows.len());
    for ((_, indices, _), group_pls) in grouped_inputs.into_iter().zip(grouped_placements) {
        for (win_idx, pl) in indices.into_iter().zip(group_pls) {
            placements.push((windows[win_idx].entry.id, pl));
        }
    }

    Some(ComputedLayout {
        placements,
        group_headers,
        hud_rect,
        scope_chips,
    })
}

/// Packs a single group's windows into balanced equal-height rows at a unified `target_row_h`.
/// Guarantees exact true aspect ratios and identical visual height across 1-window and N-window
/// groups!
fn pack_group_justified(
    inputs: &[GridInput],
    target_row_h: f64,
    max_w: f64,
    gap: f64,
    info_height: f64,
    chrome_scale: f64,
) -> (Vec<JustifiedRow>, f64, f64) {
    let count = inputs.len();
    let aspects: Vec<f64> = inputs
        .iter()
        .map(|i| (i.window_size.w / i.window_size.h.max(1.)).max(0.1))
        .collect();
    let max_label_top = inputs
        .iter()
        .map(|i| i.label_size.h * chrome_scale * (1. - LABEL_OVERLAP))
        .fold(0., f64::max);

    // Determine how many rows are needed at `target_row_h` via greedy wrap.
    let mut num_rows = 1usize;
    let mut row_w = 0.;
    for (idx, &a) in aspects.iter().enumerate() {
        let w = target_row_h * a;
        if idx > 0 && row_w > 0. && row_w + gap + w > max_w - 1e-4 {
            num_rows += 1;
            row_w = w;
        } else {
            if row_w > 0. {
                row_w += gap;
            }
            row_w += w;
        }
    }

    // Balance row splits across `num_rows` by aspect ratio so rows don't leave a lonely orphan.
    let total_aspect: f64 = aspects.iter().sum();
    let target_row_aspect = total_aspect / num_rows as f64;
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(num_rows);
    let mut start = 0;
    let mut acc = 0.;
    for (i, &a) in aspects.iter().enumerate() {
        acc += a;
        let remaining_rows = num_rows.saturating_sub(spans.len() + 1);
        if remaining_rows > 0 && acc >= target_row_aspect * 0.92 && i + 1 < count {
            spans.push((start, i + 1));
            start = i + 1;
            acc = 0.;
        }
    }
    if start < count {
        spans.push((start, count));
    }

    let mut rows = Vec::with_capacity(spans.len());
    let mut total_h = 0.;
    for (r_idx, &(s, e)) in spans.iter().enumerate() {
        let n = e - s;
        let row_aspect: f64 = aspects[s..e].iter().sum();
        let avail_w = (max_w - gap * n.saturating_sub(1) as f64 - 1e-4).max(1.);
        let h = target_row_h.min(avail_w / row_aspect.max(0.1));
        rows.push(JustifiedRow {
            start: s,
            end: e,
            height: h,
        });
        if r_idx > 0 {
            total_h += gap;
        }
        total_h += max_label_top + h + info_height;
    }

    (rows, total_h, max_label_top)
}

/// Packs a single group's windows into a balanced grid at a unified `window_scale`.
/// Returns `None` if even 1 column exceeds `max_w`.
fn pack_group_grid(
    inputs: &[GridInput],
    window_scale: f64,
    max_w: f64,
    chrome_scale: f64,
    gap: f64,
    label_gap: f64,
    info_height: f64,
) -> Option<(usize, GridMetrics)> {
    let count = inputs.len();
    let mut max_fit_cols = 0usize;
    for cols in 1..=count {
        let m = grid_metrics(
            inputs,
            cols,
            window_scale,
            chrome_scale,
            gap,
            label_gap,
            info_height,
        );
        if m.size.w <= max_w - 1e-4 {
            max_fit_cols = cols;
        } else if cols == 1 {
            return None;
        } else {
            break;
        }
    }
    if max_fit_cols == 0 {
        return None;
    }
    // Balance row lengths while keeping the minimum row count.
    let min_rows = count.div_ceil(max_fit_cols);
    let balanced_cols = count.div_ceil(min_rows).clamp(1, max_fit_cols);
    let metrics = grid_metrics(
        inputs,
        balanced_cols,
        window_scale,
        chrome_scale,
        gap,
        label_gap,
        info_height,
    );
    if metrics.size.w <= max_w - 1e-4 {
        Some((balanced_cols, metrics))
    } else {
        let fallback = grid_metrics(
            inputs,
            max_fit_cols,
            window_scale,
            chrome_scale,
            gap,
            label_gap,
            info_height,
        );
        Some((max_fit_cols, fallback))
    }
}

/// Computes a unified-scale grouped layout so every workspace/output group uses the exact same
/// visual scale (in `grid` mode) or target row height (in `justified` mode), eliminating tiny or
/// distorted cards in 1-window workspaces alongside busy workspaces.
fn compute_grouped_geometry(
    bounds: Rectangle<f64, Logical>,
    groups: &[(String, Vec<GridInput>)],
    config: &niri_config::WindowPicker,
) -> Option<(Vec<GroupHeaderPlacement>, Vec<Vec<Placement>>)> {
    let total_windows: usize = groups.iter().map(|(_, ins)| ins.len()).sum();
    if groups.is_empty() || total_windows == 0 || bounds.size.w <= 0. || bounds.size.h <= 0. {
        return None;
    }

    let num_groups = groups.len();
    let chrome_scale = (52. / total_windows as f64).sqrt().clamp(0.25, 1.);
    let gap = config.gap.max(0.) * chrome_scale;
    let label_gap = (config.label.gap.max(0.) * 0.35) * chrome_scale;
    let info_height = if config.card.on {
        config.card.info.height.max(0.) * chrome_scale
    } else {
        0.
    };
    let max_scale = config.max_scale.clamp(0.001, 1.);
    let header_gap = 8.;
    let group_gap = GROUP_GAP.min((bounds.size.h * 0.04).max(10.));
    let headers_overhead = num_groups as f64 * (GROUP_HEADER_HEIGHT + header_gap)
        + num_groups.saturating_sub(1) as f64 * group_gap;
    let avail_h = (bounds.size.h - headers_overhead - 1e-4).max(24.);

    let mut group_headers = Vec::with_capacity(num_groups);
    let mut grouped_placements = Vec::with_capacity(num_groups);
    let mut cur_y = bounds.loc.y;

    match config.layout {
        WindowPickerLayout::Justified => {
            let max_row_h = groups
                .iter()
                .flat_map(|(_, ins)| ins.iter())
                .map(|inp| inp.window_size.h * max_scale)
                .fold(0.0_f64, f64::max)
                .max(32.);

            let fits_justified = |row_h: f64| {
                let total_h: f64 = groups
                    .iter()
                    .map(|(_, ins)| {
                        pack_group_justified(
                            ins,
                            row_h,
                            bounds.size.w,
                            gap,
                            info_height,
                            chrome_scale,
                        )
                        .1
                    })
                    .sum();
                total_h <= avail_h
            };

            let row_h = if fits_justified(max_row_h) {
                max_row_h
            } else {
                let mut low = 8.0_f64;
                let mut high = max_row_h;
                for _ in 0..30 {
                    let mid = (low + high) * 0.5;
                    if fits_justified(mid) {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }
                low
            };

            for (title, ins) in groups {
                let header_w = measure_group_header_width(
                    title,
                    ins.len(),
                    &config.card.info.font,
                    bounds.size.w,
                );
                group_headers.push(GroupHeaderPlacement {
                    title: title.clone(),
                    count: ins.len(),
                    rect: Rectangle::new(
                        Point::from((bounds.loc.x, cur_y)),
                        Size::from((header_w, GROUP_HEADER_HEIGHT)),
                    ),
                });
                cur_y += GROUP_HEADER_HEIGHT + header_gap;

                let (rows, group_h, max_label_top) =
                    pack_group_justified(ins, row_h, bounds.size.w, gap, info_height, chrome_scale);
                let aspects: Vec<f64> = ins
                    .iter()
                    .map(|i| (i.window_size.w / i.window_size.h.max(1.)).max(0.1))
                    .collect();

                let mut pls = Vec::with_capacity(ins.len());
                let mut y = cur_y;
                for row in rows {
                    let n = row.end - row.start;
                    let rw: f64 = (row.start..row.end)
                        .map(|i| row.height * aspects[i])
                        .sum::<f64>()
                        + n.saturating_sub(1) as f64 * gap;
                    let mut x = bounds.loc.x + ((bounds.size.w - rw) * 0.5).max(0.);
                    let card_y = y + max_label_top;

                    for i in row.start..row.end {
                        let preview_size = Size::from((row.height * aspects[i], row.height));
                        let card_size = Size::from((preview_size.w, preview_size.h + info_height));
                        let card_loc = Point::from((x, card_y));
                        let card = Rectangle::new(card_loc, card_size);
                        let preview = Rectangle::new(card_loc, preview_size);
                        let info = Rectangle::new(
                            card_loc + Point::from((0., preview_size.h)),
                            Size::from((preview_size.w, info_height)),
                        );
                        let label_size = ins[i].label_size.upscale(chrome_scale);
                        let label_loc = Point::new(
                            card.loc.x + (card.size.w - label_size.w) / 2.,
                            card.loc.y - label_size.h * (1. - LABEL_OVERLAP),
                        );
                        pls.push(Placement {
                            card,
                            preview,
                            info,
                            info_scale: chrome_scale,
                            label: Rectangle::new(label_loc, label_size),
                        });
                        x += preview_size.w + gap;
                    }
                    y += max_label_top + row.height + info_height + gap;
                }

                grouped_placements.push(pls);
                cur_y += group_h + group_gap;
            }
        }
        WindowPickerLayout::Grid => {
            let fits_grid = |scale: f64| {
                let mut total_h = 0.;
                for (_, ins) in groups {
                    let Some((_, m)) = pack_group_grid(
                        ins,
                        scale,
                        bounds.size.w,
                        chrome_scale,
                        gap,
                        label_gap,
                        info_height,
                    ) else {
                        return false;
                    };
                    total_h += m.size.h;
                }
                total_h <= avail_h
            };

            let window_scale = if fits_grid(max_scale) {
                max_scale
            } else {
                let mut low = 0.001_f64;
                let mut high = max_scale;
                for _ in 0..30 {
                    let mid = (low + high) * 0.5;
                    if fits_grid(mid) {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }
                low
            };

            for (title, ins) in groups {
                let header_w = measure_group_header_width(
                    title,
                    ins.len(),
                    &config.card.info.font,
                    bounds.size.w,
                );
                group_headers.push(GroupHeaderPlacement {
                    title: title.clone(),
                    count: ins.len(),
                    rect: Rectangle::new(
                        Point::from((bounds.loc.x, cur_y)),
                        Size::from((header_w, GROUP_HEADER_HEIGHT)),
                    ),
                });
                cur_y += GROUP_HEADER_HEIGHT + header_gap;

                let (cols, metrics) = pack_group_grid(
                    ins,
                    window_scale,
                    bounds.size.w,
                    chrome_scale,
                    gap,
                    label_gap,
                    info_height,
                )
                .unwrap_or_else(|| {
                    (
                        1,
                        grid_metrics(
                            ins,
                            1,
                            window_scale,
                            chrome_scale,
                            gap,
                            label_gap,
                            info_height,
                        ),
                    )
                });

                let origin_x = bounds.loc.x + ((bounds.size.w - metrics.size.w) * 0.5).max(0.);
                let mut col_x = Vec::with_capacity(metrics.col_widths.len());
                let mut x = origin_x;
                for width in &metrics.col_widths {
                    col_x.push(x);
                    x += width + gap;
                }
                let mut row_y = Vec::with_capacity(metrics.row_card_heights.len());
                let mut y = cur_y;
                for row in 0..metrics.row_card_heights.len() {
                    row_y.push(y);
                    y += metrics.row_label_heights[row]
                        + label_gap
                        + metrics.row_card_heights[row]
                        + gap;
                }

                let mut pls = Vec::with_capacity(ins.len());
                for (idx, input) in ins.iter().enumerate() {
                    let col = idx % cols;
                    let row = idx / cols;
                    let preview_size = input.window_size.upscale(window_scale);
                    let card_size = Size::from((preview_size.w, preview_size.h + info_height));
                    let card_loc = Point::new(
                        col_x[col] + (metrics.col_widths[col] - card_size.w) / 2.,
                        row_y[row] + metrics.row_label_heights[row] + label_gap,
                    );
                    let card = Rectangle::new(card_loc, card_size);
                    let preview = Rectangle::new(card_loc, preview_size);
                    let info = Rectangle::new(
                        card_loc + Point::from((0., preview_size.h)),
                        Size::from((preview_size.w, info_height)),
                    );
                    let label_size = input.label_size.upscale(chrome_scale);
                    let label_loc = Point::new(
                        card.loc.x + (card.size.w - label_size.w) / 2.,
                        card.loc.y - label_size.h * (1. - LABEL_OVERLAP),
                    );
                    pls.push(Placement {
                        card,
                        preview,
                        info,
                        info_scale: chrome_scale,
                        label: Rectangle::new(label_loc, label_size),
                    });
                }

                grouped_placements.push(pls);
                cur_y += metrics.size.h + group_gap;
            }
        }
    }

    // Center the entire grouped stack vertically inside `bounds`.
    let content_bottom = (cur_y - group_gap).max(bounds.loc.y);
    let content_h = content_bottom - bounds.loc.y;
    let y_shift = ((bounds.size.h - content_h) * 0.5).max(0.);
    if y_shift > 0.5 {
        for h in &mut group_headers {
            h.rect.loc.y += y_shift;
        }
        for pls in &mut grouped_placements {
            for pl in pls {
                pl.card.loc.y += y_shift;
                pl.preview.loc.y += y_shift;
                pl.info.loc.y += y_shift;
                pl.label.loc.y += y_shift;
            }
        }
    }

    Some((group_headers, grouped_placements))
}

#[derive(Debug, Clone, Copy)]
struct JustifiedRow {
    start: usize,
    end: usize,
    height: f64,
}

/// Equal-height justified row layout: packs mixed aspect-ratio windows tightly in each row.
fn compute_justified_in_rect(
    bounds: Rectangle<f64, Logical>,
    inputs: &[GridInput],
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    if inputs.is_empty() || bounds.size.w <= 0. || bounds.size.h <= 0. {
        return None;
    }

    let count = inputs.len();
    let chrome_scale = (52. / count as f64).sqrt().clamp(0.25, 1.);
    let gap = config.gap.max(0.) * chrome_scale;
    let info_height = if config.card.on {
        config.card.info.height.max(0.) * chrome_scale
    } else {
        0.
    };
    let max_label_top = inputs
        .iter()
        .map(|i| i.label_size.h * chrome_scale * (1. - LABEL_OVERLAP))
        .fold(0., f64::max);
    let max_scale = config.max_scale.clamp(0.001, 1.);

    let aspects: Vec<f64> = inputs
        .iter()
        .map(|i| (i.window_size.w / i.window_size.h.max(1.)).max(0.1))
        .collect();
    let total_aspect: f64 = aspects.iter().sum();

    let max_rows = count.min(8);
    let mut best: Option<(Vec<JustifiedRow>, f64)> = None;

    for target_rows in 1..=max_rows {
        let target_row_aspect = total_aspect / target_rows as f64;
        let mut rows: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        let mut acc = 0.;
        for (i, &a) in aspects.iter().enumerate() {
            acc += a;
            let remaining_rows = target_rows.saturating_sub(rows.len() + 1);
            if remaining_rows > 0 && acc >= target_row_aspect * 0.92 && i + 1 < count {
                rows.push((start, i + 1));
                start = i + 1;
                acc = 0.;
            }
        }
        if start < count {
            rows.push((start, count));
        }

        let num_rows = rows.len();
        let fixed_h = num_rows as f64 * (info_height + max_label_top)
            + (num_rows.saturating_sub(1)) as f64 * gap;
        // Leave a sub-pixel safety margin so IEEE-754 summation never overflows `bounds`.
        let avail_preview_h = bounds.size.h - fixed_h - 1e-4;
        if avail_preview_h <= 10. {
            continue;
        }

        let mut row_heights = Vec::with_capacity(num_rows);
        let mut sum_h = 0.;
        let mut feasible = true;
        for &(s, e) in &rows {
            let n = e - s;
            let row_aspect: f64 = aspects[s..e].iter().sum();
            let avail_w = bounds.size.w - gap * n.saturating_sub(1) as f64 - 1e-4;
            if avail_w <= 10. {
                feasible = false;
                break;
            }
            let max_h_by_w = avail_w / row_aspect.max(0.1);
            // Use the row's largest scaled window height so a single small dialog doesn't
            // crush normal windows in the same equal-height row.
            let max_h_by_scale = inputs[s..e]
                .iter()
                .map(|inp| inp.window_size.h * max_scale)
                .fold(0.0_f64, f64::max)
                .max(40.);
            let h = max_h_by_w.min(max_h_by_scale);
            row_heights.push(JustifiedRow {
                start: s,
                end: e,
                height: h,
            });
            sum_h += h;
        }
        if !feasible {
            continue;
        }

        let vertical_shrink = (avail_preview_h / sum_h.max(1.)).min(1.);
        let mut area_score = 0.;
        for row in &mut row_heights {
            row.height *= vertical_shrink;
            for &a in &aspects[row.start..row.end] {
                area_score += row.height * (row.height * a);
            }
        }

        if best
            .as_ref()
            .is_none_or(|(_, best_area)| area_score > *best_area)
        {
            best = Some((row_heights, area_score));
        }
    }

    let (rows, _) = best?;
    let total_h: f64 = rows
        .iter()
        .map(|row| max_label_top + row.height + info_height)
        .sum::<f64>()
        + rows.len().saturating_sub(1) as f64 * gap;

    let mut placements = Vec::with_capacity(count);
    let mut y = bounds.loc.y + ((bounds.size.h - total_h) * 0.5).max(0.);

    for row in rows {
        let s = row.start;
        let e = row.end;
        let row_h = row.height;
        let n = e - s;
        let row_w: f64 =
            (s..e).map(|i| row_h * aspects[i]).sum::<f64>() + n.saturating_sub(1) as f64 * gap;
        let mut x = bounds.loc.x + ((bounds.size.w - row_w) * 0.5).max(0.);
        let card_y = y + max_label_top;

        for i in s..e {
            let preview_size = Size::from((row_h * aspects[i], row_h));
            let card_size = Size::from((preview_size.w, preview_size.h + info_height));
            let card_loc = Point::from((x, card_y));
            let card = Rectangle::new(card_loc, card_size);
            let preview = Rectangle::new(card_loc, preview_size);
            let info = Rectangle::new(
                card_loc + Point::from((0., preview_size.h)),
                Size::from((preview_size.w, info_height)),
            );
            let label_size = inputs[i].label_size.upscale(chrome_scale);
            let label_loc = Point::new(
                card.loc.x + (card.size.w - label_size.w) / 2.,
                card.loc.y - label_size.h * (1. - LABEL_OVERLAP),
            );
            placements.push(Placement {
                card,
                preview,
                info,
                info_scale: chrome_scale,
                label: Rectangle::new(label_loc, label_size),
            });
            x += preview_size.w + gap;
        }
        y += max_label_top + row_h + info_height + gap;
    }

    Some(GridLayout { placements })
}

#[cfg(test)]
fn compute_grid(
    output: Size<f64, Logical>,
    inputs: &[GridInput],
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    let area: Size<f64, Logical> = Size::from((
        output.w * config.area_width.clamp(0., 1.),
        output.h * config.area_height.clamp(0., 1.),
    ));
    let bounds = Rectangle::new(
        Point::from(((output.w - area.w) / 2., (output.h - area.h) / 2.)),
        area,
    );
    compute_grid_in_rect(bounds, inputs, config)
}

fn compute_grid_in_rect(
    bounds: Rectangle<f64, Logical>,
    inputs: &[GridInput],
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    let area = bounds.size;
    if inputs.is_empty() || area.w <= 0. || area.h <= 0. {
        return None;
    }

    if inputs.len() == 1 {
        return compute_single_window(bounds, inputs[0], config);
    }

    let count = inputs.len();
    let chrome_scale = (52. / count as f64).sqrt().clamp(0.2, 1.);
    let gap = config.gap.max(0.) * chrome_scale;
    // Account for LABEL_OVERLAP so top spacing matches the real protruding label height (fixes
    // vertical optical sag!).
    let label_gap = (config.label.gap.max(0.) * 0.35) * chrome_scale;
    let info_height = if config.card.on {
        config.card.info.height.max(0.) * chrome_scale
    } else {
        0.
    };
    let max_scale = config.max_scale.clamp(0.001, 1.);
    let aspect = (area.w / area.h).max(0.01);
    let max_cols = ((count as f64 * aspect).sqrt() * 2. + 2.).ceil() as usize;
    let max_cols = max_cols.clamp(1, count);

    let mut best: Option<(usize, f64, f64, GridMetrics)> = None;
    for cols in 1..=max_cols {
        let fits = |window_scale| {
            let metrics = grid_metrics(
                inputs,
                cols,
                window_scale,
                chrome_scale,
                gap,
                label_gap,
                info_height,
            );
            (
                metrics.size.w <= area.w && metrics.size.h <= area.h,
                metrics,
            )
        };

        let (scale, metrics) = if let (true, metrics) = fits(max_scale) {
            (max_scale, metrics)
        } else {
            let mut low = 0.;
            let mut high = max_scale;
            for _ in 0..28 {
                let mid = (low + high) / 2.;
                if fits(mid).0 {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            let (fits, metrics) = fits(low);
            if !fits || low < 0.0001 {
                continue;
            }
            (low, metrics)
        };

        let content_aspect = metrics.size.w / metrics.size.h.max(0.001);
        let aspect_error = (content_aspect / aspect).ln().abs();
        let replace = best.as_ref().is_none_or(|(_, best_scale, best_error, _)| {
            scale > *best_scale + 0.0001
                || ((scale - *best_scale).abs() <= 0.0001 && aspect_error < *best_error)
        });
        if replace {
            best = Some((cols, scale, aspect_error, metrics));
        }
    }

    let (cols, window_scale, _, metrics) = best?;
    let origin: Point<f64, Logical> = Point::new(
        bounds.loc.x + (area.w - metrics.size.w) / 2.,
        bounds.loc.y + (area.h - metrics.size.h) / 2.,
    );

    let mut col_x = Vec::with_capacity(metrics.col_widths.len());
    let mut x = origin.x;
    for width in &metrics.col_widths {
        col_x.push(x);
        x += width + gap;
    }

    let mut row_y = Vec::with_capacity(metrics.row_card_heights.len());
    let mut y = origin.y;
    for row in 0..metrics.row_card_heights.len() {
        row_y.push(y);
        y += metrics.row_label_heights[row] + label_gap + metrics.row_card_heights[row] + gap;
    }

    let mut placements = Vec::with_capacity(inputs.len());
    for (idx, input) in inputs.iter().enumerate() {
        let col = idx % cols;
        let row = idx / cols;
        let preview_size = input.window_size.upscale(window_scale);
        let card_size = Size::from((preview_size.w, preview_size.h + info_height));
        let card_loc = Point::new(
            col_x[col] + (metrics.col_widths[col] - card_size.w) / 2.,
            row_y[row] + metrics.row_label_heights[row] + label_gap,
        );
        let card = Rectangle::new(card_loc, card_size);
        let preview = Rectangle::new(card_loc, preview_size);
        let info = Rectangle::new(
            card_loc + Point::from((0., preview_size.h)),
            Size::from((preview_size.w, info_height)),
        );
        let label_size = input.label_size.upscale(chrome_scale);
        let label_loc = Point::new(
            card.loc.x + (card.size.w - label_size.w) / 2.,
            card.loc.y - label_size.h * (1. - LABEL_OVERLAP),
        );
        placements.push(Placement {
            card,
            preview,
            info,
            info_scale: chrome_scale,
            label: Rectangle::new(label_loc, label_size),
        });
    }

    Some(GridLayout { placements })
}

fn compute_single_window(
    bounds: Rectangle<f64, Logical>,
    input: GridInput,
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    let area = bounds.size;
    let label_overflow = input.label_size.h * (1. - LABEL_OVERLAP);
    let info_height = if config.card.on {
        config.card.info.height.max(0.)
    } else {
        0.
    };
    let available_height = area.h - 2. * (label_overflow + 4.);
    let max_preview_height = available_height - info_height;
    if max_preview_height <= 1. || input.window_size.w <= 0. || input.window_size.h <= 0. {
        return None;
    }

    let scale = config
        .max_scale
        .clamp(0.001, 1.)
        .min(area.w / input.window_size.w)
        .min(max_preview_height / input.window_size.h);
    let preview_size = input.window_size.upscale(scale);
    let card_size = Size::from((preview_size.w, preview_size.h + info_height));
    let card_loc = Point::new(
        bounds.loc.x + (area.w - card_size.w) / 2.,
        bounds.loc.y + (area.h - card_size.h) / 2.,
    );
    let preview = Rectangle::new(card_loc, preview_size);
    let info = Rectangle::new(
        card_loc + Point::from((0., preview_size.h)),
        Size::from((preview_size.w, info_height)),
    );
    let label_loc = Point::new(
        card_loc.x + (card_size.w - input.label_size.w) / 2.,
        card_loc.y - label_overflow,
    );

    Some(GridLayout {
        placements: vec![Placement {
            card: Rectangle::new(card_loc, card_size),
            preview,
            info,
            info_scale: 1.,
            label: Rectangle::new(label_loc, input.label_size),
        }],
    })
}

fn grid_metrics(
    inputs: &[GridInput],
    cols: usize,
    window_scale: f64,
    chrome_scale: f64,
    gap: f64,
    label_gap: f64,
    info_height: f64,
) -> GridMetrics {
    let rows = inputs.len().div_ceil(cols);
    let mut col_widths = vec![0_f64; cols];
    let mut row_label_heights = vec![0_f64; rows];
    let mut row_card_heights = vec![0_f64; rows];

    for (idx, input) in inputs.iter().enumerate() {
        let col = idx % cols;
        let row = idx / cols;
        col_widths[col] = col_widths[col]
            .max((input.window_size.w * window_scale).max(input.label_size.w * chrome_scale));
        // Reserve only the portion of the keycap that protrudes above the card top edge!
        row_label_heights[row] =
            row_label_heights[row].max(input.label_size.h * chrome_scale * (1. - LABEL_OVERLAP));
        row_card_heights[row] =
            row_card_heights[row].max(input.window_size.h * window_scale + info_height);
    }

    let width = col_widths.iter().sum::<f64>() + gap * cols.saturating_sub(1) as f64;
    let height = row_label_heights.iter().sum::<f64>()
        + row_card_heights.iter().sum::<f64>()
        + label_gap * rows as f64
        + gap * rows.saturating_sub(1) as f64;

    GridMetrics {
        col_widths,
        row_label_heights,
        row_card_heights,
        size: Size::from((width, height)),
    }
}

/// Deterministic label generator (no random shuffling — preserves muscle memory!).
fn make_labels(count: usize, configured_letters: &str) -> Vec<String> {
    let mut custom = Vec::new();
    for letter in configured_letters.chars() {
        if letter.is_ascii_alphabetic() {
            let letter = letter.to_ascii_uppercase();
            if !custom.contains(&letter) {
                custom.push(letter);
            }
        }
    }

    if custom.is_empty() {
        if count <= 26 {
            return (0..count)
                .map(|idx| char::from(b'A' + idx as u8).to_string())
                .collect();
        }

        return (0..count.min(MAX_WINDOWS))
            .map(|idx| {
                let first = char::from(b'A' + (idx / 26) as u8);
                let second = char::from(b'A' + (idx % 26) as u8);
                format!("{first}{second}")
            })
            .collect();
    }

    if count <= custom.len() {
        return custom
            .into_iter()
            .take(count)
            .map(|letter| letter.to_string())
            .collect();
    }

    let mut pool = custom;
    if pool.len() * pool.len() < count {
        for letter in 'A'..='Z' {
            if !pool.contains(&letter) {
                pool.push(letter);
            }
        }
    }

    let mut labels: Vec<String> = pool
        .iter()
        .flat_map(|first| pool.iter().map(move |second| format!("{first}{second}")))
        .collect();
    labels.truncate(count.min(MAX_WINDOWS));
    labels
}

fn letter_from_keysym(keysym: Keysym) -> Option<char> {
    let raw = keysym.raw();
    if (Keysym::a.raw()..=Keysym::z.raw()).contains(&raw) {
        return char::from_u32(u32::from(b'A') + raw - Keysym::a.raw());
    }
    if (Keysym::A.raw()..=Keysym::Z.raw()).contains(&raw) {
        return char::from_u32(u32::from(b'A') + raw - Keysym::A.raw());
    }
    None
}

fn estimated_label_size(label: &str, config: &WindowPickerLabel) -> Size<f64, Logical> {
    Size::from((
        config.size * 0.7 * label.len() as f64 + config.padding_x * 2. + 8.,
        config.size * 1.25 + config.padding_y * 2. + 8.,
    ))
}

fn lighten(color: Color, amount: f32) -> Color {
    let mix = |c: f32| c + (1. - c) * amount;
    Color::new_unpremul(mix(color.r), mix(color.g), mix(color.b), color.a)
}

fn darken(color: Color, amount: f32) -> Color {
    let mix = |c: f32| c * (1. - amount);
    Color::new_unpremul(mix(color.r), mix(color.g), mix(color.b), color.a)
}

fn hex(color: Color) -> String {
    let channel = |c: f32| (c.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

fn generate_info_texture(
    renderer: &mut GlesRenderer,
    icons: &mut AppIconCache,
    key: &InfoKey,
    scale: f64,
    font_scale: f64,
    show_icons: bool,
    config: &WindowPickerInfo,
) -> anyhow::Result<PickerTexture> {
    let _span = tracy_client::span!("window_picker::generate_info_texture");

    let width = key.width.max(1);
    let height = key.height.max(1);
    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0., 0., 0., 0.);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    let s = scale * font_scale;
    let padding_x = config.padding_x * s;
    let padding_y = config.padding_y * s;
    let gap = 8. * s;
    let logical_h = f64::from(height) / scale.max(0.1);
    let compact = logical_h < 34.;

    let make_font = |size: f64, bold: bool| {
        let mut font = FontDescription::from_string(&config.font);
        if bold {
            font.set_weight(pango::Weight::Semibold);
        }
        font.set_absolute_size((size * s).max(6. * scale) * f64::from(pango::SCALE));
        font
    };

    // Optional left application icon (real XDG PNG or frosted colored monogram badge).
    let mut text_left = padding_x;
    if show_icons && f64::from(width) > 96. * scale {
        let icon_sz = (f64::from(height) * if compact { 0.62 } else { 0.56 })
            .clamp(14. * scale, 28. * scale)
            .round() as i32;
        let icon_x = padding_x;
        let icon_y = (f64::from(height) - f64::from(icon_sz)) * 0.5;

        if !key.redacted {
            if let Some(icon_surf) = icons.get_icon_surface(&key.app_id, icon_sz) {
                cr.save()?;
                rounded_rect(
                    &cr,
                    icon_x,
                    icon_y,
                    f64::from(icon_sz),
                    f64::from(icon_sz),
                    f64::from(icon_sz) * 0.22,
                );
                cr.clip();
                cr.set_source_surface(&icon_surf, icon_x, icon_y)?;
                cr.paint()?;
                cr.restore()?;
            } else {
                draw_monogram_badge(&cr, &key.app_id, icon_x, icon_y, f64::from(icon_sz), scale)?;
            }
        } else {
            draw_monogram_badge(&cr, "lock", icon_x, icon_y, f64::from(icon_sz), scale)?;
        }
        text_left = icon_x + f64::from(icon_sz) + 9. * s;
    }

    // Right-aligned pure colored workspace name (no chip/pill background or border).
    let mut right_reserved = padding_x;
    if !key.workspace.is_empty() && f64::from(width) > 120. * scale {
        let ws_font = make_font(config.workspace_size, false);
        let ws_layout = pangocairo::functions::create_layout(&cr);
        ws_layout.context().set_round_glyph_positions(false);
        ws_layout.set_single_paragraph_mode(true);
        ws_layout.set_font_description(Some(&ws_font));
        ws_layout.set_text(&key.workspace);
        ws_layout.set_ellipsize(pango::EllipsizeMode::End);
        let max_ws_w = (f64::from(width) * 0.30).max(24. * scale);
        ws_layout.set_width((max_ws_w * f64::from(pango::SCALE)) as i32);
        let (ws_w, ws_h) = ws_layout.pixel_size();
        let ws_x = (f64::from(width) - padding_x - f64::from(ws_w)).max(text_left + 16. * scale);
        let ws_y = if compact {
            (f64::from(height) - f64::from(ws_h)) * 0.5
        } else {
            padding_y + ((config.title_size - config.workspace_size).max(0.) * s * 0.45)
        };

        cr.move_to(ws_x, ws_y);
        set_source_color(&cr, config.workspace_color);
        pangocairo::functions::show_layout(&cr, &ws_layout);

        right_reserved = f64::from(width) - ws_x + gap;
    }

    // Optional status indicator dot (urgent amber / floating cyan) beside workspace badge.
    if (key.is_urgent || key.is_floating) && f64::from(width) > 140. * scale {
        let dot_r = (3.2 * s).max(2.);
        let dot_x = f64::from(width) - right_reserved - dot_r;
        let dot_y = if compact {
            f64::from(height) * 0.5
        } else {
            padding_y + config.title_size * s * 0.6
        };
        cr.arc(dot_x, dot_y, dot_r, 0., 2. * PI);
        if key.is_urgent {
            cr.set_source_rgba(1.0, 0.58, 0.22, 0.95);
        } else {
            cr.set_source_rgba(0.45, 0.85, 0.79, 0.85);
        }
        cr.fill()?;
        right_reserved += dot_r * 2. + 6. * s;
    }

    // Primary bold title.
    let title_font = make_font(config.title_size, true);
    let title_layout = pangocairo::functions::create_layout(&cr);
    title_layout.context().set_round_glyph_positions(false);
    title_layout.set_single_paragraph_mode(true);
    title_layout.set_font_description(Some(&title_font));
    title_layout.set_text(&key.title);
    title_layout.set_ellipsize(pango::EllipsizeMode::End);
    let title_width = (f64::from(width) - text_left - right_reserved).max(8.);
    title_layout.set_width((title_width * f64::from(pango::SCALE)) as i32);
    let (_, title_h) = title_layout.pixel_size();
    let title_y = if compact {
        (f64::from(height) - f64::from(title_h)) * 0.5
    } else {
        padding_y
    };
    cr.move_to(text_left, title_y);
    set_source_color(&cr, config.title_color);
    pangocairo::functions::show_layout(&cr, &title_layout);

    // Secondary `app_id` line — centered horizontally at the bottom of the card.
    if !compact {
        let app_font = make_font(config.app_id_size, false);
        let app_layout = pangocairo::functions::create_layout(&cr);
        app_layout.context().set_round_glyph_positions(false);
        app_layout.set_single_paragraph_mode(true);
        app_layout.set_font_description(Some(&app_font));
        app_layout.set_text(&key.app_id);
        app_layout.set_ellipsize(pango::EllipsizeMode::End);
        let app_left = if show_icons { text_left } else { padding_x };
        let app_max_w = (f64::from(width) - app_left - padding_x).max(8.);
        app_layout.set_width((app_max_w * f64::from(pango::SCALE)) as i32);
        let (app_w, app_h) = app_layout.pixel_size();
        let max_app_x = (f64::from(width) - padding_x - f64::from(app_w)).max(app_left);
        let app_x = ((f64::from(width) - f64::from(app_w)) * 0.5).clamp(app_left, max_app_x);
        let min_app_y = title_y + f64::from(title_h) + 2.5 * s;
        let bottom_padding = 6. * s;
        let app_y = (f64::from(height) - bottom_padding - f64::from(app_h)).max(min_app_y);
        if app_y + f64::from(app_h) * 0.7 <= f64::from(height) {
            cr.move_to(app_x, app_y);
            set_source_color(&cr, config.app_id_color);
            pangocairo::functions::show_layout(&cr, &app_layout);
        }
    }

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;
    Ok(buffer)
}

fn draw_monogram_badge(
    cr: &cairo::Context,
    app_id: &str,
    x: f64,
    y: f64,
    sz: f64,
    scale: f64,
) -> anyhow::Result<()> {
    let (r, g, b) = monogram_color(app_id);
    let radius = sz * 0.28;
    rounded_rect(cr, x, y, sz, sz, radius);
    let grad = cairo::LinearGradient::new(x, y, x, y + sz);
    grad.add_color_stop_rgba(
        0.,
        (r + 0.16).min(1.),
        (g + 0.16).min(1.),
        (b + 0.16).min(1.),
        0.92,
    );
    grad.add_color_stop_rgba(1., r * 0.78, g * 0.78, b * 0.78, 0.92);
    cr.set_source(&grad)?;
    cr.fill_preserve()?;
    cr.set_source_rgba(1., 1., 1., 0.25);
    cr.set_line_width((0.9 * scale).max(1.));
    cr.stroke()?;

    let initial = app_id
        .rsplit('.')
        .next()
        .unwrap_or(app_id)
        .chars()
        .find(|c| c.is_alphanumeric())
        .unwrap_or('W')
        .to_ascii_uppercase()
        .to_string();

    let mut font = FontDescription::from_string("Sans Bold");
    font.set_absolute_size(sz * 0.54 * f64::from(pango::SCALE));
    let layout = pangocairo::functions::create_layout(cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));
    layout.set_text(&initial);
    let (tw, th) = layout.pixel_size();
    cr.move_to(
        x + (sz - f64::from(tw)) * 0.5,
        y + (sz - f64::from(th)) * 0.5,
    );
    cr.set_source_rgba(1., 1., 1., 0.96);
    pangocairo::functions::show_layout(cr, &layout);
    Ok(())
}

fn generate_hud_texture(
    renderer: &mut GlesRenderer,
    key: &HudCacheKey,
    scale: f64,
    accent: Color,
    info_cfg: &WindowPickerInfo,
) -> anyhow::Result<PickerTexture> {
    let width = key.width.max(1);
    let height = key.height.max(1);
    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0., 0., 0., 0.);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    let make_font = |sz: f64, bold: bool| {
        let mut f = FontDescription::from_string(&info_cfg.font);
        if bold {
            f.set_weight(pango::Weight::Semibold);
        }
        f.set_absolute_size(sz * scale * f64::from(pango::SCALE));
        f
    };

    // Scope chips on the left: All | WS | Out
    let chip_h = f64::from(height) - 12. * scale;
    let chip_y = 6. * scale;
    let chip_w = 56. * scale;
    let labels = [
        (0u8, format!("All {}", key.counts.0)),
        (1u8, format!("WS {}", key.counts.1)),
        (2u8, format!("Out {}", key.counts.2)),
    ];
    let chip_font = make_font(10.5, true);
    let mut x = 8. * scale;
    for (idx, text) in labels {
        let active = key.scope == idx;
        if active {
            rounded_rect(&cr, x, chip_y, chip_w, chip_h, chip_h * 0.5);
            cr.set_source_rgba(
                f64::from(accent.r),
                f64::from(accent.g),
                f64::from(accent.b),
                0.28,
            );
            cr.fill_preserve()?;
            cr.set_source_rgba(
                f64::from(accent.r),
                f64::from(accent.g),
                f64::from(accent.b),
                0.65,
            );
            cr.set_line_width((1.0 * scale).max(1.));
            cr.stroke()?;
        }

        let layout = pangocairo::functions::create_layout(&cr);
        layout.context().set_round_glyph_positions(false);
        layout.set_font_description(Some(&chip_font));
        layout.set_text(&text);
        let (tw, th) = layout.pixel_size();
        cr.move_to(
            x + (chip_w - f64::from(tw)) * 0.5,
            chip_y + (chip_h - f64::from(th)) * 0.5,
        );
        if active {
            cr.set_source_rgba(0.96, 0.99, 1.0, 0.98);
        } else {
            cr.set_source_rgba(0.82, 0.88, 0.92, 0.72);
        }
        pangocairo::functions::show_layout(&cr, &layout);
        x += chip_w + 4. * scale;
    }

    // Right-side compact hint text.
    let hint_text = if key.quick_look {
        "Space: exit preview · Enter: jump"
    } else if key.search_mode {
        "Enter: jump · Esc: keycaps"
    } else {
        "/: search · Space: preview"
    };
    let hint_font = make_font(9.8, false);
    let hint_layout = pangocairo::functions::create_layout(&cr);
    hint_layout.context().set_round_glyph_positions(false);
    hint_layout.set_font_description(Some(&hint_font));
    hint_layout.set_text(hint_text);
    let (hw, hh) = hint_layout.pixel_size();
    let hint_x = f64::from(width) - 14. * scale - f64::from(hw);
    if hint_x > x + 60. * scale {
        cr.move_to(hint_x, (f64::from(height) - f64::from(hh)) * 0.5);
        cr.set_source_rgba(0.78, 0.85, 0.90, 0.56);
        pangocairo::functions::show_layout(&cr, &hint_layout);
    }

    // Center content: live fuzzy search bar, 2-letter prefix indicator, or active full title.
    let center_left = x + 10. * scale;
    let center_right = if hint_x > center_left + 40. * scale {
        hint_x - 12. * scale
    } else {
        f64::from(width) - 12. * scale
    };
    let center_w = (center_right - center_left).max(16.);

    let (center_text, is_accent) = if key.search_mode {
        (format!("/ {}_", key.search_query), true)
    } else if let Some(prefix) = key.prefix {
        (format!("Key: {prefix}_"), true)
    } else {
        (key.active_title.clone(), false)
    };

    let main_font = make_font(12.2, is_accent);
    let main_layout = pangocairo::functions::create_layout(&cr);
    main_layout.context().set_round_glyph_positions(false);
    main_layout.set_single_paragraph_mode(true);
    main_layout.set_font_description(Some(&main_font));
    main_layout.set_text(&center_text);
    main_layout.set_ellipsize(pango::EllipsizeMode::End);
    main_layout.set_width((center_w * f64::from(pango::SCALE)) as i32);
    let (_, mh) = main_layout.pixel_size();
    cr.move_to(center_left, (f64::from(height) - f64::from(mh)) * 0.5);
    if is_accent {
        set_source_color(&cr, lighten(accent, 0.35));
    } else {
        cr.set_source_rgba(0.95, 0.98, 1.0, 0.94);
    }
    pangocairo::functions::show_layout(&cr, &main_layout);

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;
    Ok(buffer)
}

fn generate_group_header_texture(
    renderer: &mut GlesRenderer,
    title: &str,
    count: usize,
    size: Size<f64, Logical>,
    scale: f64,
    info_cfg: &WindowPickerInfo,
) -> anyhow::Result<PickerTexture> {
    let width = to_physical_precise_round::<i32>(scale, size.w).max(1);
    let height = to_physical_precise_round::<i32>(scale, size.h).max(1);
    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0., 0., 0., 0.);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    let mut font = FontDescription::from_string(&info_cfg.font);
    font.set_weight(pango::Weight::Semibold);
    font.set_absolute_size(11.5 * scale * f64::from(pango::SCALE));

    // Always keep ` · {count}` completely visible; only ellipsize `{title}` if the pill reaches
    // its maximum width on very long workspace names.
    let pad_x = 12. * scale;
    let inner_w = (f64::from(width) - pad_x * 2.).max(8.);

    let suffix = format!(" · {count}");
    let suffix_layout = pangocairo::functions::create_layout(&cr);
    suffix_layout.context().set_round_glyph_positions(false);
    suffix_layout.set_single_paragraph_mode(true);
    suffix_layout.set_font_description(Some(&font));
    suffix_layout.set_text(&suffix);
    let (sw, sh) = suffix_layout.pixel_size();

    let title_max_w = (inner_w - f64::from(sw)).max(8.);
    let title_layout = pangocairo::functions::create_layout(&cr);
    title_layout.context().set_round_glyph_positions(false);
    title_layout.set_single_paragraph_mode(true);
    title_layout.set_font_description(Some(&font));
    title_layout.set_text(title);
    title_layout.set_ellipsize(pango::EllipsizeMode::End);
    title_layout.set_width((title_max_w * f64::from(pango::SCALE)) as i32);
    let (tw, th) = title_layout.pixel_size();

    let total_w = f64::from(tw) + f64::from(sw);
    let start_x = ((f64::from(width) - total_w) * 0.5).max(pad_x);
    cr.set_source_rgba(0.92, 0.97, 1.0, 0.94);
    cr.move_to(start_x, (f64::from(height) - f64::from(th)) * 0.5);
    pangocairo::functions::show_layout(&cr, &title_layout);
    cr.move_to(
        start_x + f64::from(tw),
        (f64::from(height) - f64::from(sh)) * 0.5,
    );
    pangocairo::functions::show_layout(&cr, &suffix_layout);

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;
    Ok(buffer)
}

fn generate_label_texture(
    renderer: &mut GlesRenderer,
    label: &str,
    style: LabelStyle,
    scale: f64,
    config: &WindowPickerLabel,
    accent: Color,
) -> anyhow::Result<PickerTexture> {
    let _span = tracy_client::span!("window_picker::generate_label_texture");

    let mut font = FontDescription::from_string(&config.font);
    font.set_absolute_size(config.size * scale * f64::from(pango::SCALE));

    let highlighted = style.highlight != HighlightKind::Normal;
    let markup = if style.typed_len > 0 && label.len() > 1 {
        let typed = &label[..usize::from(style.typed_len)];
        let rest = &label[usize::from(style.typed_len)..];
        if highlighted {
            format!("<span underline=\"single\">{typed}</span>{rest}")
        } else {
            format!("<span foreground=\"{}\">{typed}</span>{rest}", hex(accent))
        }
    } else {
        label.to_owned()
    };

    let dummy = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&dummy)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_markup(&markup);
    let (text_width, text_height) = layout.pixel_size();
    ensure!(text_width > 0 && text_height > 0);

    let padding_x: i32 = to_physical_precise_round(scale, config.padding_x);
    let padding_y: i32 = to_physical_precise_round(scale, config.padding_y);
    let shadow: i32 = to_physical_precise_round(scale, 4.);
    let width = (text_width + padding_x * 2 + shadow * 2).min(16383);
    let height = (text_height + padding_y * 2 + shadow * 2).min(16383);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0., 0., 0., 0.);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    let radius = (config.corner_radius * scale)
        .min(f64::from(height) / 2.)
        .max(0.);
    let x = f64::from(shadow);
    let y = f64::from(shadow);
    let panel_width = f64::from(width - shadow * 2);
    let panel_height = f64::from(height - shadow * 2);
    let key_depth = (scale * 2.).min(panel_height / 4.);

    let (face, text) = match style.highlight {
        HighlightKind::Hovered => (accent, config.text_color),
        HighlightKind::Focused => (darken(accent, 0.18), config.text_color),
        HighlightKind::Normal => (config.background_color, config.text_color),
    };

    // Soft drop shadow.
    rounded_rect(&cr, x, y + scale * 2., panel_width, panel_height, radius);
    cr.set_source_rgba(0., 0., 0., 0.38);
    cr.fill()?;

    // Key edge: the face is raised by key_depth above a darker base.
    rounded_rect(
        &cr,
        x,
        y + key_depth,
        panel_width,
        panel_height - key_depth,
        radius,
    );
    set_source_color(&cr, darken(face, 0.45));
    cr.fill()?;

    // Face with a subtle top-to-bottom gradient.
    rounded_rect(&cr, x, y, panel_width, panel_height - key_depth, radius);
    let gradient = cairo::LinearGradient::new(0., y, 0., y + panel_height - key_depth);
    let top = lighten(face, 0.16);
    gradient.add_color_stop_rgba(
        0.,
        f64::from(top.r),
        f64::from(top.g),
        f64::from(top.b),
        f64::from(top.a),
    );
    gradient.add_color_stop_rgba(
        1.,
        f64::from(face.r),
        f64::from(face.g),
        f64::from(face.b),
        f64::from(face.a),
    );
    cr.set_source(&gradient)?;
    cr.fill()?;

    // Crisp rim outline.
    rounded_rect(&cr, x, y, panel_width, panel_height - key_depth, radius);
    let outline = if highlighted {
        lighten(accent, 0.38)
    } else {
        let mut color = config.border_color;
        color.a *= 0.48;
        color
    };
    set_source_color(&cr, outline);
    cr.set_line_width((scale * 1.25).max(1.));
    cr.stroke()?;

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_markup(&markup);
    cr.move_to(f64::from(shadow + padding_x), f64::from(shadow + padding_y));
    set_source_color(&cr, text);
    pangocairo::functions::show_layout(&cr, &layout);

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;
    Ok(buffer)
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.).min(height / 2.).max(0.);
    if radius == 0. {
        cr.rectangle(x, y, width, height);
        return;
    }
    cr.new_sub_path();
    cr.arc(x + width - radius, y + radius, radius, -FRAC_PI_2, 0.);
    cr.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.,
        FRAC_PI_2,
    );
    cr.arc(x + radius, y + height - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, PI + FRAC_PI_2);
    cr.close_path();
}

fn set_source_color(cr: &cairo::Context, color: Color) {
    cr.set_source_rgba(
        f64::from(color.r),
        f64::from(color.g),
        f64::from(color.b),
        f64::from(color.a),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_scheme_switches_to_uniform_pairs() {
        assert_eq!(make_labels(3, ""), ["A", "B", "C"]);
        let labels = make_labels(28, "");
        assert_eq!(labels[0], "AA");
        assert_eq!(labels[25], "AZ");
        assert_eq!(labels[26], "BA");
        assert_eq!(labels[27], "BB");

        assert_eq!(make_labels(3, "qwe"), ["Q", "W", "E"]);
        let labels = make_labels(10, "abc");
        assert_eq!(labels.len(), 10);
        assert!(labels.iter().all(|label| label.len() == 2));
        // Deterministic ordering across invocations!
        assert_eq!(labels, make_labels(10, "abc"));
        let unique: HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len());
    }

    #[test]
    fn grid_stays_centered_inside_configured_area() {
        let config = niri_config::WindowPicker::default();
        let output = Size::from((1920., 1080.));
        let inputs = vec![
            GridInput {
                window_size: Size::from((1200., 800.)),
                label_size: Size::from((60., 44.)),
            };
            8
        ];
        let layout = compute_grid(output, &inputs, &config).unwrap();
        let area = Rectangle::new(
            Point::new(output.w * 0.1, output.h * 0.1),
            output.upscale(0.8),
        );

        for placement in layout.placements {
            assert!(area.contains_rect(placement.preview));
            assert!(area.contains_rect(placement.label));
        }
    }

    #[test]
    fn single_window_card_is_exactly_centered() {
        let config = niri_config::WindowPicker::default();
        let output = Size::from((1920., 1080.));
        let input = GridInput {
            window_size: Size::from((1000., 700.)),
            label_size: Size::from((60., 44.)),
        };
        let placement = compute_grid(output, &[input], &config).unwrap().placements[0];
        let center = placement.card.loc + placement.card.size.to_point().upscale(0.5);

        assert_eq!(center, output.to_point().upscale(0.5));
    }

    #[test]
    fn justified_layout_packs_mixed_aspect_ratios() {
        let config = niri_config::WindowPicker::default();
        let bounds = Rectangle::new(Point::new(100., 80.), Size::from((1720., 920.)));
        let inputs = vec![
            GridInput {
                window_size: Size::from((1920., 1080.)),
                label_size: Size::from((52., 40.)),
            },
            GridInput {
                window_size: Size::from((500., 1080.)),
                label_size: Size::from((52., 40.)),
            },
            GridInput {
                window_size: Size::from((1200., 900.)),
                label_size: Size::from((52., 40.)),
            },
            GridInput {
                window_size: Size::from((800., 1000.)),
                label_size: Size::from((52., 40.)),
            },
        ];
        let layout = compute_justified_in_rect(bounds, &inputs, &config).unwrap();
        assert_eq!(layout.placements.len(), 4);
        for pl in layout.placements {
            assert!(bounds.contains_rect(pl.preview));
        }
    }

    #[test]
    fn fuzzy_match_supports_substring_and_subsequence() {
        assert!(fuzzy_match("Visual Studio Code", "studio"));
        assert!(fuzzy_match("Visual Studio Code", "vsc"));
        assert!(!fuzzy_match("Firefox", "zxc"));
    }

    #[test]
    fn spring_stagger_settles_every_card_to_exact_identity() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let anim_cfg = niri_config::Animation {
            off: false,
            kind: ConfigAnimKind::Spring(ConfigSpringParams {
                damping_ratio: 0.72,
                stiffness: 360,
                epsilon: 0.0001,
            }),
        };
        let anim = Animation::new(clock.clone(), 0., 1., 0., anim_cfg);
        let count = 6usize;
        let (card_dur, step_delay, max_stagger) =
            compute_open_timing(count, 800, Some(600), anim.duration());
        let anim = anim.with_duration(card_dur);
        // Total open duration across ALL staggered cards must match requested 600ms!
        let total = anim.duration() + max_stagger;
        assert!(
            total.as_millis().abs_diff(600) <= 2,
            "expected total open duration ~600ms, got {:?}",
            total
        );
        // Before 550ms, the last card must still be animating; at 601ms all cards must be done.
        clock.set_unadjusted(Duration::from_millis(550));
        assert!(!anim.is_done_with_delay(max_stagger));
        clock.set_unadjusted(Duration::from_millis(602));
        assert!(anim.is_done_with_delay(max_stagger));

        let output = Size::from((1920., 1080.));
        let placement = Placement {
            card: Rectangle::new(Point::new(400., 300.), Size::from((420., 290.))),
            preview: Rectangle::new(Point::new(400., 300.), Size::from((420., 232.))),
            info: Rectangle::new(Point::new(400., 532.), Size::from((420., 58.))),
            info_scale: 1.,
            label: Rectangle::new(Point::new(580., 280.), Size::from((60., 40.))),
        };
        let entry = PickerEntry {
            id: MappedId::next(),
            label: String::from("A"),
            app_id: Some(String::from("test")),
            title: Some(String::from("Test")),
            workspace: String::from("ws 1"),
            workspace_idx: 0,
            output_name: String::from("DP-1"),
            is_on_active_output: true,
            is_on_active_workspace: true,
            is_floating: false,
            is_urgent: false,
            origin_rect: Some(Rectangle::new(
                Point::new(100., 100.),
                Size::from((1200., 800.)),
            )),
            workspace_dir: 0,
            motion: WindowPickerMotionStyle::Hero,
        };

        for i in 0..count {
            let e = anim.value_with_delay(step_delay.saturating_mul(i as u32));
            assert!(
                (e - 1.0).abs() < 1e-9,
                "card {i} settled at {e} instead of 1.0"
            );
            for style in MOTION_CYCLE {
                let tf = open_transform(style, &entry, &placement, output, e, true, i);
                assert!(
                    tf.offset.x.abs() < 1e-6 && tf.offset.y.abs() < 1e-6,
                    "style {style:?} card {i} non-zero offset {:?}",
                    tf.offset
                );
                assert!(
                    (tf.scale - 1.0).abs() < 1e-6,
                    "style {style:?} card {i} non-unit scale {}",
                    tf.scale
                );
                assert!(
                    tf.angle.abs() < 1e-6,
                    "style {style:?} card {i} non-zero angle {}",
                    tf.angle
                );
                assert!(
                    tf.flip_y.abs() < 1e-6,
                    "style {style:?} card {i} non-zero flip_y {}",
                    tf.flip_y
                );
                assert!(
                    (tf.alpha - 1.0).abs() < 1e-6,
                    "style {style:?} card {i} non-unit alpha {}",
                    tf.alpha
                );
            }
        }
    }

    #[test]
    fn grouped_layout_keeps_uniform_scale_for_uneven_workspaces() {
        let bounds = Rectangle::new(Point::new(192., 80.), Size::from((1536., 864.)));
        let win_1080p = GridInput {
            window_size: Size::from((1920., 1080.)),
            label_size: Size::from((52., 40.)),
        };
        // WS 1 has 8 windows, WS 2 has 1 window, WS 3 has 1 window (exact user scenario!).
        let groups = vec![
            (String::from("ws 1"), vec![win_1080p; 8]),
            (String::from("ws 2"), vec![win_1080p; 1]),
            (String::from("ws 3"), vec![win_1080p; 1]),
        ];

        for layout_mode in [WindowPickerLayout::Justified, WindowPickerLayout::Grid] {
            let config = niri_config::WindowPicker {
                layout: layout_mode,
                group_by: WindowPickerGroupBy::Workspace,
                max_scale: 0.25,
                ..Default::default()
            };

            let (headers, grouped_pls) =
                compute_grouped_geometry(bounds, &groups, &config).unwrap();
            assert_eq!(headers.len(), 3);
            assert_eq!(grouped_pls.len(), 3);

            let ws1_sample = grouped_pls[0][0].preview.size;
            let ws2_single = grouped_pls[1][0].preview.size;
            let ws3_single = grouped_pls[2][0].preview.size;

            // Single-window workspaces MUST have the EXACT same proportional size as WS 1!
            assert!(
                (ws1_sample.w - ws2_single.w).abs() < 1.0
                    && (ws1_sample.h - ws2_single.h).abs() < 1.0,
                "mode={layout_mode:?}: WS1 {:?} vs WS2 {:?}",
                ws1_sample,
                ws2_single
            );
            assert!(
                (ws1_sample.w - ws3_single.w).abs() < 1.0
                    && (ws1_sample.h - ws3_single.h).abs() < 1.0,
                "mode={layout_mode:?}: WS1 {:?} vs WS3 {:?}",
                ws1_sample,
                ws3_single
            );

            // Aspect ratio must remain true 16:9 and every card must stay inside `bounds`.
            for pls in &grouped_pls {
                for pl in pls {
                    let ratio = pl.preview.size.w / pl.preview.size.h;
                    assert!(
                        (ratio - 16.0 / 9.0).abs() < 1e-3,
                        "mode={layout_mode:?}: distorted aspect ratio {ratio}"
                    );
                    assert!(
                        bounds.contains_rect(pl.card),
                        "mode={layout_mode:?}: card {:?} overflowed bounds {:?}",
                        pl.card,
                        bounds
                    );
                }
            }
        }
    }
}
