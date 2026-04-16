//! Automated diagnostics.
//!
//! Both the CLI (`fs25-ffb --diagnose`) and the GUI's "Troubleshooting" tab
//! call into this module. Every check returns a `Check` with a severity and
//! a human-readable explanation + one-line suggested fix.

use crate::config::Config;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub severity: Severity,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Ok,
            detail: detail.into(),
            fix: None,
        }
    }
    pub fn warn(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

pub fn run_cli_report() -> Result<()> {
    let (cfg, cfg_path) = Config::load(None).unwrap_or_else(|_| (Config::default(), PathBuf::from("<none>")));
    println!("fs25-ffb diagnostic report");
    println!("  config: {:?}", cfg_path);
    println!();

    let checks = run_all(&cfg);
    let mut fail_count = 0;
    let mut warn_count = 0;
    for c in &checks {
        let tag = match c.severity {
            Severity::Ok => "  OK  ",
            Severity::Warn => " WARN ",
            Severity::Fail => " FAIL ",
        };
        println!("[{tag}] {}: {}", c.name, c.detail);
        if let Some(fix) = &c.fix {
            println!("        fix: {}", fix);
        }
        if c.severity == Severity::Fail {
            fail_count += 1;
        } else if c.severity == Severity::Warn {
            warn_count += 1;
        }
    }
    println!();
    println!("summary: {} OK, {} warnings, {} failures",
             checks.len() - warn_count - fail_count, warn_count, fail_count);
    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub fn run_all(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(check_kernel_version());
    out.push(check_user_in_input_group());
    out.push(check_dev_input_readable());
    out.push(check_udev_rule_installed());
    out.push(check_device_visible(cfg));
    out.push(check_ffb_capable(cfg));
    out.push(check_steam_compatdata(cfg));
    out.push(check_mod_installed(cfg));
    out.push(check_telemetry_fresh(cfg));
    out.push(check_boxflat_running());
    out.push(check_proton_version(cfg));
    out.push(check_conflicting_processes());
    out.push(check_sdl_hidapi_env());
    out.push(check_cpu_governor());
    out
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_kernel_version() -> Check {
    let Ok(out) = Command::new("uname").arg("-r").output() else {
        return Check::warn("kernel-version", "could not run `uname -r`",
                           "install coreutils");
    };
    let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Moza R5 FFB requires kernel 6.12.24, 6.13.12, 6.14.3 or 6.15+ (via
    // hid-universal-pidff). Generic PIDFF works on 6.x but FFB bits for
    // Moza specifically landed late. Warn otherwise.
    if parse_kver(&ver).unwrap_or((0, 0, 0)) < (6, 12, 0) {
        Check::warn(
            "kernel-version",
            format!("running kernel {} (Moza R5 FFB wants 6.12+)", ver),
            "on CachyOS: `sudo pacman -S linux-cachyos` (6.14+) and reboot",
        )
    } else {
        Check::ok("kernel-version", format!("kernel {} is new enough", ver))
    }
}

fn parse_kver(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let a: u32 = it.next()?.parse().ok()?;
    let b: u32 = it.next()?.parse().ok()?;
    let c: u32 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((a, b, c))
}

fn check_user_in_input_group() -> Check {
    let Ok(out) = Command::new("id").arg("-Gn").output() else {
        return Check::warn("input-group", "could not run `id -Gn`", "install coreutils");
    };
    let s = String::from_utf8_lossy(&out.stdout);
    if s.split_whitespace().any(|g| g == "input") {
        Check::ok("input-group", "user is in `input` group")
    } else {
        Check::warn(
            "input-group",
            "user is NOT in `input` group",
            "`sudo gpasswd -a $USER input` and log out / back in",
        )
    }
}

fn check_dev_input_readable() -> Check {
    let p = Path::new("/dev/input");
    match std::fs::read_dir(p) {
        Ok(_) => Check::ok("dev-input", "/dev/input is readable"),
        Err(e) => Check::fail(
            "dev-input",
            format!("cannot read /dev/input: {}", e),
            "check udev, input group membership, or apparmor/SELinux",
        ),
    }
}

fn check_udev_rule_installed() -> Check {
    let paths = [
        "/etc/udev/rules.d/99-fs25-ffb.rules",
        "/usr/lib/udev/rules.d/99-fs25-ffb.rules",
    ];
    if paths.iter().any(|p| Path::new(p).exists()) {
        Check::ok("udev-rule", "99-fs25-ffb.rules installed")
    } else {
        Check::warn(
            "udev-rule",
            "udev rule not installed (not always required, but recommended)",
            "`sudo install -m0644 packaging/99-fs25-ffb.rules /etc/udev/rules.d/ && \
             sudo udevadm control --reload && sudo udevadm trigger`",
        )
    }
}

fn check_device_visible(cfg: &Config) -> Check {
    match crate::device::find_device(&cfg.device_hint) {
        Ok((path, dev)) => {
            let id = dev.input_id();
            Check::ok(
                "device",
                format!(
                    "found {} at {:?} ({:#06x}:{:#06x})",
                    dev.name().unwrap_or("(unnamed)"),
                    path,
                    id.vendor(),
                    id.product()
                ),
            )
        }
        Err(e) => Check::fail(
            "device",
            format!("no FFB wheel detected: {}", e),
            "plug the wheel in, power it on, run boxflat once to confirm firmware \
             sees it, then re-run the daemon",
        ),
    }
}

fn check_ffb_capable(cfg: &Config) -> Check {
    match crate::device::find_device(&cfg.device_hint) {
        Ok((_, dev)) => {
            let ff = dev.supported_ff();
            match ff {
                None => Check::fail(
                    "ffb-capable",
                    "device does not advertise any FFB bits",
                    "check kernel module (hid-universal-pidff / hid-mozawheel / hid-logitech)",
                ),
                Some(set) if set.iter().count() == 0 => Check::fail(
                    "ffb-capable",
                    "device advertises FF event type but zero effect types",
                    "upgrade kernel to 6.14+ or install hid-universal-pidff dkms",
                ),
                Some(set) => Check::ok(
                    "ffb-capable",
                    format!("supports: {:?}", set.iter().collect::<Vec<_>>()),
                ),
            }
        }
        Err(_) => Check::warn("ffb-capable", "no device found to query", "see `device` above"),
    }
}

fn check_steam_compatdata(cfg: &Config) -> Check {
    let Ok(home) = std::env::var("HOME") else {
        return Check::fail("compatdata", "HOME not set", "run under a real user session");
    };
    let appid = cfg.paths.fs25_steam_appid;
    let candidates = [
        PathBuf::from(&home).join(format!(".local/share/Steam/steamapps/compatdata/{}", appid)),
        PathBuf::from(&home).join(format!(".steam/steam/steamapps/compatdata/{}", appid)),
    ];
    if let Some(p) = candidates.iter().find(|p| p.exists()) {
        Check::ok("compatdata", format!("Proton prefix found at {:?}", p))
    } else {
        Check::warn(
            "compatdata",
            "no Proton prefix for FS25 (AppID 2300320) yet",
            "launch FS25 once in Steam so Proton creates the prefix",
        )
    }
}

fn check_mod_installed(_cfg: &Config) -> Check {
    let Ok(home) = std::env::var("HOME") else {
        return Check::warn("mod-installed", "HOME not set", "");
    };
    let mods = PathBuf::from(&home).join(
        ".local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/\
         Documents/My Games/FarmingSimulator2025/mods",
    );
    let zip = mods.join("FS25_FFBEnhancer.zip");
    let dir = mods.join("FS25_FFBEnhancer");
    if zip.exists() || dir.exists() {
        Check::ok("mod-installed", format!("mod present at {:?}", mods))
    } else {
        Check::warn(
            "mod-installed",
            format!("FS25_FFBEnhancer not found in {:?}", mods),
            "run `./packaging/install.sh` or copy mod/FS25_FFBEnhancer/ into the mods dir",
        )
    }
}

fn check_telemetry_fresh(cfg: &Config) -> Check {
    let path = match crate::daemon::resolve_telemetry_path(cfg) {
        Ok(p) => p,
        Err(e) => {
            return Check::warn("telemetry", format!("cannot resolve path: {}", e), "");
        }
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return Check::warn(
            "telemetry",
            format!("telemetry file does not exist at {:?}", path),
            "start FS25 with the mod active; the file is created on first vehicle",
        );
    };
    let age = meta.modified().ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs_f32())
        .unwrap_or(9999.0);
    if age > 10.0 {
        Check::warn(
            "telemetry",
            format!("telemetry file {} seconds stale", age as u32),
            "FS25 is paused, not in a vehicle, or the Lua mod failed to load",
        )
    } else {
        Check::ok("telemetry", format!("updated {:.1}s ago", age))
    }
}

fn check_boxflat_running() -> Check {
    // boxflat talks to the wheel over the same hidraw. We don't conflict
    // with it for evdev FFB writes, but knowing it's running helps rule
    // out duplicate effect stacking.
    if pidof("boxflat").is_some() {
        Check::ok(
            "boxflat",
            "boxflat is running (OK, does not conflict with evdev FFB)",
        )
    } else {
        Check::ok("boxflat", "boxflat not running (fine)")
    }
}

fn check_proton_version(cfg: &Config) -> Check {
    let Ok(home) = std::env::var("HOME") else {
        return Check::warn("proton", "HOME not set", "");
    };
    let ver_file = PathBuf::from(home).join(format!(
        ".local/share/Steam/steamapps/compatdata/{}/version",
        cfg.paths.fs25_steam_appid
    ));
    match std::fs::read_to_string(&ver_file) {
        Ok(s) => {
            let v = s.trim();
            let rec_ok = v.contains("9.") || v.contains("10.") || v.contains("-GE-");
            if rec_ok {
                Check::ok("proton", format!("Proton {}", v))
            } else {
                Check::warn(
                    "proton",
                    format!("Proton {} - may work but 9.0+/GE-Proton recommended", v),
                    "set FS25's Steam compatibility tool to Proton 9.0 or GE-Proton",
                )
            }
        }
        Err(_) => Check::warn(
            "proton",
            "could not read compatdata/version",
            "launch FS25 once to populate it",
        ),
    }
}

fn check_conflicting_processes() -> Check {
    let mut found = Vec::new();
    for p in ["oversteer", "new-lg4ff-loader", "fanatec-led"] {
        if pidof(p).is_some() {
            found.push(p);
        }
    }
    if found.is_empty() {
        Check::ok("other-drivers", "no conflicting wheel managers running")
    } else {
        Check::warn(
            "other-drivers",
            format!("running: {:?}", found),
            "these only manage config, they don't conflict with FFB writes; \
             still worth disabling autocenter in them",
        )
    }
}

fn check_sdl_hidapi_env() -> Check {
    match std::env::var("SDL_JOYSTICK_HIDAPI") {
        Ok(v) if v == "0" => Check::ok(
            "sdl-hidapi",
            "SDL_JOYSTICK_HIDAPI=0 (recommended for Moza under Proton)",
        ),
        Ok(v) => Check::warn(
            "sdl-hidapi",
            format!("SDL_JOYSTICK_HIDAPI={} (set to 0 for best FFB)", v),
            "in Steam, set launch options: SDL_JOYSTICK_HIDAPI=0 %command%",
        ),
        Err(_) => Check::warn(
            "sdl-hidapi",
            "SDL_JOYSTICK_HIDAPI not set",
            "Steam launch options: SDL_JOYSTICK_HIDAPI=0 %command%",
        ),
    }
}

fn check_cpu_governor() -> Check {
    let path = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor";
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let g = s.trim();
            if g == "performance" || g == "schedutil" {
                Check::ok("cpu-governor", format!("governor = {}", g))
            } else {
                Check::warn(
                    "cpu-governor",
                    format!("governor = {} (may cause FFB jitter)", g),
                    "`sudo cpupower frequency-set -g performance` (CachyOS ships with \
                     schedutil by default which is fine)",
                )
            }
        }
        Err(_) => Check::ok("cpu-governor", "unknown (skipping)"),
    }
}

fn pidof(name: &str) -> Option<u32> {
    let out = Command::new("pidof").arg(name).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().next()?.parse().ok()
}
