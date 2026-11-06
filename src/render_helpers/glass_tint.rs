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
        }
    }
}
