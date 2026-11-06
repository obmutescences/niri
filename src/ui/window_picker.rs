use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI};
use std::mem;
use std::rc::Rc;
use std::time::Duration;

use anyhow::ensure;
use niri_config::{
    Color, Config, CornerRadius, WindowPickerInfo, WindowPickerLabel, WindowPickerMotionStyle,
};
use pango::FontDescription;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::Color32F;
use smithay::input::keyboard::Keysym;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::{Animation, Clock};
use crate::layout::{LayoutElement as _, LayoutElementRenderElement};
use crate::niri::Niri;
use crate::niri_render_elements;
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};
use crate::render_helpers::offscreen::OffscreenBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shader_element::ShaderRenderElement;
use crate::render_helpers::shaders::ProgramType;
use crate::render_helpers::shadow::ShadowRenderElement;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::RenderCtx;
use crate::utils::{output_size, to_physical_precise_round, with_toplevel_role};
use crate::window::mapped::MappedId;
use crate::window::Mapped;

const MAX_WINDOWS: usize = 26 * 26;
const PREFIX_DIM_ALPHA: f32 = 0.22;
/// Zoom applied to the hovered preview.
const HOVER_SCALE: f64 = 1.03;
/// How far the label overlaps the top edge of its preview (fraction of label height).
const LABEL_OVERLAP: f64 = 1. / 3.;
/// Gaussian sigma of the hover glow.
const GLOW_SIGMA: f64 = 22.;

/// Visual style of a label texture: keycaps invert while their preview is hovered or
/// focused, and show the already-typed prefix letter in the accent color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LabelStyle {
    highlighted: bool,
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

#[derive(Debug)]
struct PickerEntry {
    id: MappedId,
    label: String,
    app_id: Option<String>,
    title: Option<String>,
    workspace: String,
    motion: WindowPickerMotionStyle,
}

#[derive(Debug)]
pub struct WindowPickerSession {
    entries: Vec<PickerEntry>,
    output: Output,
    prefix: Option<char>,
    /// The currently focused window when the picker opened, if any.
    focused_id: Option<MappedId>,
    clock: Clock,
}

impl WindowPickerSession {
    pub fn collect(niri: &Niri) -> Option<Self> {
        let output = niri.layout.active_output()?.clone();
        let custom_letters = niri.config.borrow().window_picker.label.letters.clone();
        let mut windows = Vec::new();
        let mut seen = HashSet::new();
        let mut focused_id = None;

        let mut workspace_labels = HashMap::new();
        for (_, ws_idx, ws) in niri.layout.workspaces() {
            let label = ws
                .name()
                .cloned()
                .unwrap_or_else(|| format!("ws {}", ws_idx + 1));
            for mapped in ws.windows() {
                workspace_labels.insert(mapped.id(), label.clone());
            }
        }

        for (_, mapped) in niri.layout.windows() {
            let size = mapped.size();
            if size.w > 0 && size.h > 0 && seen.insert(mapped.id()) {
                if mapped.is_focused() {
                    focused_id = Some(mapped.id());
                }
                let (app_id, title) = with_toplevel_role(mapped.toplevel(), |role| {
                    (role.app_id.clone(), role.title.clone())
                });
                let workspace = workspace_labels
                    .get(&mapped.id())
                    .cloned()
                    .unwrap_or_default();
                windows.push((
                    mapped.id(),
                    mapped.get_focus_timestamp(),
                    app_id,
                    title,
                    workspace,
                ));
            }
        }

        windows.sort_by(|(_, left, ..), (_, right, ..)| right.cmp(left));
        windows.truncate(MAX_WINDOWS);
        if windows.is_empty() {
            return None;
        }

        // Keep the focus marker only if the focused window survived truncation.
        if !windows
            .iter()
            .any(|(id, ..)| Some(id) == focused_id.as_ref())
        {
            focused_id = None;
        }

        let labels = make_labels(windows.len(), &custom_letters);
        let entries = windows
            .into_iter()
            .zip(labels)
            .map(|((id, _, app_id, title, workspace), label)| PickerEntry {
                id,
                label,
                app_id,
                title,
                workspace,
                motion: WindowPickerMotionStyle::Slide,
            })
            .collect();
        let clock = niri.clock.clone();

        Some(Self {
            entries,
            output,
            prefix: None,
            focused_id,
            clock,
        })
    }
}

#[derive(Debug)]
enum WindowPickerState {
    Closed,
    Open {
        session: WindowPickerSession,
        anim: Animation,
    },
    Closing {
        session: WindowPickerSession,
        anim: Animation,
        /// Animation progress at the moment closing started.
        from_progress: f64,
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
            Self::Open { anim, .. } | Self::Closing { anim, .. } => Some(anim),
            Self::Closed => None,
        }
    }
}

fn ease_out_cubic(progress: f64) -> f64 {
    1. - (1. - progress).powi(3)
}

