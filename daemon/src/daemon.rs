//! Daemon loop: polls the telemetry file, runs the effect engine, writes
//! to the FFB device. Shares live state with the GUI via `shared::Shared`.

use crate::config::Config;
use crate::effects::EffectEngine;
use crate::ffb::FfbDevice;
use crate::shared::{Heartbeat, Shared};
use crate::telemetry::TelemetryReader;
use anyhow::Result;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Handle returned when we spin up the daemon thread from the GUI.
pub struct DaemonHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub config: Arc<Mutex<Config>>,
    pub shared: Shared,
}

impl DaemonHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the daemon on its own thread. The returned handle is cheap to keep
/// around; the GUI stores it for the process lifetime.
pub fn spawn(config: Arc<Mutex<Config>>, shared: Shared) -> DaemonHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let cfg2 = config.clone();
    let shared2 = shared.clone();

    let join = thread::Builder::new()
        .name("fs25-ffb-daemon".into())
        .spawn(move || {
            if let Err(e) = run_loop(cfg2, shared2.clone(), stop2) {
                tracing::error!("daemon exited with error: {:#}", e);
                shared2.write().last_error = Some(format!("{:#}", e));
                shared2.write().daemon_running = false;
            }
        })
        .expect("spawning daemon thread");

    DaemonHandle {
        stop,
        join: Some(join),
        config,
        shared,
    }
}

/// Blocking entry point used by `fs25-ffb --daemon`.
pub fn run_blocking(config_path: Option<PathBuf>) -> Result<()> {
    let (cfg, path) = Config::load(config_path.as_deref())?;
    tracing::info!("loaded config from {:?}", path);
    let cfg = Arc::new(Mutex::new(cfg));
    let shared = crate::shared::new_shared();
    let stop = Arc::new(AtomicBool::new(false));

    // SIGTERM / Ctrl-C handler via a small manual loop; we avoid ctrlc
    // crate dependency.
    let stop2 = stop.clone();
    ctrlc_like(move || stop2.store(true, Ordering::Relaxed));

    run_loop(cfg, shared, stop)
}

// ---------------------------------------------------------------------------
// main loop
// ---------------------------------------------------------------------------

fn run_loop(cfg: Arc<Mutex<Config>>, shared: Shared, stop: Arc<AtomicBool>) -> Result<()> {
    // Resolve telemetry path (autodetect or from config).
    let telemetry_path = resolve_telemetry_path(&cfg.lock())?;
    tracing::info!("watching telemetry at {:?}", telemetry_path);
    shared.write().telemetry_path = Some(telemetry_path.clone());

    // Open device. We tolerate "no device yet" by retrying every 2s: the
    // user can plug the wheel in after starting the daemon.
    let mut dev = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let hint = cfg.lock().device_hint.clone();
        match FfbDevice::open(&hint) {
            Ok(d) => break d,
            Err(e) => {
                tracing::warn!("no FFB device yet: {:#}", e);
                shared.write().last_error = Some(format!("{:#}", e));
                thread::sleep(Duration::from_secs(2));
            }
        }
    };

    let info = dev.info().clone();
    tracing::info!(
        "opened {} at {} ({:#06x}:{:#06x})",
        info.name,
        info.path,
        info.vendor_id,
        info.product_id
    );
    shared.write().device = Some(info);
    shared.write().daemon_running = true;
    shared.write().last_error = None;

    let mut reader = TelemetryReader::new(&telemetry_path);
    let mut engine = EffectEngine::new();

    let mut last_tick = Instant::now();
    let mut last_heartbeat = Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let now = Instant::now();
        let dt = (now - last_tick).as_secs_f32().min(0.1);
        last_tick = now;

        // Respect configured output rate.
        let output_hz = cfg.lock().tuning.output_hz.clamp(60, 500);
        let period = Duration::from_micros(1_000_000 / output_hz as u64);

        // Read telemetry (non-blocking; may return None).
        let frame = match reader.poll() {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("telemetry read error: {:#}", e);
                None
            }
        };

        if let Some(frame) = frame {
            let out = engine.compute(&frame, &cfg.lock().tuning, dt);
            if let Err(e) = dev.apply(&out) {
                tracing::error!("FFB apply failed: {:#}", e);
                shared.write().last_error = Some(format!("{:#}", e));
                // Try to reopen once before bailing.
                thread::sleep(Duration::from_millis(200));
                let hint = cfg.lock().device_hint.clone();
                match FfbDevice::open(&hint) {
                    Ok(d) => dev = d,
                    Err(e2) => tracing::error!("reopen failed: {:#}", e2),
                }
            }

            let mut s = shared.write();
            let lat_ms = frame
                .received_at
                .map(|t| t.elapsed().as_secs_f32() * 1000.0)
                .unwrap_or(0.0);
            s.history.push(frame.steering_angle, out.constant, out.spring_strength, lat_ms);
            s.telemetry = Some(frame);
            s.last_output = out;
            s.last_heartbeat = Heartbeat {
                sequence: s.last_heartbeat.sequence.wrapping_add(1),
                updated: Instant::now(),
            };
            last_heartbeat = Instant::now();
        } else {
            // No new telemetry; let stale detection decide whether to damp.
            if now.duration_since(last_heartbeat) > Duration::from_millis(500) {
                // Fade everything to zero after half a second of silence so
                // the wheel doesn't lock up with stale spring if FS25 is
                // paused or crashed.
                let _ = dev.apply(&Default::default());
            }
        }

        thread::sleep(period);
    }

    dev.stop_all().ok();
    shared.write().daemon_running = false;
    Ok(())
}

// ---------------------------------------------------------------------------
// Telemetry path resolution
// ---------------------------------------------------------------------------

/// Compose the default telemetry path under Steam's Proton compatdata, or
/// take it from config.paths.telemetry_file.
pub fn resolve_telemetry_path(cfg: &Config) -> Result<PathBuf> {
    if let Some(p) = &cfg.paths.telemetry_file {
        return Ok(p.clone());
    }
    let appid = cfg.paths.fs25_steam_appid;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let candidates = [
        cfg.paths
            .steam_root
            .clone()
            .unwrap_or_else(|| home.join(".local/share/Steam")),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ];
    for root in &candidates {
        let p = root
            .join("steamapps/compatdata")
            .join(appid.to_string())
            .join("pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025")
            .join("modSettings/FS25_FFBEnhancer/telemetry.bin");
        if p.parent().map_or(false, Path::exists) {
            return Ok(p);
        }
    }
    // Fall back to the first candidate even if it doesn't exist yet; the
    // Lua mod will create it when the user launches the game.
    Ok(candidates[0]
        .join("steamapps/compatdata")
        .join(appid.to_string())
        .join("pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025")
        .join("modSettings/FS25_FFBEnhancer/telemetry.bin"))
}

// ---------------------------------------------------------------------------
// Minimal ctrl-c / SIGTERM handler without pulling a dep.
// ---------------------------------------------------------------------------

fn ctrlc_like(f: impl Fn() + Send + 'static) {
    use nix::sys::signal::{self, SigAction, SigHandler, SigSet, Signal};
    static mut CB: Option<Box<dyn Fn() + Send>> = None;
    extern "C" fn on_sig(_: i32) {
        unsafe {
            if let Some(cb) = &CB {
                cb();
            }
        }
    }
    unsafe {
        CB = Some(Box::new(f));
    }
    let action = SigAction::new(
        SigHandler::Handler(on_sig),
        signal::SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe {
        let _ = signal::sigaction(Signal::SIGINT, &action);
        let _ = signal::sigaction(Signal::SIGTERM, &action);
    }
}
