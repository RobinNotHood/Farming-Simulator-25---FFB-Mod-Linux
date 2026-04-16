//! Low-level FFB driver. Uploads effects via the Linux kernel `evdev`
//! FF_UPLOAD ioctl and starts/stops them with EV_FF write events.
//!
//! We use `input_linux::EvdevHandle` for the `EVIOCSFF` ioctl because
//! the `evdev` crate does not expose effect upload. The `evdev` crate
//! is still used for enumeration (nicer API for device metadata).
//!
//! `input_linux_sys::ff_effect` stores effect-kind-specific data in a
//! `[u64; 4]` union-like buffer. We access it through the typed
//! accessors on `ff_effect_union` (`.constant_mut()`, `.condition_mut()`,
//! `.periodic_mut()`, `.rumble_mut()`).
//!
//! Kernel reference: Documentation/input/ff.rst.
//!
//! Effects we allocate up-front and re-use:
//!
//! * effect_constant : FF_CONSTANT  -- lateral force pull
//! * effect_spring   : FF_SPRING    -- centering torque
//! * effect_damper   : FF_DAMPER    -- steering-rate damping
//! * effect_rumble   : FF_PERIODIC  -- surface texture (sine wave)
//! * effect_kick     : FF_CONSTANT  -- short collision one-shot
//!
//! On devices that lack FF_SPRING we synthesise a spring from FF_CONSTANT
//! plus telemetry steering position; same for FF_DAMPER. That keeps the
//! behaviour consistent across wheel vendors.

use crate::clap_lite::TestEffect;
use crate::config::DeviceHint;
use crate::effects::EffectOutput;
use crate::shared::DeviceInfo;
use anyhow::{anyhow, Context, Result};
use input_linux::EvdevHandle;
use input_linux_sys as sys;
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct FfbDevice {
    path: PathBuf,
    file: File,
    info: DeviceInfo,
    // slot ids of uploaded effects
    id_constant: Option<i16>,
    id_spring: Option<i16>,
    id_damper: Option<i16>,
    id_rumble: Option<i16>,
    #[allow(dead_code)] // reserved for collision one-shot (future use)
    id_kick: Option<i16>,
    // last values written (to debounce unchanged uploads)
    last_constant: i16,
    last_spring: u16,
    last_damper: u16,
    last_rumble_mag: u16,
    last_rumble_period: u16,
}

impl FfbDevice {
    /// Open a device by hint and pre-upload the five effect slots. Returns
    /// an error if the kernel rejects uploads (e.g. wrong driver, no FF bits).
    pub fn open(hint: &DeviceHint) -> Result<Self> {
        let (path, info) = crate::device::open_matching(hint)?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| {
                format!("opening {:?} for RW (need udev rule or input group)", path)
            })?;

        let mut d = Self {
            path,
            file,
            info,
            id_constant: None,
            id_spring: None,
            id_damper: None,
            id_rumble: None,
            id_kick: None,
            last_constant: 0,
            last_spring: 0,
            last_damper: 0,
            last_rumble_mag: 0,
            last_rumble_period: 0,
        };

        // Set master gain to 100%; user tunes via our own master_gain slider.
        // Do this *before* uploading so effect strengths reflect raw data.
        let _ = d.set_gain(0xFFFF);
        let _ = d.set_autocenter(0);

        if d.info.supports_constant {
            d.id_constant = Some(d.upload_constant(0, 0xFFFF)?);
            d.id_kick = Some(d.upload_constant(0, 300)?); // 300ms one-shot
        }
        if d.info.supports_spring {
            d.id_spring = Some(d.upload_spring(0, 0)?);
        }
        if d.info.supports_damper {
            d.id_damper = Some(d.upload_damper(0)?);
        }
        if d.info.supports_periodic {
            d.id_rumble = Some(d.upload_periodic(0, 50)?);
        } else if d.info.supports_rumble {
            d.id_rumble = Some(d.upload_rumble(0)?);
        }

