use std::array;
use std::cell::RefCell;
use std::rc::Rc;

use glam::{Mat3, Vec2};
use niri_config::CornerRadius;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{Color32F, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::effect_buffer::EffectBuffer;
use crate::render_helpers::glass_tint::{extend_tint_uniforms, GlassTintOptions};
use crate::render_helpers::liquid_glass::{
    extend_liquid_glass_uniforms, geo_to_tex_linear, LiquidGlassOptions,
};
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::shaders::{mat3_uniform, Shaders};
use crate::render_helpers::{RenderCtx, RenderTarget};
use crate::utils::region::TransformedRegion;

#[derive(Debug)]
pub struct Xray {
    // The buffers are per-render-target to avoid constant rerendering when screencasting.
    pub background: [Rc<RefCell<EffectBuffer>>; RenderTarget::COUNT],
    pub backdrop: [Rc<RefCell<EffectBuffer>>; RenderTarget::COUNT],
    pub backdrop_color: Color32F,
    pub workspaces: Vec<(Rectangle<f64, Logical>, Color32F)>,
    /// Pointer position relative to this output's top-left corner, for interactive glass glow.
    pub pointer_pos: Option<Point<f64, Logical>>,
}

/// Position for drawing xray background.
#[derive(Debug, Clone, Copy)]
pub struct XrayPos {
    /// Position of geometry relative to the backdrop in zoomed coordinates.
    ///
    /// Should be upscaled by `zoom` to get position in backdrop coordinates.
    pub pos_in_backdrop: Point<f64, Logical>,

    /// Zoom factor between backdrop coordinates and geometry.
    pub zoom: f64,

    /// Optional Engine A 3D transform pivot (output-logical coords) applied around the workspace.
    pub pivot_3d: Point<f64, Logical>,

    /// Optional Engine A 3D anisotropic scale applied around `pivot_3d`.
    pub scale_3d: Scale<f64>,
}

impl XrayPos {
    pub fn new(pos_in_backdrop: Point<f64, Logical>, zoom: f64) -> Self {
        Self {
            pos_in_backdrop: pos_in_backdrop.downscale(zoom),
            zoom,
            pivot_3d: Point::new(0., 0.),
            scale_3d: Scale::from(1.),
        }
    }

    pub fn offset(mut self, offset: Point<f64, Logical>) -> Self {
        self.pos_in_backdrop += offset;
        self
    }

    pub fn with_3d(mut self, pivot_3d: Point<f64, Logical>, scale_3d: Scale<f64>) -> Self {
        self.pivot_3d = pivot_3d;
        self.scale_3d = scale_3d;
        self
    }

    fn map_backdrop_rect(&self, rect: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        let sx = self.scale_3d.x;
        let sy = self.scale_3d.y;
        if (sx - 1.).abs() < 1e-6 && (sy - 1.).abs() < 1e-6 {
            return rect;
        }
        let px = self.pivot_3d.x;
        let py = self.pivot_3d.y;
        Rectangle::new(
            Point::from((px + (rect.loc.x - px) * sx, py + (rect.loc.y - py) * sy)),
            Size::from((rect.size.w * sx, rect.size.h * sy)),
        )
    }
}

impl Default for XrayPos {
    fn default() -> Self {
        Self {
            pos_in_backdrop: Point::new(0., 0.),
            zoom: 1.,
            pivot_3d: Point::new(0., 0.),
            scale_3d: Scale::from(1.),
        }
    }
}

#[derive(Debug)]
pub struct XrayElement {
    buffer: Rc<RefCell<EffectBuffer>>,
    id: Id,
    geometry: Rectangle<f64, Logical>,
    src: Rectangle<f64, Buffer>,
    subregion: Option<TransformedRegion>,
    input_to_clip_geo: Mat3,
    clip_geo_size: Vec2,
    corner_radius: CornerRadius,
    scale: f32,
    blur_options: Option<BlurOptions>,
    noise: f32,
    saturation: f32,
    bg_color: Color32F,
    program: Option<GlesTexProgram>,
    tint: Option<GlassTintOptions>,
    liquid_glass: Option<LiquidGlassOptions>,
    cursor_pos_px: Option<[f32; 2]>,
    focus_progress: f32,
}

impl Xray {
    pub fn new() -> Self {
        Self {
            background: array::from_fn(|_| Rc::new(RefCell::new(EffectBuffer::new()))),
            backdrop: array::from_fn(|_| Rc::new(RefCell::new(EffectBuffer::new()))),
            backdrop_color: Color32F::TRANSPARENT,
            workspaces: Vec::new(),
            pointer_pos: None,
        }
    }

    pub fn render(
        &self,
        ctx: RenderCtx<GlesRenderer>,
        params: RenderParams,
        xray_pos: XrayPos,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
        tint_options: Option<GlassTintOptions>,
        liquid_glass_options: Option<LiquidGlassOptions>,
        cursor_pos_px: Option<[f32; 2]>,
        focus_progress: f32,
        push: &mut dyn FnMut(XrayElement),
    ) {
        let program = Shaders::get(ctx.renderer).postprocess_and_clip.clone();

        let zoom = xray_pos.zoom;
        let pos_in_backdrop = xray_pos.pos_in_backdrop.upscale(zoom);

        let (clip_geo, corner_radius) = params
            .clip
            .unwrap_or((params.geometry, CornerRadius::default()));

        let clip_offset = clip_geo.loc - params.geometry.loc;
        let clip_pos_in_backdrop = pos_in_backdrop + clip_offset.upscale(zoom);

        let geo_in_backdrop = Rectangle::new(pos_in_backdrop, params.geometry.size.upscale(zoom));
        let geo_in_backdrop_3d = xray_pos.map_backdrop_rect(geo_in_backdrop);

        let mut backdrop = self.backdrop[ctx.target as usize].borrow_mut();
        let backdrop_geo = Rectangle::from_size(backdrop.logical_size());
        let intersection_with_backdrop = backdrop_geo.intersection(geo_in_backdrop_3d);

        let mut skip_backdrop = intersection_with_backdrop.is_none();

        let mut background = self.background[ctx.target as usize].borrow_mut();
        let prev = background.commit();
        if background.prepare_with_blur(ctx.renderer, blur_options) {
            if background.commit() != prev {
                trace!("background damaged");
            }

            let clip_geo_size = Vec2::new(clip_geo.size.w as f32, clip_geo.size.h as f32);
            let buf_size = background.logical_size();

            for (ws_geo, bg_color) in &self.workspaces {
                let crop = if bg_color.is_opaque() && ws_geo.contains_rect(geo_in_backdrop) {
                    skip_backdrop = true;
                    Some(geo_in_backdrop)
                } else {
                    ws_geo.intersection(geo_in_backdrop)
                };

                let Some(crop) = crop else {
                    continue;
                };

                if bg_color.is_opaque()
                    && intersection_with_backdrop
                        .is_some_and(|backdrop| crop.contains_rect(backdrop))
                {
                    skip_backdrop = true;
                }

                let ws_zoom = ws_geo.size / buf_size;

                let src = Rectangle::new(crop.loc - ws_geo.loc, crop.size).downscale(ws_zoom);
                let src = src.to_buffer(background.scale(), Transform::Normal, &buf_size);

                let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);
                let pos_against_buf = (clip_pos_in_backdrop - ws_geo.loc).downscale(ws_zoom);
                let pos_against_buf = Vec2::new(pos_against_buf.x as f32, pos_against_buf.y as f32);
                let ws_zoom_vec = Vec2::new(ws_zoom.x as f32, ws_zoom.y as f32);
                let input_to_clip_geo = Mat3::from_scale(ws_zoom_vec / zoom as f32)
                    * Mat3::from_scale(buf_size / clip_geo_size)
                    * Mat3::from_translation(-pos_against_buf / buf_size);

                let mut geometry =
                    Rectangle::new(crop.loc - geo_in_backdrop.loc, crop.size).downscale(zoom);
                geometry.loc += params.geometry.loc;

                let elem = XrayElement {
                    buffer: self.background[ctx.target as usize].clone(),
                    id: background.id().clone(),
                    geometry,
                    src,
                    subregion: params.subregion.clone(),
                    input_to_clip_geo,
                    clip_geo_size,
                    corner_radius,
                    scale: params.scale as f32,
                    blur_options,
                    noise,
                    saturation,
                    bg_color: *bg_color,
                    program: program.clone(),
                    tint: tint_options,
                    liquid_glass: liquid_glass_options,
                    cursor_pos_px,
                    focus_progress,
                };
                push(elem);
            }
        }

        if skip_backdrop {
            return;
        }

        let prev = backdrop.commit();
        if backdrop.prepare_with_blur(ctx.renderer, blur_options) {
            if backdrop.commit() != prev {
                trace!("backdrop damaged");
            }

            let buf_size = backdrop.logical_size();
            let src = geo_in_backdrop_3d.to_buffer(backdrop.scale(), Transform::Normal, &buf_size);

            let mut clip_geo_in_backdrop = Rectangle::new(clip_offset, clip_geo.size).upscale(zoom);
            clip_geo_in_backdrop.loc += geo_in_backdrop.loc;
            let clip_geo_in_backdrop = xray_pos.map_backdrop_rect(clip_geo_in_backdrop);

            let clip_pos_in_backdrop = Vec2::new(
                clip_geo_in_backdrop.loc.x as f32,
                clip_geo_in_backdrop.loc.y as f32,
            );
            let clip_geo_size = Vec2::new(
                clip_geo_in_backdrop.size.w as f32,
                clip_geo_in_backdrop.size.h as f32,
            );

            let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);
            let input_to_clip_geo = Mat3::from_scale(buf_size / clip_geo_size)
                * Mat3::from_translation(-clip_pos_in_backdrop / buf_size);

            let eff_scale = ((xray_pos.scale_3d.x * xray_pos.scale_3d.y).sqrt() * zoom) as f32;
            let elem = XrayElement {
                buffer: self.backdrop[ctx.target as usize].clone(),
                id: backdrop.id().clone(),
                geometry: params.geometry,
                src,
                subregion: params.subregion.clone(),
                input_to_clip_geo,
                clip_geo_size,
                corner_radius: corner_radius.scaled_by(eff_scale),
                scale: params.scale as f32,
                blur_options,
                noise,
                saturation,
                bg_color: self.backdrop_color,
                program: program.clone(),
                tint: tint_options,
                liquid_glass: liquid_glass_options,
                cursor_pos_px: cursor_pos_px.map(|[x, y]| [x * zoom as f32, y * zoom as f32]),
                focus_progress,
            };
            push(elem);
        }
    }
}

