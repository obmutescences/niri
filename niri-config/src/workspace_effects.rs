use knuffel::errors::DecodeError;

use crate::animations::{parse_curve_node, Animation, Curve, EasingParams, Kind};
use crate::focus_animation::decode_spring_child;
use crate::utils::{expect_only_children, parse_arg_node, Flag, MergeWith};
use crate::FloatOrInt;

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDipMode {
    /// Follows workspace switch animation/gesture progress smoothly (backward-compatible default).
    #[default]
    Sync,
    /// Velocity-coupled spring camera zoom: seamlessly unifies gestures, single switches, and
    /// rapid chained switches with cruise-hold.
    Velocity,
    /// Single-phase analytical impulse spring (or independent easing) with zero turnaround kink.
    Impulse,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceDip {
    pub enabled: bool,
    pub mode: WorkspaceDipMode,
    pub strength: f64,
    /// Independent dip animation timing.
    ///
    /// When `None` in `Sync` mode, the dip follows the workspace switch animation/gesture
    /// progress. When set (or in `Impulse` / `Velocity` mode), drives the dip's own dynamics.
    pub anim: Option<Animation>,
    /// Optional total duration override in ms (also time-scales `Spring` curves).
    pub duration_ms: Option<u32>,
    /// Extra depth multiplier for multi-workspace jumps (`0.0` = disabled).
    pub distance_boost: f64,
    /// Keep the camera cruising at dipped depth during rapid chained workspace switches.
    pub cruise_hold: bool,
    /// Peak position of the sync bell curve in `(0.05..0.95)`; `0.5` = classic `sin(pi * t)`.
    pub peak_position: f64,
    /// Dynamic rounded corner radius (logical px) while dipped; returns to 0 at rest.
    pub corner_radius: f32,
    /// Workspace shadow opacity while dipped (`0..1`).
    pub shadow_opacity: f32,
    /// Subtle backdrop darkening while dipped (`0..1`).
    pub backdrop_dim: f32,
    /// Subtle counter-zoom parallax applied to backdrop while dipped (`1.0` = disabled).
    pub backdrop_parallax: f64,
}

impl Default for WorkspaceDip {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: WorkspaceDipMode::Sync,
            strength: 0.04,
            anim: None,
            duration_ms: None,
            distance_boost: 0.0,
            cruise_hold: true,
            peak_position: 0.5,
            corner_radius: 0.0,
            shadow_opacity: 0.0,
            backdrop_dim: 0.0,
            backdrop_parallax: 1.0,
        }
    }
}

impl MergeWith<WorkspaceDipPart> for WorkspaceDip {
    fn merge_with(&mut self, part: &WorkspaceDipPart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }

