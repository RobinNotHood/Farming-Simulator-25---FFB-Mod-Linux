//! Tuning configuration persistence.
//!
//! On-disk format is TOML so users can hand-edit. Config lives at
//! `$XDG_CONFIG_HOME/fs25-ffb/config.toml` by default; the GUI reloads on
//! focus and writes on every slider change (debounced).

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub profile_name: String,
    pub device_hint: DeviceHint,
    pub tuning: TuningConfig,
    pub paths: PathsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            profile_name: "realistic".to_string(),
            device_hint: DeviceHint::default(),
            tuning: TuningConfig::default(),
            paths: PathsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceHint {
    /// If Some, prefer the device at this evdev path.
    pub path: Option<String>,
    /// Otherwise match by vendor/product; Moza R5 = 346e/0002 by default.
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    /// Fallback: match by substring of device name.
    pub name_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningConfig {
    pub enabled: bool,

    pub master_gain: f32,

    // Centering spring (dominant feel on tractors).
    pub spring_gain: f32,
    /// Non-zero baseline even at 0 km/h so the wheel never goes dead. Real
    /// tractor hydraulics have a detent-like resistance at standstill.
    #[serde(default = "default_spring_floor")]
    pub spring_floor: f32,
    pub speed_curve_start_kmh: f32,
    pub speed_curve_max_kmh: f32,
    pub mass_spring_scale: f32,
    pub no_power_steer_boost: f32,

    // Lateral constant force (secondary; gated by speed + steering input).
    pub lateral_gain: f32,

    // Damper (co-dominant with spring; models hydraulic weight).
    pub damper_gain: f32,
    pub implement_damper_scale: f32,

    // Rumble / texture (event-only; no speed-swept sine).
    pub rumble_gain: f32,
    pub idle_rumble: f32,

    // Slope / roll (kept for future HUD; no longer drives spring_center).
    pub slope_gain: f32,
    pub slope_max: f32,

    // Collision one-shot gain (Lua already threshold+cooldown gates impacts).
    pub collision_gain: f32,

    // Update rate cap for evdev writes.
    pub output_hz: u32,
}

fn default_spring_floor() -> f32 {
    0.18
}

impl Default for TuningConfig {
    fn default() -> Self {
        // Tractor-tuned defaults for Moza R5. Damper-dominant, spring
        // always non-zero, constant heavily gated so rough terrain doesn't
        // "rip" the wheel. See effects::EffectEngine::compute for the
        // formulas these values feed.
        Self {
            enabled: true,
            master_gain: 1.0,

            spring_gain: 0.65,
            spring_floor: 0.18,
            speed_curve_start_kmh: 5.0,
            speed_curve_max_kmh: 22.0,
            mass_spring_scale: 0.35,
            no_power_steer_boost: 1.4,

            lateral_gain: 0.18,

            damper_gain: 0.55,
            implement_damper_scale: 0.50,

            rumble_gain: 0.25,
            idle_rumble: 0.05,

            // slope_center coupling is disabled in effects::compute; keep
            // the fields around so existing configs deserialize and the
            // values can be repurposed later.
            slope_gain: 0.0,
            slope_max: 0.35,

            collision_gain: 0.25,

            output_hz: 240,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    /// Absolute path to the telemetry file the Lua mod writes. Leave None
    /// to autodetect under Steam compatdata.
    pub telemetry_file: Option<PathBuf>,
    /// FS25 Steam AppID. Used to compose the compatdata path; override for
    /// Epic or non-Steam installs.
    pub fs25_steam_appid: u32,
    /// Override of the Steam root. Defaults to ~/.local/share/Steam and
    /// ~/.steam/steam.
    pub steam_root: Option<PathBuf>,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            telemetry_file: None,
            fs25_steam_appid: 2_300_320,
            steam_root: None,
        }
    }
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let pd = ProjectDirs::from("org", "fs25-ffb", "fs25-ffb")
            .context("cannot determine config directory")?;
        Ok(pd.config_dir().join("config.toml"))
    }

    pub fn load(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };
        if !path.exists() {
            let cfg = Config::default();
            cfg.save_to(&path)?;
            return Ok((cfg, path));
        }
        let data = std::fs::read_to_string(&path).with_context(|| format!("reading {:?}", path))?;
        let cfg: Config = toml::from_str(&data).with_context(|| format!("parsing {:?}", path))?;
        Ok((cfg, path))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {:?}", parent))?;
        }
        let data = toml::to_string_pretty(self)?;
        std::fs::write(path, data).with_context(|| format!("writing {:?}", path))?;
        Ok(())
    }
}

/// Factory-preset profiles. Applied by the GUI "Profiles" tab. Each variant
/// layers overrides on top of the tractor-tuned `TuningConfig::default()`.
pub fn preset(name: &str) -> Option<TuningConfig> {
    match name {
        "arcade" => Some(TuningConfig {
            master_gain: 1.1,
            spring_gain: 0.45,
            spring_floor: 0.10,
            damper_gain: 0.35,
            lateral_gain: 0.25,
            rumble_gain: 0.35,
            collision_gain: 0.35,
            ..TuningConfig::default()
        }),
        "realistic" => Some(TuningConfig::default()),
        "heavy" => Some(TuningConfig {
            spring_gain: 0.80,
            spring_floor: 0.22,
            mass_spring_scale: 0.50,
            damper_gain: 0.75,
            implement_damper_scale: 0.65,
            lateral_gain: 0.22,
            collision_gain: 0.40,
            ..TuningConfig::default()
        }),
        "quiet" => Some(TuningConfig {
            master_gain: 0.7,
            spring_gain: 0.45,
            spring_floor: 0.12,
            rumble_gain: 0.0,
            idle_rumble: 0.0,
            ..TuningConfig::default()
        }),
        _ => None,
    }
}

pub fn preset_names() -> &'static [&'static str] {
    &["arcade", "realistic", "heavy", "quiet"]
}
