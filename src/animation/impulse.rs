use std::time::Duration;

use super::{Animation, Clock, Curve, Spring, SpringParams};

/// Computes the exact initial velocity `v0` so that a spring starting at `rest_value` with
/// `from == to == rest_value` reaches an exact peak offset of `peak_delta` (`rest_value +
/// peak_delta`) before smoothly settling back to `rest_value`.
pub fn impulse_initial_velocity(peak_delta: f64, params: SpringParams) -> f64 {
    if peak_delta.abs() <= f64::EPSILON || params.stiffness <= 0. || params.mass <= 0. {
        return 0.;
    }

    let beta = params.damping / (2. * params.mass);
    let omega0 = (params.stiffness / params.mass).sqrt();

    if beta <= f64::EPSILON {
        // Undamped harmonic oscillator: x(t) = (v0 / omega0) * sin(omega0 * t)
        return peak_delta * omega0;
    }

    if (beta - omega0).abs() <= f64::from(f32::EPSILON) {
        // Critically damped: x(t) = v0 * t * exp(-omega0 * t), peak at t = 1/omega0.
        peak_delta * std::f64::consts::E * omega0
    } else if beta < omega0 {
        // Underdamped: x(t) = (v0 / omega1) * exp(-beta * t) * sin(omega1 * t),
        // peak at t_peak = atan(omega1 / beta) / omega1,
        // x(t_peak) = (v0 / omega0) * exp(-(beta / omega1) * atan(omega1 / beta)).
        let omega1 = (omega0 * omega0 - beta * beta).sqrt();
        let angle = (omega1 / beta).atan();
        peak_delta * omega0 * ((beta / omega1) * angle).exp()
    } else {
        // Overdamped: x(t) = (v0 / omega2) * exp(-beta * t) * sinh(omega2 * t),
        // peak at t_peak = atanh(omega2 / beta) / omega2.
        let omega2 = (beta * beta - omega0 * omega0).sqrt();
        let ratio = (omega2 / beta).clamp(-0.999_999, 0.999_999);
        let t_peak = ratio.atanh() / omega2;
        let peak_unit = (-beta * t_peak).exp() * (omega2 * t_peak).sinh() / omega2;
        if peak_unit.abs() <= f64::EPSILON {
            0.
        } else {
            peak_delta / peak_unit
        }
    }
}

/// Creates a C-infinity continuous single-phase impulse animation starting and ending at
/// `rest_value`, peaking at `rest_value + peak_delta`.
///
/// For spring configs this uses a true physical spring impulse (preserving bounce/overshoot).
/// If `duration_override_ms` is given, the spring is time-scaled to match that total settling
/// duration while preserving its exact bounce shape.
pub fn impulse_spring(
    clock: Clock,
    rest_value: f64,
    peak_delta: f64,
    config: niri_config::Animation,
    duration_override_ms: Option<u32>,
) -> Animation {
    if config.off || peak_delta.abs() <= f64::EPSILON {
        return Animation::ease(clock, rest_value, rest_value, 0., 0, Curve::Linear);
    }

    let params = match config.kind {
        niri_config::animations::Kind::Spring(p) => {
            // When explicitly time-scaled via `duration-ms`, avoid wasting >40% of the duration on
            // an invisible sub-0.3% exponential tail so the peak & rebound feel crisp and
            // responsive.
            let eff_epsilon = if duration_override_ms.is_some() {
                p.epsilon.max(0.003)
            } else {
                p.epsilon
            };
            SpringParams::new(p.damping_ratio, f64::from(p.stiffness), eff_epsilon)
        }
        niri_config::animations::Kind::Easing(p) => {
            // Convert easing duration into a critically damped impulse spring so even easing
            // configs benefit from smooth zero-kink impulse motion when `mode "impulse"` is used.
            let target_ms = duration_override_ms.unwrap_or(p.duration_ms).max(1);
            let params = SpringParams::new(0.92, 900., 0.0005);
            let v0 = impulse_initial_velocity(peak_delta, params);
            let spring = Spring {
                from: rest_value,
                to: rest_value,
                initial_velocity: v0,
                params,
            };
            return Animation::spring(clock, spring)
                .with_duration(Duration::from_millis(u64::from(target_ms)));
        }
    };

    let v0 = impulse_initial_velocity(peak_delta, params);
    let spring = Spring {
        from: rest_value,
        to: rest_value,
        initial_velocity: v0,
        params,
    };
    let anim = Animation::spring(clock, spring);
    if let Some(ms) = duration_override_ms {
        anim.with_duration(Duration::from_millis(u64::from(ms.max(1))))
    } else {
        anim
    }
}

/// Smooth asymmetric bell curve on `t in [0, 1]` with `bell(0) == 0`, `bell(1) == 0`, and
/// exact peak `bell(peak_pos) == 1.0`.
///
/// When `peak_pos == 0.5`, this reduces identically to `sin(pi * t)`.
pub fn asymmetric_bell(t: f64, peak_pos: f64) -> f64 {
    let t = t.clamp(0., 1.);
    if t <= 0. || t >= 1. {
        return 0.;
    }
    let p = peak_pos.clamp(0.05, 0.95);
    let u = if (p - 0.5).abs() < 1e-6 {
        t
    } else {
        let gamma = -std::f64::consts::LN_2 / p.ln();
        t.powf(gamma)
    };
    (std::f64::consts::PI * u).sin().clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;

    use super::*;

    fn sample_peak(spring: Spring, rest: f64, negative: bool) -> f64 {
        let dur = spring.duration().as_secs_f64();
        assert!(dur > 0.01 && dur < 10.0, "unexpected duration: {dur}");
        let mut best = rest;
        let steps = 4000;
        for i in 0..=steps {
            let t = Duration::from_secs_f64(dur * (i as f64 / steps as f64));
            let v = spring.value_at(t);
            if negative {
                if v < best {
                    best = v;
                }
            } else if v > best {
                best = v;
            }
        }
        best
    }

    #[test]
    fn impulse_spring_hits_exact_peak_across_damping_regimes() {
        for damping in [0.5, 0.75, 1.0, 1.35] {
            let params = SpringParams::new(damping, 900., 0.0001);
            for delta in [-0.25, 0.15] {
                let v0 = impulse_initial_velocity(delta, params);
                let spring = Spring {
                    from: 1.0,
                    to: 1.0,
                    initial_velocity: v0,
                    params,
                };
                let peak = sample_peak(spring, 1.0, delta < 0.);
                assert_abs_diff_eq!(peak, 1.0 + delta, epsilon = 1e-3);
            }
        }
    }

    #[test]
    fn asymmetric_bell_properties() {
        assert_abs_diff_eq!(asymmetric_bell(0.0, 0.35), 0.0, epsilon = 1e-9);
        assert_abs_diff_eq!(asymmetric_bell(1.0, 0.35), 0.0, epsilon = 1e-9);
        assert_abs_diff_eq!(asymmetric_bell(0.35, 0.35), 1.0, epsilon = 1e-6);
        assert_abs_diff_eq!(asymmetric_bell(0.5, 0.5), 1.0, epsilon = 1e-6);
    }
}
