use knuffel::errors::DecodeError;

use crate::animations::{parse_curve_node, Animation, Curve, EasingParams, Kind, SpringParams};
use crate::appearance::Color;
use crate::utils::{expect_only_children, parse_arg_node, Flag, MergeWith};
use crate::FloatOrInt;

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FocusAnimationMode {
    /// Single-phase analytical impulse spring: C-infinity smooth, zero velocity kink.
    #[default]
    Impulse,
    /// Classic two-phase animation (shrink/in then expand/out).
    TwoPhase,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusAnimationPreset {
    Pulse,
    Pop,
    Jelly,
    Tilt,
    Lift,
    Nudge,
    Glow,
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FocusGlowStyle {
    /// Soft glowing halo hugging the rounded window border.
    Pulse,
    /// Expanding rounded ring ripple radiating outward while fading out.
    #[default]
    Ripple,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusAnimation {
    pub enabled: bool,
    pub mode: FocusAnimationMode,
    pub anim: Animation,
    /// Optional explicit total settling duration in ms (also scales `Spring` curves while
    /// keeping their bounce shape).
    pub duration_ms: Option<u32>,
    /// Optional shrink/in-phase duration in ms for `TwoPhase` mode.
    pub in_duration_ms: Option<u32>,
    /// Optional shrink/in-phase easing curve for `TwoPhase` mode.
    pub in_curve: Option<Curve>,
    pub disable_on_solo: bool,
    pub disable_on_floating: bool,
    pub disable_on_fullscreen: bool,
    pub disable_on_maximized: bool,
    pub disable_on_open: bool,
    pub disable_on_workspace_switch: bool,
    pub debounce_ms: u32,
    pub brightness_pulse: f32,
    pub opacity_transition_ms: u32,
    pub lift: f32,
    pub scale: FocusScale,
    pub nudge: FocusNudge,
    pub tilt: FocusTilt,
    pub glow: FocusGlow,
}

impl Default for FocusAnimation {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: FocusAnimationMode::Impulse,
            anim: Animation {
                off: false,
                kind: Kind::Easing(EasingParams {
                    duration_ms: 200,
                    curve: Curve::EaseOutQuad,
                }),
            },
            duration_ms: None,
            in_duration_ms: None,
            in_curve: None,
            disable_on_solo: false,
            disable_on_floating: false,
            disable_on_fullscreen: true,
            disable_on_maximized: false,
            disable_on_open: true,
            disable_on_workspace_switch: false,
            debounce_ms: 35,
            brightness_pulse: 0.0,
            opacity_transition_ms: 0,
            lift: 0.0,
            scale: FocusScale::default(),
            nudge: FocusNudge::default(),
            tilt: FocusTilt::default(),
            glow: FocusGlow::default(),
        }
    }
}

impl FocusAnimation {
    pub fn apply_preset(&mut self, preset: FocusAnimationPreset) {
        self.mode = FocusAnimationMode::Impulse;
        match preset {
            FocusAnimationPreset::Pulse => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.52,
                        stiffness: 950,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = true;
                self.scale.flash_scale = 0.86;
                self.scale.jelly = 0.0;
                self.nudge.enabled = false;
                self.tilt.enabled = false;
                self.glow.enabled = false;
                self.brightness_pulse = 0.06;
                self.lift = 0.0;
            }
            FocusAnimationPreset::Pop => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.50,
                        stiffness: 900,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = true;
                self.scale.flash_scale = 1.09;
                self.scale.jelly = 0.0;
                self.nudge.enabled = false;
                self.tilt.enabled = false;
                self.glow.enabled = false;
                self.brightness_pulse = 0.08;
                self.lift = 8.0;
            }
            FocusAnimationPreset::Jelly => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.45,
                        stiffness: 850,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = true;
                self.scale.flash_scale = 0.86;
                self.scale.jelly = 0.60;
                self.nudge.enabled = true;
                self.nudge.distance = 16.0;
                self.tilt.enabled = false;
                self.glow.enabled = true;
                self.glow.style = FocusGlowStyle::Ripple;
                self.brightness_pulse = 0.06;
                self.lift = 4.0;
            }
            FocusAnimationPreset::Tilt => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.52,
                        stiffness: 900,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = true;
                self.scale.flash_scale = 0.92;
                self.scale.jelly = 0.0;
                self.nudge.enabled = false;
                self.tilt.enabled = true;
                self.tilt.angle = 14.0;
                self.tilt.pointer_press = true;
                self.glow.enabled = true;
                self.glow.style = FocusGlowStyle::Pulse;
                self.brightness_pulse = 0.10;
                self.lift = 0.0;
            }
            FocusAnimationPreset::Lift => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.52,
                        stiffness: 900,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = false;
                self.scale.jelly = 0.0;
                self.nudge.enabled = false;
                self.tilt.enabled = false;
                self.glow.enabled = false;
                self.brightness_pulse = 0.10;
                self.lift = 24.0;
            }
            FocusAnimationPreset::Nudge => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Spring(SpringParams {
                        damping_ratio: 0.55,
                        stiffness: 950,
                        epsilon: 0.0001,
                    }),
                };
                self.scale.enabled = false;
                self.scale.jelly = 0.0;
                self.nudge.enabled = true;
                self.nudge.distance = 32.0;
                self.tilt.enabled = false;
                self.glow.enabled = true;
                self.glow.style = FocusGlowStyle::Ripple;
                self.brightness_pulse = 0.0;
                self.lift = 0.0;
            }
            FocusAnimationPreset::Glow => {
                self.anim = Animation {
                    off: false,
                    kind: Kind::Easing(EasingParams {
                        duration_ms: 280,
                        curve: Curve::EaseOutCubic,
                    }),
                };
                self.scale.enabled = false;
                self.scale.jelly = 0.0;
                self.nudge.enabled = false;
                self.tilt.enabled = false;
                self.glow.enabled = true;
                self.glow.style = FocusGlowStyle::Ripple;
                self.glow.spread = 26.0;
                self.glow.strength = 1.15;
                self.brightness_pulse = 0.10;
                self.lift = 0.0;
            }
        }
    }

    pub fn effective_disable_on_solo(&self) -> bool {
        self.disable_on_solo || self.scale.disable_on_solo
    }

    pub fn effective_disable_on_floating(&self) -> bool {
        self.disable_on_floating || self.scale.disable_on_floating
    }

    pub fn has_any_visual_channel(&self) -> bool {
        self.scale.enabled
            || self.nudge.enabled
            || self.tilt.enabled
            || self.glow.enabled
            || self.brightness_pulse > 0.001
            || self.lift.abs() > 0.05
    }
}

