use std::cell::RefCell;
use std::sync::Once;

use glam::{Mat3, Vec2};
use niri_config::CornerRadius;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    ffi, Capability, GlesError, GlesFrame, GlesRenderer, GlesTexture, Uniform,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame as _, FrameContext, Offscreen, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::{Blur, BlurOptions};
use crate::render_helpers::glass_tint::{extend_tint_uniforms, GlassTintOptions};
use crate::render_helpers::liquid_glass::{
    extend_liquid_glass_uniforms, geo_to_tex_linear, LiquidGlassOptions,
};
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::resources::Resources;
use crate::render_helpers::shaders::{mat3_uniform, Shaders};
use crate::render_helpers::xray::{bind_sharp_texture, unbind_sharp_texture};
use crate::utils::region::TransformedRegion;

#[derive(Debug)]
pub struct FramebufferEffect {
    id: Id,
    commit: CommitCounter,
}

/// Optional 3D perspective/rotation clip for blurred Window Picker cards so the live blurred
/// glass surface moves and tilts in exact lockstep with `picker_card.frag`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerspectiveClip {
    pub card_size: [f32; 2],
    pub dest_center: [f32; 2],
    pub persp_half_size: [f32; 2],
    pub angle: f32,
    pub card_scale: f32,
    pub tilt: [f32; 2],
    pub flip_y: f32,
    pub alpha: f32,
}

#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    corner_radius: CornerRadius,
    subregion: Option<TransformedRegion>,
    scale: f32,
    blur_options: Option<BlurOptions>,
    noise: f32,
    saturation: f32,
    tint: Option<GlassTintOptions>,
    liquid_glass: Option<LiquidGlassOptions>,
    cursor_pos_px: Option<[f32; 2]>,
    focus_progress: f32,
    perspective: Option<PerspectiveClip>,
}

#[derive(Debug)]
struct Inner {
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    intermediate: Option<GlesTexture>,
    /// Normalized sub-rect inside `framebuffer` / `intermediate` corresponding to `clamped_dst`
    /// (in buffer orientation). Equals `(0, 0, 1, 1)` when halo padding is 0.
    sub_uv: Rectangle<f64, Logical>,
    /// Reusable storage for subregion-filtered damage rects.
    subregion_damage: Vec<Rectangle<i32, Physical>>,
}

impl FramebufferEffect {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
        }
    }

    pub fn damage(&mut self) {
        self.commit.increment();
    }

    pub fn render(
        &self,
        ns: Option<usize>,
        params: RenderParams,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
        tint: Option<GlassTintOptions>,
        liquid_glass: Option<LiquidGlassOptions>,
    ) -> FramebufferEffectElement {
        self.render_interactive(
            ns,
            params,
            blur_options,
            noise,
            saturation,
            tint,
            liquid_glass,
            None,
            0.,
        )
    }

    pub fn render_interactive(
        &self,
        ns: Option<usize>,
        params: RenderParams,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
        tint: Option<GlassTintOptions>,
        liquid_glass: Option<LiquidGlassOptions>,
        cursor_pos_px: Option<[f32; 2]>,
        focus_progress: f32,
    ) -> FramebufferEffectElement {
        let (clip_geo, corner_radius) = params
            .clip
            .unwrap_or((params.geometry, CornerRadius::default()));

        let mut id = self.id.clone();
        if let Some(ns) = ns {
            id = id.namespaced(ns);
        }

        FramebufferEffectElement {
            id,
            commit: self.commit,
            geometry: params.geometry,
            clip_geo,
            corner_radius,
            subregion: params.subregion,
            scale: params.scale as f32,
            blur_options,
            noise,
            saturation,
            tint,
            liquid_glass,
            cursor_pos_px,
            focus_progress,
            perspective: None,
        }
    }

    pub fn render_perspective(
        &self,
        ns: Option<usize>,
        params: RenderParams,
        blur_options: Option<BlurOptions>,
        perspective: PerspectiveClip,
    ) -> FramebufferEffectElement {
        let mut elem = self.render(ns, params, blur_options, 0., 1., None, None);
        elem.perspective = Some(perspective);
        elem
    }
}

