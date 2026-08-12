use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI};
use std::rc::Rc;
use std::time::Duration;

use anyhow::ensure;
use niri_config::{Color, Config, CornerRadius, WindowPickerLabel};
use pango::FontDescription;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::Color32F;
use smithay::input::keyboard::Keysym;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::Clock;
use crate::layout::{LayoutElement as _, LayoutElementRenderElement};
use crate::niri::Niri;
use crate::niri_render_elements;
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::{RenderCtx, RenderTarget};
use crate::utils::{output_size, to_physical_precise_round};
use crate::window::mapped::MappedId;
use crate::window::Mapped;

const MAX_WINDOWS: usize = 26 * 26;
const PREFIX_DIM_ALPHA: f32 = 0.22;

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
        SolidColor = SolidColorRenderElement,
        FramebufferEffect = FramebufferEffectElement,
    }
}

#[derive(Debug)]
struct PickerEntry {
    id: MappedId,
    label: String,
}

#[derive(Debug)]
pub struct WindowPickerSession {
    entries: Vec<PickerEntry>,
    output: Output,
    prefix: Option<char>,
    clock: Clock,
    opened_at: Duration,
}

impl WindowPickerSession {
    pub fn collect(niri: &Niri) -> Option<Self> {
        let output = niri.layout.active_output()?.clone();
        let mut windows = Vec::new();
        let mut seen = HashSet::new();

        for (_, mapped) in niri.layout.windows() {
            let size = mapped.size();
            if size.w > 0 && size.h > 0 && seen.insert(mapped.id()) {
                windows.push((mapped.id(), mapped.get_focus_timestamp()));
            }
        }

        windows.sort_by(|(_, left), (_, right)| right.cmp(left));
        windows.truncate(MAX_WINDOWS);
        if windows.is_empty() {
            return None;
        }

        let labels = make_labels(windows.len());
        let entries = windows
            .into_iter()
            .zip(labels)
            .map(|((id, _), label)| PickerEntry { id, label })
            .collect();
        let clock = niri.clock.clone();
        let opened_at = clock.now();

        Some(Self {
            entries,
            output,
            prefix: None,
            clock,
            opened_at,
        })
    }

    fn progress(&self, duration_ms: u16) -> f64 {
        if duration_ms == 0 {
            return 1.;
        }

        let elapsed = self.clock.now().saturating_sub(self.opened_at);
        let progress = elapsed.as_secs_f64() / (f64::from(duration_ms) / 1000.);
        let progress = progress.clamp(0., 1.);
        1. - (1. - progress).powi(3)
    }

    fn animation_is_ongoing(&self, duration_ms: u16) -> bool {
        duration_ms != 0
            && self.clock.now().saturating_sub(self.opened_at)
                < Duration::from_millis(u64::from(duration_ms))
    }
}

#[derive(Debug, Default)]
struct LabelCache {
    scale: f64,
    config: Option<WindowPickerLabel>,
    textures: HashMap<String, Option<PickerTexture>>,
}

impl LabelCache {
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        label: &str,
        scale: f64,
        config: &WindowPickerLabel,
    ) -> Option<PickerTexture> {
        if self.scale != scale || self.config.as_ref() != Some(config) {
            self.scale = scale;
            self.config = Some(config.clone());
            self.textures.clear();
        }

        self.textures
            .entry(label.to_owned())
            .or_insert_with(|| generate_label_texture(renderer, label, scale, config).ok())
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
            .get(label)
            .and_then(Option::as_ref)
            .map(PickerTexture::logical_size)
    }

    fn clear(&mut self) {
        self.config = None;
        self.textures.clear();
    }
}

#[derive(Debug, Default)]
struct BackdropBuffers {
    dim: SolidColorBuffer,
    tint: SolidColorBuffer,
}