        Ok(d)
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Push the current effect target. Called at `output_hz`.
    pub fn apply(&mut self, out: &EffectOutput) -> Result<()> {
        if let Some(id) = self.id_constant {
            let lvl = scale_to_i16(out.constant);
            // Widen to i32 before subtracting: i16::MIN - i16::MAX wraps.
            if (lvl as i32 - self.last_constant as i32).abs() > 150 {
                self.update_constant(id, lvl, 0xFFFF)?;
                self.start(id)?;
                self.last_constant = lvl;
            }
        }
        if let Some(id) = self.id_spring {
            let s = (out.spring_strength * 0xFFFF as f32) as u16;
            let center = scale_to_i16(out.spring_center);
            if s.abs_diff(self.last_spring) > 256 || s > 0 {
                self.update_spring(id, s, center)?;
                if s > 0 {
                    self.start(id)?;
                } else {
                    self.stop(id)?;
                }
                self.last_spring = s;
            }
        }
        if let Some(id) = self.id_damper {
            let d = (out.damper * 0xFFFF as f32) as u16;
            if d.abs_diff(self.last_damper) > 256 {
                self.update_damper(id, d)?;
                if d > 0 {
                    self.start(id)?;
                } else {
                    self.stop(id)?;
                }
                self.last_damper = d;
            }
        }
        if let Some(id) = self.id_rumble {
            let mag = (out.rumble_magnitude * 0x7FFF as f32) as u16;
            let period = out.rumble_period_ms;
            if mag.abs_diff(self.last_rumble_mag) > 256
                || period.abs_diff(self.last_rumble_period) > 5
            {
                if self.info.supports_periodic {
                    self.update_periodic(id, mag, period)?;
                } else {
                    self.update_rumble(id, mag)?;
                }
                if mag > 0 {
                    self.start(id)?;
                } else {
                    self.stop(id)?;
                }
                self.last_rumble_mag = mag;
                self.last_rumble_period = period;
            }
        }
        Ok(())
    }

    /// Stop everything. Called on shutdown and on pause.
    pub fn stop_all(&mut self) -> Result<()> {
        for id in [
            self.id_constant,
            self.id_spring,
            self.id_damper,
            self.id_rumble,
        ]
        .into_iter()
        .flatten()
        {
            let _ = self.stop(id);
        }
        let _ = self.set_gain(0xFFFF);
        Ok(())
    }

    // ---------------------------------------------------------------
    // ioctl wrappers: upload/update an effect slot
    // ---------------------------------------------------------------

    fn upload_effect(&self, eff: &mut sys::ff_effect) -> Result<()> {
        // SAFETY: `from_raw_fd`-style usage via as_raw_fd; the handle
        // does not take ownership.
        let handle = unsafe { EvdevHandle::from_fd(self.file.as_raw_fd()) };
        let rc = handle.send_force_feedback(eff);
        std::mem::forget(handle); // don't close the fd on drop
        rc.map(|_| ()).with_context(|| {
            format!(
                "EVIOCSFF failed on {:?}. Try `fs25-ffb --diagnose`.",
                self.path
            )
        })
    }

