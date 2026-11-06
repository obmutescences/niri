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
        }
    }
}