pub struct WindowPickerUi {
    session: Option<WindowPickerSession>,
    config: Rc<RefCell<Config>>,
    label_cache: RefCell<LabelCache>,
    backdrop_buffers: RefCell<HashMap<Output, BackdropBuffers>>,
    framebuffer_effect: RefCell<FramebufferEffect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowPickerKeyResult {
    Handled,
    Selected(MappedId),
}

impl WindowPickerUi {
    pub fn new(config: Rc<RefCell<Config>>) -> Self {
        Self {
            session: None,
            config,
            label_cache: RefCell::new(LabelCache::default()),
            backdrop_buffers: RefCell::new(HashMap::new()),
            framebuffer_effect: RefCell::new(FramebufferEffect::new()),
        }
    }

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub fn output(&self) -> Option<&Output> {
        self.session.as_ref().map(|session| &session.output)
    }

    pub fn open(&mut self, session: WindowPickerSession) {
        self.label_cache.get_mut().clear();
        self.framebuffer_effect.get_mut().damage();
        self.session = Some(session);
    }

    pub fn close(&mut self) -> bool {
        let was_open = self.session.take().is_some();
        if was_open {
            self.backdrop_buffers.get_mut().clear();
            self.framebuffer_effect.get_mut().damage();
        }
        was_open
    }

    pub fn update_config(&mut self) {
        self.label_cache.get_mut().clear();
        self.framebuffer_effect.get_mut().damage();
    }

    pub fn retain_windows(&mut self, live_ids: &HashSet<MappedId>) -> bool {
        let Some(session) = &mut self.session else {
            return false;
        };

        let previous_len = session.entries.len();
        session.entries.retain(|entry| live_ids.contains(&entry.id));
        if session.entries.len() == previous_len {
            return false;
        }

        let is_empty = session.entries.is_empty();
        if !is_empty {
            // A removed entry can invalidate the currently entered two-letter prefix.
            session.prefix = None;
        }

        if is_empty {
            self.close();
        }
        true
    }

    pub fn handle_key(&mut self, raw: Keysym) -> WindowPickerKeyResult {
        let Some(session) = &mut self.session else {
            return WindowPickerKeyResult::Handled;
        };

        if raw == Keysym::Escape {
            self.close();
            return WindowPickerKeyResult::Handled;
        }

        if raw == Keysym::BackSpace {
            session.prefix = None;
            return WindowPickerKeyResult::Handled;
        }

        let Some(letter) = letter_from_keysym(raw) else {
            return WindowPickerKeyResult::Handled;
        };

        // Labels stay stable while the picker is open, including when windows close. Determine
        // whether this session uses one or two letters from the labels rather than the live count.
        if session
            .entries
            .first()
            .is_some_and(|entry| entry.label.len() == 1)
        {
            let selected = session
                .entries
                .iter()
                .find(|entry| entry.label.starts_with(letter))
                .map(|entry| entry.id);
            if let Some(id) = selected {
                return WindowPickerKeyResult::Selected(id);
            }
            return WindowPickerKeyResult::Handled;
        }

        if let Some(prefix) = session.prefix {
            let selected = session
                .entries
                .iter()
                .find(|entry| {
                    let mut chars = entry.label.chars();
                    chars.next() == Some(prefix) && chars.next() == Some(letter)
                })
                .map(|entry| entry.id);
            if let Some(id) = selected {
                return WindowPickerKeyResult::Selected(id);
            }
        } else if session
            .entries
            .iter()
            .any(|entry| entry.label.starts_with(letter))
        {
            session.prefix = Some(letter);
        }

        WindowPickerKeyResult::Handled
    }

    pub fn window_under(
        &self,
        niri: &Niri,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<MappedId> {
        let session = self.session.as_ref()?;
        if output != &session.output {
            return None;
        }

        let config = self.config.borrow().window_picker.clone();
        let progress = session.progress(config.animation_ms);
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
                let preview = animate_rect(placement.preview, output_size, progress);
                preview
                    .contains(pos_within_output)
                    .then_some(window.entry.id)
            })
    }

