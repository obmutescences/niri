use std::collections::HashMap;
use std::rc::Rc;

use niri_config::{Color, CornerRadius, FocusGlow, FocusGlowStyle, GradientInterpolation};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, Uniform};
use smithay::backend::renderer::Texture as _;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size};

use super::border::BorderRenderElement;
use super::offscreen::OffscreenRenderElement;
use super::shader_element::ShaderRenderElement;
use super::shaders::{ProgramType, Shaders};
use super::shadow::ShadowRenderElement;

/// Parameters for rendering a focused tile through `tile_focus.frag` (jelly scale + 3D tilt +
/// brightness pulse with 1px perspective AA).
#[derive(Debug, Clone, Copy)]
pub struct TileFocusShaderParams {
    pub tile_size: Size<f64, Logical>,
    pub location: Point<f64, Logical>,
    pub scale: Scale<f64>,
    pub scale_xy: [f32; 2],
    pub tilt: [f32; 2],
    pub brightness_pulse: f32,
    pub alpha: f32,
}

/// Builds a `ShaderRenderElement` applying anisotropic jelly scale, 3D perspective tilt, and
/// brightness pulse to an offscreen tile texture.
pub fn render_tile_focus_shader(
    renderer: &mut GlesRenderer,
    offscreen: &OffscreenRenderElement,
    params: TileFocusShaderParams,
) -> Option<ShaderRenderElement> {
    Shaders::get(renderer).program(ProgramType::TileFocus)?;

    let scale = params.scale;
    let texture = offscreen.texture();
    let tex_size: Size<f64, Logical> = Size::from((
        texture.width() as f64 / scale.x,
        texture.height() as f64 / scale.y,
    ));
    let src_size = offscreen.logical_size();
    let src_offset = offscreen.offset();

    // Pivot at the tile's own geometric center inside the offscreen texture.
    let tile_center = params.tile_size.to_point().downscale(2.);
    let src_pivot: Point<f64, Logical> =
        Point::from((tile_center.x - src_offset.x, tile_center.y - src_offset.y));
    let screen_pivot = params.location + tile_center;

    let sx = f64::from(params.scale_xy[0]).max(0.01);
    let sy = f64::from(params.scale_xy[1]).max(0.01);
    let tilt_mag = f64::from(params.tilt[0].hypot(params.tilt[1]));

    // Pad destination bounds generously enough for both >1.0 overshoot scaling and 3D perspective
    // corner expansion without ever clipping the tile or its shadow.
    let half_w = src_pivot.x.max(src_size.w - src_pivot.x).max(1.) * sx;
    let half_h = src_pivot.y.max(src_size.h - src_pivot.y).max(1.) * sy;
    let persp_pad = if tilt_mag > 1e-4 {
        (half_w.max(half_h) * (0.18 * tilt_mag.min(1.5))).max(18.)
    } else {
        4.
    };

    let dest_loc = Point::from((
        screen_pivot.x - half_w - persp_pad,
        screen_pivot.y - half_h - persp_pad,
    ));
    let dest_size = Size::from(((half_w + persp_pad) * 2., (half_h + persp_pad) * 2.));
    let dest_rect = Rectangle::new(dest_loc, dest_size)
        .to_physical_precise_round(scale)
        .to_logical(scale);

    let dest_center: Point<f64, Logical> = Point::from((
        dest_rect.loc.x + dest_rect.size.w * 0.5 - screen_pivot.x,
        dest_rect.loc.y + dest_rect.size.h * 0.5 - screen_pivot.y,
    ));

    let persp_half_size = [
        ((params.tile_size.w * 0.5 * sx) as f32).max(1.0),
        ((params.tile_size.h * 0.5 * sy) as f32).max(1.0),
    ];

    let elem = ShaderRenderElement::new(
        ProgramType::TileFocus,
        dest_rect.size,
        None,
        scale.x as f32,
        params.alpha,
        Rc::new([
            Uniform::new("src_size", [src_size.w as f32, src_size.h as f32]),
            Uniform::new("tex_size", [tex_size.w as f32, tex_size.h as f32]),
            Uniform::new("src_pivot", [src_pivot.x as f32, src_pivot.y as f32]),
            Uniform::new(
                "dest_size",
                [dest_rect.size.w as f32, dest_rect.size.h as f32],
            ),
            Uniform::new("dest_center", [dest_center.x as f32, dest_center.y as f32]),
            Uniform::new("persp_half_size", persp_half_size),
            Uniform::new("scale_xy", params.scale_xy),
            Uniform::new("tilt", params.tilt),
            Uniform::new("brightness_pulse", params.brightness_pulse),
        ]),
        HashMap::from([(String::from("tex"), texture.clone())]),
        Kind::Unspecified,
    )
    .with_location(dest_rect.loc);

    Some(elem)
}

