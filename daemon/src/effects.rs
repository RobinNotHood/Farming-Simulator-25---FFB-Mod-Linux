//! FFB effect engine.
//!
//! Translates telemetry -> a bundle of effect targets (constant force,
//! spring, damper, periodic rumble). The ffb module turns that bundle into
//! evdev ioctl writes.
//!
//! Each effect is clamped to [-1, 1] or [0, 1]; ffb::scale_to_i16 maps to
//! -32767..32767 at the kernel boundary. We keep all the physics reasoning
//! in this module so it stays unit-testable without /dev/input access.

use crate::config::TuningConfig;
use crate::telemetry::Telemetry;

/// All outputs are normalized. Positive constant force = pull right.
#[derive(Debug, Default, Clone, Copy)]
pub struct EffectOutput {
    pub constant: f32,         // -1..1
    pub spring_strength: f32,  // 0..1
    pub spring_center: f32,    // -1..1
    pub damper: f32,           // 0..1
    pub rumble_magnitude: f32, // 0..1
    pub rumble_period_ms: u16,
    pub torque_estimate_nm: f32, // debug only
}

pub struct EffectEngine {
    smoothed_lateral: f32,
    smoothed_rough: f32,
    rumble_phase: f32,
}

impl EffectEngine {
    pub fn new() -> Self {
        Self {
            smoothed_lateral: 0.0,
            smoothed_rough: 0.0,
            rumble_phase: 0.0,
        }
    }

    /// Compute effects for the given telemetry. `dt` is the elapsed time
    /// since the last compute(), in seconds. `cfg` may be mutated by the
    /// GUI between ticks; we take it by ref each call to pick up changes
    /// without restart.
    pub fn compute(&mut self, t: &Telemetry, cfg: &TuningConfig, dt: f32) -> EffectOutput {
        if !t.in_vehicle() || !cfg.enabled {
            return EffectOutput::default();
        }

        let speed = t.speed_mps.abs();
        let speed_kmh = speed * 3.6;

        // -------------------------------------------------------------
        // Speed-dependent centering spring
        //
        // FS25's stock centering is a weak constant. We emit a real SPRING
        // effect keyed on speed: barely any force at standstill, peaking
        // around 30 km/h. On slow tractors 100% spring above 30 km/h feels
        // natural; the user can still override with the strength slider.
        // -------------------------------------------------------------
        let speed_factor = smoothstep(
            cfg.speed_curve_start_kmh,
            cfg.speed_curve_max_kmh,
            speed_kmh,
        );
        let ps_factor = if t.has_power_steering() {
            1.0
        } else {
            cfg.no_power_steer_boost
        };
        let mass_factor = 1.0 + (t.total_mass / 10_000.0).clamp(0.0, 2.0) * cfg.mass_spring_scale;
        let spring_strength =
            (cfg.spring_gain * speed_factor * ps_factor * mass_factor).clamp(0.0, 1.0);

        // Shift center toward downhill when on a slope, scaled by roll.
        let slope_offset = (t.roll * cfg.slope_gain).clamp(-cfg.slope_max, cfg.slope_max);

        // -------------------------------------------------------------
        // Lateral-force constant (the "weight of the front axle")
        //
        // In a real vehicle, the steering wheel pulls in the direction of
        // the tire slip angle. We approximate slip*load and feed it as a
        // constant force. Low-pass it so the output isn't twitchy.
        // -------------------------------------------------------------
        let raw_lat = t.lateral_accel * 0.1 + (t.slip_front - t.slip_rear) * 0.8;
        self.smoothed_lateral = lerp(self.smoothed_lateral, raw_lat, (dt * 12.0).min(1.0));

        let lat_const = (self.smoothed_lateral * cfg.lateral_gain * speed_factor).clamp(-1.0, 1.0);

        // -------------------------------------------------------------
        // Damper: resists rapid steering input, scales with implement mass
        // -------------------------------------------------------------
        let implement_factor =
            1.0 + (t.attached_mass / 5_000.0).clamp(0.0, 3.0) * cfg.implement_damper_scale;
        let damper = (cfg.damper_gain * implement_factor).clamp(0.0, 1.0);

        // -------------------------------------------------------------
        // Rumble: tire deformation + suspension bumps + road roughness +
        //         engine idle vibration
        //
        // We drive a periodic (triangle) effect; magnitude = how hard it
        // shakes, period = how fast. Lower period at higher speed.
        // -------------------------------------------------------------
        let susp_avg = (t.susp_fl + t.susp_fr) * 0.5;
        let susp_event = (susp_avg - 0.3).max(0.0); // only high compressions rumble
        let rough = t.ground_roughness * (1.0 - t.ground_hardness);
        self.smoothed_rough = lerp(self.smoothed_rough, rough, (dt * 8.0).min(1.0));

        let idle_rumble = if t.rpm > 0.01 && speed < 0.5 {
            cfg.idle_rumble * (1.0 - t.rpm).max(0.0)
        } else {
            0.0
        };

        let mut rumble = cfg.rumble_gain
            * ((susp_event * 1.2) + (self.smoothed_rough * speed_factor * 0.8) + idle_rumble);
        rumble = rumble.clamp(0.0, 1.0);

        let rumble_period_ms = {
            // Higher speed / rougher ground -> faster rumble.
            let base = 80.0 - speed_factor * 50.0 - self.smoothed_rough * 20.0;
            base.clamp(15.0, 90.0) as u16
        };

        // -------------------------------------------------------------
        // Collision one-shot: momentary constant spike
        // -------------------------------------------------------------
        let collision_kick = t.collision.clamp(0.0, 1.0) * cfg.collision_gain;
        let sign = if t.steering_angle >= 0.0 { -1.0 } else { 1.0 };
        let collision_const = collision_kick * sign;

        let constant = (lat_const + collision_const).clamp(-1.0, 1.0);

        // -------------------------------------------------------------
        // Reversing: invert constant, soften spring (real drivers expect
        // wheel to be lighter when crawling backward).
        // -------------------------------------------------------------
        let (constant, spring_strength) = if t.reversing() {
            (-constant * 0.7, spring_strength * 0.5)
        } else {
            (constant, spring_strength)
        };

        // -------------------------------------------------------------
        // Global master gain
        // -------------------------------------------------------------
        let g = cfg.master_gain.clamp(0.0, 2.0);

        let out = EffectOutput {
            constant: (constant * g).clamp(-1.0, 1.0),
            spring_strength: (spring_strength * g).clamp(0.0, 1.0),
            spring_center: slope_offset.clamp(-1.0, 1.0),
            damper: (damper * g).clamp(0.0, 1.0),
            rumble_magnitude: (rumble * g).clamp(0.0, 1.0),
            rumble_period_ms,
            torque_estimate_nm: lat_const.abs() * 8.0 + spring_strength * 4.0,
        };

        self.rumble_phase += dt;
        out
    }
}