impl FramebufferEffectElement {
    /// Logical padding around the captured window so Kawase blur and Liquid Glass refraction
    /// can sample real pixels beyond the window border without dark edges or CLAMP_TO_EDGE smear.
    fn halo_logical(&self) -> f64 {
        let blur_halo = self.blur_options.map_or(0.0, |b| {
            let passes = b.passes.clamp(1, 6);
            b.offset * f64::from(1u32 << (passes - 1))
        });
        let glass_halo = self.liquid_glass.map_or(0.0, |lg| {
            if lg.refraction_strength > 0.0 || lg.interior_warp.abs() > 0.0001 {
                lg.thickness.max(lg.bevel_px).max(16.0)
            } else {
                0.0
            }
        });
        (blur_halo + glass_halo).clamp(0.0, 48.0)
    }

    fn compute_input_to_clip_geo(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
        sub_uv: Rectangle<f64, Logical>,
    ) -> Mat3 {
        let offset = crop.loc - (self.clip_geo.loc - self.geometry.loc);
        let offset = Vec2::new(offset.x as f32, offset.y as f32);
        let crop_size = Vec2::new(crop.size.w as f32, crop.size.h as f32);
        let clip_size = Vec2::new(self.clip_geo.size.w as f32, self.clip_geo.size.h as f32);

        // Map v_coords inside halo-padded `sub_uv` back to [0, 1] inside `clamped_dst`.
        let sub_loc = Vec2::new(sub_uv.loc.x as f32, sub_uv.loc.y as f32);
        let sub_size = Vec2::new(
            (sub_uv.size.w as f32).max(1e-5),
            (sub_uv.size.h as f32).max(1e-5),
        );
        let sub_to_unit = Mat3::from_scale(Vec2::ONE / sub_size) * Mat3::from_translation(-sub_loc);

        // Revert the effect of the texture transform.
        let transform_mat = Mat3::from_translation(Vec2::new(0.5, 0.5))
            * transform.matrix()
            * Mat3::from_translation(Vec2::new(-0.5, -0.5));

        // Map [0, 1] inside crop -> [0, 1] inside clip_geo.
        Mat3::from_scale(crop_size / clip_size)
            * Mat3::from_translation(offset / crop_size)
            * transform_mat
            * sub_to_unit
    }

    fn compute_perspective_uniforms(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
        sub_uv: Rectangle<f64, Logical>,
        persp: PerspectiveClip,
    ) -> Vec<Uniform<'static>> {
        let input_to_clip_geo = self.compute_input_to_clip_geo(crop, transform, sub_uv);
        let clip_geo_size = (self.clip_geo.size.w as f32, self.clip_geo.size.h as f32);

