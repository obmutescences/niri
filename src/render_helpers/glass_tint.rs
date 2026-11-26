use smithay::backend::renderer::gles::Uniform;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct GlassTintOptions {
    pub from: [f32; 4],
    pub to: [f32; 4],
    pub angle: f32,
    pub mode: f32,
    pub opacity: f32,
    pub linear_size: f32,
    pub linear_depth: f32,
    pub radial_size: f32,
    pub radial_depth: f32,
    pub oklab: f32,
}

impl GlassTintOptions {
    pub fn is_visible(&self) -> bool {
        self.opacity > 0.0 && (self.from[3] > 0.0 || self.to[3] > 0.0)
    }
}

impl From<niri_config::BackgroundTint> for GlassTintOptions {
    fn from(config: niri_config::BackgroundTint) -> Self {
        let mode = match config.mode {
            niri_config::TintMode::Linear => 0.0,
            niri_config::TintMode::Radial => 1.0,
        };

        Self {
            from: config.from.to_array_unpremul(),
            to: config.to.to_array_unpremul(),
            angle: config.angle as f32,
            mode,
            opacity: config.opacity as f32,
            linear_size: config.linear_size as f32,
            linear_depth: config.linear_depth as f32,
            radial_size: config.radial_size as f32,
            radial_depth: config.radial_depth as f32,
            oklab: if config.oklab { 1.0 } else { 0.0 },
        }
    }
}

pub fn extend_tint_uniforms(uniforms: &mut Vec<Uniform<'static>>, tint: Option<GlassTintOptions>) {
    let tint = tint.unwrap_or_default();
    uniforms.extend([
        Uniform::new("tint_from", tint.from),
        Uniform::new("tint_to", tint.to),
        Uniform::new("tint_angle", tint.angle),
        Uniform::new("tint_mode", tint.mode),
        Uniform::new("tint_opacity", tint.opacity),
        Uniform::new("tint_linear_size", tint.linear_size),
        Uniform::new("tint_linear_depth", tint.linear_depth),
        Uniform::new("tint_radial_size", tint.radial_size),
        Uniform::new("tint_radial_depth", tint.radial_depth),
        Uniform::new("tint_oklab", tint.oklab),
    ]);
}
