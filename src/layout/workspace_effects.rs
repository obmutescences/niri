use std::time::Duration;

use niri_config::{WorkspaceDip, WorkspaceDipMode, WorkspaceSwitch3D, WorkspaceSwitch3DPivot};
use smithay::utils::{Logical, Point, Rectangle, Scale};

use crate::animation::impulse::{asymmetric_bell, impulse_spring};
use crate::animation::{Animation, Clock};
use crate::render_helpers::workspace_3d::Workspace3DBufferPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DipPhase {
    Shrinking,
    Expanding,
}

#[derive(Debug)]
enum DipRuntime {
    /// Two-phase explicit easing/spring (backward-compatible with legacy `workspace-dip` timing).
    TwoPhase {
        phase: DipPhase,
        anim: Animation,
        distance_factor: f64,
    },
    /// Single-phase analytical impulse spring (`0 -> 1 -> 0`, zero velocity kink).
    Impulse {
        anim: Animation,
        distance_factor: f64,
    },
    /// Velocity-coupled spring camera tracker (`depth` smoothly follows real-time switch speed
    /// and cruises during rapid chained switches, then springs cleanly back to 0 at rest).
    Velocity {
        depth: f64,
        vel: f64,
        last_time: Duration,
        last_idx: f64,
        cruise_floor: f64,
    },
}

/// Out-and-back rubber-band offset for the view when switching past first/last workspace.
#[derive(Debug)]
struct WorkspaceBounce {
    anim: Animation,
    /// +1 nudges the view down (attempted switch down at the last workspace), -1 up.
    dir: f64,
}

/// Self-contained workspace transition effects state stored on `Monitor`.
///
/// Consolidates `workspace-dip`, `workspace-switch-3d` (Engine A & B), and `workspace-bounce`
/// so `src/layout/monitor.rs` stays clean and low-conflict.
#[derive(Debug)]
pub struct WorkspaceEffectsState {
    dip: Option<DipRuntime>,
    bounce: Option<WorkspaceBounce>,
    /// Last sampled switch velocity (workspaces/sec) for `tilt-slide` & `velocity` dip.
    last_switch_velocity: f64,
    last_sample_time: Option<Duration>,
    last_sample_idx: Option<f64>,
    /// Lazily-allocated 3D offscreen buffer pool for Engine B perspective shaders.
    pub ws3d_buffers: Workspace3DBufferPool,
}

impl Default for WorkspaceEffectsState {
    fn default() -> Self {
        Self {
            dip: None,
            bounce: None,
            last_switch_velocity: 0.,
            last_sample_time: None,
            last_sample_idx: None,
            ws3d_buffers: Workspace3DBufferPool::default(),
        }
    }
}

/// Per-workspace Engine A (zero-FBO) transform computed by `workspace_3d_transform`.
#[derive(Debug, Clone, Copy)]
pub struct Workspace3DEngineATransform {
    pub scale: Scale<f64>,
    pub pivot: Point<f64, Logical>,
    pub eased: f64,
    pub dim_alpha: f32,
}

impl WorkspaceEffectsState {
    pub fn is_dip_active(&self) -> bool {
        self.dip.is_some()
    }

    pub fn is_bounce_active(&self) -> bool {
        self.bounce.is_some()
    }

    pub fn is_animating(&self) -> bool {
        self.dip.is_some() || self.bounce.is_some()
    }

    pub fn switch_velocity(&self) -> f64 {
        self.last_switch_velocity
    }

    /// Starts an edge rubber-band bounce using a smooth impulse spring (falling back cleanly on
    /// instant-complete test clocks).
    pub fn start_bounce(&mut self, clock: Clock, dir: f64) {
        let spring_cfg = niri_config::Animation {
            off: false,
            kind: niri_config::animations::Kind::Spring(niri_config::animations::SpringParams {
                damping_ratio: 0.72,
                stiffness: 650,
                epsilon: 0.001,
            }),
        };
        let anim = impulse_spring(clock, 0., 1., spring_cfg, Some(260));
        self.bounce = Some(WorkspaceBounce { anim, dir });
    }