impl XrayElement {
    fn compute_uniforms(&self, has_sharp_tex: bool) -> Vec<Uniform<'static>> {
        let mut uniforms = vec![
            Uniform::new("niri_scale", self.scale),
            Uniform::new("geo_size", <[f32; 2]>::from(self.clip_geo_size)),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", self.input_to_clip_geo),
            Uniform::new(
                "geo_to_tex_linear",
                geo_to_tex_linear(self.input_to_clip_geo),
            ),
            Uniform::new("tex_sharp", 1i32),
            Uniform::new("has_sharp_tex", if has_sharp_tex { 1.0f32 } else { 0.0f32 }),
            Uniform::new("noise", self.noise),
            Uniform::new("saturation", self.saturation),
            Uniform::new("bg_color", self.bg_color.components()),
        ];

        extend_tint_uniforms(&mut uniforms, self.tint);
        extend_liquid_glass_uniforms(
            &mut uniforms,
            self.liquid_glass,
            self.cursor_pos_px,
            self.focus_progress,
        );

        uniforms
    }
}

pub(crate) fn bind_sharp_texture(
    frame: &mut GlesFrame<'_, '_>,
    sharp_tex: Option<&GlesTexture>,
) -> Result<(), GlesError> {
    if let Some(sharp) = sharp_tex {
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, sharp.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
    }
    Ok(())
}

