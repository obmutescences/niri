use glam::Mat3;
use smithay::backend::renderer::gles::Uniform;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct LiquidGlassOptions {
    pub refraction_strength: f64,
    pub power_factor: f64,
    pub refraction_power: f64,
    pub glow_weight: f64,
    pub bevel_width: f64,
    pub bevel_px: f64,
    pub fringing: f64,
    /// Whole-surface lens warp (0 disables). Positive values magnify the center.
    pub interior_warp: f64,
    pub rim_color: [f32; 4],
    pub rim_strength: f64,
    pub edge_lighting: f64,
    pub inner_shadow: f64,
    pub sheen_color: [f32; 4],
    pub sheen_strength: f64,
    pub physical_refraction: bool,
    pub ior: f64,
    pub thickness: f64,
    pub bevel_clarity: f64,
    pub dispersion: f64,
    pub adaptive_contrast: bool,
    pub cursor_glow: f64,
    pub focus_shimmer: f64,
}

impl From<niri_config::LiquidGlass> for LiquidGlassOptions {
    fn from(config: niri_config::LiquidGlass) -> Self {
        Self {
            refraction_strength: config.refraction_strength,
            power_factor: config.power_factor,
            refraction_power: config.refraction_power,
            glow_weight: config.glow_weight,
            bevel_width: config.bevel_width,
            bevel_px: config.bevel_px,
            fringing: config.fringing,
            interior_warp: config.interior_warp,
            rim_color: config.rim_color.to_array_unpremul(),
            rim_strength: config.rim_strength,
            edge_lighting: config.edge_lighting,
            inner_shadow: config.inner_shadow,
            sheen_color: config.sheen_color.to_array_unpremul(),
            sheen_strength: config.sheen_strength,
            physical_refraction: config.physical_refraction,
            ior: config.ior,
            thickness: config.thickness,
            bevel_clarity: config.bevel_clarity,
            dispersion: config.dispersion,
            adaptive_contrast: config.adaptive_contrast,
            cursor_glow: config.cursor_glow,
            focus_shimmer: config.focus_shimmer,
        }
    }
}

/// Computes the 2x2 inverse linear map from window geometry UV (`coords_geo`) back to texture UV
/// (`v_coords`), packed as `[m00, m10, m01, m11]`.
pub fn geo_to_tex_linear(input_to_geo: Mat3) -> [f32; 4] {
    let inv = if input_to_geo.determinant().abs() > 1e-8 {
        input_to_geo.inverse()
    } else {
        Mat3::IDENTITY
    };
    [inv.x_axis.x, inv.x_axis.y, inv.y_axis.x, inv.y_axis.y]
}

pub fn extend_liquid_glass_uniforms(
    uniforms: &mut Vec<Uniform<'static>>,
    liquid_glass: Option<LiquidGlassOptions>,
    cursor_pos_px: Option<[f32; 2]>,
    focus_progress: f32,
) {
    let enabled = liquid_glass.is_some();
    let lg = liquid_glass.unwrap_or_default();
    let (cursor_glow, cursor_pos) = match cursor_pos_px {
        Some(pos) if enabled && lg.cursor_glow > 0.0 => (lg.cursor_glow as f32, pos),
        _ => (0.0f32, [-10000.0f32, -10000.0f32]),
    };

    uniforms.extend([
        Uniform::new("lg_enabled", if enabled { 1.0f32 } else { 0.0f32 }),
        Uniform::new("lg_refraction_strength", lg.refraction_strength as f32),
        Uniform::new("lg_power_factor", lg.power_factor as f32),
        Uniform::new("lg_refraction_power", lg.refraction_power as f32),
        Uniform::new(
            "lg_physical_refraction",
            if lg.physical_refraction {
                1.0f32
            } else {
                0.0f32
            },
        ),
        Uniform::new("lg_glow_weight", lg.glow_weight as f32),
        Uniform::new("lg_edge_lighting", lg.edge_lighting as f32),
        Uniform::new("lg_fringing", lg.fringing as f32),
        Uniform::new("lg_bevel_width", lg.bevel_width as f32),
        Uniform::new("lg_bevel_px", lg.bevel_px as f32),
        Uniform::new("lg_interior_warp", lg.interior_warp as f32),
        Uniform::new("lg_rim_color", lg.rim_color),
        Uniform::new("lg_rim_strength", lg.rim_strength as f32),
        Uniform::new("lg_inner_shadow", lg.inner_shadow as f32),
        Uniform::new("lg_sheen_color", lg.sheen_color),
        Uniform::new("lg_sheen_strength", lg.sheen_strength as f32),
        Uniform::new("lg_ior", lg.ior as f32),
        Uniform::new("lg_thickness", lg.thickness as f32),
        Uniform::new("lg_bevel_clarity", lg.bevel_clarity as f32),
        Uniform::new("lg_dispersion", lg.dispersion as f32),
        Uniform::new(
            "lg_adaptive_contrast",
            if lg.adaptive_contrast { 1.0f32 } else { 0.0f32 },
        ),
        Uniform::new("lg_cursor_glow", cursor_glow),
        Uniform::new("lg_cursor_pos", cursor_pos),
        Uniform::new(
            "lg_focus_shimmer",
            if enabled {
                lg.focus_shimmer as f32
            } else {
                0.0f32
            },
        ),
        Uniform::new("lg_focus_progress", focus_progress),
    ]);
}