/// Renders either an expanding ripple ring (`BorderRenderElement`) or a soft rounded halo
/// (`ShadowRenderElement`) around the focused tile without requiring any extra shader program.
pub enum FocusGlowElement {
    Pulse(ShadowRenderElement),
    Ripple(BorderRenderElement),
}

pub fn build_focus_glow_element(
    glow: FocusGlow,
    progress: f32,
    tile_location: Point<f64, Logical>,
    tile_size: Size<f64, Logical>,
    corner_radius: CornerRadius,
    scale: f32,
) -> Option<FocusGlowElement> {
    if !glow.enabled || glow.strength <= 0.001 || glow.spread <= 0.1 {
        return None;
    }
    let t = progress.clamp(0.0, 1.0);
    if t <= 0.001 || t >= 0.999 {
        return None;
    }

    let base_rgba = glow.color.to_array_unpremul();
    let radius = corner_radius.fit_to(tile_size.w as f32, tile_size.h as f32);

    match glow.style {
        FocusGlowStyle::Pulse => {
            // Smooth bell envelope peaking quickly around t = 0.25 then fading cleanly.
            let env = (t * std::f32::consts::PI).sin().powf(0.85);
            let alpha = (base_rgba[3] * glow.strength * env).clamp(0.0, 1.0);
            if alpha <= 0.002 {
                return None;
            }
            let spread = f64::from(glow.spread * (0.45 + 0.55 * env));
            let softness = (spread * 1.4).max(6.0);
            let pad = spread + softness;
            let area_size = Size::from((tile_size.w + pad * 2., tile_size.h + pad * 2.));
            let geo = Rectangle::new(
                Point::from((pad - spread, pad - spread)),
                Size::from((tile_size.w + spread * 2., tile_size.h + spread * 2.)),
            );
            let win_geo = Rectangle::new(Point::from((pad, pad)), tile_size);
            let color = Color::new_unpremul(base_rgba[0], base_rgba[1], base_rgba[2], alpha);

            let elem = ShadowRenderElement::new(
                area_size,
                geo,
                color,
                softness as f32,
                radius.expanded_by(spread as f32),
                scale,
                win_geo,
                radius,
                1.0,
            )
            .with_location(tile_location - Point::from((pad, pad)));

            Some(FocusGlowElement::Pulse(elem))
        }
        FocusGlowStyle::Ripple => {
            // Smooth ease-out expansion outward from the window edge while fading cleanly to 0.
            let ease = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
            let fade = (1.0 - t) * (1.0 - t);
            let alpha = (base_rgba[3] * glow.strength * fade).clamp(0.0, 1.0);
            if alpha <= 0.002 {
                return None;
            }
            let expand = f64::from(glow.spread * ease);
            let ring_width = (2.5f32 * (1.0 - 0.5 * ease)).max(1.0);
            let outer_pad = expand + f64::from(ring_width);
            let outer_size =
                Size::from((tile_size.w + outer_pad * 2., tile_size.h + outer_pad * 2.));
            let outer_radius = radius.expanded_by(outer_pad as f32);
            let color = Color::new_unpremul(base_rgba[0], base_rgba[1], base_rgba[2], alpha);

            let elem = BorderRenderElement::new(
                outer_size,
                Rectangle::from_size(outer_size),
                GradientInterpolation::default(),
                color,
                color,
                0.,
                Rectangle::from_size(outer_size),
                ring_width,
                outer_radius,
                scale,
                1.0,
            )
            .with_location(tile_location - Point::from((outer_pad, outer_pad)));

            Some(FocusGlowElement::Ripple(elem))
        }
    }
}
