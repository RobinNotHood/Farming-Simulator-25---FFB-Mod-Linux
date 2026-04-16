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
    ///
    /// Design notes (tractor feel):
    ///
    /// * Spring is the primary centering source and has a non-zero floor
    ///   (`cfg.spring_floor`) even at rest so the wheel never goes dead.
    ///   `spring_center` is hard 0 — we don't couple it to roll, because
    ///   that makes the spring fight the constant force unpredictably.
    /// * Damper scales with both speed and implement mass and is the
    ///   dominant "weight of the hydraulics" feel.
    /// * Constant force is heavily gated: it only engages when the vehicle
    ///   is actually moving (>5 km/h) AND the driver is actually steering.
    ///   We then draw from slip differential (a real physical signal) plus
    ///   a small attenuated lateral accel. Output is hard-capped to
    ///   `1 - spring_strength` so |constant| + spring never saturates.
    /// * Rumble fires only on real suspension compressions and at idle;
    ///   the old speed-swept sine sounded like a race sim, not a tractor.
    pub fn compute(&mut self, t: &Telemetry, cfg: &TuningConfig, dt: f32) -> EffectOutput {
        if !t.in_vehicle() || !cfg.enabled {
            return EffectOutput::default();
        }

        let speed = t.speed_mps.abs();
        let speed_kmh = speed * 3.6;

        // ---- Spring (dominant centering) ---------------------------------
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

        let spring_dynamic = cfg.spring_gain * speed_factor * ps_factor * mass_factor;
        let spring_strength = (cfg.spring_floor + spring_dynamic).clamp(0.0, 1.0);
        // Always zero — centering must be predictable. The old roll-coupled
        // center offset fought the constant-force output on slopes.
        let spring_center = 0.0_f32;

        // ---- Damper (hydraulic weight) -----------------------------------
        let implement_factor =
            1.0 + (t.attached_mass / 5_000.0).clamp(0.0, 3.0) * cfg.implement_damper_scale;
        let speed_damp = 1.0 + 0.5 * speed_factor;
        let damper = (cfg.damper_gain * implement_factor * speed_damp).clamp(0.0, 1.0);

        // ---- Constant (gated lateral pull + verified impacts) ------------
        //
        // Both gates must open: real driving speed and real steering input.
        // This is what kills the "rip" on a tractor rolling straight ahead
        // on ploughed soil — speed may be fine, but steering is ~0.
        let speed_gate = smoothstep(4.0, 8.0, speed_kmh);
        let steer_gate = smoothstep(0.03, 0.15, t.steering_angle.abs());
        let gate = speed_gate * steer_gate;

        // Slip-differential is a physical signal; lateral_accel is only
        // lightly mixed in as seasoning (and is now local-frame from Lua
        // so straight driving no longer jitters).
        let raw_lat = (t.slip_front - t.slip_rear) * 0.6 + t.lateral_accel.clamp(-4.0, 4.0) * 0.05;
        self.smoothed_lateral = lerp(self.smoothed_lateral, raw_lat, (dt * 6.0).min(1.0));
        let lat_const =
            (self.smoothed_lateral * cfg.lateral_gain * speed_factor * gate).clamp(-0.5, 0.5);

        // Collisions (already impulse-thresholded + cooldown-gated in Lua).
        let collision_kick = t.collision.clamp(0.0, 1.0) * cfg.collision_gain;
        let collision_sign = if t.steering_angle >= 0.0 { -1.0 } else { 1.0 };
        let constant_raw = lat_const + collision_kick * collision_sign;

        // Saturation guard: total of |constant| + spring may not exceed 1.0,
        // else MOZA firmware clips unpredictably and the wheel "rips".
        let headroom = (1.0 - spring_strength).max(0.0);
        let constant_gated = constant_raw.clamp(-headroom, headroom);

        // ---- Rumble (event-only) -----------------------------------------
        //
        // No speed-swept sine. Only real suspension events fire the shaker,
        // plus a subtle idle shake when the engine is on and we're stopped.
        let susp_avg = (t.susp_fl + t.susp_fr) * 0.5;
        let susp_event = (susp_avg - 0.55).max(0.0);
        let rough = t.ground_roughness * (1.0 - t.ground_hardness);
        self.smoothed_rough = lerp(self.smoothed_rough, rough, (dt * 8.0).min(1.0));
        let idle_rumble = if t.rpm > 0.01 && speed < 0.5 {
            cfg.idle_rumble * (1.0 - t.rpm).max(0.0)
        } else {
            0.0
        };
        let rumble = (cfg.rumble_gain * (susp_event * 0.8 + idle_rumble)).clamp(0.0, 1.0);
        let rumble_period_ms: u16 = 60;

        // ---- Reverse handling --------------------------------------------
        //
        // Invert the constant pull (driver sees the world flipped). Spring
        // stays stable; real tractors don't loosen up in reverse, and
        // weakening it here just made the wheel float.
        let constant_final = if t.reversing() {
            -constant_gated * 0.6
        } else {
            constant_gated
        };

        let g = cfg.master_gain.clamp(0.0, 2.0);

        self.rumble_phase += dt;

        EffectOutput {
            constant: (constant_final * g).clamp(-1.0, 1.0),
            spring_strength: (spring_strength * g).clamp(0.0, 1.0),
            spring_center,
            damper: (damper * g).clamp(0.0, 1.0),
            rumble_magnitude: (rumble * g).clamp(0.0, 1.0),
            rumble_period_ms,
            torque_estimate_nm: lat_const.abs() * 8.0 + spring_strength * 6.0,
        }
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
        // In the new model lateral constant is gated on speed AND steer
        // input; slip differential drives the magnitude. Give both gates
        // something to open with.
        t.speed_mps = 5.0; // 18 km/h, past the 8 km/h gate
        t.steering_angle = 0.2; // past the 0.15 steer gate
        t.slip_front = 0.5;
        t.slip_rear = 0.0;

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