    /// Current rubber-band Y offset in logical px (`0` outside a bounce).
    pub fn bounce_offset_y(&self) -> f64 {
        const AMPLITUDE: f64 = 28.;
        match &self.bounce {
            Some(bounce) => bounce.dir * AMPLITUDE * bounce.anim.value(),
            None => 0.,
        }
    }

    /// Normalized signed bounce deflection in `[-1, 1]` for 3D bounce tilt.
    pub fn bounce_norm(&self) -> f64 {
        match &self.bounce {
            Some(bounce) => (bounce.dir * bounce.anim.value()).clamp(-1.2, 1.2),
            None => 0.,
        }
    }

    fn compute_distance_factor(dip_cfg: &WorkspaceDip, distance: f64) -> f64 {
        if dip_cfg.distance_boost <= 0. || distance <= 1. {
            return 1.;
        }
        (1. + dip_cfg.distance_boost * distance.max(1.).ln()).clamp(1., 2.5)
    }

    fn dip_two_phase_configs(
        dip_cfg: &WorkspaceDip,
    ) -> Option<(niri_config::Animation, niri_config::Animation)> {
        let anim = dip_cfg.anim?;
        use niri_config::animations::Kind as ConfigKind;
        let configs = match anim.kind {
            ConfigKind::Easing(p) => {
                let half = p.duration_ms.div_ceil(2).max(1);
                (
                    niri_config::Animation {
                        off: false,
                        kind: ConfigKind::Easing(niri_config::animations::EasingParams {
                            duration_ms: half,
                            curve: p.curve,
                        }),
                    },
                    niri_config::Animation {
                        off: false,
                        kind: ConfigKind::Easing(niri_config::animations::EasingParams {
                            duration_ms: p.duration_ms.saturating_sub(half).max(1),
                            curve: p.curve,
                        }),
                    },
                )
            }
            ConfigKind::Spring(p) => (
                niri_config::Animation {
                    off: false,
                    kind: ConfigKind::Spring(p),
                },
                niri_config::Animation {
                    off: false,
                    kind: ConfigKind::Spring(p),
                },
            ),
        };
        Some(configs)
    }

    /// Called whenever a workspace switch animation starts (or restarts mid-flight).
    pub fn on_workspace_switch_start(
        &mut self,
        clock: Clock,
        dip_cfg: &WorkspaceDip,
        from_idx: f64,
        to_idx: f64,
    ) {
        if !dip_cfg.enabled || dip_cfg.strength <= 0. {
            self.dip = None;
            return;
        }

        let distance = (to_idx - from_idx).abs();
        let distance_factor = Self::compute_distance_factor(dip_cfg, distance);

        match dip_cfg.mode {
            WorkspaceDipMode::Velocity => {
                let now = clock.now();
                let cruise = if dip_cfg.cruise_hold {
                    (0.55 * distance_factor).clamp(0.35, 1.0)
                } else {
                    0.0
                };
                match &mut self.dip {
                    Some(DipRuntime::Velocity {
                        cruise_floor,
                        last_time,
                        ..
                    }) => {
                        *cruise_floor = (*cruise_floor).max(cruise);
                        *last_time = now;
                    }
                    _ => {
                        self.dip = Some(DipRuntime::Velocity {
                            depth: 0.0,
                            vel: 0.0,
                            last_time: now,
                            last_idx: from_idx,
                            cruise_floor: cruise,
                        });
                    }
                }
            }
            WorkspaceDipMode::Impulse => {
                let base_anim = dip_cfg.anim.unwrap_or(niri_config::Animation {
                    off: false,
                    kind: niri_config::animations::Kind::Spring(
                        niri_config::animations::SpringParams {
                            damping_ratio: 0.78,
                            stiffness: 480,
                            epsilon: 0.0005,
                        },
                    ),
                });
                // If already dipped with `cruise_hold` during chained switches, restart cleanly
                // from peak without jerking back to 0.
                let anim = impulse_spring(clock, 0., 1., base_anim, dip_cfg.duration_ms);
                self.dip = Some(DipRuntime::Impulse {
                    anim,
                    distance_factor,
                });
            }
            WorkspaceDipMode::Sync => {
                if dip_cfg.anim.is_none() {
                    // Pure sync mode: computed directly from switch animation / gesture progress.
                    return;
                }
                let current = match &self.dip {
                    Some(DipRuntime::TwoPhase { anim, .. }) => anim.clamped_value(),
                    Some(DipRuntime::Impulse { anim, .. }) => anim.value().clamp(0., 1.),
                    Some(DipRuntime::Velocity { depth, .. }) => depth.clamp(0., 1.),
                    None => 0.,
                };

                if let Some((shrink, _)) = Self::dip_two_phase_configs(dip_cfg) {
                    let mut anim = Animation::new(clock, current, 1., 0., shrink);
                    if let Some(total_ms) = dip_cfg.duration_ms {
                        let half_ms = u64::from(total_ms.div_ceil(2).max(1));
                        anim = anim.with_duration(Duration::from_millis(half_ms));
                    }
                    self.dip = Some(DipRuntime::TwoPhase {
                        phase: DipPhase::Shrinking,
                        anim,
                        distance_factor,
                    });
                }
            }
        }
    }

