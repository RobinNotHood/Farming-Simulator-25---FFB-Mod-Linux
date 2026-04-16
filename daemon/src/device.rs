//! evdev device enumeration and matching.

use crate::config::DeviceHint;
use crate::shared::DeviceInfo;
use anyhow::{anyhow, Result};
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

/// List every visible input device. `evdev::enumerate` returns an
/// iterator directly (not a `Result`) in 0.12.
fn enumerate_all() -> Vec<(PathBuf, Device)> {
    evdev::enumerate().collect()
}

/// Find and open an FFB-capable device matching the caller's hint, and
/// return both the path and a ready-to-use `DeviceInfo`. The caller is
/// responsible for re-opening the path RW to actually send FFB (we only
/// probe with the evdev crate here).
pub fn open_matching(hint: &DeviceHint) -> Result<(PathBuf, DeviceInfo)> {
    let (path, dev) = find_device(hint)?;
    let info = describe(&path, &dev);
    Ok((path, info))
}

/// Picks the best device based on hints: exact path > vendor/product match >
/// name substring > first device advertising FF_CONSTANT from a known vendor.
pub fn find_device(hint: &DeviceHint) -> Result<(PathBuf, Device)> {
    let candidates = enumerate_all();
    if candidates.is_empty() {
        return Err(anyhow!(
            "no /dev/input/eventX devices visible. Is your user in the 'input' group? \
             Run `fs25-ffb --diagnose` for a checklist."
        ));
    }

    // 1. Exact path match.
    if let Some(p) = &hint.path {
        for (path, _) in &candidates {
            if path.to_string_lossy() == p.as_str() {
                let fresh = Device::open(path)?;
                return Ok((path.clone(), fresh));
            }
        }
    }

    // 2. Vendor/product match.
    if let (Some(v), Some(p)) = (hint.vendor_id, hint.product_id) {
        for (path, dev) in &candidates {
            let id = dev.input_id();
            if id.vendor() == v && id.product() == p {
                let fresh = Device::open(path)?;
                return Ok((path.clone(), fresh));
            }
        }
    }

    // 3. Name substring match.
    if let Some(s) = &hint.name_contains {
        let sl = s.to_lowercase();
        for (path, dev) in &candidates {
            if dev.name().unwrap_or("").to_lowercase().contains(&sl) {
                let fresh = Device::open(path)?;
                return Ok((path.clone(), fresh));
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
        let fresh = Device::open(path)?;
        return Ok((path.clone(), fresh));
    }

    // 5. Any FFB-capable device at all.
    for (path, dev) in &candidates {
        if ffb_capable(dev) {
            let fresh = Device::open(path)?;
            return Ok((path.clone(), fresh));
        }
    }

    Err(anyhow!(
        "no FFB-capable device found. Connect your wheel, ensure /dev/input/eventX \
         is readable (udev rule), and check `fs25-ffb --diagnose`."
    ))
}

pub fn ffb_capable(dev: &Device) -> bool {
    let Some(ff) = dev.supported_ff() else {
        return false;
    };
    ff.contains(FFEffectType::FF_CONSTANT)
        || ff.contains(FFEffectType::FF_PERIODIC)
        || ff.contains(FFEffectType::FF_RUMBLE)
}

pub fn describe(path: &std::path::Path, dev: &Device) -> DeviceInfo {
    let id = dev.input_id();
    let ff = dev.supported_ff();
    let max_effects = dev.max_ff_effects();

    let has = |ty: FFEffectType| -> bool { ff.is_some_and(|s| s.contains(ty)) };

    DeviceInfo {
        name: dev.name().unwrap_or("(unnamed)").to_string(),
        path: path.to_string_lossy().into_owned(),
        vendor_id: id.vendor(),
        product_id: id.product(),
        supports_constant: has(FFEffectType::FF_CONSTANT),
        supports_spring: has(FFEffectType::FF_SPRING),
        supports_damper: has(FFEffectType::FF_DAMPER),
        supports_periodic: has(FFEffectType::FF_PERIODIC),
        supports_rumble: has(FFEffectType::FF_RUMBLE),
        ff_effects_max: max_effects,
    }
}