    fn upload_constant(&self, level: i16, duration_ms: u16) -> Result<i16> {
        let mut eff = new_effect(sys::FF_CONSTANT, duration_ms);
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            u.constant_mut().level = level;
        }
        self.upload_effect(&mut eff)?;
        Ok(eff.id)
    }

    fn update_constant(&self, id: i16, level: i16, duration_ms: u16) -> Result<()> {
        let mut eff = new_effect(sys::FF_CONSTANT, duration_ms);
        eff.id = id;
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            u.constant_mut().level = level;
        }
        self.upload_effect(&mut eff)
    }

    fn upload_spring(&self, strength: u16, center: i16) -> Result<i16> {
        let mut eff = new_effect(sys::FF_SPRING, 0);
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let conds = u.condition_mut();
            for cond in conds.iter_mut() {
                cond.right_saturation = strength;
                cond.left_saturation = strength;
                cond.right_coeff = strength as i16;
                cond.left_coeff = strength as i16;
                cond.deadband = 0;
                cond.center = center;
            }
        }
        self.upload_effect(&mut eff)?;
        Ok(eff.id)
    }

    fn update_spring(&self, id: i16, strength: u16, center: i16) -> Result<()> {
        let mut eff = new_effect(sys::FF_SPRING, 0);
        eff.id = id;
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let conds = u.condition_mut();
            for cond in conds.iter_mut() {
                cond.right_saturation = strength;
                cond.left_saturation = strength;
                let c = (strength as i32).min(0x7FFF) as i16;
                cond.right_coeff = c;
                cond.left_coeff = c;
                cond.center = center;
            }
        }
        self.upload_effect(&mut eff)
    }

    fn upload_damper(&self, strength: u16) -> Result<i16> {
        let mut eff = new_effect(sys::FF_DAMPER, 0);
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let conds = u.condition_mut();
            for cond in conds.iter_mut() {
                cond.right_saturation = strength;
                cond.left_saturation = strength;
                cond.right_coeff = strength as i16;
                cond.left_coeff = strength as i16;
            }
        }
        self.upload_effect(&mut eff)?;
        Ok(eff.id)
    }

    fn update_damper(&self, id: i16, strength: u16) -> Result<()> {
        let mut eff = new_effect(sys::FF_DAMPER, 0);
        eff.id = id;
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let conds = u.condition_mut();
            for cond in conds.iter_mut() {
                cond.right_saturation = strength;
                cond.left_saturation = strength;
                cond.right_coeff = strength as i16;
                cond.left_coeff = strength as i16;
            }
        }
        self.upload_effect(&mut eff)
    }

    fn upload_periodic(&self, magnitude: u16, period_ms: u16) -> Result<i16> {
        let mut eff = new_effect(sys::FF_PERIODIC, 0);
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let p = u.periodic_mut();
            p.waveform = sys::FF_SINE;
            p.period = period_ms;
            p.magnitude = magnitude as i16;
            p.offset = 0;
            p.phase = 0;
        }
        self.upload_effect(&mut eff)?;
        Ok(eff.id)
    }

    fn update_periodic(&self, id: i16, magnitude: u16, period_ms: u16) -> Result<()> {
        let mut eff = new_effect(sys::FF_PERIODIC, 0);
        eff.id = id;
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let p = u.periodic_mut();
            p.waveform = sys::FF_SINE;
            p.period = period_ms;
            p.magnitude = magnitude as i16;
        }
        self.upload_effect(&mut eff)
    }

    fn upload_rumble(&self, strong: u16) -> Result<i16> {
        let mut eff = new_effect(sys::FF_RUMBLE, 0);
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let r = u.rumble_mut();
            r.strong_magnitude = strong;
            r.weak_magnitude = strong / 2;
        }
        self.upload_effect(&mut eff)?;
        Ok(eff.id)
    }

    fn update_rumble(&self, id: i16, strong: u16) -> Result<()> {
        let mut eff = new_effect(sys::FF_RUMBLE, 0);
        eff.id = id;
        {
            let u: &mut sys::ff_effect_union = (&mut eff).into();
            let r = u.rumble_mut();
            r.strong_magnitude = strong;
            r.weak_magnitude = strong / 2;
        }
        self.upload_effect(&mut eff)
    }

    // ---------------------------------------------------------------
    // EV_FF event writes
    // ---------------------------------------------------------------

    fn start(&self, id: i16) -> Result<()> {
        self.write_ev(sys::EV_FF as u16, id as u16, 1)
    }

    fn stop(&self, id: i16) -> Result<()> {
        self.write_ev(sys::EV_FF as u16, id as u16, 0)
    }

    fn set_gain(&self, gain_0_ffff: u16) -> Result<()> {
        self.write_ev(sys::EV_FF as u16, sys::FF_GAIN, gain_0_ffff as i32)
    }

    fn set_autocenter(&self, strength_0_ffff: u16) -> Result<()> {
        self.write_ev(
            sys::EV_FF as u16,
            sys::FF_AUTOCENTER,
            strength_0_ffff as i32,
        )
    }

    fn write_ev(&self, type_: u16, code: u16, value: i32) -> Result<()> {
        let ev = sys::input_event {
            time: sys::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };
        // SAFETY: we write exactly one input_event struct; the fd is
        // open for RW and owned by `self.file`.
        let n = unsafe {
            libc::write(
                self.file.as_raw_fd(),
                &ev as *const _ as *const libc::c_void,
                std::mem::size_of::<sys::input_event>(),
            )
        };
        if n < 0 {
            return Err(anyhow!(
                "write(EV_FF) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl Drop for FfbDevice {
    fn drop(&mut self) {
        let _ = self.stop_all();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_effect(kind: u16, duration_ms: u16) -> sys::ff_effect {
    // SAFETY: ff_effect is POD; all-zeros is a valid initial state.
    let mut eff: sys::ff_effect = unsafe { std::mem::zeroed() };
    eff.type_ = kind;
    eff.id = -1;
    eff.replay.length = duration_ms;
    eff.replay.delay = 0;
    eff.direction = 0x4000; // 90 deg, magnitude in +X
    eff
}

/// Scale -1..1 float to i16 for `FF_CONSTANT.level` (-32767..32767).
#[inline]
pub fn scale_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Public CLI entry point for `fs25-ffb --test <kind>`.
pub fn run_manual_test(kind: TestEffect) -> Result<()> {
    let (cfg, _) = crate::config::Config::load(None)?;
    let mut dev = FfbDevice::open(&cfg.device_hint)?;
    let info = dev.info().clone();
    println!(
        "Using {} at {} (vendor 0x{:04X}, product 0x{:04X})",
        info.name, info.path, info.vendor_id, info.product_id
    );

    match kind {
        TestEffect::Constant => {
            println!("3 seconds of 40% constant force to the right...");
            dev.apply(&EffectOutput {
                constant: 0.4,
                ..Default::default()
            })?;
            std::thread::sleep(Duration::from_secs(3));
        }
        TestEffect::Spring => {
            println!("5 seconds of 50% centering spring (turn the wheel and release)...");
            dev.apply(&EffectOutput {
                spring_strength: 0.5,
                ..Default::default()
            })?;
            std::thread::sleep(Duration::from_secs(5));
        }
        TestEffect::Damper => {
            println!("5 seconds of 60% damper (wheel should feel slow)...");
            dev.apply(&EffectOutput {
                damper: 0.6,
                ..Default::default()
            })?;
            std::thread::sleep(Duration::from_secs(5));
        }
        TestEffect::Rumble => {
            println!("3 seconds of rumble...");
            dev.apply(&EffectOutput {
                rumble_magnitude: 0.5,
                rumble_period_ms: 40,
                ..Default::default()
            })?;
            std::thread::sleep(Duration::from_secs(3));
        }
        TestEffect::Sine => {
            println!("Sweeping sine for 5 seconds...");
            for i in 0..50 {
                let period = 80 - i;
                dev.apply(&EffectOutput {
                    rumble_magnitude: 0.4,
                    rumble_period_ms: period as u16,
                    ..Default::default()
                })?;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    dev.stop_all()?;
    println!("done.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    // Sanity-check the `ff_effect` layout we depend on. If the upstream
    // struct grows we want CI to flag it, not silent corruption.
    #[test]
    fn ff_effect_layout_fits_u64_4() {
        // The typed accessors we use cast the 4-u64 payload to the
        // specific effect struct. Largest user of the payload is
        // `ff_periodic_effect` which must fit within 32 bytes on LP64.
        assert!(size_of::<sys::ff_periodic_effect>() <= 32);
        assert!(size_of::<sys::ff_condition_effect>() * 2 <= 32);
        assert!(size_of::<sys::ff_constant_effect>() <= 32);
        assert!(size_of::<sys::ff_rumble_effect>() <= 32);
    }

    #[test]
    fn scale_to_i16_boundaries() {
        assert_eq!(scale_to_i16(1.5), 32767);
        assert_eq!(scale_to_i16(-1.5), -32767);
        assert_eq!(scale_to_i16(0.0), 0);
    }
}
