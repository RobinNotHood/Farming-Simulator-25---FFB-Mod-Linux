//! evdev device enumeration and matching.

use crate::config::DeviceHint;
use crate::shared::DeviceInfo;
use anyhow::{anyhow, Context, Result};
use evdev::{Device, FFEffectType};
use std::path::PathBuf;

pub const MOZA_VENDOR_ID: u16 = 0x346e;
/// Known Moza wheelbase products. R5 = 0x0002, R9 = 0x0005, R12 = 0x0006,
/// R16/R21 = 0x0000/0x0001 depending on firmware. We match on vendor and
/// require FF_CONSTANT capability rather than hardcoding every PID.
pub const KNOWN_VENDORS: &[u16] = &[
    MOZA_VENDOR_ID, // Moza
    0x046d,         // Logitech (G29/G920/G923)
    0x044f,         // Thrustmaster (T150/T248/T300/TX/TS-XW)
    0x0eb7,         // Fanatec
    0x11ff,         // Simagic
    0x16d0,         // Simucube
    0x2341,         // Arduino-based DIY wheels
    0x06a3,         // Saitek
];

pub fn enumerate() -> Vec<(PathBuf, Device)> {
    let mut out = Vec::new();
    if let Ok(paths) = evdev::enumerate() {
        for (path, dev) in paths {
            out.push((path, dev));
        }
    }
    out
}

/// Picks the best device based on hints: exact path > vendor/product match >
/// name substring > first device advertising FF_CONSTANT from a known vendor.
pub fn find_device(hint: &DeviceHint) -> Result<(PathBuf, Device)> {
    let candidates = enumerate();
    if candidates.is_empty() {
        return Err(anyhow!(
            "no /dev/input/eventX devices visible. Is your user in the 'input' group? \
             Run `fs25-ffb --diagnose` for a checklist."
        ));
    }

    // 1. Exact path.
    if let Some(p) = &hint.path {
        for (path, dev) in &candidates {
            if path.to_string_lossy() == p.as_str() {
                return Ok((path.clone(), open_fresh(path)?));
            }
            drop(dev); // silence unused
        }
    }

    // 2. Vendor/product.
    if let (Some(v), Some(p)) = (hint.vendor_id, hint.product_id) {
        for (path, dev) in &candidates {
            let id = dev.input_id();
            if id.vendor() == v && id.product() == p {
                return Ok((path.clone(), open_fresh(path)?));
            }
        }
    }

    // 3. Name substring.
    if let Some(s) = &hint.name_contains {
        let sl = s.to_lowercase();
        for (path, dev) in &candidates {
            if dev.name().unwrap_or("").to_lowercase().contains(&sl) {
                return Ok((path.clone(), open_fresh(path)?));
            }
        }
    }

    // 4. Any FFB-capable wheel from a known vendor.
    for (path, dev) in &candidates {
        let id = dev.input_id();
        if !KNOWN_VENDORS.contains(&id.vendor()) {
            continue;
        }
        if !ffb_capable(dev) {
            continue;
        }
        return Ok((path.clone(), open_fresh(path)?));
    }

    // 5. Any FFB-capable device at all.
    for (path, dev) in &candidates {
        if ffb_capable(dev) {
            return Ok((path.clone(), open_fresh(path)?));
        }
    }

    Err(anyhow!(
        "no FFB-capable device found. Connect your wheel, ensure /dev/input/eventX \
         is readable (udev rule), and check `fs25-ffb --diagnose`."
    ))
}

fn open_fresh(path: &PathBuf) -> Result<Device> {
    Device::open(path).with_context(|| format!("opening {:?}", path))
}

pub fn ffb_capable(dev: &Device) -> bool {
    let Some(ff) = dev.supported_ff() else {
        return false;
    };
    ff.contains(FFEffectType::FF_CONSTANT)
        || ff.contains(FFEffectType::FF_PERIODIC)
        || ff.contains(FFEffectType::FF_RUMBLE)
}

pub fn describe(path: &PathBuf, dev: &Device) -> DeviceInfo {
    let id = dev.input_id();
    let ff = dev.supported_ff();
    let max_effects = dev.max_ff_effects().unwrap_or(0);
    DeviceInfo {
        name: dev.name().unwrap_or("(unnamed)").to_string(),
        path: path.to_string_lossy().into_owned(),
        vendor_id: id.vendor(),
        product_id: id.product(),
        supports_constant: ff.map_or(false, |f| f.contains(FFEffectType::FF_CONSTANT)),
        supports_spring: ff.map_or(false, |f| f.contains(FFEffectType::FF_SPRING)),
        supports_damper: ff.map_or(false, |f| f.contains(FFEffectType::FF_DAMPER)),
        supports_periodic: ff.map_or(false, |f| f.contains(FFEffectType::FF_PERIODIC)),
        supports_rumble: ff.map_or(false, |f| f.contains(FFEffectType::FF_RUMBLE)),
        ff_effects_max: max_effects,
    }
}