        if let Some(mode) = part.mode {
            self.mode = mode;
        }
        if let Some(strength) = part.strength {
            self.strength = strength.0.clamp(0., 1.);
        }
        if part.anim.is_some() {
            self.anim = part.anim;
            if part.duration_ms.is_none() {
                self.duration_ms = None;
            }
        }
        if let Some(ms) = part.duration_ms {
            self.duration_ms = Some(ms);
        }
        if let Some(v) = part.distance_boost {
            self.distance_boost = v.0.clamp(0., 2.);
        }
        if let Some(v) = part.cruise_hold {
            self.cruise_hold = v.0;
        }
        if let Some(v) = part.peak_position {
            self.peak_position = v.0.clamp(0.05, 0.95);
        }
        if let Some(v) = part.corner_radius {
            self.corner_radius = (v.0 as f32).clamp(0., 128.);
        }
        if let Some(v) = part.shadow_opacity {
            self.shadow_opacity = (v.0 as f32).clamp(0., 1.);
        }
        if let Some(v) = part.backdrop_dim {
            self.backdrop_dim = (v.0 as f32).clamp(0., 1.);
        }
        if let Some(v) = part.backdrop_parallax {
            self.backdrop_parallax = v.0.clamp(0.5, 1.5);
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WorkspaceDipPart {
    pub off: bool,
    pub on: bool,
    pub mode: Option<WorkspaceDipMode>,
    pub strength: Option<FloatOrInt<0, 1>>,
    pub anim: Option<Animation>,
    pub duration_ms: Option<u32>,
    pub distance_boost: Option<FloatOrInt<0, 2>>,
    pub cruise_hold: Option<Flag>,
    pub peak_position: Option<FloatOrInt<0, 1>>,
    pub corner_radius: Option<FloatOrInt<0, 128>>,
    pub shadow_opacity: Option<FloatOrInt<0, 1>>,
    pub backdrop_dim: Option<FloatOrInt<0, 1>>,
    pub backdrop_parallax: Option<FloatOrInt<0, 2>>,
}

impl<S> knuffel::Decode<S> for WorkspaceDipPart
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut off = false;
        let mut on = false;
        let mut mode = None;
        let mut strength = None;
        let mut easing_curve = None;
        let mut duration_ms = None;
        let mut spring_params = None;
        let mut distance_boost = None;
        let mut cruise_hold = None;
        let mut peak_position = None;
        let mut corner_radius = None;
        let mut shadow_opacity = None;
        let mut backdrop_dim = None;
        let mut backdrop_parallax = None;

        for child in node.children() {
            match &**child.node_name {
                "off" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    off = true;
                }
                "on" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    on = true;
                }
                "mode" => {
                    mode = Some(parse_arg_node("mode", child, ctx)?);
                }
                "strength" => {
                    strength = Some(parse_arg_node("strength", child, ctx)?);
                }
                "spring" => {
                    if easing_curve.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            "cannot set both spring and curve parameters at once",
                        ));
                    }
                    spring_params = Some(decode_spring_child(child, ctx, 0.6, 400, 0.0001)?);
                }
                "duration-ms" => {
                    duration_ms = Some(parse_arg_node("duration-ms", child, ctx)?);
                }
                "curve" => {
                    if spring_params.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            "cannot set both spring and curve parameters at once",
                        ));
                    }
                    easing_curve = parse_curve_node(child, ctx)?;
                }
                "distance-boost" => {
                    distance_boost = Some(parse_arg_node("distance-boost", child, ctx)?);
                }
                "cruise-hold" => {
                    cruise_hold = Some(Flag::decode_node(child, ctx)?);
                }
                "peak-position" => {
                    peak_position = Some(parse_arg_node("peak-position", child, ctx)?);
                }
                "corner-radius" => {
                    corner_radius = Some(parse_arg_node("corner-radius", child, ctx)?);
                }
                "shadow-opacity" => {
                    shadow_opacity = Some(parse_arg_node("shadow-opacity", child, ctx)?);
                }
                "backdrop-dim" => {
                    backdrop_dim = Some(parse_arg_node("backdrop-dim", child, ctx)?);
                }
                "backdrop-parallax" => {
                    backdrop_parallax = Some(parse_arg_node("backdrop-parallax", child, ctx)?);
                }
                name_str => {
                    ctx.emit_error(DecodeError::unexpected(
                        child,
                        "node",
                        format!("unexpected node `{}`", name_str.escape_default()),
                    ));
                }
            }
        }

        let (anim, spring_duration_override) = if let Some(p) = spring_params {
            (
                Some(Animation {
                    off: false,
                    kind: Kind::Spring(p),
                }),
                duration_ms,
            )
        } else if duration_ms.is_some() || easing_curve.is_some() {
            (
                Some(Animation {
                    off: false,
                    kind: Kind::Easing(EasingParams {
                        duration_ms: duration_ms.unwrap_or(200),
                        curve: easing_curve.unwrap_or(Curve::EaseOutQuad),
                    }),
                }),
                None,
            )
        } else {
            (None, None)
        };

        Ok(Self {
            off,
            on,
            mode,
            strength,
            anim,
            duration_ms: spring_duration_override,
            distance_boost,
            cruise_hold,
            peak_position,
            corner_radius,
            shadow_opacity,
            backdrop_dim,
            backdrop_parallax,
        })
    }
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSwitch3DStyle {
    /// Engine A (zero-FBO): classic depth + squash recession (backward-compatible default).
    #[default]
    Depth,
    /// Engine A (zero-FBO): depth + hinge pivot + 3-layer parallax + depth dimming.
    Parallax,
    /// Engine B (true 3D perspective shader): vertical 3D cylinder carousel.
    Carousel,
    /// Engine B (true 3D perspective shader): seamless 90-degree vertical 3D cube.
    Cube,
    /// Engine B (true 3D perspective shader): layered VisionOS/iOS depth deck.
    Cards,
    /// Engine B (true 3D perspective shader): velocity-coupled gyroscopic tilt slide.
    TiltSlide,
    /// Engine B (true 3D perspective shader): symmetric book/page fold.
    Fold,
}