#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if b <= a {
        return if x >= a { 1.0 } else { 0.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TuningConfig;

    fn default_tele() -> Telemetry {
        Telemetry {
            flags: 0x01 | 0x04, // in vehicle, has power steering
            ..Telemetry::default()
        }
    }

    #[test]
    fn no_output_when_not_in_vehicle() {
        let mut engine = EffectEngine::new();
        let mut t = default_tele();
        t.flags = 0;
        let out = engine.compute(&t, &TuningConfig::default(), 0.016);
        assert_eq!(out.constant, 0.0);
        assert_eq!(out.spring_strength, 0.0);
    }

    #[test]
    fn spring_grows_with_speed() {
        let mut engine = EffectEngine::new();
        let mut t = default_tele();
        let cfg = TuningConfig::default();

        t.speed_mps = 0.0;
        let low = engine.compute(&t, &cfg, 0.016).spring_strength;

        t.speed_mps = 15.0; // ~54 km/h
                            // warm up the smoother
        for _ in 0..60 {
            engine.compute(&t, &cfg, 0.016);
        }
        let high = engine.compute(&t, &cfg, 0.016).spring_strength;

        assert!(
            high > low,
            "expected spring at speed > at rest (got {} vs {})",
            high,
            low
        );
    }

    #[test]
    fn reversing_inverts_constant() {
        let mut engine = EffectEngine::new();
        let mut t = default_tele();
        t.speed_mps = 5.0;
        t.lateral_accel = 3.0;

        for _ in 0..30 {
            engine.compute(&t, &TuningConfig::default(), 0.016);
        }
        let forward = engine.compute(&t, &TuningConfig::default(), 0.016).constant;

        t.flags |= 0x08;
        let reverse = engine.compute(&t, &TuningConfig::default(), 0.016).constant;
        // With reverse flag set the sign flips (approximately).
        assert!(forward.signum() != reverse.signum() || reverse.abs() < forward.abs());
    }

    #[test]
    fn collision_creates_kick() {
        let mut engine = EffectEngine::new();
        let mut t = default_tele();
        t.collision = 0.8;
        t.steering_angle = 0.1;

        let out = engine.compute(&t, &TuningConfig::default(), 0.016);
        assert!(out.constant.abs() > 0.1);
    }
}