fn ease_in_cubic(progress: f64) -> f64 {
    progress * progress * progress
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

fn keyframe(t: f64, first: f64, mid: f64, last: f64, mid_at: f64) -> f64 {
    if t < mid_at {
        lerp(first, mid, ease_out_cubic(t / mid_at))
    } else {
        lerp(mid, last, ease_out_cubic((t - mid_at) / (1. - mid_at)))
    }
}

#[derive(Debug, Clone, Copy)]
struct CardTransform {
    offset: Point<f64, Logical>,
    scale: f64,
    angle: f64,
    alpha: f64,
}

impl CardTransform {
    fn identity() -> Self {
        Self {
            offset: Point::from((0., 0.)),
            scale: 1.,
            angle: 0.,
            alpha: 1.,
        }
    }
}

const MOTION_CYCLE: [WindowPickerMotionStyle; 10] = [
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
    let size = Size::from(((max.x - min.x).max(0.), (max.y - min.y).max(0.)));
    let center = Point::from(((min.x + max.x) * 0.5, (min.y + max.y) * 0.5));
    (size, center)
}

fn scatter_transform(
    card: Rectangle<f64, Logical>,
    output: Size<f64, Logical>,
    t: f64,
    index: usize,
) -> CardTransform {
    let t = ease_in_cubic(t.clamp(0., 1.));
    let target_center = card_center(card);
    let output_center = output.to_point().upscale(0.5);
    let delta = target_center - output_center;
    let mut angle = delta.y.atan2(delta.x);
    if delta.x.abs() < f64::EPSILON && delta.y.abs() < f64::EPSILON {
        angle = if index.is_multiple_of(2) { 0. } else { PI };
    }
    angle += if index.is_multiple_of(2) { -0.17 } else { 0.17 };
    let distance = output.w.hypot(output.h) * 0.78;
    let offset = Point::from((angle.cos(), angle.sin())).upscale(distance * t);
    let rotation = if index.is_multiple_of(2) { -1. } else { 1. }
        * (6. + (index % 3) as f64 * 2.).to_radians()
        * t;
    CardTransform {
        offset,
        scale: lerp(1., 0.82, t),
        angle: rotation,
        alpha: 1. - t,
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
            -output.w * 0.62
        } else {
            output.w * 0.62
        };
        (x, delta.y * 0.12)
    } else {
        let y = if delta.y < 0. {
            -output.h * 0.62
        } else {
            output.h * 0.62
        };
        (delta.x * 0.12, y)
    };
    Point::from((x, y)).upscale(1. - progress)
}