        vec![
            Uniform::new("niri_scale", self.scale),
            Uniform::new("geo_size", clip_geo_size),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", input_to_clip_geo),
            Uniform::new("pb_card_size", persp.card_size),
            Uniform::new("pb_dest_center", persp.dest_center),
            Uniform::new("pb_persp_half_size", persp.persp_half_size),
            Uniform::new("pb_angle", persp.angle),
            Uniform::new("pb_card_scale", persp.card_scale),
            Uniform::new("pb_tilt", persp.tilt),
            Uniform::new("pb_flip_y", persp.flip_y),
        ]
    }

    fn compute_uniforms(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
        sub_uv: Rectangle<f64, Logical>,
        has_sharp_tex: bool,
    ) -> Vec<Uniform<'static>> {
        let input_to_clip_geo = self.compute_input_to_clip_geo(crop, transform, sub_uv);
        let clip_geo_size = (self.clip_geo.size.w as f32, self.clip_geo.size.h as f32);

        let mut uniforms = vec![
            Uniform::new("niri_scale", self.scale),
            Uniform::new("geo_size", clip_geo_size),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", input_to_clip_geo),
            Uniform::new("geo_to_tex_linear", geo_to_tex_linear(input_to_clip_geo)),
            Uniform::new("tex_sharp", 1i32),
            Uniform::new("has_sharp_tex", if has_sharp_tex { 1.0f32 } else { 0.0f32 }),
            Uniform::new("noise", self.noise),
            Uniform::new("saturation", self.saturation),
            Uniform::new("bg_color", [0f32, 0., 0., 0.]),
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

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("FramebufferEffectElement::capture_framebuffer");
        let location = gpu_span_location!("FramebufferEffectElement::capture_framebuffer");
        frame.with_gpu_span(location, |frame| {
            let output_rect = Rectangle::from_size(frame.output_size());
            let transform = frame.transformation();

            let supports_blit = frame.capabilities().contains(&Capability::Blit);
            let pooled_blit_fbo = Resources::get(frame).map(|r| r.borrow().blit_fbo);
            let mut guard = frame.renderer();

            let inner = cache
                .get_or_insert::<RefCell<Inner>, _>(|| RefCell::new(Inner::new(guard.as_mut())));
            let mut inner = inner.borrow_mut();
            let inner = &mut *inner;

            inner.intermediate = None;
            inner.sub_uv = Rectangle::new(Point::new(0., 0.), Size::from((1., 1.)));

            if !supports_blit {
                static WARN: Once = Once::new();
                WARN.call_once(|| {
                    warn!("non-xray background effects disabled: renderer does not support framebuffer blitting");
                });
                return Ok(());
            }

            let clamped_dst = match dst.intersection(output_rect) {
                Some(clamped) if clamped.size.w > 0 && clamped.size.h > 0 => clamped,
                _ => return Ok(()),
            };

            // Expand capture region by an outer halo (clamped to output bounds) so blur and
            // liquid glass refraction sample real neighboring pixels outside the window border.
            let halo_phys = (self.halo_logical() * f64::from(self.scale)).round() as i32;
            let expanded_dst = if halo_phys > 0 {
                let expanded = Rectangle::new(
                    Point::from((clamped_dst.loc.x - halo_phys, clamped_dst.loc.y - halo_phys)),
                    Size::from((
                        clamped_dst.size.w + halo_phys * 2,
                        clamped_dst.size.h + halo_phys * 2,
                    )),
                );
                expanded.intersection(output_rect).unwrap_or(clamped_dst)
            } else {
                clamped_dst
            };

            let clamp_scale = Scale {
                x: expanded_dst.size.w as f64 / dst.size.w.max(1) as f64,
                y: expanded_dst.size.h as f64 / dst.size.h.max(1) as f64,
            };

            let exp_buf = transform.transform_rect_in(expanded_dst, &output_rect.size);
            let clamp_buf = transform.transform_rect_in(clamped_dst, &output_rect.size);

            inner.sub_uv = Rectangle::new(
                Point::new(
                    (clamp_buf.loc.x - exp_buf.loc.x) as f64 / exp_buf.size.w.max(1) as f64,
                    (clamp_buf.loc.y - exp_buf.loc.y) as f64 / exp_buf.size.h.max(1) as f64,
                ),
                Size::from((
                    clamp_buf.size.w as f64 / exp_buf.size.w.max(1) as f64,
                    clamp_buf.size.h as f64 / exp_buf.size.h.max(1) as f64,
                )),
            );

            let size: Size<i32, Physical> = src
                .size
                .to_logical(1., Transform::Normal)
                .upscale(clamp_scale)
                .to_physical_precise_round(self.scale);
            let size = transform.transform_size(size);
            let size = Size::<i32, Physical>::from((size.w.max(1), size.h.max(1)))
                .to_logical(1)
                .to_buffer(1, Transform::Normal);

            if inner
                .framebuffer
                .as_ref()
                .is_some_and(|fb| fb.size() != size)
            {
                inner.framebuffer = None;
            }
            let framebuffer = if let Some(fb) = &inner.framebuffer {
                fb
            } else {
                trace!("creating framebuffer texture sized {} × {}", size.w, size.h);
                let renderer = guard.as_mut();
                let texture = renderer.create_buffer(Fourcc::Abgr8888, size)?;
                inner.framebuffer.insert(texture)
            };

            let mut blur = Option::zip(inner.blur.as_mut(), self.blur_options);
            if let Some((b, options)) = &mut blur {
                let renderer = guard.as_mut();
                if let Err(err) = b.prepare_textures(
                    |fourcc, size| renderer.create_buffer(fourcc, size),
                    framebuffer,
                    *options,
                ) {
                    warn!("error preparing blur textures: {err:?}");
                    blur = None;
                }
            }

            drop(guard);

            frame.with_context(|gl| unsafe {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut current_fbo = 0i32;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);

                gl.Disable(ffi::SCISSOR_TEST);

                let mut temp_fbo = 0;
                let fbo = if let Some(pooled) = pooled_blit_fbo {
                    pooled
                } else {
                    gl.GenFramebuffers(1, &mut temp_fbo as *mut _);
                    temp_fbo
                };
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    framebuffer.tex_id(),
                    0,
                );

                gl.BlitFramebuffer(
                    exp_buf.loc.x,
                    exp_buf.loc.y,
                    exp_buf.loc.x + exp_buf.size.w,
                    exp_buf.loc.y + exp_buf.size.h,
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::LINEAR,
                );

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    0,
                    0,
                );
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
                gl.Enable(ffi::SCISSOR_TEST);

                if temp_fbo != 0 {
                    gl.DeleteFramebuffers(1, &mut temp_fbo as *mut _);
                }

                if gl.GetError() != ffi::NO_ERROR {
                    Err(GlesError::BlitError)
                } else {
                    Ok(())
                }
            })??;

            if self.blur_options.is_none() {
                inner.intermediate = Some(framebuffer.clone());
                return Ok(());
            }

            if let Some((blur, options)) = blur {
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match blur.render(renderer, framebuffer, options) {
                    Ok(blurred) => inner.intermediate = Some(blurred),
                    Err(err) => {
                        warn!("error rendering blur: {err:?}");
                    }
                }
            }

            Ok(())
        })
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<Inner>>() else {
            return Ok(());
        };
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;

        let Some(texture) = &inner.intermediate else {
            return Ok(());
        };

        let output_rect = Rectangle::from_size(frame.output_size());
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        let filtered = &mut inner.subregion_damage;
        filtered.clear();

        if let Some(subregion) = &self.subregion {
            let mut crop = src.to_logical(1., Transform::Normal, &src.size);
            crop.loc += self.geometry.loc;
            subregion.filter_damage(crop, dst, damage, filtered);
        } else {
            filtered.extend(damage.iter());
        };

        if clamped_dst != dst {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            filtered.retain_mut(|d| {
                if let Some(mut crop) = d.intersection(r) {
                    crop.loc -= clamp_offset;
                    *d = crop;
                    true
                } else {
                    false
                }
            });
        }

        if filtered.is_empty() {
            return Ok(());
        }
        let damage = &filtered[..];

        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let tex_size = texture.size().to_f64();
        let render_src = Rectangle::new(
            Point::from((
                inner.sub_uv.loc.x * tex_size.w,
                inner.sub_uv.loc.y * tex_size.h,
            )),
            Size::from((
                inner.sub_uv.size.w * tex_size.w,
                inner.sub_uv.size.h * tex_size.h,
            )),
        );

        if let Some(persp) = self.perspective {
            let program = Shaders::get_from_frame(frame).picker_blur.clone();
            let uniforms = program.is_some().then(|| {
                self.compute_perspective_uniforms(crop, frame.transformation(), inner.sub_uv, persp)
            });
            let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);
            return frame.render_texture_from_to(
                texture,
                render_src,
                clamped_dst,
                damage,
                &[],
                frame.transformation().invert(),
                persp.alpha,
                program.as_ref(),
                uniforms,
            );
        }

        let sharp_tex = if self.blur_options.is_some()
            && self.liquid_glass.is_some_and(|lg| lg.bevel_clarity > 0.001)
        {
            inner.framebuffer.as_ref()
        } else {
            None
        };
        let use_sharp = sharp_tex.is_some();

        let program = Shaders::get_from_frame(frame).postprocess_and_clip.clone();
        let uniforms = program
            .is_some()
            .then(|| self.compute_uniforms(crop, frame.transformation(), inner.sub_uv, use_sharp));
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        bind_sharp_texture(frame, sharp_tex)?;
        let res = frame.render_texture_from_to(
            texture,
            render_src,
            clamped_dst,
            damage,
            &[],
            frame.transformation().invert(),
            1.,
            program.as_ref(),
            uniforms,
        );
        let _ = unbind_sharp_texture(frame, use_sharp);
        res
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::capture_framebuffer(&self, gles_frame, src, dst, cache)?;
        Ok(())
    }

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

impl Inner {
    fn new(renderer: &mut GlesRenderer) -> Self {
        Inner {
            framebuffer: None,
            blur: Blur::new(renderer),
            intermediate: None,
            sub_uv: Rectangle::new(Point::new(0., 0.), Size::from((1., 1.))),
            subregion_damage: Vec::new(),
        }
    }
}