    /// Called when a touchpad/DnD workspace gesture begins so `Velocity` dip responds live.
    pub fn on_gesture_begin(&mut self, clock: Clock, dip_cfg: &WorkspaceDip, current_idx: f64) {
        if !dip_cfg.enabled || dip_cfg.strength <= 0. {
            return;
        }
        if dip_cfg.mode == WorkspaceDipMode::Velocity && self.dip.is_none() {
            self.dip = Some(DipRuntime::Velocity {
                depth: 0.0,
                vel: 0.0,
                last_time: clock.now(),
                last_idx: current_idx,
                cruise_floor: 0.0,
            });
        }
    }

    /// Advances dip, bounce, velocity tracking, and releases idle 3D offscreen buffers.
    pub fn advance(
        &mut self,
        clock: &Clock,
        dip_cfg: &WorkspaceDip,
        render_idx: f64,
        switch_anim: Option<&Animation>,
        is_gesture: bool,
        is_switch_active: bool,
    ) {
        if let Some(bounce) = &self.bounce {
            if bounce.anim.is_done() {
                self.bounce = None;
            }
        }

        // Track real-time switch velocity (workspaces / sec) for both tilt-slide and velocity dip.
        let now = clock.now();
        if let Some(anim) = switch_anim {
            self.last_switch_velocity = anim.velocity();
            self.last_sample_time = Some(now);
            self.last_sample_idx = Some(render_idx);
        } else if is_gesture {
            if let (Some(prev_t), Some(prev_idx)) = (self.last_sample_time, self.last_sample_idx) {
                let dt = now.saturating_sub(prev_t).as_secs_f64();
                if dt > 1e-4 {
                    let inst = (render_idx - prev_idx) / dt;
                    self.last_switch_velocity = self.last_switch_velocity * 0.55 + inst * 0.45;
                }
            }
            self.last_sample_time = Some(now);
            self.last_sample_idx = Some(render_idx);
        } else {
            self.last_switch_velocity = 0.;
            self.last_sample_time = None;
            self.last_sample_idx = None;
        }

        // Free 3D offscreen buffers immediately once completely idle so resting frames use 0 VRAM.
        if !is_switch_active && !self.is_bounce_active() {
            self.ws3d_buffers.clear();
        }

        // Advance active dip runtime.
        let finish_dip = match &mut self.dip {
            None => false,
            Some(DipRuntime::Impulse { anim, .. }) => anim.is_done(),
            Some(DipRuntime::TwoPhase {
                phase,
                anim,
                distance_factor: _,
            }) => {
                if !anim.is_done() {
                    false
                } else {
                    match *phase {
                        DipPhase::Shrinking => {
                            if let Some((_, expand)) = Self::dip_two_phase_configs(dip_cfg) {
                                *phase = DipPhase::Expanding;
                                let mut next = Animation::new(clock.clone(), 1., 0., 0., expand);
                                if let Some(total_ms) = dip_cfg.duration_ms {
                                    let half = total_ms.div_ceil(2);
                                    let rem = u64::from(total_ms.saturating_sub(half).max(1));
                                    next = next.with_duration(Duration::from_millis(rem));
                                }
                                *anim = next;
                                false
                            } else {
                                true
                            }
                        }
                        DipPhase::Expanding => true,
                    }
                }
            }
            Some(DipRuntime::Velocity {
                depth,
                vel,
                last_time,
                last_idx,
                cruise_floor,
            }) => {
                if clock.should_complete_instantly() {
                    true
                } else {
                    let dt = now.saturating_sub(*last_time).as_secs_f64().clamp(0., 0.05);
                    *last_time = now;

                    // Target depth combines real-time velocity + fractional displacement (for
                    // slow touchpad drags) + cruise floor during active chained switches.
                    let speed = self.last_switch_velocity.abs();
                    let frac = (render_idx - render_idx.round()).abs() * 2.0;
                    let vel_target = (speed * 0.42).tanh();
                    let target = if is_gesture {
                        (frac.powf(0.85) * 0.75 + vel_target * 0.45).clamp(0., 1.15)
                    } else if is_switch_active {
                        let base = vel_target.max(frac * 0.85);
                        if dip_cfg.cruise_hold && frac > 0.08 {
                            base.max(*cruise_floor)
                        } else {
                            *cruise_floor *= (-dt * 10.0).exp();
                            base
                        }
                    } else {
                        *cruise_floor = 0.;
                        0.0
                    };
                    *last_idx = render_idx;

                    // Spring-damper parameters (respect custom user spring if configured).
                    let (stiffness, damping_ratio) = match dip_cfg.anim.map(|a| a.kind) {
                        Some(niri_config::animations::Kind::Spring(p)) => (
                            f64::from(p.stiffness).clamp(60., 3000.),
                            p.damping_ratio.clamp(0.3, 2.0),
                        ),
                        _ => (260.0, 0.86),
                    };
                    let damping = 2.0 * damping_ratio * stiffness.sqrt();

                    // Semi-implicit Euler sub-stepping at <= 2ms for rock-solid numerical
                    // stability.
                    let steps = ((dt / 0.002).ceil() as u32).max(1);
                    let h = dt / f64::from(steps);
                    for _ in 0..steps {
                        let acc = stiffness * (target - *depth) - damping * (*vel);
                        *vel += acc * h;
                        *depth += *vel * h;
                    }
                    // Allow subtle spring rebound overshoot near 0 when returning, clamped safely.
                    *depth = depth.clamp(-0.08, 1.35);

                    !is_switch_active && depth.abs() < 0.0015 && vel.abs() < 0.01
                }
            }
        };

        if finish_dip {
            self.dip = None;
        }
    }