impl WorkspaceSwitch3DStyle {
    pub fn uses_perspective_shader(self) -> bool {
        matches!(
            self,
            Self::Carousel | Self::Cube | Self::Cards | Self::TiltSlide | Self::Fold
        )
    }
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSwitch3DPivot {
    /// Anchor upper workspaces on their bottom edge and lower ones on their top edge so
    /// adjacent workspaces stay tightly connected without tearing black gaps.
    #[default]
    Hinge,
    /// Legacy behavior: anchor each workspace at its own center.
    Center,
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCubeSide {
    #[default]
    Outside,
    Inside,
}

/// 3D depth & perspective effect for workspace switches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceSwitch3D {
    pub enabled: bool,
    pub style: WorkspaceSwitch3DStyle,
    /// Scale applied to workspaces at full "distance": 1.0 = no shrink, 0.0 = shrink to nothing.
    pub depth: f64,
    /// Extra vertical squash for far workspaces (Engine A 2D approximation): 1.0 = none.
    pub squash: f64,
    /// Distance (in screen heights) over which the effect ramps from none to full.
    pub radius: f64,
    /// Exponent of the ramp curve: 1.0 = linear, higher = effect concentrated near the edges.
    pub curve_power: f64,
    /// Anchor pivot for Engine A scaling (`hinge` eliminates inter-workspace gap tearing).
    pub pivot: WorkspaceSwitch3DPivot,
    /// Multi-layer parallax strength (wallpaper / tiling / floating) in Engine A (`0..1`).
    pub parallax: f64,
    /// Compresses inter-workspace gap during switches (`0..1`).
    pub gap_compression: f64,
    /// Maximum 3D X-axis tilt angle in degrees for Engine B (`0..85`).
    pub angle: f64,
    /// Virtual camera field of view in degrees for Engine B (`15..110`).
    pub fov: f64,
    /// Cube face orientation for `style "cube"`.
    pub cube_side: WorkspaceCubeSide,
    /// Sensitivity mapping switch velocity to tilt angle for `style "tilt-slide"`.
    pub velocity_tilt: f64,
    /// Depth dimming applied to tilted / distant workspaces (`0..1`).
    pub dim: f32,
    /// Opacity fade applied to distant workspaces (`0..1`).
    pub fade: f32,
    /// Subtle diagonal specular glass sheen sweep during 3D rotation (`0..1`).
    pub sheen: f32,
    /// Dynamic rounded corner radius during 3D switches; smoothly returns to 0 at rest.
    pub corner_radius: f32,
    /// Workspace shadow opacity during 3D switches (`0..1`).
    pub shadow_opacity: f32,
    /// Keep windows currently moving between workspaces un-squashed in the foreground.
    pub exempt_moving_windows: bool,
    /// Couple edge rubber-band bounce into a subtle 3D tilt impulse.
    pub bounce_3d: bool,
}

impl Default for WorkspaceSwitch3D {
    fn default() -> Self {
        Self {
            enabled: false,
            style: WorkspaceSwitch3DStyle::Depth,
            depth: 0.6,
            squash: 0.8,
            radius: 1.0,
            curve_power: 1.5,
            pivot: WorkspaceSwitch3DPivot::Hinge,
            parallax: 0.0,
            gap_compression: 0.0,
            angle: 32.0,
            fov: 45.0,
            cube_side: WorkspaceCubeSide::Outside,
            velocity_tilt: 1.0,
            dim: 0.0,
            fade: 0.0,
            sheen: 0.0,
            corner_radius: 0.0,
            shadow_opacity: 0.0,
            exempt_moving_windows: true,
            bounce_3d: true,
        }
    }
}

impl MergeWith<WorkspaceSwitch3DPart> for WorkspaceSwitch3D {
    fn merge_with(&mut self, part: &WorkspaceSwitch3DPart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }

        if let Some(style) = part.style {
            self.style = style;
        }
        if let Some(depth) = part.depth {
            self.depth = depth.0.clamp(0., 1.);
        }
        if let Some(squash) = part.squash {
            self.squash = squash.0.clamp(0., 1.);
        }
        if let Some(radius) = part.radius {
            self.radius = radius.0.max(0.);
        }
        if let Some(curve_power) = part.curve_power {
            self.curve_power = curve_power.0.max(0.);
        }
        if let Some(pivot) = part.pivot {
            self.pivot = pivot;
        }
        if let Some(parallax) = part.parallax {
            self.parallax = parallax.0.clamp(0., 1.);
        }
        if let Some(gap_compression) = part.gap_compression {
            self.gap_compression = gap_compression.0.clamp(0., 1.);
        }
        if let Some(angle) = part.angle {
            self.angle = angle.0.clamp(0., 85.);
        }
        if let Some(fov) = part.fov {
            self.fov = fov.0.clamp(15., 110.);
        }
        if let Some(cube_side) = part.cube_side {
            self.cube_side = cube_side;
        }
        if let Some(velocity_tilt) = part.velocity_tilt {
            self.velocity_tilt = velocity_tilt.0.clamp(0., 5.);
        }
        if let Some(dim) = part.dim {
            self.dim = (dim.0 as f32).clamp(0., 1.);
        }
        if let Some(fade) = part.fade {
            self.fade = (fade.0 as f32).clamp(0., 1.);
        }
        if let Some(sheen) = part.sheen {
            self.sheen = (sheen.0 as f32).clamp(0., 1.);
        }
        if let Some(corner_radius) = part.corner_radius {
            self.corner_radius = (corner_radius.0 as f32).clamp(0., 128.);
        }
        if let Some(shadow_opacity) = part.shadow_opacity {
            self.shadow_opacity = (shadow_opacity.0 as f32).clamp(0., 1.);
        }
        if let Some(exempt) = part.exempt_moving_windows {
            self.exempt_moving_windows = exempt.0;
        }
        if let Some(bounce_3d) = part.bounce_3d {
            self.bounce_3d = bounce_3d.0;
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WorkspaceSwitch3DPart {
    pub off: bool,
    pub on: bool,
    pub style: Option<WorkspaceSwitch3DStyle>,
    pub depth: Option<FloatOrInt<0, 1>>,
    pub squash: Option<FloatOrInt<0, 1>>,
    pub radius: Option<FloatOrInt<0, 10000>>,
    pub curve_power: Option<FloatOrInt<0, 10000>>,
    pub pivot: Option<WorkspaceSwitch3DPivot>,
    pub parallax: Option<FloatOrInt<0, 1>>,
    pub gap_compression: Option<FloatOrInt<0, 1>>,
    pub angle: Option<FloatOrInt<0, 85>>,
    pub fov: Option<FloatOrInt<15, 110>>,
    pub cube_side: Option<WorkspaceCubeSide>,
    pub velocity_tilt: Option<FloatOrInt<0, 5>>,
    pub dim: Option<FloatOrInt<0, 1>>,
    pub fade: Option<FloatOrInt<0, 1>>,
    pub sheen: Option<FloatOrInt<0, 1>>,
    pub corner_radius: Option<FloatOrInt<0, 128>>,
    pub shadow_opacity: Option<FloatOrInt<0, 1>>,
    pub exempt_moving_windows: Option<Flag>,
    pub bounce_3d: Option<Flag>,
}

impl<S> knuffel::Decode<S> for WorkspaceSwitch3DPart
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut part = Self::default();

        for child in node.children() {
            match &**child.node_name {
                "off" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    part.off = true;
                }
                "on" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    part.on = true;
                }
                "style" => {
                    part.style = Some(parse_arg_node("style", child, ctx)?);
                }
                "depth" => {
                    part.depth = Some(parse_arg_node("depth", child, ctx)?);
                }
                "squash" => {
                    part.squash = Some(parse_arg_node("squash", child, ctx)?);
                }
                "radius" => {
                    part.radius = Some(parse_arg_node("radius", child, ctx)?);
                }
                "curve-power" => {
                    part.curve_power = Some(parse_arg_node("curve-power", child, ctx)?);
                }
                "pivot" => {
                    part.pivot = Some(parse_arg_node("pivot", child, ctx)?);
                }
                "parallax" => {
                    part.parallax = Some(parse_arg_node("parallax", child, ctx)?);
                }
                "gap-compression" => {
                    part.gap_compression = Some(parse_arg_node("gap-compression", child, ctx)?);
                }
                "angle" => {
                    part.angle = Some(parse_arg_node("angle", child, ctx)?);
                }
                "fov" => {
                    part.fov = Some(parse_arg_node("fov", child, ctx)?);
                }
                "cube-side" => {
                    part.cube_side = Some(parse_arg_node("cube-side", child, ctx)?);
                }
                "velocity-tilt" => {
                    part.velocity_tilt = Some(parse_arg_node("velocity-tilt", child, ctx)?);
                }
                "dim" => {
                    part.dim = Some(parse_arg_node("dim", child, ctx)?);
                }
                "fade" => {
                    part.fade = Some(parse_arg_node("fade", child, ctx)?);
                }
                "sheen" => {
                    part.sheen = Some(parse_arg_node("sheen", child, ctx)?);
                }
                "corner-radius" => {
                    part.corner_radius = Some(parse_arg_node("corner-radius", child, ctx)?);
                }
                "shadow-opacity" => {
                    part.shadow_opacity = Some(parse_arg_node("shadow-opacity", child, ctx)?);
                }
                "exempt-moving-windows" => {
                    part.exempt_moving_windows = Some(Flag::decode_node(child, ctx)?);
                }
                "bounce-3d" => {
                    part.bounce_3d = Some(Flag::decode_node(child, ctx)?);
                }
                name_str => {
                    ctx.emit_error(DecodeError::unexpected(
                        child,
                        "node",
                        format!("unexpected node `{}`", name_str.escape_default()),
                    ));
                }
            }
        }

