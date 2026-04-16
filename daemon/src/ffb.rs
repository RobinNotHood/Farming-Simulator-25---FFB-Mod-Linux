//! Low-level FFB driver. Uploads effects via the Linux kernel `evdev`
//! FF_UPLOAD ioctl and starts/stops them with EV_FF write events.
//!
//! We use the `input-linux` crate for the ioctl bindings because `evdev`
//! 0.12 does not expose effect upload. The `evdev` crate is still used for
//! enumeration (nicer API for device metadata).
//!
//! Kernel reference: Documentation/input/ff.rst.
//!
//! Effects we allocate up-front and re-use:
//!   * effect_constant : FF_CONSTANT  -- lateral force pull
//!   * effect_spring   : FF_SPRING    -- centering torque
//!   * effect_damper   : FF_DAMPER    -- steering-rate damping
//!   * effect_rumble   : FF_PERIODIC  -- surface texture (sine wave)
//!   * effect_kick     : FF_CONSTANT  -- short collision one-shot
//!
//! On devices that lack FF_SPRING we synthesise a spring from FF_CONSTANT
//! + telemetry steering position; same for FF_DAMPER. This keeps behaviour
//! consistent across wheel vendors.

use crate::clap_lite::TestEffect;
use crate::config::DeviceHint;
use crate::effects::EffectOutput;
use crate::shared::DeviceInfo;
use anyhow::{anyhow, Context, Result};
use input_linux::{
    sys as raw, AbsoluteAxis, EventKind, ForceFeedbackKind,
};
use nix::ioctl_write_ptr;
use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsRawFd, RawFd};
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
    id_kick: Option<i16>,
    // last values written (to debounce unchanged uploads)
    last_constant: i16,
    last_spring: u16,
    last_damper: u16,
    last_rumble_mag: u16,
    last_rumble_period: u16,
    last_kick: i16,
}

impl FfbDevice {
    /// Open a device by hint and pre-upload the five effect slots. Returns
    /// an error if the kernel rejects uploads (e.g. wrong driver, no FF bits).
    pub fn open(hint: &DeviceHint) -> Result<Self> {
        let (path, dev) = crate::device::find_device(hint)?;
        let info = crate::device::describe(&path, &dev);
        drop(dev); // re-open RW

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {:?} for RW (need udev rule or input group)", path))?;

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
            last_kick: 0,
        };