    pub fn are_animations_ongoing(&self) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        session.animation_is_ongoing(self.config.borrow().window_picker.animation_ms)
    }

    pub fn render_output<R: NiriRenderer>(
        &self,
        niri: &Niri,
        output: &Output,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let Some(session) = &self.session else {
            return;
        };
        if ctx.target != RenderTarget::Output {
            return;
        }

        let config = self.config.borrow().window_picker.clone();
        let progress = session.progress(config.animation_ms);
        let alpha = progress as f32;
        let size = output_size(output);
        let scale = output.current_scale().fractional_scale();

        if *output == session.output {
            let mut windows = picker_windows(niri, session, &config.label);
            for window in &mut windows {
                let texture = self.label_cache.borrow_mut().get(
                    ctx.as_gles().renderer,
                    &window.entry.label,
                    scale,
                    &config.label,
                );
                if let Some(texture) = &texture {
                    window.label_size = texture.logical_size();
                }
                window.texture = texture;
            }

            let inputs = grid_inputs(&windows);

            if let Some(layout) = compute_grid(size, &inputs, &config) {
                for (window, placement) in windows.iter().zip(layout.placements) {
                    let entry_alpha = if session
                        .prefix
                        .is_some_and(|prefix| !window.entry.label.starts_with(prefix))
                    {
                        PREFIX_DIM_ALPHA * alpha
                    } else {
                        alpha
                    };
                    let preview = animate_rect(placement.preview, size, progress);
                    let final_center =
                        placement.preview.loc + placement.preview.size.to_point().upscale(0.5);
                    let animated_center = preview.loc + preview.size.to_point().upscale(0.5);
                    let label_loc = placement.label.loc + (animated_center - final_center);
                    if let Some(texture) = &window.texture {
                        let texture = TextureRenderElement::from_texture_buffer(
                            texture.clone(),
                            label_loc,
                            entry_alpha,
                            None,
                            Some(placement.label.size),
                            Kind::Unspecified,
                        );
                        push(WindowPickerUiRenderElement::Label(
                            PrimaryGpuTextureRenderElement(texture),
                        ));
                    }

                    render_thumbnail(ctx.r(), scale, window.mapped, preview, entry_alpha, push);
                }
            }
        }

        self.render_backdrop(output, size, progress, &config, push);
    }

    fn render_backdrop<R: NiriRenderer>(
        &self,
        output: &Output,
        size: Size<f64, Logical>,
        progress: f64,
        config: &niri_config::WindowPicker,
        push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
    ) {
        let backdrop = config.backdrop;
        let mut buffers = self.backdrop_buffers.borrow_mut();
        let buffers = buffers.entry(output.clone()).or_default();

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
                offset: backdrop.blur.offset,
            });
            let saturation = 1. + (backdrop.saturation - 1.) * progress;
            let effect = self.framebuffer_effect.borrow().render(
                None,
                params,
                blur,
                0.,
                saturation as f32,
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

fn render_thumbnail<R: NiriRenderer>(
    mut ctx: RenderCtx<R>,
    output_scale: f64,
    mapped: &Mapped,
    preview: Rectangle<f64, Logical>,
    alpha: f32,
    push: &mut dyn FnMut(WindowPickerUiRenderElement<R>),
) {
    let geo = Rectangle::from_size(mapped.size().to_f64());
    let radius = if mapped.sizing_mode().is_normal() {
        mapped.geometry_corner_radius()
    } else {
        CornerRadius::default()
    };

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

fn animate_rect(
    rect: Rectangle<f64, Logical>,
    output: Size<f64, Logical>,
    progress: f64,
) -> Rectangle<f64, Logical> {
    let output_center = output.to_point().upscale(0.5);
    let target_center = rect.loc + rect.size.to_point().upscale(0.5);
    let mut direction = target_center - output_center;
    if direction.x.abs() < f64::EPSILON && direction.y.abs() < f64::EPSILON {
        direction.y = -1.;
    }

    let x_factor = if direction.x < 0. {
        (-rect.size.w / 2. - output_center.x) / direction.x
    } else if direction.x > 0. {
        (output.w + rect.size.w / 2. - output_center.x) / direction.x
    } else {
        f64::INFINITY
    };
    let y_factor = if direction.y < 0. {
        (-rect.size.h / 2. - output_center.y) / direction.y
    } else if direction.y > 0. {
        (output.h + rect.size.h / 2. - output_center.y) / direction.y
    } else {
        f64::INFINITY
    };
    let entry_center = output_center + direction.upscale(x_factor.min(y_factor));
    let center = entry_center + (target_center - entry_center).upscale(progress);

    let scale = 0.9 + progress * 0.1;
    let size = rect.size.upscale(scale);
    Rectangle::new(center - size.to_point().upscale(0.5), size)
}

#[derive(Debug, Clone, Copy)]
struct GridInput {
    window_size: Size<f64, Logical>,
    label_size: Size<f64, Logical>,
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    preview: Rectangle<f64, Logical>,
    label: Rectangle<f64, Logical>,
}

#[derive(Debug)]
struct GridLayout {
    placements: Vec<Placement>,
}

#[derive(Debug)]
struct GridMetrics {
    col_widths: Vec<f64>,
    row_label_heights: Vec<f64>,
    row_window_heights: Vec<f64>,
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
    let max_scale = config.max_scale.clamp(0.001, 1.);
    let aspect = (area.w / area.h).max(0.01);
    let max_cols = ((count as f64 * aspect).sqrt() * 2. + 2.).ceil() as usize;
    let max_cols = max_cols.clamp(1, count);

    let mut best: Option<(usize, f64, f64, GridMetrics)> = None;
    for cols in 1..=max_cols {
        let fits = |window_scale| {
            let metrics = grid_metrics(inputs, cols, window_scale, chrome_scale, gap, label_gap);
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

    let mut row_y = Vec::with_capacity(metrics.row_window_heights.len());
    let mut y = origin.y;
    for row in 0..metrics.row_window_heights.len() {
        row_y.push(y);
        y += metrics.row_label_heights[row] + label_gap + metrics.row_window_heights[row] + gap;
    }

    let mut placements = Vec::with_capacity(inputs.len());
    for (idx, input) in inputs.iter().enumerate() {
        let col = idx % cols;
        let row = idx / cols;
        let preview_size = input.window_size.upscale(window_scale);
        let preview_loc = Point::new(
            col_x[col] + (metrics.col_widths[col] - preview_size.w) / 2.,
            row_y[row]
                + metrics.row_label_heights[row]
                + label_gap
                + (metrics.row_window_heights[row] - preview_size.h) / 2.,
        );
        let label_size = input.label_size.upscale(chrome_scale);
        let label_loc = Point::new(
            preview_loc.x + (preview_size.w - label_size.w) / 2.,
            preview_loc.y - label_gap - label_size.h,
        );
        placements.push(Placement {
            preview: Rectangle::new(preview_loc, preview_size),
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
    let available_height = area.h - 2. * (input.label_size.h + label_gap);
    if available_height <= 0. || input.window_size.w <= 0. || input.window_size.h <= 0. {
        return None;
    }

    let scale = config
        .max_scale
        .clamp(0.001, 1.)
        .min(area.w / input.window_size.w)
        .min(available_height / input.window_size.h);
    let preview_size = input.window_size.upscale(scale);
    let preview_loc = Point::new(
        (output.w - preview_size.w) / 2.,
        (output.h - preview_size.h) / 2.,
    );
    let label_loc = Point::new(
        preview_loc.x + (preview_size.w - input.label_size.w) / 2.,
        preview_loc.y - label_gap - input.label_size.h,
    );

    Some(GridLayout {
        placements: vec![Placement {
            preview: Rectangle::new(preview_loc, preview_size),
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
) -> GridMetrics {
    let rows = inputs.len().div_ceil(cols);
    let mut col_widths = vec![0_f64; cols];
    let mut row_label_heights = vec![0_f64; rows];
    let mut row_window_heights = vec![0_f64; rows];

    for (idx, input) in inputs.iter().enumerate() {
        let col = idx % cols;
        let row = idx / cols;
        col_widths[col] = col_widths[col]
            .max((input.window_size.w * window_scale).max(input.label_size.w * chrome_scale));
        row_label_heights[row] = row_label_heights[row].max(input.label_size.h * chrome_scale);
        row_window_heights[row] = row_window_heights[row].max(input.window_size.h * window_scale);
    }

    let width = col_widths.iter().sum::<f64>() + gap * cols.saturating_sub(1) as f64;
    let height = row_label_heights.iter().sum::<f64>()
        + row_window_heights.iter().sum::<f64>()
        + label_gap * rows as f64
        + gap * rows.saturating_sub(1) as f64;

    GridMetrics {
        col_widths,
        row_label_heights,
        row_window_heights,
        size: Size::from((width, height)),
    }
}

fn make_labels(count: usize) -> Vec<String> {
    if count <= 26 {
        return (0..count)
            .map(|idx| char::from(b'A' + idx as u8).to_string())
            .collect();
    }

    (0..count.min(MAX_WINDOWS))
        .map(|idx| {
            let first = char::from(b'A' + (idx / 26) as u8);
            let second = char::from(b'A' + (idx % 26) as u8);
            format!("{first}{second}")
        })
        .collect()
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

fn generate_label_texture(
    renderer: &mut GlesRenderer,
    label: &str,
    scale: f64,
    config: &WindowPickerLabel,
) -> anyhow::Result<PickerTexture> {
    let _span = tracy_client::span!("window_picker::generate_label_texture");

    let mut font = FontDescription::from_string(&config.font);
    font.set_absolute_size(config.size * scale * f64::from(pango::SCALE));

    let dummy = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&dummy)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_text(label);
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

    rounded_rect(&cr, x, y + scale * 2., panel_width, panel_height, radius);
    cr.set_source_rgba(0., 0., 0., 0.38);
    cr.fill()?;

    rounded_rect(&cr, x, y, panel_width, panel_height, radius);
    set_source_color(&cr, config.background_color);
    cr.fill_preserve()?;
    set_source_color(&cr, config.border_color);
    cr.set_line_width((scale * 1.25).max(1.));
    cr.stroke()?;

    if panel_width > radius * 2. + 2. {
        cr.move_to(x + radius, y + scale);
        cr.line_to(x + panel_width - radius, y + scale);
        cr.set_source_rgba(1., 1., 1., 0.16);
        cr.set_line_width(scale.max(1.));
        cr.stroke()?;
    }

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_text(label);
    cr.move_to(f64::from(shadow + padding_x), f64::from(shadow + padding_y));
    set_source_color(&cr, config.text_color);
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
        assert_eq!(make_labels(3), ["A", "B", "C"]);
        let labels = make_labels(28);
        assert_eq!(labels[0], "AA");
        assert_eq!(labels[25], "AZ");
        assert_eq!(labels[26], "BA");
        assert_eq!(labels[27], "BB");
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
    fn single_window_preview_is_exactly_centered() {
        let config = niri_config::WindowPicker::default();
        let output = Size::from((1920., 1080.));
        let input = GridInput {
            window_size: Size::from((1000., 700.)),
            label_size: Size::from((60., 44.)),
        };
        let placement = compute_grid(output, &[input], &config).unwrap().placements[0];
        let center = placement.preview.loc + placement.preview.size.to_point().upscale(0.5);

        assert_eq!(center, output.to_point().upscale(0.5));
    }
}