fn open_transform(
    style: WindowPickerMotionStyle,
    card: Rectangle<f64, Logical>,
    output: Size<f64, Logical>,
    t: f64,
    index: usize,
) -> CardTransform {
    let t = t.clamp(0., 1.);
    let target_center = card_center(card);
    let output_center = output.to_point().upscale(0.5);
    let mut tf = CardTransform::identity();
    match style {
        WindowPickerMotionStyle::Pop => {
            tf.scale = keyframe(t, 0.72, 1.035, 1., 0.72);
            tf.alpha = ease_out_cubic(t);
        }
        WindowPickerMotionStyle::Slide => {
            let e = ease_out_cubic(t);
            tf.offset = slide_offset(target_center, output, e);
            tf.scale = lerp(0.96, 1., e);
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Drop => {
            let y = keyframe(t, -output.h * 0.78, 10., 0., 0.72);
            tf.offset = Point::from((0., y));
            tf.scale = keyframe(t, 0.95, 1.01, 1., 0.72);
            tf.angle = keyframe(t, -1.5_f64.to_radians(), 0.3_f64.to_radians(), 0., 0.72);
            tf.alpha = ease_out_cubic(t);
        }
        WindowPickerMotionStyle::Burst => {
            let spin = if index.is_multiple_of(2) { -1. } else { 1. }
                * (32. + (index % 4) as f64 * 9.).to_radians();
            let e = ease_out_cubic(t);
            tf.offset = (target_center - output_center).upscale(-(1. - e));
            tf.scale = keyframe(t, 0.24, 1.04, 1., 0.72);
            tf.angle = keyframe(t, spin, -spin * 0.14, 0., 0.72);
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Vortex => {
            let spin = if index.is_multiple_of(2) { 1. } else { -1. }
                * (155. + (index % 3) as f64 * 28.).to_radians();
            let delta = target_center - output_center;
            let radius = delta.x.hypot(delta.y);
            let angle = delta.y.atan2(delta.x) + 0.95;
            let mid = Point::from((angle.cos(), angle.sin())).upscale(radius * 0.48);
            let start = Point::from((0., 0.));
            let end = target_center - output_center;
            let u = t;
            let first = start + (mid - start).upscale(u);
            let second = mid + (end - mid).upscale(u);
            tf.offset = first + (second - first).upscale(u) - end;
            tf.scale = keyframe(t, 0.38, 0.94, 1., 0.58);
            tf.angle = keyframe(t, spin, spin * 0.26, 0., 0.58);
            tf.alpha = ease_out_cubic(t);
        }
        WindowPickerMotionStyle::Flip => {
            let dir = if index.is_multiple_of(2) { 1. } else { -1. };
            let e = ease_out_cubic(t);
            tf.angle = keyframe(t, dir * PI, dir * 0.22, 0., 0.72);
            tf.scale = keyframe(t, 0.62, 1.035, 1., 0.72);
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Cascade => {
            let delay = (index % 4) as f64 * 0.07;
            let local = ((t - delay) / (1. - delay)).clamp(0., 1.);
            let e = ease_out_cubic(local);
            let dir = if index.is_multiple_of(2) { -1. } else { 1. };
            let drop = output.h * (0.45 + (index % 3) as f64 * 0.08);
            tf.offset = Point::from((dir * output.w * 0.08 * (1. - e), -drop * (1. - e)));
            tf.scale = lerp(0.9, 1., e);
            tf.angle = dir * (6. + (index % 3) as f64 * 3.).to_radians() * (1. - e);
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Orbit => {
            let e = ease_out_cubic(t);
            let dir = if index.is_multiple_of(2) { 1. } else { -1. };
            let target_delta = target_center - output_center;
            let target_angle = target_delta.y.atan2(target_delta.x);
            let target_radius = target_delta.x.hypot(target_delta.y);
            let start_angle = target_angle + dir * (1.6 + (index % 3) as f64 * 0.25);
            let start_radius = output.w.hypot(output.h) * (0.42 + (index % 3) as f64 * 0.05);
            let angle = lerp(start_angle, target_angle, e);
            let radius = lerp(start_radius, target_radius, e);
            let position = output_center + Point::from((angle.cos(), angle.sin())).upscale(radius);
            tf.offset = position - target_center;
            tf.scale = keyframe(t, 0.48, 1.025, 1., 0.7);
            tf.angle = keyframe(
                t,
                dir * (0.7 + (index % 3) as f64 * 0.15),
                -dir * 0.1,
                0.,
                0.7,
            );
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Wave => {
            let e = ease_out_cubic(t);
            let dir = if index.is_multiple_of(2) { -1. } else { 1. };
            let phase = index as f64 * 0.85;
            let wobble = (phase + t * 2. * PI).sin() * output.h * 0.04 * (1. - e);
            tf.offset = Point::from((dir * output.w * 0.66 * (1. - e), wobble));
            tf.scale = keyframe(t, 0.94, 1.015, 1., 0.72);
            tf.angle = dir * (7. + (index % 3) as f64 * 3.).to_radians() * (1. - e);
            tf.alpha = e;
        }
        WindowPickerMotionStyle::Zoom => {
            tf.scale = keyframe(t, 2.3, 0.98, 1., 0.8);
            tf.angle = keyframe(t, 0.04, -0.01, 0., 0.8);
            tf.alpha = ease_out_cubic(t);
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
    accent: Option<Color>,
    textures: HashMap<(String, LabelStyle), Option<PickerTexture>>,
}

impl LabelCache {
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        label: &str,
        style: LabelStyle,
        scale: f64,
        config: &WindowPickerLabel,
        accent: Color,
    ) -> Option<PickerTexture> {
        if self.scale != scale
            || self.config.as_ref() != Some(config)
            || self.accent != Some(accent)
        {
            self.scale = scale;
            self.config = Some(config.clone());
            self.accent = Some(accent);
            self.textures.clear();
        }

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

        // All style variants share the same text and padding, hence the same size.
        self.textures
            .iter()
            .find(|((cached, _), _)| cached == label)
            .and_then(|(_, texture)| texture.as_ref())
            .map(PickerTexture::logical_size)
    }

    fn clear(&mut self) {
        self.config = None;
        self.accent = None;
        self.textures.clear();
    }
}

#[derive(Debug, Default)]
struct BackdropBuffers {
    dim: SolidColorBuffer,
    tint: SolidColorBuffer,
}

#[derive(Debug, Default)]
struct InfoCache {
    scale: f64,
    config: Option<WindowPickerInfo>,
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
}

impl InfoCache {
    #[allow(clippy::too_many_arguments)]
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        app_id: &str,
        title: &str,
        workspace: &str,
        width: f64,
        height: f64,
        font_scale: f64,
        scale: f64,
        config: &WindowPickerInfo,
    ) -> Option<PickerTexture> {
        if self.scale != scale || self.config.as_ref() != Some(config) {
            self.scale = scale;
            self.config = Some(config.clone());
            self.textures.clear();
        }

        let width = to_physical_precise_round::<i32>(scale, width).max(1);
        let height = to_physical_precise_round::<i32>(scale, height).max(1);
        let key = InfoKey {
            app_id: app_id.to_owned(),
            title: truncate_title(title),
            workspace: workspace.to_owned(),
            width,
            height,
            font_scale: (font_scale * 10_000.).round() as i64,
        };
        self.textures
            .entry(key.clone())
            .or_insert_with(|| {
                generate_info_texture(renderer, &key, scale, font_scale, config).ok()
            })
            .clone()
    }

    fn clear(&mut self) {
        self.config = None;
        self.textures.clear();
    }
}

fn truncate_title(title: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut truncated: String = title.chars().take(MAX_CHARS).collect();
    if title.chars().count() > MAX_CHARS {
        truncated.push('…');
    }
    truncated
}

type CardBlur = (FramebufferEffect, Rectangle<f64, Logical>);

pub struct WindowPickerUi {
    state: WindowPickerState,
    config: Rc<RefCell<Config>>,
    label_cache: RefCell<LabelCache>,
    backdrop_buffers: RefCell<HashMap<Output, BackdropBuffers>>,
    framebuffer_effect: RefCell<FramebufferEffect>,
    /// Window being flown back to its real position during the closing animation.
    fly_back: Option<MappedId>,
    /// Preview the cursor is currently over (hover highlight).
    hover: Option<MappedId>,
    /// When the current hover started, for the zoom-in animation.
    hover_started_at: Option<Duration>,
    glow: RefCell<ShadowRenderElement>,
    card_shadow: RefCell<ShadowRenderElement>,
    info_cache: RefCell<InfoCache>,
    card_blurs: RefCell<HashMap<MappedId, CardBlur>>,
    card_offscreens: RefCell<HashMap<MappedId, OffscreenBuffer>>,
    /// Next style to use in `cycle` mode. Advances across picker openings so even a
    /// single-window picker changes animation each time.
    motion_cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPickerKeyResult {
    Handled,
    Selected(MappedId),
}

impl WindowPickerUi {
    pub fn new(config: Rc<RefCell<Config>>) -> Self {
        Self {
            state: WindowPickerState::Closed,
            config,
            label_cache: RefCell::new(LabelCache::default()),
            backdrop_buffers: RefCell::new(HashMap::new()),
            framebuffer_effect: RefCell::new(FramebufferEffect::new()),
            fly_back: None,
            hover: None,
            hover_started_at: None,
            glow: RefCell::new(ShadowRenderElement::empty()),
            card_shadow: RefCell::new(ShadowRenderElement::empty()),
            info_cache: RefCell::new(InfoCache::default()),
            card_blurs: RefCell::new(HashMap::new()),
            card_offscreens: RefCell::new(HashMap::new()),
            motion_cursor: 0,
        }
    }

    fn progress(&self) -> f64 {
        self.state.anim().map_or(0., Animation::clamped_value)
    }

    fn raw_progress(&self) -> f64 {
        self.state.anim().map_or(0., Animation::value).clamp(0., 1.)
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

    pub fn open(&mut self, mut session: WindowPickerSession) {
        let config = self.config.borrow().window_picker.clone();
        let style = config.motion.style;
        let mut motion_cursor = self.motion_cursor;
        for entry in &mut session.entries {
            entry.motion = if style == WindowPickerMotionStyle::Cycle {
                let motion = MOTION_CYCLE[motion_cursor % MOTION_CYCLE.len()];
                motion_cursor += 1;
                motion
            } else {
                style
            };
        }
        if style == WindowPickerMotionStyle::Cycle {
            self.motion_cursor = motion_cursor % MOTION_CYCLE.len();
        }

        let anim_cfg = config.animation.to_animation(config.animation_ms_open);
        let anim = Animation::new(session.clock.clone(), 0., 1., 0., anim_cfg);

        self.label_cache.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.card_blurs.get_mut().clear();
        self.card_offscreens.get_mut().clear();
        self.framebuffer_effect.get_mut().damage();
        self.fly_back = None;
        self.hover = None;
        self.hover_started_at = None;
        self.state = WindowPickerState::Open { session, anim };
    }

    pub fn close(&mut self) -> bool {
        // Already closing (e.g. a fly-back selection started it); keep the running animation.
        if matches!(self.state, WindowPickerState::Closing { .. }) {
            return true;
        }

        let state = mem::replace(&mut self.state, WindowPickerState::Closed);
        let WindowPickerState::Open { mut session, anim } = state else {
            self.state = state;
            return false;
        };

        let config = self.config.borrow().window_picker.clone();
        session.prefix = None;

        if config.animation_ms_close == 0 && config.animation.kind.is_none() {
            self.finish_close();
            return true;
        }

        let mut close_cfg = config.animation.to_animation(config.animation_ms_close);
        // Give the fly-back preview enough time to travel.
        if self.fly_back.is_some() {
            if let niri_config::animations::Kind::Easing(p) = &mut close_cfg.kind {
                p.duration_ms = p.duration_ms.max(280);
            }
        }

        let current = anim.clamped_value();
        let close_anim = Animation::new(session.clock.clone(), current, 0., 0., close_cfg);
        self.state = WindowPickerState::Closing {
            session,
            anim: close_anim,
            from_progress: current,
        };
        self.framebuffer_effect.get_mut().damage();
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
        self.backdrop_buffers.get_mut().clear();
        self.card_blurs.get_mut().clear();
        self.card_offscreens.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.framebuffer_effect.get_mut().damage();
        self.fly_back = None;
    }

    /// Updates the hovered preview (from pointer motion); `None` clears it.
    ///
    /// Returns whether the hovered entry changed, so the caller can queue a redraw.
    pub fn set_hover(&mut self, id: Option<MappedId>) -> bool {
        let changed = self.hover != id;
        if changed {
            self.hover = id;
            self.hover_started_at = match (id, self.state.session()) {
                (Some(_), Some(session)) => Some(session.clock.now()),
                _ => None,
            };
        }
        changed
    }

    /// Current zoom factor of the hovered preview, easing in over 90 ms.
    fn hover_zoom(&self, session: &WindowPickerSession) -> f64 {
        let Some(started) = self.hover_started_at else {
            return 1.;
        };
        let t = session.clock.now().saturating_sub(started).as_secs_f64() / 0.09;
        1. + (HOVER_SCALE - 1.) * ease_out_cubic(t.clamp(0., 1.))
    }

    pub fn advance_animations(&mut self) -> bool {
        let finished = matches!(self.state, WindowPickerState::Closing { .. })
            && self.state.anim().is_some_and(Animation::is_clamped_done);
        if finished {
            self.close_immediately();
        }
        finished
    }

    pub fn update_config(&mut self) {
        self.label_cache.get_mut().clear();
        self.info_cache.get_mut().clear();
        self.card_blurs.get_mut().clear();
        self.card_offscreens.get_mut().clear();
        self.framebuffer_effect.get_mut().damage();
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

        self.card_blurs
            .get_mut()
            .retain(|id, _| live_ids.contains(id));
        self.card_offscreens
            .get_mut()
            .retain(|id, _| live_ids.contains(id));

        let is_empty = session.entries.is_empty();
        if !is_empty {
            // A removed entry can invalidate the currently entered two-letter prefix.
            session.prefix = None;
        }

        if is_empty {
            self.close_immediately();
        }
        true
    }

    pub fn handle_key(&mut self, raw: Keysym) -> WindowPickerKeyResult {
        if raw == Keysym::Escape {
            self.close();
            return WindowPickerKeyResult::Handled;
        }

        // Labels stay stable while the picker is open, including when windows close. Determine
        // whether this session uses one or two letters from the labels rather than the live count.
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

            if session
                .entries
                .first()
                .is_some_and(|entry| entry.label.len() == 1)
            {
                session
                    .entries
                    .iter()
                    .find(|entry| entry.label.starts_with(letter))
                    .map(|entry| entry.id)
            } else if let Some(prefix) = session.prefix {
                session
                    .entries
                    .iter()
                    .find(|entry| {
                        let mut chars = entry.label.chars();
                        chars.next() == Some(prefix) && chars.next() == Some(letter)
                    })
                    .map(|entry| entry.id)
            } else {
                if session
                    .entries
                    .iter()
                    .any(|entry| entry.label.starts_with(letter))
                {
                    session.prefix = Some(letter);
                }
                None
            }
        };

        // Fly the selected preview back to its window while the picker closes.
        if let Some(id) = selected {
            self.fly_back = Some(id);
            self.close();
            return WindowPickerKeyResult::Selected(id);
        }

        WindowPickerKeyResult::Handled
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
        let output_size = output_size(output);
        let scale = output.current_scale().fractional_scale();
        let mut windows = picker_windows(niri, session, &config.label);
        let label_cache = self.label_cache.borrow();
        for window in &mut windows {
            if let Some(size) = label_cache.logical_size(&window.entry.label, scale, &config.label)
            {
                window.label_size = size;
            }
        }
        let inputs = grid_inputs(&windows);
        let layout = compute_grid(output_size, &inputs, &config)?;

        windows
            .iter()
            .zip(layout.placements)
            .find_map(|(window, placement)| {
                let zoom = if self.hover == Some(window.entry.id) {
                    self.hover_zoom(session)
                } else if session.focused_id == Some(window.entry.id) {
                    HOVER_SCALE
                } else {
                    1.
                };
                let card = scale_rect(
                    placement.card,
                    card_center(placement.card),
                    zoom,
                    Point::from((0., 0.)),
                );
                card.contains(pos_within_output).then_some(window.entry.id)
            })
    }

    pub fn are_animations_ongoing(&self) -> bool {
        if self
            .state
            .anim()
            .is_some_and(|anim| !anim.is_clamped_done())
        {
            return true;
        }

        // Keep redrawing while a preview is hovered or focused so the glow can pulse and the
        // hover zoom can ease in.
        let WindowPickerState::Open { session, .. } = &self.state else {
            return false;
        };
        if self.hover.is_none() && session.focused_id.is_none() {
            return false;
        }
        let config = self.config.borrow().window_picker.clone();
        config.selection.glow || self.hover_zoom(session) < HOVER_SCALE
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
        let raw_progress = self.raw_progress();
        let closing = self.is_closing();
        let alpha = progress as f32;
        let size = output_size(output);
        let scale = output.current_scale().fractional_scale();

        if *output == session.output {
            let selection = config.selection;
            let mut windows = picker_windows(niri, session, &config.label);
            for window in &mut windows {
                let highlighted = !closing
                    && (self.hover == Some(window.entry.id)
                        || session.focused_id == Some(window.entry.id));
                let typed_len = match session.prefix {
                    Some(prefix) if window.entry.label.starts_with(prefix) => 1,
                    _ => 0,
                };
                let style = LabelStyle {
                    highlighted,
                    typed_len,
                };
                let texture = self.label_cache.borrow_mut().get(
                    ctx.as_gles().renderer,
                    &window.entry.label,
                    style,
                    scale,
                    &config.label,
                    selection.hover_color,
                );
                if let Some(texture) = &texture {
                    window.label_size = texture.logical_size();
                }
                window.texture = texture;
            }

            let inputs = grid_inputs(&windows);
            let count = windows.len();

            // Normalized per-entry stagger: later entries start later; on close the order is
            // reversed so the last entry to fly in is the first to fly out.
            let frac = if count > 1 && config.stagger_ms > 0 {
                let total = config.animation_ms_open.max(1) as f64 / 1000.;
                ((f64::from(config.stagger_ms) / 1000.) / total).min(0.5 / (count - 1) as f64)
            } else {
                0.
            };
            let span = 1. - frac * count.saturating_sub(1) as f64;
            let local_t = |raw: f64, ord: usize| {
                ((raw - frac * ord as f64) / span.max(f64::EPSILON)).clamp(0., 1.)
            };
            let close_from = self.close_from_progress();

            // Subtle breathing of focus glow; window contents never pulse.
            let glow_pulse = 0.8 + 0.2 * (session.clock.now().as_secs_f64() * 3.0).sin();

            if let Some(layout) = compute_grid(size, &inputs, &config) {
                for (i, (window, placement)) in windows.iter().zip(layout.placements).enumerate() {
                    let ord = if closing { count - 1 - i } else { i };
                    let style = window.entry.motion;
                    let open_at = if closing { close_from } else { raw_progress };
                    let mut tf =
                        open_transform(style, placement.card, size, local_t(open_at, ord), i);

                    if closing {
                        let close_t = if close_from > f64::EPSILON {
                            ((close_from - progress) / close_from).clamp(0., 1.)
                        } else {
                            1.
                        };
                        let start = open_transform(
                            style,
                            placement.card,
                            size,
                            local_t(close_from, ord),
                            i,
                        );
                        let scatter = scatter_transform(placement.card, size, close_t, i);
                        tf.offset = start.offset + (scatter.offset - start.offset).upscale(close_t);
                        tf.scale = lerp(start.scale, scatter.scale, close_t);
                        tf.angle = lerp(start.angle, scatter.angle, close_t);
                        tf.alpha = start.alpha * (1. - close_t);
                    }

                    let hovered = self.hover == Some(window.entry.id);
                    let focused = session.focused_id == Some(window.entry.id);
                    let is_fly_back = closing && self.fly_back == Some(window.entry.id);

                    if is_fly_back {
                        let grow = 1. + 0.55 * (1. - progress);
                        tf.offset = Point::from((0., 0.));
                        tf.scale = grow;
                        tf.angle = 0.;
                        tf.alpha = ((progress - 0.12) / 0.3).clamp(0., 1.);
                    } else if !closing {
                        let zoom = if hovered {
                            self.hover_zoom(session)
                        } else if focused {
                            HOVER_SCALE
                        } else {
                            1.
                        };
                        tf.scale *= zoom;
                    }

                    let prefix_dim = session
                        .prefix
                        .is_some_and(|prefix| !window.entry.label.starts_with(prefix));
                    let mut entry_alpha = if prefix_dim {
                        PREFIX_DIM_ALPHA * alpha
                    } else {
                        alpha * tf.alpha.clamp(0., 1.) as f32
                    };
                    if is_fly_back {
                        entry_alpha = ((progress - 0.12) / 0.3).clamp(0., 1.) as f32;
                    }

                    let rotated = tf.angle.abs() > 1e-4;
                    if rotated {
                        // Rotation needs one composite texture per card so the window
                        // thumbnail, keycap and info strip rotate as one unit.
                        let mut gctx = ctx.as_gles();
                        let mut elems = Vec::new();
                        self.render_card_contents(
                            gctx.r(),
                            scale,
                            window,
                            &placement,
                            CardTransform::identity(),
                            1.,
                            &config,
                            &mut |elem| elems.push(elem),
                        );
                        let rendered = self
                            .card_offscreens
                            .borrow_mut()
                            .entry(window.entry.id)
                            .or_default()
                            .render(gctx.renderer, Scale::from(scale), &elems);
                        let Ok((elem, _sync, _data)) = rendered else {
                            continue;
                        };
                        let texture = elem.texture().clone();
                        let tex_rect = Rectangle::new(elem.offset(), elem.logical_size());
                        if tex_rect.size.w <= 0. || tex_rect.size.h <= 0. {
                            continue;
                        }

                        let pivot = card_center(placement.card) - tex_rect.loc;
                        let (dest_size, dest_center) =
                            rotated_bounds(tex_rect.size, pivot, tf.angle, tf.scale);
                        if dest_size.w <= 0. || dest_size.h <= 0. {
                            continue;
                        }
                        let dest_loc = card_center(placement.card) + tf.offset + dest_center
                            - dest_size.to_point().upscale(0.5);

                        let uniforms = Rc::new([
                            Uniform::new(
                                "src_size",
                                [tex_rect.size.w as f32, tex_rect.size.h as f32],
                            ),
                            Uniform::new("dest_size", [dest_size.w as f32, dest_size.h as f32]),
                            Uniform::new(
                                "src_center",
                                [
                                    (tex_rect.size.w * 0.5) as f32,
                                    (tex_rect.size.h * 0.5) as f32,
                                ],
                            ),
                            Uniform::new("angle", tf.angle as f32),
                            Uniform::new("card_scale", tf.scale as f32),
                        ]);
                        let textures = HashMap::from([(String::from("tex"), texture)]);
                        let card = ShaderRenderElement::new(
                            ProgramType::PickerCard,
                            dest_size,
                            None,
                            scale as f32,
                            entry_alpha,
                            uniforms,
                            textures,
                            Kind::Unspecified,
                        )
                        .with_location(dest_loc);
                        push(WindowPickerUiRenderElement::Card(card));
                    } else {
                        self.render_card_contents(
                            ctx.r(),
                            scale,
                            window,
                            &placement,
                            tf,
                            entry_alpha,
                            &config,
                            push,
                        );
                    }

                    // Glow and per-card blur sit behind the rotated card texture.
                    if !closing && selection.glow && tf.angle.abs() < 1e-4 {
                        let glow_style = if hovered {
                            Some((selection.hover_color, entry_alpha * glow_pulse as f32 * 0.9))
                        } else if focused {
                            Some((
                                selection.focused_color,
                                entry_alpha * glow_pulse as f32 * 0.55,
                            ))
                        } else {
                            None
                        };
                        if let Some((color, glow_alpha)) = glow_style {
                            let card_rect = scale_rect(
                                placement.card,
                                card_center(placement.card),
                                tf.scale,
                                tf.offset,
                            );
                            let margin = (GLOW_SIGMA * 3.).ceil();
                            let shader_size =
                                card_rect.size + Size::from((margin, margin)).upscale(2.);
                            let radius = CornerRadius::from(
                                config.card.corner_radius.max(0.) as f32 * tf.scale as f32,
                            );
                            let mut glow = self.glow.borrow_mut();
                            glow.update(
                                shader_size,
                                Rectangle::new(Point::from((margin, margin)), card_rect.size),
                                color,
                                GLOW_SIGMA as f32,
                                radius,
                                scale as f32,
                                Rectangle::zero(),
                                CornerRadius::default(),
                                glow_alpha,
                            );
                            push(WindowPickerUiRenderElement::Shadow(
                                glow.clone()
                                    .with_location(card_rect.loc - Point::from((margin, margin))),
                            ));
                        }
                    }

                    // With no card fill color, the per-card blur is the card surface itself.
                    // It can follow any axis-aligned transform; rotation needs the composite.
                    let blur_ok = !closing && tf.angle.abs() < 1e-4;
                    if config.card.on && config.card.blur.on && blur_ok {
                        let card_rect = scale_rect(
                            placement.card,
                            card_center(placement.card),
                            tf.scale,
                            tf.offset,
                        );
                        let radius = CornerRadius::from(
                            config.card.corner_radius.max(0.) as f32 * tf.scale as f32,
                        );
                        let mut blurs = self.card_blurs.borrow_mut();
                        let (effect, last_rect) = blurs
                            .entry(window.entry.id)
                            .or_insert_with(|| (FramebufferEffect::new(), Rectangle::default()));
                        if *last_rect != card_rect {
                            effect.damage();
                            *last_rect = card_rect;
                        }
                        let params = RenderParams {
                            geometry: card_rect,
                            subregion: None,
                            clip: Some((card_rect, radius)),
                            scale,
                        };
                        let blur = BlurOptions {
                            passes: config.card.blur.passes,
                            offset: config.card.blur.offset.round(),
                        };
                        let effect = effect.render(None, params, Some(blur), 0., 1., None, None);
                        push(WindowPickerUiRenderElement::FramebufferEffect(effect));
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
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let pivot = card_center(placement.card);
        let card = scale_rect(placement.card, pivot, transform.scale, transform.offset);
        let preview = scale_rect(placement.preview, pivot, transform.scale, transform.offset);
        let info = scale_rect(placement.info, pivot, transform.scale, transform.offset);
        let label = scale_rect(placement.label, pivot, transform.scale, transform.offset);
        let card_radius = config.card.corner_radius.max(0.) as f32 * transform.scale as f32;

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

        if config.card.on {
            let app_id = window.entry.app_id.as_deref().unwrap_or("unknown");
            let title = window
                .entry
                .title
                .as_deref()
                .filter(|title| !title.is_empty())
                .unwrap_or(app_id);
            let gctx = ctx.as_gles();
            let info_texture = self.info_cache.borrow_mut().get(
                gctx.renderer,
                app_id,
                title,
                &window.entry.workspace,
                placement.info.size.w,
                placement.info.size.h,
                placement.info_scale,
                scale,
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

        let mut ctx = ctx;
        render_thumbnail(
            ctx.r(),
            scale,
            window.mapped,
            preview,
            entry_alpha,
            thumbnail_clip_radius(window.mapped, config),
            push,
        );

        if config.card.on {
            let radius = CornerRadius::from(card_radius);

            if config.card.shadow {
                let margin = (GLOW_SIGMA * 2.).ceil();
                let shader_size = card.size + Size::from((margin, margin)).upscale(2.);
                let mut shadow = self.card_shadow.borrow_mut();
                shadow.update(
                    shader_size,
                    Rectangle::new(Point::from((margin, margin)), card.size),
                    Color::from_rgba8_unpremul(0, 0, 0, 255),
                    12. * transform.scale as f32,
                    radius,
                    scale as f32,
                    Rectangle::new(Point::from((margin, margin)), card.size),
                    radius,
                    0.35,
                );
                push(WindowPickerUiRenderElement::Shadow(
                    shadow
                        .clone()
                        .with_location(card.loc - Point::from((margin, margin))),
                ));
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
        let mut buffers = self.backdrop_buffers.borrow_mut();
        let buffers = buffers.entry(output.clone()).or_default();

        // On close, drop the backdrop noticeably faster than the previews and cut it
        // entirely at the very end, so it never lingers after the motion is done.
        let progress = if closing {
            let fast = (progress * 1.8).clamp(0., 1.);
            if progress <= 0.02 {
                0.
            } else {
                fast
            }
        } else {
            progress
        };

        let dim_alpha = ((1. - backdrop.brightness) * progress).clamp(0., 1.) as f32;
        buffers
            .dim
            .update(size, Color32F::new(0., 0., 0., dim_alpha));
        if dim_alpha > 0. {
            push(WindowPickerUiRenderElement::SolidColor(
                SolidColorRenderElement::from_buffer(
                    &buffers.dim,
                    Point::new(0., 0.),
                    1.,
                    Kind::Unspecified,
                ),
            ));
        }

        let mut tint = backdrop.color;
        tint.a *= progress as f32;
        buffers
            .tint
            .update(size, Color32F::from(tint.to_array_premul()));
        if tint.a > 0. {
            push(WindowPickerUiRenderElement::SolidColor(
                SolidColorRenderElement::from_buffer(
                    &buffers.tint,
                    Point::new(0., 0.),
                    1.,
                    Kind::Unspecified,
                ),
            ));
        }

        // Render elements are supplied front-to-back. Putting the framebuffer effect after the
        // overlays makes it capture the untouched desktop first; overlays and previews are then
        // drawn sharp on top during the reverse-order render pass.
        let effect_needed = backdrop.blur.on || backdrop.saturation != 1.;
        if effect_needed {
            if progress < 1. {
                self.framebuffer_effect.borrow_mut().damage();
            }
            let params = RenderParams {
                geometry: Rectangle::from_size(size),
                subregion: None,
                clip: Some((Rectangle::from_size(size), CornerRadius::default())),
                scale: output.current_scale().fractional_scale(),
            };
            let blur = backdrop.blur.on.then_some(BlurOptions {
                passes: backdrop.blur.passes,
                // Focus-pull: the blur radius ramps in with the open animation.
                offset: (backdrop.blur.offset * progress).round(),
            });
            let saturation = 1. + (backdrop.saturation - 1.) * progress;
            let effect = self.framebuffer_effect.borrow().render(
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

struct RenderedWindow<'a> {
    entry: &'a PickerEntry,
    mapped: &'a Mapped,
    texture: Option<PickerTexture>,
    label_size: Size<f64, Logical>,
}

fn picker_windows<'a>(
    niri: &'a Niri,
    session: &'a WindowPickerSession,
    label_config: &WindowPickerLabel,
) -> Vec<RenderedWindow<'a>> {
    let by_id: HashMap<_, _> = niri
        .layout
        .windows()
        .map(|(_, mapped)| (mapped.id(), mapped))
        .collect();

    session
        .entries
        .iter()
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

fn thumbnail_clip_radius(mapped: &Mapped, config: &niri_config::WindowPicker) -> CornerRadius {
    if config.card.on {
        let radius = config.card.corner_radius.max(0.) as f32;
        CornerRadius {
            top_left: radius,
            top_right: radius,
            bottom_right: 0.,
            bottom_left: 0.,
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

fn compute_grid(
    output: Size<f64, Logical>,
    inputs: &[GridInput],
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    if inputs.is_empty() || output.w <= 0. || output.h <= 0. {
        return None;
    }

    let area: Size<f64, Logical> = Size::from((
        output.w * config.area_width.clamp(0., 1.),
        output.h * config.area_height.clamp(0., 1.),
    ));
    if area.w <= 0. || area.h <= 0. {
        return None;
    }

    if inputs.len() == 1 {
        return compute_single_window(output, area, inputs[0], config);
    }

    let count = inputs.len();
    let chrome_scale = (52. / count as f64).sqrt().clamp(0.2, 1.);
    let gap = config.gap.max(0.) * chrome_scale;
    let label_gap = config.label.gap.max(0.) * chrome_scale;
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
        (output.w - metrics.size.w) / 2.,
        (output.h - metrics.size.h) / 2.,
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
        // Anchor the keycap to the card's top edge: one third sits inside the card.
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
    output: Size<f64, Logical>,
    area: Size<f64, Logical>,
    input: GridInput,
    config: &niri_config::WindowPicker,
) -> Option<GridLayout> {
    let label_gap = config.label.gap.max(0.);
    let info_height = if config.card.on {
        config.card.info.height.max(0.)
    } else {
        0.
    };
    let available_height = area.h - 2. * (input.label_size.h + label_gap);
    if available_height <= 0. || input.window_size.w <= 0. || input.window_size.h <= 0. {
        return None;
    }

    let scale = config
        .max_scale
        .clamp(0.001, 1.)
        .min(area.w / input.window_size.w)
        .min(available_height / (input.window_size.h + info_height));
    let preview_size = input.window_size.upscale(scale);
    let card_size = Size::from((preview_size.w, preview_size.h + info_height));
    let card_loc = Point::new((output.w - card_size.w) / 2., (output.h - card_size.h) / 2.);
    let preview = Rectangle::new(card_loc, preview_size);
    let info = Rectangle::new(
        card_loc + Point::from((0., preview_size.h)),
        Size::from((preview_size.w, info_height)),
    );
    let label_loc = Point::new(
        card_loc.x + (card_size.w - input.label_size.w) / 2.,
        card_loc.y - input.label_size.h * (1. - LABEL_OVERLAP),
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
        row_label_heights[row] = row_label_heights[row].max(input.label_size.h * chrome_scale);
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

    // The configured single letters cannot cover all windows. Switch every label
    // to a random two-letter combination so all labels keep the same input length.
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
    fastrand::shuffle(&mut labels);
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
    key: &InfoKey,
    scale: f64,
    font_scale: f64,
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

    let padding_x = config.padding_x * scale * font_scale;
    let padding_y = config.padding_y * scale * font_scale;
    let gap = 8. * scale * font_scale;

    let make_font = |size: f64| {
        let mut font = FontDescription::from_string(&config.font);
        font.set_absolute_size(size * scale * font_scale * f64::from(pango::SCALE));
        font
    };

    let workspace_font = make_font(config.workspace_size);
    let workspace_layout = pangocairo::functions::create_layout(&cr);
    workspace_layout.context().set_round_glyph_positions(false);
    workspace_layout.set_single_paragraph_mode(true);
    workspace_layout.set_font_description(Some(&workspace_font));
    workspace_layout.set_text(&key.workspace);
    let (workspace_w, _) = workspace_layout.pixel_size();
    let chip_w = workspace_w as f64;
    let chip_x = (width as f64 - padding_x - chip_w).max(padding_x);
    let chip_y = padding_y;
    cr.move_to(chip_x, chip_y);
    set_source_color(&cr, config.workspace_color);
    pangocairo::functions::show_layout(&cr, &workspace_layout);

    let title_font = make_font(config.title_size);
    let title_layout = pangocairo::functions::create_layout(&cr);
    title_layout.context().set_round_glyph_positions(false);
    title_layout.set_single_paragraph_mode(true);
    title_layout.set_font_description(Some(&title_font));
    title_layout.set_text(&key.title);
    title_layout.set_ellipsize(pango::EllipsizeMode::End);
    let title_width = (width as f64 - padding_x * 2. - chip_w - gap).max(1.);
    title_layout.set_width((title_width * f64::from(pango::SCALE)) as i32);
    cr.move_to(padding_x, padding_y);
    set_source_color(&cr, config.title_color);
    pangocairo::functions::show_layout(&cr, &title_layout);

    // The app id is independent of the title/workspace padding: centered across
    // the whole card, with a fixed 5 logical pixels above the card bottom.
    let app_font = make_font(config.app_id_size);
    let app_layout = pangocairo::functions::create_layout(&cr);
    app_layout.context().set_round_glyph_positions(false);
    app_layout.set_single_paragraph_mode(true);
    app_layout.set_font_description(Some(&app_font));
    app_layout.set_text(&key.app_id);
    app_layout.set_ellipsize(pango::EllipsizeMode::End);
    app_layout.set_alignment(pango::Alignment::Center);
    app_layout.set_width((width as f64 * f64::from(pango::SCALE)) as i32);
    let (_, app_h) = app_layout.pixel_size();
    let bottom_padding = 5. * scale;
    cr.move_to(0., (height as f64 - bottom_padding - app_h as f64).max(0.));
    set_source_color(&cr, config.app_id_color);
    pangocairo::functions::show_layout(&cr, &app_layout);

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

    // Color spans for the already-typed prefix letters. Labels are plain A-Z, so no
    // markup escaping is needed.
    let markup = if style.typed_len > 0 && label.len() > 1 {
        let typed = &label[..usize::from(style.typed_len)];
        let rest = &label[usize::from(style.typed_len)..];
        if style.highlighted {
            // On an inverted keycap the typed letters are underlined instead.
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
    // The darker bottom strip faking key travel.
    let key_depth = (scale * 2.).min(panel_height / 4.);

    // The highlighted state only changes the keycap face; the letter keeps the
    // configured text color so the label remains readable and consistent.
    let (face, text) = if style.highlighted {
        (accent, config.text_color)
    } else {
        (config.background_color, config.text_color)
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
    let top = lighten(face, 0.14);
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

    // Faint outline for definition against the backdrop.
    rounded_rect(&cr, x, y, panel_width, panel_height - key_depth, radius);
    let outline = if style.highlighted {
        lighten(accent, 0.35)
    } else {
        let mut color = config.border_color;
        color.a *= 0.45;
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
}