    /// Computes the current normalized dip depth (0 at rest, ~1 at peak dip; can slightly go
    /// negative on underdamped spring rebound for organic zoom settle).
    pub fn current_dip_depth(
        &self,
        clock: &Clock,
        dip_cfg: &WorkspaceDip,
        render_idx: f64,
        switch_anim: Option<&Animation>,
        is_gesture: bool,
        overview_fade: f64,
    ) -> f64 {
        if !dip_cfg.enabled || dip_cfg.strength <= 0. || overview_fade <= 1e-4 {
            return 0.;
        }

        let raw = match &self.dip {
            Some(DipRuntime::Velocity { depth, .. }) => *depth,
            Some(DipRuntime::Impulse {
                anim,
                distance_factor,
            }) => anim.value() * (*distance_factor),
            Some(DipRuntime::TwoPhase {
                anim,
                distance_factor,
                ..
            }) => {
                // Keep exact clamped value for Easing (preserving legacy test determinism) while
                // permitting gentle spring overshoot when the user configured a Spring!
                let v = match dip_cfg.anim.map(|a| a.kind) {
                    Some(niri_config::animations::Kind::Spring(_)) => {
                        anim.value().clamp(-0.15, 1.25)
                    }
                    _ => anim.clamped_value().clamp(0., 1.),
                };
                v * (*distance_factor)
            }
            None => {
                if dip_cfg.mode != WorkspaceDipMode::Sync || dip_cfg.anim.is_some() {
                    return 0.;
                }
                if is_gesture {
                    // Seamless live touchpad/DnD gesture response!
                    let frac = (render_idx - render_idx.round()).abs() * 2.0;
                    (std::f64::consts::FRAC_PI_2 * frac.clamp(0., 1.)).sin()
                } else if let Some(anim) = switch_anim {
                    let from = anim.from();
                    let to = anim.to();
                    if (to - from).abs() <= f64::EPSILON {
                        return 0.;
                    }
                    let distance_factor = Self::compute_distance_factor(dip_cfg, (to - from).abs());
                    // Use monotonic normalized time when `peak_position != 0.5` or spring switch
                    // to eliminate overshoot double-dip artifacts!
                    let dur = anim.duration().as_secs_f64();
                    let progress = if dur > 1e-6 {
                        (clock.now().saturating_sub(anim.start_time()).as_secs_f64() / dur)
                            .clamp(0., 1.)
                    } else {
                        ((anim.value() - from) / (to - from)).clamp(0., 1.)
                    };
                    asymmetric_bell(progress, dip_cfg.peak_position) * distance_factor
                } else {
                    return 0.;
                }
            }
        };

        raw * overview_fade
    }