impl MergeWith<FocusAnimationPart> for FocusAnimation {
    fn merge_with(&mut self, part: &FocusAnimationPart) {
        if let Some(preset) = part.preset {
            self.apply_preset(preset);
        }

        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }

        if let Some(mode) = part.mode {
            self.mode = mode;
        }
        if let Some(anim) = &part.anim {
            self.anim = *anim;
            // Clear stale spring duration override if a new pure easing anim replaces it without
            // setting an explicit duration override.
            if matches!(anim.kind, Kind::Easing(_)) && part.duration_ms.is_none() {
                self.duration_ms = None;
            }
        }
        if let Some(ms) = part.duration_ms {
            self.duration_ms = Some(ms);
        }
        if let Some(ms) = part.in_duration_ms {
            self.in_duration_ms = Some(ms);
        }
        if let Some(curve) = part.in_curve {
            self.in_curve = Some(curve);
        }

        if let Some(v) = part.disable_on_solo {
            self.disable_on_solo = v.0;
            self.scale.disable_on_solo = v.0;
        }
        if let Some(v) = part.disable_on_floating {
            self.disable_on_floating = v.0;
            self.scale.disable_on_floating = v.0;
        }
        if let Some(v) = part.disable_on_fullscreen {
            self.disable_on_fullscreen = v.0;
        }
        if let Some(v) = part.disable_on_maximized {
            self.disable_on_maximized = v.0;
        }
        if let Some(v) = part.disable_on_open {
            self.disable_on_open = v.0;
        }
        if let Some(v) = part.disable_on_workspace_switch {
            self.disable_on_workspace_switch = v.0;
        }
        if let Some(ms) = part.debounce_ms {
            self.debounce_ms = ms;
        }
        if let Some(v) = part.brightness_pulse {
            self.brightness_pulse = (v.0 as f32).clamp(0.0, 0.5);
        }
        if let Some(ms) = part.opacity_transition_ms {
            self.opacity_transition_ms = ms;
        }
        if let Some(v) = part.lift {
            self.lift = (v.0 as f32).clamp(-64.0, 64.0);
        }