pub(crate) fn unbind_sharp_texture(
    frame: &mut GlesFrame<'_, '_>,
    had_sharp: bool,
) -> Result<(), GlesError> {
    if had_sharp {
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
    }
    Ok(())
}

impl Element for XrayElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.buffer.borrow().commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
}

impl RenderElement<GlesRenderer> for XrayElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let mut buffer = self.buffer.borrow_mut();
        let (texture, sharp_tex) = match buffer.render_pair_with_blur(frame, self.blur_options) {
            Ok(x) => x,
            Err(err) => {
                warn!("error rendering effect buffer: {err:?}");
                return Ok(());
            }
        };

        let mut filtered_damage = Vec::new();
        let damage = if let Some(subregion) = &self.subregion {
            let src_to_geo = self.geometry.size / self.src.size;

            let mut crop = src;
            crop.loc -= self.src.loc;
            crop = crop.upscale(src_to_geo);
            let mut crop = crop.to_logical(1., Transform::Normal, &Size::default());
            crop.loc += self.geometry.loc;

            subregion.filter_damage(crop, dst, damage, &mut filtered_damage);

            if filtered_damage.is_empty() {
                return Ok(());
            }
            &filtered_damage[..]
        } else {
            damage
        };

        // Scale src proportionally if the blurred texture is rendered at half resolution.
        let full_size = sharp_tex
            .as_ref()
            .map(|s| s.size())
            .unwrap_or_else(|| texture.size());
        let tex_scale = Scale {
            x: texture.size().w as f64 / full_size.w.max(1) as f64,
            y: texture.size().h as f64 / full_size.h.max(1) as f64,
        };
        let render_src = src.upscale(tex_scale);

        let use_sharp =
            sharp_tex.is_some() && self.liquid_glass.is_some_and(|lg| lg.bevel_clarity > 0.001);
        let uniforms = self
            .program
            .is_some()
            .then(|| self.compute_uniforms(use_sharp));
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        bind_sharp_texture(frame, if use_sharp { sharp_tex.as_ref() } else { None })?;
        let res = frame.render_texture_from_to(
            &texture,
            render_src,
            dst,
            damage,
            &[],
            Transform::Normal,
            1.,
            self.program.as_ref(),
            uniforms,
        );
        let _ = unbind_sharp_texture(frame, use_sharp);
        res
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for XrayElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::draw(
            &self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}
