use std::time::Duration;

use niri_config::utils::MergeWith as _;
use niri_config::{FocusAnimation, FocusAnimationMode, FocusAnimationPart};
use smithay::utils::{Logical, Point, Size};

use crate::animation::impulse::impulse_spring;
use crate::animation::{Animation, Clock, Curve};

/// Source context passed when updating a tile's focus animation state.
#[derive(Debug, Clone, Copy, Default)]
pub struct FocusTriggerContext {
    pub is_focused: bool,
    pub is_solo_window: bool,
    pub is_floating: bool,
    pub is_fullscreen: bool,
    pub is_maximized: bool,
    pub is_opening: bool,
    pub layout_has_keyboard_focus: bool,
    /// Normalized direction of the focus shift (e.g., `(1, 0)` moving right, `(0, 1)` moving
    /// down).
    pub direction: [f64; 2],
    /// Optional pointer press position normalized inside the tile to `[-1, 1]^2`.
    pub pointer_press_norm: Option<[f32; 2]>,
    /// Whether this focus gain was caused by switching workspaces.
    pub from_workspace_switch: bool,
}

/// Resolved per-frame transform and visual pulse produced by `FocusEffectState`.
#[derive(Debug, Clone, Copy)]
pub struct FocusFrameTransform {
    pub scale_xy: [f32; 2],
    pub nudge: Point<f64, Logical>,
    pub lift_y: f64,
    pub tilt: [f32; 2],
    pub brightness_pulse: f32,
    pub glow_progress: Option<f32>,
    pub shimmer_progress: f32,
    /// Dynamic extra shadow softness/offset boost when `lift` is active (0..1).
    pub lift_factor: f32,
    /// Whether offscreen + `tile_focus.frag` is required on this frame.
    pub needs_offscreen: bool,
}