        if let Some(scale) = &part.scale {
            self.scale.merge_with(scale);
            // Wire legacy fields previously accepted inside `scale {}` directly into the real
            // channels so nothing is ever silently discarded.
            if let Some(g) = scale.glow {
                self.glow.enabled = g.0 > 0.0;
                self.glow.strength = (g.0 as f32).clamp(0.0, 2.0);
            }
            if let Some(d) = scale.directional {
                self.nudge.enabled = d.0;
            }
            if let Some(nd) = scale.no_directional {
                if nd.0 {
                    self.nudge.enabled = false;
                }
            }
        }
        if let Some(nudge) = &part.nudge {
            self.nudge.merge_with(nudge);
        }
        if let Some(tilt) = &part.tilt {
            self.tilt.merge_with(tilt);
        }
        if let Some(glow) = &part.glow {
            self.glow.merge_with(glow);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusScale {
    pub enabled: bool,
    pub flash_scale: f32,
    pub jelly: f32,
    pub disable_on_solo: bool,
    pub disable_on_floating: bool,
}

impl Default for FocusScale {
    fn default() -> Self {
        Self {
            enabled: false,
            flash_scale: 0.9,
            jelly: 0.0,
            disable_on_solo: false,
            disable_on_floating: false,
        }
    }
}

impl MergeWith<FocusScalePart> for FocusScale {
    fn merge_with(&mut self, part: &FocusScalePart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }
        if let Some(flash_scale) = part.flash_scale {
            self.flash_scale = (flash_scale.0 as f32).clamp(0.0, 2.0);
        }
        if let Some(jelly) = part.jelly {
            self.jelly = (jelly.0 as f32).clamp(0.0, 1.0);
        }
        if let Some(disable_on_solo) = part.disable_on_solo {
            self.disable_on_solo = disable_on_solo.0;
        }
        if let Some(disable_on_floating) = part.disable_on_floating {
            self.disable_on_floating = disable_on_floating.0;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct FocusScalePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub flash_scale: Option<FloatOrInt<0, 2>>,
    #[knuffel(child, unwrap(argument))]
    pub jelly: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub disable_on_solo: Option<Flag>,
    #[knuffel(child)]
    pub disable_on_floating: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub glow: Option<FloatOrInt<0, 2>>,
    #[knuffel(child)]
    pub directional: Option<Flag>,
    #[knuffel(child)]
    pub no_directional: Option<Flag>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusNudge {
    pub enabled: bool,
    pub distance: f32,
}

impl Default for FocusNudge {
    fn default() -> Self {
        Self {
            enabled: false,
            distance: 12.0,
        }
    }
}

impl MergeWith<FocusNudgePart> for FocusNudge {
    fn merge_with(&mut self, part: &FocusNudgePart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }
        if let Some(d) = part.distance {
            self.distance = (d.0 as f32).clamp(0.0, 120.0);
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct FocusNudgePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub distance: Option<FloatOrInt<0, 120>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusTilt {
    pub enabled: bool,
    pub angle: f32,
    pub pointer_press: bool,
}

impl Default for FocusTilt {
    fn default() -> Self {
        Self {
            enabled: false,
            angle: 3.5,
            pointer_press: true,
        }
    }
}

impl MergeWith<FocusTiltPart> for FocusTilt {
    fn merge_with(&mut self, part: &FocusTiltPart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }
        if let Some(a) = part.angle {
            self.angle = (a.0 as f32).clamp(0.0, 25.0);
        }
        if let Some(p) = part.pointer_press {
            self.pointer_press = p.0;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct FocusTiltPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub angle: Option<FloatOrInt<0, 25>>,
    #[knuffel(child)]
    pub pointer_press: Option<Flag>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusGlow {
    pub enabled: bool,
    pub style: FocusGlowStyle,
    pub color: Color,
    pub spread: f32,
    pub strength: f32,
}

impl Default for FocusGlow {
    fn default() -> Self {
        Self {
            enabled: false,
            style: FocusGlowStyle::Ripple,
            color: Color::from_rgba8_unpremul(0x31, 0xf6, 0xb9, 0xcc),
            spread: 16.0,
            strength: 0.85,
        }
    }
}

impl MergeWith<FocusGlowPart> for FocusGlow {
    fn merge_with(&mut self, part: &FocusGlowPart) {
        if part.off {
            self.enabled = false;
        } else if part.on {
            self.enabled = true;
        }
        if let Some(style) = part.style {
            self.style = style;
        }
        if let Some(color) = part.color {
            self.color = color;
        }
        if let Some(spread) = part.spread {
            self.spread = (spread.0 as f32).clamp(0.0, 128.0);
        }
        if let Some(strength) = part.strength {
            self.strength = (strength.0 as f32).clamp(0.0, 2.0);
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct FocusGlowPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub style: Option<FocusGlowStyle>,
    #[knuffel(child, unwrap(argument, str))]
    pub color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub spread: Option<FloatOrInt<0, 128>>,
    #[knuffel(child, unwrap(argument))]
    pub strength: Option<FloatOrInt<0, 2>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct FocusAnimationPart {
    pub off: bool,
    pub on: bool,
    pub mode: Option<FocusAnimationMode>,
    pub preset: Option<FocusAnimationPreset>,
    pub anim: Option<Animation>,
    pub duration_ms: Option<u32>,
    pub in_duration_ms: Option<u32>,
    pub in_curve: Option<Curve>,
    pub disable_on_solo: Option<Flag>,
    pub disable_on_floating: Option<Flag>,
    pub disable_on_fullscreen: Option<Flag>,
    pub disable_on_maximized: Option<Flag>,
    pub disable_on_open: Option<Flag>,
    pub disable_on_workspace_switch: Option<Flag>,
    pub debounce_ms: Option<u32>,
    pub brightness_pulse: Option<FloatOrInt<0, 1>>,
    pub opacity_transition_ms: Option<u32>,
    pub lift: Option<FloatOrInt<-64, 64>>,
    pub scale: Option<FocusScalePart>,
    pub nudge: Option<FocusNudgePart>,
    pub tilt: Option<FocusTiltPart>,
    pub glow: Option<FocusGlowPart>,
}

impl MergeWith<FocusAnimationPart> for FocusAnimationPart {
    fn merge_with(&mut self, other: &FocusAnimationPart) {
        if other.off {
            self.off = true;
            self.on = false;
        } else if other.on {
            self.on = true;
            self.off = false;
        }
        if other.mode.is_some() {
            self.mode = other.mode;
        }
        if other.preset.is_some() {
            self.preset = other.preset;
        }
        if other.anim.is_some() {
            self.anim = other.anim;
        }
        if other.duration_ms.is_some() {
            self.duration_ms = other.duration_ms;
        }
        if other.in_duration_ms.is_some() {
            self.in_duration_ms = other.in_duration_ms;
        }
        if other.in_curve.is_some() {
            self.in_curve = other.in_curve;
        }
        if other.disable_on_solo.is_some() {
            self.disable_on_solo = other.disable_on_solo;
        }
        if other.disable_on_floating.is_some() {
            self.disable_on_floating = other.disable_on_floating;
        }
        if other.disable_on_fullscreen.is_some() {
            self.disable_on_fullscreen = other.disable_on_fullscreen;
        }
        if other.disable_on_maximized.is_some() {
            self.disable_on_maximized = other.disable_on_maximized;
        }
        if other.disable_on_open.is_some() {
            self.disable_on_open = other.disable_on_open;
        }
        if other.disable_on_workspace_switch.is_some() {
            self.disable_on_workspace_switch = other.disable_on_workspace_switch;
        }
        if other.debounce_ms.is_some() {
            self.debounce_ms = other.debounce_ms;
        }
        if other.brightness_pulse.is_some() {
            self.brightness_pulse = other.brightness_pulse;
        }
        if other.opacity_transition_ms.is_some() {
            self.opacity_transition_ms = other.opacity_transition_ms;
        }
        if other.lift.is_some() {
            self.lift = other.lift;
        }
        if other.scale.is_some() {
            self.scale = other.scale;
        }
        if other.nudge.is_some() {
            self.nudge = other.nudge;
        }
        if other.tilt.is_some() {
            self.tilt = other.tilt;
        }
        if other.glow.is_some() {
            self.glow = other.glow;
        }
    }
}

pub(crate) fn decode_spring_child<S>(
    child: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
    default_damping: f64,
    default_stiffness: u32,
    default_epsilon: f64,
) -> Result<SpringParams, DecodeError<S>>
where
    S: knuffel::traits::ErrorSpan,
{
    use knuffel::traits::DecodeScalar;

    for arg in &child.arguments {
        ctx.emit_error(DecodeError::unexpected(
            &arg.literal,
            "argument",
            "unexpected argument",
        ));
    }
    for grand in child.children() {
        ctx.emit_error(DecodeError::unexpected(
            grand,
            "node",
            format!("unexpected node `{}`", grand.node_name.escape_default()),
        ));
    }

    let mut damping_ratio = None;
    let mut stiffness = None;
    let mut epsilon = None;
    for (name, val) in &child.properties {
        match &***name {
            "damping-ratio" => {
                damping_ratio = Some(DecodeScalar::decode(val, ctx)?);
            }
            "stiffness" => {
                stiffness = Some(DecodeScalar::decode(val, ctx)?);
            }
            "epsilon" => {
                epsilon = Some(DecodeScalar::decode(val, ctx)?);
            }
            unknown => {
                ctx.emit_error(DecodeError::unexpected(
                    name,
                    "property",
                    format!("unexpected spring property `{}`", unknown.escape_default()),
                ));
            }
        }
    }

    Ok(SpringParams {
        damping_ratio: damping_ratio.unwrap_or(default_damping),
        stiffness: stiffness.unwrap_or(default_stiffness),
        epsilon: epsilon.unwrap_or(default_epsilon),
    })
}

impl<S> knuffel::Decode<S> for FocusAnimationPart
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
        let mut preset = None;
        let mut easing_curve = None;
        let mut duration_ms = None;
        let mut spring_params = None;
        let mut in_duration_ms = None;
        let mut in_curve = None;
        let mut disable_on_solo = None;
        let mut disable_on_floating = None;
        let mut disable_on_fullscreen = None;
        let mut disable_on_maximized = None;
        let mut disable_on_open = None;
        let mut disable_on_workspace_switch = None;
        let mut debounce_ms = None;
        let mut brightness_pulse = None;
        let mut opacity_transition_ms = None;
        let mut lift = None;
        let mut scale = None;
        let mut nudge = None;
        let mut tilt = None;
        let mut glow = None;

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
                "preset" => {
                    preset = Some(parse_arg_node("preset", child, ctx)?);
                }
                "scale" => {
                    scale = Some(FocusScalePart::decode_node(child, ctx)?);
                }
                "nudge" => {
                    nudge = Some(FocusNudgePart::decode_node(child, ctx)?);
                }
                "tilt" => {
                    tilt = Some(FocusTiltPart::decode_node(child, ctx)?);
                }
                "glow" => {
                    glow = Some(FocusGlowPart::decode_node(child, ctx)?);
                }
                "spring" => {
                    spring_params = Some(decode_spring_child(child, ctx, 0.5, 800, 0.0001)?);
                }
                "duration-ms" => {
                    duration_ms = Some(parse_arg_node("duration-ms", child, ctx)?);
                }
                "curve" => {
                    easing_curve = parse_curve_node(child, ctx)?;
                }
                "in-duration-ms" => {
                    in_duration_ms = Some(parse_arg_node("in-duration-ms", child, ctx)?);
                }
                "in-curve" => {
                    in_curve = parse_curve_node(child, ctx)?;
                }
                "disable-on-solo" => {
                    disable_on_solo = Some(Flag::decode_node(child, ctx)?);
                }
                "disable-on-floating" => {
                    disable_on_floating = Some(Flag::decode_node(child, ctx)?);
                }
                "disable-on-fullscreen" => {
                    disable_on_fullscreen = Some(Flag::decode_node(child, ctx)?);
                }
                "disable-on-maximized" => {
                    disable_on_maximized = Some(Flag::decode_node(child, ctx)?);
                }
                "disable-on-open" => {
                    disable_on_open = Some(Flag::decode_node(child, ctx)?);
                }
                "disable-on-workspace-switch" => {
                    disable_on_workspace_switch = Some(Flag::decode_node(child, ctx)?);
                }
                "debounce-ms" => {
                    debounce_ms = Some(parse_arg_node("debounce-ms", child, ctx)?);
                }
                "brightness-pulse" => {
                    brightness_pulse = Some(parse_arg_node("brightness-pulse", child, ctx)?);
                }
                "opacity-transition-ms" => {
                    opacity_transition_ms =
                        Some(parse_arg_node("opacity-transition-ms", child, ctx)?);
                }
                "lift" => {
                    lift = Some(parse_arg_node("lift", child, ctx)?);
                }
                _ => {
                    ctx.emit_error(DecodeError::unexpected(
                        child,
                        "node",
                        format!("unexpected node `{}`", child.node_name.escape_default()),
                    ));
                }
            }
        }

        // Support `spring` + `duration-ms` together (spring shape time-scaled to `duration_ms`).
        // Also when `preset` is used with only `duration-ms` (no `curve` and no `spring`),
        // preserve the preset's spring physics and time-scale it to `duration_ms`.
        let (anim, spring_duration_override) = if let Some(p) = spring_params {
            (
                Some(Animation {
                    off: false,
                    kind: Kind::Spring(p),
                }),
                duration_ms,
            )
        } else if preset.is_some() && duration_ms.is_some() && easing_curve.is_none() {
            (None, duration_ms)
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
            preset,
            anim,
            duration_ms: spring_duration_override,
            in_duration_ms,
            in_curve,
            disable_on_solo,
            disable_on_floating,
            disable_on_fullscreen,
            disable_on_maximized,
            disable_on_open,
            disable_on_workspace_switch,
            debounce_ms,
            brightness_pulse,
            opacity_transition_ms,
            lift,
            scale,
            nudge,
            tilt,
            glow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_animation_parsing() {
        let config = r#"
            layout {
                focus-animation {
                    on
                    duration-ms 300
                    scale {
                        on
                        flash-scale 0.9
                        disable-on-floating true
                    }
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let animation = parsed.layout.focus_animation;
        assert!(animation.enabled);
        if let Kind::Easing(p) = animation.anim.kind {
            assert_eq!(p.duration_ms, 300);
        } else {
            panic!("Expected easing");
        }
        assert!(animation.scale.enabled);
        assert_eq!(animation.scale.flash_scale, 0.9);
        assert!(animation.scale.disable_on_floating);
    }

    #[test]
    fn focus_animation_spring_parsing() {
        let config = r#"
            layout {
                focus-animation {
                    on
                    spring damping-ratio=0.5 stiffness=800 epsilon=0.0001
                    scale {
                        on
                        flash-scale 0.95
                    }
                }
            }
        "#;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let animation = parsed.layout.focus_animation;
        assert!(animation.enabled);
        if let Kind::Spring(p) = animation.anim.kind {
            assert_eq!(p.damping_ratio, 0.5);
            assert_eq!(p.stiffness, 800);
        } else {
            panic!("Expected spring");
        }
        assert!(animation.scale.enabled);
        assert_eq!(animation.scale.flash_scale, 0.95);
    }

    #[test]
    fn focus_animation_spring_with_duration_and_channels() {
        let config = r##"
            layout {
                focus-animation {
                    on
                    mode "impulse"
                    spring damping-ratio=0.55 stiffness=900 epsilon=0.0001
                    duration-ms 240
                    debounce-ms 40
                    brightness-pulse 0.08
                    opacity-transition-ms 180
                    lift 4
                    scale {
                        on
                        flash-scale 0.94
                        jelly 0.45
                    }
                    nudge {
                        on
                        distance 12
                    }
                    tilt {
                        on
                        angle 4.5
                        pointer-press true
                    }
                    glow {
                        on
                        style "ripple"
                        color "#31f6b9cc"
                        spread 18
                        strength 0.9
                    }
                }
            }
            window-rule {
                match app-id="mpv"
                focus-animation {
                    off
                }
            }
        "##;
        let parsed = crate::Config::parse_mem(config).unwrap();
        let fa = parsed.layout.focus_animation;
        assert!(fa.enabled);
        assert_eq!(fa.mode, FocusAnimationMode::Impulse);
        assert_eq!(fa.duration_ms, Some(240));
        assert_eq!(fa.debounce_ms, 40);
        assert_eq!(fa.scale.jelly, 0.45);
        assert!(fa.nudge.enabled);
        assert_eq!(fa.nudge.distance, 12.0);
        assert!(fa.tilt.enabled);
        assert_eq!(fa.tilt.angle, 4.5);
        assert!(fa.glow.enabled);
        assert_eq!(fa.glow.style, FocusGlowStyle::Ripple);
        assert_eq!(fa.opacity_transition_ms, 180);
        assert_eq!(parsed.window_rules.len(), 1);
        assert!(parsed.window_rules[0].focus_animation.unwrap().off);
    }

    #[test]
    fn focus_animation_cubic_bezier_missing_args_errors_without_panic() {
        let config = r#"
            layout {
                focus-animation {
                    on
                    curve "cubic-bezier" 0.2 0.8
                }
            }
        "#;
        assert!(crate::Config::parse_mem(config).is_err());
    }
}
