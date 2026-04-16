//! fs25-ffb: force-feedback enhancer for Farming Simulator 25 on Linux.
//!
//! Single binary that does both the background FFB engine and the
//! configuration GUI. Architecture:
//!
//!   +-------------+   telemetry.bin    +--------+   /dev/input/eventX
//!   | FS25 + Lua  | -----------------> | daemon | ------------------> wheel
//!   +-------------+                    +--------+
//!                                          |
//!                                      config.toml
//!                                          |
//!                                      +--------+
//!                                      |  GUI   |
//!                                      +--------+
//!
//! The daemon thread is always spawned. In GUI mode (default) the main thread
//! runs eframe; in --daemon mode the main thread just parks on the daemon.

use anyhow::Result;
use clap_lite::{parse_args, Mode};
use tracing_subscriber::EnvFilter;

mod app;
mod clap_lite;
mod config;
mod daemon;
mod device;
mod effects;
mod ffb;
mod logging;
mod shared;
mod telemetry;
mod troubleshoot;

fn main() -> Result<()> {
    // RUST_LOG=info,fs25_ffb=debug gives a good default when debugging; the
    // GUI also displays live logs via a ring buffer in `logging`.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .with_writer(logging::RingTee::new())
        .init();

    let args = parse_args();
    match args.mode {
        Mode::Daemon => daemon::run_blocking(args.config_path),
        Mode::Gui => app::run(args.config_path),
        Mode::Test(effect) => ffb::run_manual_test(effect),
        Mode::Diagnose => troubleshoot::run_cli_report(),
        Mode::Version => {
            println!("fs25-ffb {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