    /// Applies workspace-dip zoom scaling with safe bounds.
    pub fn apply_dip_zoom(&self, base_zoom: f64, dip_cfg: &WorkspaceDip, depth: f64) -> f64 {
        if depth.abs() <= 1e-6 {
            return base_zoom;
        }
        // Clamp final zoom to prevent negative/tiny scaling under extreme distance_boost or
        // rebound.
        let factor = (1. - dip_cfg.strength * depth).clamp(0.15, 1.06);
        base_zoom * factor
    }

    /// Computes Engine A (zero-FBO) 2.5D transform for a workspace, supporting hinge pivot,
    /// distance curve, and depth dimming.
    pub fn engine_a_transform(
        geo: Rectangle<f64, Logical>,
        out_h: f64,
        cfg: WorkspaceSwitch3D,
        effect_weight: f64,
        layer_parallax_scale: f64,
    ) -> Option<Workspace3DEngineATransform> {
        if !cfg.enabled
            || cfg.style.uses_perspective_shader()
            || effect_weight <= 1e-4
            || out_h <= 0.
        {
            return None;
        }
        if cfg.depth >= 1. && cfg.squash >= 1. && cfg.dim <= 0.001 && cfg.parallax <= 0.001 {
            return None;
        }

        let center_y = geo.loc.y + geo.size.h * 0.5;
        let signed_dist = (center_y - out_h * 0.5) / out_h;
        let distance = signed_dist.abs();

        let t = (distance / cfg.radius.max(f64::EPSILON)).clamp(0., 1.);
        let eased = t.powf(cfg.curve_power.max(0.)) * effect_weight;

        // Layer parallax modulates effective depth subtly across wallpaper / tiling / floating.
        let eff_depth = (1. - (1. - cfg.depth) * eased * layer_parallax_scale).clamp(0.05, 1.25);
        let eff_squash = (1. - (1. - cfg.squash) * eased).clamp(0.05, 1.0);

        let pivot_y = match cfg.pivot {
            WorkspaceSwitch3DPivot::Center => center_y,
            WorkspaceSwitch3DPivot::Hinge => {
                // Upper workspaces anchor near their bottom edge; lower workspaces anchor near
                // their top edge so the boundary between adjacent workspaces stays tight!
                let bias = (signed_dist * 2.0).clamp(-1.0, 1.0);
                center_y - bias * (geo.size.h * 0.5)
            }
        };
        let pivot = Point::from((geo.loc.x + geo.size.w * 0.5, pivot_y));
        let dim_alpha = (cfg.dim * eased as f32).clamp(0., 0.85);

        Some(Workspace3DEngineATransform {
            scale: Scale::from((eff_depth, eff_depth * eff_squash)),
            pivot,
            eased,
            dim_alpha,
        })
    }
}