impl Default for FocusFrameTransform {
    fn default() -> Self {
        Self {
            scale_xy: [1.0, 1.0],
            nudge: Point::from((0., 0.)),
            lift_y: 0.,
            tilt: [0.0, 0.0],
            brightness_pulse: 0.0,
            glow_progress: None,
            shimmer_progress: 0.0,
            lift_factor: 0.0,
            needs_offscreen: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwoPhaseStage {
    In,
    Out,
}

#[derive(Debug)]
enum PrimaryAnim {
    Idle,
    /// Normalized single-phase impulse: `0.0 -> 1.0 -> 0.0` (C-infinity smooth).
    Impulse(Animation),
    /// Classic two-phase: `In (0.0 -> 1.0)` then `Out (current -> 0.0)` with seamless timestamp
    /// and velocity handoff.
    TwoPhase {
        stage: TwoPhaseStage,
        anim: Animation,
        config: FocusAnimation,
    },
}

/// Self-contained focus animation & smooth opacity state stored on each `Tile`.
///
/// Isolating all focus animation logic in this dedicated module keeps `src/layout/tile.rs` minimal
/// and low-conflict for future upstream merges.
#[derive(Debug)]
pub struct FocusEffectState {
    clock: Clock,
    initialized: bool,
    was_focused: bool,
    last_trigger_time: Option<Duration>,
    primary: PrimaryAnim,
    active_config: FocusAnimation,
    direction: [f64; 2],
    tilt_axis: [f32; 2],
    glow_anim: Option<Animation>,
    glass_shimmer: Option<Animation>,
    shimmer_dir_sign: f32,
    opacity_anim: Option<Animation>,
    last_target_opacity: Option<f32>,
}

impl FocusEffectState {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            initialized: false,
            was_focused: false,
            last_trigger_time: None,
            primary: PrimaryAnim::Idle,
            active_config: FocusAnimation::default(),
            direction: [0., 0.],
            tilt_axis: [0., 0.],
            glow_anim: None,
            glass_shimmer: None,
            shimmer_dir_sign: 1.0,
            opacity_anim: None,
            last_target_opacity: None,
        }
    }

    pub fn resolved_config(
        layout_cfg: &FocusAnimation,
        rule_override: Option<&FocusAnimationPart>,
    ) -> FocusAnimation {
        let mut cfg = *layout_cfg;
        if let Some(part) = rule_override {
            cfg.merge_with(part);
        }
        cfg
    }

    pub fn is_animating(&self) -> bool {
        !matches!(self.primary, PrimaryAnim::Idle)
            || self.glow_anim.is_some()
            || self.glass_shimmer.is_some()
            || self.opacity_anim.is_some()
    }

    pub fn advance(&mut self) {
        if let Some(shimmer) = &self.glass_shimmer {
            if shimmer.is_done() {
                self.glass_shimmer = None;
            }
        }
        if let Some(glow) = &self.glow_anim {
            if glow.is_done() {
                self.glow_anim = None;
            }
        }
        if let Some(op) = &self.opacity_anim {
            if op.is_done() {
                self.opacity_anim = None;
            }
        }

        let next = match &self.primary {
            PrimaryAnim::Idle => None,
            PrimaryAnim::Impulse(anim) => {
                if anim.is_done() {
                    Some(PrimaryAnim::Idle)
                } else {
                    None
                }
            }
            PrimaryAnim::TwoPhase {
                stage,
                anim,
                config,
            } => {
                if !anim.is_done() {
                    None
                } else if *stage == TwoPhaseStage::In {
                    let current = anim.value();
                    let end_time = anim.end_time();
                    let out_anim =
                        Self::make_two_phase_out(self.clock.clone(), current, 0.0, *config)
                            .with_start_time(end_time);
                    Some(PrimaryAnim::TwoPhase {
                        stage: TwoPhaseStage::Out,
                        anim: out_anim,
                        config: *config,
                    })
                } else {
                    Some(PrimaryAnim::Idle)
                }
            }
        };

        if let Some(next) = next {
            self.primary = next;
        }
    }

    /// Smoothly tracks `window-rule` opacity changes (e.g. `is-active=false { opacity 0.7 }`)
    /// when `opacity-transition-ms > 0`.
    pub fn update_opacity_transition(&mut self, target_opacity: f32, transition_ms: u32) {
        let target = target_opacity.clamp(0.0, 1.0);
        if transition_ms == 0 {
            self.last_target_opacity = Some(target);
            self.opacity_anim = None;
            return;
        }

        match self.last_target_opacity {
            None => {
                self.last_target_opacity = Some(target);
            }
            Some(prev) if (prev - target).abs() > 0.002 => {
                let current = self
                    .opacity_anim
                    .as_ref()
                    .map_or(f64::from(prev), |a| a.clamped_value().clamp(0., 1.));
                self.last_target_opacity = Some(target);
                self.opacity_anim = Some(Animation::ease(
                    self.clock.clone(),
                    current,
                    f64::from(target),
                    0.,
                    u64::from(transition_ms),
                    Curve::EaseOutCubic,
                ));
            }
            Some(_) => {}
        }
    }

    /// Returns the smoothly interpolated opacity if transitioning, or `target_opacity`.
    pub fn effective_opacity(&self, target_opacity: f32) -> f32 {
        if let Some(anim) = &self.opacity_anim {
            anim.clamped_value().clamp(0., 1.) as f32
        } else {
            target_opacity
        }
    }

    pub fn update(
        &mut self,
        layout_cfg: &FocusAnimation,
        rule_override: Option<&FocusAnimationPart>,
        ctx: FocusTriggerContext,
        has_liquid_glass_shimmer: bool,
    ) {
        let cfg = Self::resolved_config(layout_cfg, rule_override);

        // When keyboard focus moves to non-window UI (layer shell, popup, picker), keep
        // `was_focused` frozen so closing that overlay doesn't falsely re-trigger on the same tile.
        if !ctx.layout_has_keyboard_focus {
            self.advance();
            return;
        }

        // First observation for a freshly created tile: if `disable-on-open` is set and the window
        // starts focused (usually already playing `open_animation`), absorb the initial focus edge.
        if !self.initialized {
            self.initialized = true;
            if ctx.is_focused && (cfg.disable_on_open || ctx.is_opening) {
                self.was_focused = true;
                self.advance();
                return;
            }
        }

        // Trigger one-shot directional liquid glass focus shimmer on focus gain.
        if ctx.is_focused && !self.was_focused && has_liquid_glass_shimmer {
            let shimmer_ms = cfg
                .duration_ms
                .or(match cfg.anim.kind {
                    niri_config::animations::Kind::Easing(p) => Some(p.duration_ms),
                    niri_config::animations::Kind::Spring(_) => None,
                })
                .unwrap_or(280)
                .clamp(120, 600);
            self.shimmer_dir_sign = if ctx.direction[0] < -0.1 { -1.0 } else { 1.0 };
            self.glass_shimmer = Some(Animation::ease(
                self.clock.clone(),
                0.,
                1.,
                0.,
                u64::from(shimmer_ms),
                Curve::EaseOutCubic,
            ));
        }

        // Handle losing focus mid-animation: smoothly relax back to rest from current position
        // and velocity instead of finishing a loud bounce on an already-unfocused window.
        if !ctx.is_focused && self.was_focused {
            self.was_focused = false;
            if let PrimaryAnim::TwoPhase {
                stage: TwoPhaseStage::In,
                anim,
                config,
            } = &self.primary
            {
                let current = anim.value();
                let vel = anim.velocity();
                let relax = Self::make_two_phase_out(self.clock.clone(), current, vel, *config);
                self.primary = PrimaryAnim::TwoPhase {
                    stage: TwoPhaseStage::Out,
                    anim: relax,
                    config: *config,
                };
            }
            self.advance();
            return;
        }

        if ctx.is_floating && cfg.effective_disable_on_floating() {
            self.primary = PrimaryAnim::Idle;
            self.glow_anim = None;
            self.was_focused = ctx.is_focused;
            self.advance();
            return;
        }

        let blocked = !cfg.enabled
            || !cfg.has_any_visual_channel()
            || (ctx.is_solo_window && cfg.effective_disable_on_solo())
            || (ctx.is_fullscreen && cfg.disable_on_fullscreen)
            || (ctx.is_maximized && cfg.disable_on_maximized)
            || (ctx.is_opening && cfg.disable_on_open)
            || (ctx.from_workspace_switch && cfg.disable_on_workspace_switch);

        if ctx.is_focused && !self.was_focused && !blocked {
            let now = self.clock.now();
            let debounced = self.last_trigger_time.is_some_and(|prev| {
                now.saturating_sub(prev) < Duration::from_millis(u64::from(cfg.debounce_ms))
            });
            self.was_focused = true;

            if !debounced {
                self.last_trigger_time = Some(now);
                self.start_trigger(cfg, ctx);
                return;
            }
        }

        self.was_focused = ctx.is_focused;
        self.advance();
    }

    fn start_trigger(&mut self, cfg: FocusAnimation, ctx: FocusTriggerContext) {
        self.active_config = cfg;
        self.direction = ctx.direction;

        // Compute 3D tilt axis: pointer click position takes priority when `pointer-press` is on
        // AND the cursor is genuinely off-center (ignoring centered cursors produced by
        // `warp-mouse-to-focus` on keyboard switches). Otherwise tilt along the focus shift
        // direction, falling back to a crisp top-edge press.
        self.tilt_axis = if cfg.tilt.enabled {
            let off_center_pointer = cfg
                .tilt
                .pointer_press
                .then_some(ctx.pointer_press_norm)
                .flatten()
                .filter(|[px, py]| px.hypot(*py) >= 0.18);

            if let Some([px, py]) = off_center_pointer {
                [px.clamp(-1., 1.), py.clamp(-1., 1.)]
            } else if ctx.direction[0].hypot(ctx.direction[1]) > 0.05 {
                [
                    (ctx.direction[0] as f32).clamp(-1., 1.),
                    (ctx.direction[1] as f32).clamp(-1., 1.),
                ]
            } else {
                [0.0, -0.85]
            }
        } else {
            [0., 0.]
        };

        // Start primary impulse / two-phase animation normalized to peak = 1.0, rest = 0.0.
        self.primary = match cfg.mode {
            FocusAnimationMode::Impulse => {
                let anim = impulse_spring(self.clock.clone(), 0.0, 1.0, cfg.anim, cfg.duration_ms);
                PrimaryAnim::Impulse(anim)
            }
            FocusAnimationMode::TwoPhase => {
                let current = self.current_primary_value();
                let in_anim = Self::make_two_phase_in(self.clock.clone(), current, cfg);
                PrimaryAnim::TwoPhase {
                    stage: TwoPhaseStage::In,
                    anim: in_anim,
                    config: cfg,
                }
            }
        };

        // Monotonic 0 -> 1 envelope for glow / ripple so ripples expand cleanly without reversing.
        if cfg.glow.enabled && cfg.glow.strength > 0.001 {
            let glow_ms = cfg
                .duration_ms
                .or(match cfg.anim.kind {
                    niri_config::animations::Kind::Easing(p) => Some(p.duration_ms),
                    niri_config::animations::Kind::Spring(_) => None,
                })
                .unwrap_or(280)
                .clamp(120, 700);
            self.glow_anim = Some(Animation::ease(
                self.clock.clone(),
                0.,
                1.,
                0.,
                u64::from(glow_ms),
                Curve::Linear,
            ));
        } else {
            self.glow_anim = None;
        }
    }

    fn current_primary_value(&self) -> f64 {
        match &self.primary {
            PrimaryAnim::Idle => 0.0,
            PrimaryAnim::Impulse(a) | PrimaryAnim::TwoPhase { anim: a, .. } => a.value(),
        }
    }

    fn make_two_phase_in(clock: Clock, current: f64, cfg: FocusAnimation) -> Animation {
        let (dur_ms, curve) = match cfg.anim.kind {
            niri_config::animations::Kind::Easing(p) => (
                cfg.in_duration_ms
                    .unwrap_or_else(|| p.duration_ms.div_ceil(2)),
                cfg.in_curve.unwrap_or(p.curve),
            ),
            niri_config::animations::Kind::Spring(_) => (
                cfg.in_duration_ms.unwrap_or(55),
                cfg.in_curve
                    .unwrap_or(niri_config::animations::Curve::EaseOutQuad),
            ),
        };
        Animation::ease(
            clock,
            current,
            1.0,
            0.,
            u64::from(dur_ms.max(1)),
            Curve::from(curve),
        )
    }

    fn make_two_phase_out(
        clock: Clock,
        current: f64,
        initial_velocity: f64,
        cfg: FocusAnimation,
    ) -> Animation {
        match cfg.anim.kind {
            niri_config::animations::Kind::Easing(p) => {
                let in_ms = cfg
                    .in_duration_ms
                    .unwrap_or_else(|| p.duration_ms.div_ceil(2));
                let out_ms = p.duration_ms.saturating_sub(in_ms).max(1);
                Animation::ease(
                    clock,
                    current,
                    0.0,
                    initial_velocity,
                    u64::from(out_ms),
                    Curve::from(p.curve),
                )
            }
            niri_config::animations::Kind::Spring(p) => {
                let anim = Animation::new(
                    clock,
                    current,
                    0.0,
                    initial_velocity,
                    niri_config::Animation {
                        off: false,
                        kind: niri_config::animations::Kind::Spring(p),
                    },
                );
                if let Some(total_ms) = cfg.duration_ms {
                    let in_ms = cfg.in_duration_ms.unwrap_or(55);
                    let out_ms = total_ms.saturating_sub(in_ms).max(1);
                    anim.with_duration(Duration::from_millis(u64::from(out_ms)))
                } else {
                    anim
                }
            }
        }
    }

    /// Evaluates the exact transform and visual pulse values for the current frame.
    pub fn sample(&self, _tile_size: Size<f64, Logical>) -> FocusFrameTransform {
        let mut out = FocusFrameTransform::default();

        if let Some(shimmer) = &self.glass_shimmer {
            let p = shimmer.clamped_value().clamp(0., 1.) as f32;
            out.shimmer_progress = p * self.shimmer_dir_sign;
        }

        if let Some(glow) = &self.glow_anim {
            out.glow_progress = Some(glow.clamped_value().clamp(0., 1.) as f32);
        }

        if matches!(self.primary, PrimaryAnim::Idle) {
            return out;
        }

        let p = self.current_primary_value();
        let cfg = &self.active_config;

        // 1. Scale / Jelly squash-and-stretch.
        if cfg.scale.enabled {
            let d = (f64::from(cfg.scale.flash_scale) - 1.0) * p;
            let jelly = f64::from(cfg.scale.jelly).clamp(0.0, 1.0);
            let (sx, sy) = if jelly <= 1e-4 {
                (1.0 + d, 1.0 + d)
            } else {
                // Area-preserving directional squash & stretch: compress along the primary travel
                // axis (or vertical if neutral) and counter-stretch orthogonally.
                let horiz = self.direction[0].abs() > self.direction[1].abs() + 0.1;
                let (j_sx, j_sy) = if horiz {
                    (1.0 + d, 1.0 - d * 0.55)
                } else {
                    (1.0 - d * 0.55, 1.0 + d)
                };
                let uni = 1.0 + d;
                (uni + (j_sx - uni) * jelly, uni + (j_sy - uni) * jelly)
            };
            out.scale_xy = [(sx as f32).clamp(0.05, 2.5), (sy as f32).clamp(0.05, 2.5)];
        }

        // 2. Directional momentum nudge (zero-FBO eligible).
        if cfg.nudge.enabled && cfg.nudge.distance > 0.01 {
            let dist = f64::from(cfg.nudge.distance) * p;
            out.nudge = Point::from((self.direction[0] * dist, self.direction[1] * dist));
        }

        // 3. Elevation lift + dynamic shadow factor (zero-FBO eligible).
        if cfg.lift.abs() > 0.01 {
            out.lift_y = -f64::from(cfg.lift) * p;
            out.lift_factor = (p as f32).clamp(0.0, 1.2);
        }

        // 4. 3D perspective tilt (directional swivel or pointer-press indentation).
        if cfg.tilt.enabled && cfg.tilt.angle > 0.01 {
            // Map configured degrees (e.g. 4.5 deg) into shader normalized tilt strength.
            let strength = ((cfg.tilt.angle / 12.0).clamp(0.0, 2.5)) * (p as f32);
            let tx = self.tilt_axis[0] * strength;
            let mut ty = self.tilt_axis[1] * strength;
            // Provide a subtle default vertical nod if no pointer/directional axis was present.
            if tx.abs() < 1e-4 && ty.abs() < 1e-4 {
                ty = -0.35 * strength;
            }
            out.tilt = [tx.clamp(-2.0, 2.0), ty.clamp(-2.0, 2.0)];
        }

        // 5. Activation brightness pulse.
        if cfg.brightness_pulse > 0.001 {
            out.brightness_pulse =
                (cfg.brightness_pulse * (p as f32).clamp(0.0, 1.0)).clamp(0., 0.5);
        }

        let scale_active =
            (out.scale_xy[0] - 1.0).abs() > 1e-4 || (out.scale_xy[1] - 1.0).abs() > 1e-4;
        let tilt_active = out.tilt[0].abs() > 1e-4 || out.tilt[1].abs() > 1e-4;
        let bright_active = out.brightness_pulse > 1e-4;
        out.needs_offscreen = scale_active || tilt_active || bright_active;

        out
    }

    pub fn active_glow(&self) -> niri_config::FocusGlow {
        self.active_config.glow
    }

    /// Returns whether the tile's scale/tilt animation is currently active (for tests/inspection).
    pub fn has_active_primary(&self) -> bool {
        !matches!(self.primary, PrimaryAnim::Idle)
    }
}