        Ok(part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animations::SpringParams;

    #[test]
    fn workspace_switch_3d_default_off() {
        let config = r#"
            layout {
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let ws3d = parsed.layout.workspace_switch_3d;
        assert!(!ws3d.enabled);
        assert_eq!(ws3d.depth, 0.6);
        assert_eq!(ws3d.squash, 0.8);
        assert_eq!(ws3d.radius, 1.0);
        assert_eq!(ws3d.curve_power, 1.5);
    }

    #[test]
    fn workspace_switch_3d_parsing() {
        let config = r#"
            layout {
                workspace-switch-3d {
                    on
                    style "carousel"
                    depth 0.4
                    squash 0.7
                    radius 2.5
                    curve-power 3
                    pivot "hinge"
                    angle 36
                    fov 48
                    dim 0.35
                    sheen 0.15
                    corner-radius 18
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let ws3d = parsed.layout.workspace_switch_3d;
        assert!(ws3d.enabled);
        assert_eq!(ws3d.style, WorkspaceSwitch3DStyle::Carousel);
        assert_eq!(ws3d.depth, 0.4);
        assert_eq!(ws3d.squash, 0.7);
        assert_eq!(ws3d.radius, 2.5);
        assert_eq!(ws3d.curve_power, 3.0);
        assert_eq!(ws3d.angle, 36.0);
        assert_eq!(ws3d.fov, 48.0);
        assert_eq!(ws3d.dim, 0.35);
        assert_eq!(ws3d.sheen, 0.15);
        assert_eq!(ws3d.corner_radius, 18.0);
    }

    #[test]
    fn workspace_switch_3d_off_wins() {
        let config = r#"
            layout {
                workspace-switch-3d {
                    on
                    off
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        assert!(!parsed.layout.workspace_switch_3d.enabled);
    }

    #[test]
    fn workspace_switch_3d_out_of_range_rejected() {
        let config = r#"
            layout {
                workspace-switch-3d {
                    depth 5
                }
            }
        "#;
        assert!(crate::Config::parse_mem(config).is_err());
    }

    #[test]
    fn workspace_switch_3d_wide_ranges_accepted() {
        let config = r#"
            layout {
                workspace-switch-3d {
                    on
                    radius 5000
                    curve-power 8000
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let ws3d = parsed.layout.workspace_switch_3d;
        assert!(ws3d.enabled);
        assert_eq!(ws3d.radius, 5000.);
        assert_eq!(ws3d.curve_power, 8000.);
    }

    #[test]
    fn workspace_dip_default_off() {
        let config = r#"
            layout {
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        assert!(!parsed.layout.workspace_dip.enabled);
        assert_eq!(parsed.layout.workspace_dip.strength, 0.04);
    }

    #[test]
    fn workspace_dip_parsing() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                    mode "velocity"
                    strength 0.06
                    distance-boost 0.3
                    cruise-hold true
                    corner-radius 18
                    shadow-opacity 0.6
                    backdrop-dim 0.2
                    backdrop-parallax 1.04
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let dip = parsed.layout.workspace_dip;
        assert!(dip.enabled);
        assert_eq!(dip.mode, WorkspaceDipMode::Velocity);
        assert_eq!(dip.strength, 0.06);
        assert_eq!(dip.distance_boost, 0.3);
        assert_eq!(dip.corner_radius, 18.0);
        assert_eq!(dip.shadow_opacity, 0.6);
        assert_eq!(dip.backdrop_dim, 0.2);
        assert_eq!(dip.backdrop_parallax, 1.04);
    }

    #[test]
    fn workspace_dip_off_wins() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                    strength 0.06
                    off
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        assert!(!parsed.layout.workspace_dip.enabled);
        assert_eq!(parsed.layout.workspace_dip.strength, 0.06);
    }

    #[test]
    fn workspace_dip_strength_out_of_range_rejected() {
        let config = r#"
            layout {
                workspace-dip {
                    strength 5
                }
            }
        "#;
        assert!(crate::Config::parse_mem(config).is_err());
    }

    #[test]
    fn workspace_dip_default_has_no_own_timing() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        assert!(parsed.layout.workspace_dip.enabled);
        assert!(parsed.layout.workspace_dip.anim.is_none());
    }

    #[test]
    fn workspace_dip_easing_parsing() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                    strength 0.06
                    duration-ms 300
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let dip = parsed.layout.workspace_dip;
        assert!(dip.enabled);
        assert_eq!(
            dip.anim,
            Some(Animation {
                off: false,
                kind: Kind::Easing(EasingParams {
                    duration_ms: 300,
                    curve: Curve::EaseOutQuad,
                }),
            })
        );
    }

    #[test]
    fn workspace_dip_spring_parsing() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                    spring damping-ratio=0.55 stiffness=350 epsilon=0.001
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let dip = parsed.layout.workspace_dip;
        assert!(dip.enabled);
        assert_eq!(
            dip.anim,
            Some(Animation {
                off: false,
                kind: Kind::Spring(SpringParams {
                    damping_ratio: 0.55,
                    stiffness: 350,
                    epsilon: 0.001,
                }),
            })
        );
    }

    #[test]
    fn workspace_dip_spring_with_duration_coexists() {
        let config = r#"
            layout {
                workspace-dip {
                    on
                    spring damping-ratio=0.5 stiffness=400 epsilon=0.0001
                    duration-ms 300
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let dip = parsed.layout.workspace_dip;
        assert!(matches!(dip.anim.unwrap().kind, Kind::Spring(_)));
        assert_eq!(dip.duration_ms, Some(300));
    }

    #[test]
    fn workspace_dip_spring_and_curve_conflict() {
        let config = r#"
            layout {
                workspace-dip {
                    spring damping-ratio=0.5 stiffness=400 epsilon=0.0001
                    curve "ease-out-quad"
                }
            }
        "#;
        assert!(crate::Config::parse_mem(config).is_err());
    }
}