        // Set master gain to 100%; user tunes via our own master_gain slider.
        // Do this *before* uploading so effect strengths reflect the raw data.
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

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Push the current effect target. Called at output_hz.
    pub fn apply(&mut self, out: &EffectOutput) -> Result<()> {
        // FF_CONSTANT
        if let Some(id) = self.id_constant {
            let lvl = scale_to_i16(out.constant);
            if (lvl - self.last_constant).abs() > 150 {
                self.update_constant(id, lvl, 0xFFFF)?;
                self.start(id)?;
                self.last_constant = lvl;
            }
        }
        // FF_SPRING
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
        // FF_DAMPER
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
        // Periodic rumble
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
            self.id_kick,
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
    // Raw ioctl wrappers. The full struct ff_effect layout is in
    // linux/input.h; we rely on input-linux's sys bindings.
    // ---------------------------------------------------------------

    fn upload_constant(&self, level: i16, duration_ms: u16) -> Result<i16> {
        let mut eff = self.new_effect(raw::FF_CONSTANT as u16, duration_ms);
        unsafe {
            eff.u.constant.level = level;
            eff.u.constant.envelope.attack_length = 0;
            eff.u.constant.envelope.attack_level = 0;
            eff.u.constant.envelope.fade_length = 0;
            eff.u.constant.envelope.fade_level = 0;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(eff.id)
    }

    fn update_constant(&self, id: i16, level: i16, duration_ms: u16) -> Result<()> {
        let mut eff = self.new_effect(raw::FF_CONSTANT as u16, duration_ms);
        eff.id = id;
        unsafe {
            eff.u.constant.level = level;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(())
    }

    fn upload_spring(&self, strength: u16, center: i16) -> Result<i16> {
        let mut eff = self.new_effect(raw::FF_SPRING as u16, 0);
        unsafe {
            for axis in 0..2 {
                eff.u.condition[axis].right_saturation = strength;
                eff.u.condition[axis].left_saturation = strength;
                eff.u.condition[axis].right_coeff = strength as i16;
                eff.u.condition[axis].left_coeff = strength as i16;
                eff.u.condition[axis].deadband = 0;
                eff.u.condition[axis].center = center;
            }
        }
        self.ioctl_upload(&mut eff)?;
        Ok(eff.id)
    }

    fn update_spring(&self, id: i16, strength: u16, center: i16) -> Result<()> {
        let mut eff = self.new_effect(raw::FF_SPRING as u16, 0);
        eff.id = id;
        unsafe {
            for axis in 0..2 {
                eff.u.condition[axis].right_saturation = strength;
                eff.u.condition[axis].left_saturation = strength;
                let c = (strength as i32).min(0x7FFF) as i16;
                eff.u.condition[axis].right_coeff = c;
                eff.u.condition[axis].left_coeff = c;
                eff.u.condition[axis].center = center;
            }
        }
        self.ioctl_upload(&mut eff)?;
        Ok(())
    }

    fn upload_damper(&self, strength: u16) -> Result<i16> {
        let mut eff = self.new_effect(raw::FF_DAMPER as u16, 0);
        unsafe {
            for axis in 0..2 {
                eff.u.condition[axis].right_saturation = strength;
                eff.u.condition[axis].left_saturation = strength;
                eff.u.condition[axis].right_coeff = strength as i16;
                eff.u.condition[axis].left_coeff = strength as i16;
            }
        }
        self.ioctl_upload(&mut eff)?;
        Ok(eff.id)
    }

    fn update_damper(&self, id: i16, strength: u16) -> Result<()> {
        let mut eff = self.new_effect(raw::FF_DAMPER as u16, 0);
        eff.id = id;
        unsafe {
            for axis in 0..2 {
                eff.u.condition[axis].right_saturation = strength;
                eff.u.condition[axis].left_saturation = strength;
                eff.u.condition[axis].right_coeff = strength as i16;
                eff.u.condition[axis].left_coeff = strength as i16;
            }
        }
        self.ioctl_upload(&mut eff)?;
        Ok(())
    }

    fn upload_periodic(&self, magnitude: u16, period_ms: u16) -> Result<i16> {
        let mut eff = self.new_effect(raw::FF_PERIODIC as u16, 0);
        unsafe {
            eff.u.periodic.waveform = raw::FF_SINE as u16;
            eff.u.periodic.period = period_ms;
            eff.u.periodic.magnitude = magnitude as i16;
            eff.u.periodic.offset = 0;
            eff.u.periodic.phase = 0;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(eff.id)
    }

    fn update_periodic(&self, id: i16, magnitude: u16, period_ms: u16) -> Result<()> {
        let mut eff = self.new_effect(raw::FF_PERIODIC as u16, 0);
        eff.id = id;
        unsafe {
            eff.u.periodic.waveform = raw::FF_SINE as u16;
            eff.u.periodic.period = period_ms;
            eff.u.periodic.magnitude = magnitude as i16;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(())
    }

    fn upload_rumble(&self, strong: u16) -> Result<i16> {
        let mut eff = self.new_effect(raw::FF_RUMBLE as u16, 0);
        unsafe {
            eff.u.rumble.strong_magnitude = strong;
            eff.u.rumble.weak_magnitude = strong / 2;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(eff.id)
    }

    fn update_rumble(&self, id: i16, strong: u16) -> Result<()> {
        let mut eff = self.new_effect(raw::FF_RUMBLE as u16, 0);
        eff.id = id;
        unsafe {
            eff.u.rumble.strong_magnitude = strong;
            eff.u.rumble.weak_magnitude = strong / 2;
        }
        self.ioctl_upload(&mut eff)?;
        Ok(())
    }

    fn new_effect(&self, kind: u16, duration_ms: u16) -> raw::ff_effect {
        // SAFETY: ff_effect is POD; zeroed state is valid on all arches.
        let mut eff: raw::ff_effect = unsafe { std::mem::zeroed() };
        eff.type_ = kind;
        eff.id = -1;
        eff.replay.length = duration_ms;
        eff.replay.delay = 0;
        eff.direction = 0x4000; // 90 deg, magnitude in +X
        eff
    }

    fn ioctl_upload(&self, eff: &mut raw::ff_effect) -> Result<()> {
        ioctl_write_ptr!(evioc_sendff, b'E', 0x80, raw::ff_effect);
        let fd: RawFd = self.file.as_raw_fd();
        let rc = unsafe { evioc_sendff(fd, eff as *mut _) };
        rc.map(|_| ()).with_context(|| {
            format!(
                "EVIOCSFF failed on {:?}. Try `fs25-ffb --diagnose`.",
                self.path
            )
        })
    }

    fn start(&self, id: i16) -> Result<()> {
        self.write_ev(raw::EV_FF as u16, id as u16, 1)
    }

    fn stop(&self, id: i16) -> Result<()> {
        self.write_ev(raw::EV_FF as u16, id as u16, 0)
    }

    fn set_gain(&self, gain_0_ffff: u16) -> Result<()> {
        self.write_ev(raw::EV_FF as u16, raw::FF_GAIN as u16, gain_0_ffff as i32)
    }

    fn set_autocenter(&self, strength_0_ffff: u16) -> Result<()> {
        self.write_ev(
            raw::EV_FF as u16,
            raw::FF_AUTOCENTER as u16,
            strength_0_ffff as i32,
        )
    }

    fn write_ev(&self, type_: u16, code: u16, value: i32) -> Result<()> {
        use nix::libc::{timeval, write};
        let ev = raw::input_event {
            time: timeval { tv_sec: 0, tv_usec: 0 },
            type_,
            code,
            value,
        };
        let fd = self.file.as_raw_fd();
        let n = unsafe {
            write(
                fd,
                &ev as *const _ as *const _,
                std::mem::size_of::<raw::input_event>(),
            )
        };
        if n < 0 {
            return Err(anyhow!(
                "write(EV_FF) failed (errno {})",
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

/// Scale -1..1 float to i16 for FF_CONSTANT.level (-32767..32767).
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

// Silence unused-import warnings when building on hosts without all kernel
// bits exposed via input-linux. AbsoluteAxis/EventKind/ForceFeedbackKind
// are referenced indirectly through constants above.
#[allow(dead_code)]
fn _unused_imports(_a: AbsoluteAxis, _e: EventKind, _f: ForceFeedbackKind) {}
