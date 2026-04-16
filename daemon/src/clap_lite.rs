//! Tiny argument parser. We avoid pulling clap in for a five-option CLI.

use std::path::PathBuf;

#[derive(Debug)]
pub enum Mode {
    Gui,
    Daemon,
    /// Manual FFB test: emit one effect of the given kind and exit.
    Test(TestEffect),
    /// Print a troubleshooting report and exit.
    Diagnose,
    Version,
}

#[derive(Debug, Clone, Copy)]
pub enum TestEffect {
    Constant,
    Spring,
    Damper,
    Rumble,
    Sine,
}

pub struct Args {
    pub mode: Mode,
    pub config_path: Option<PathBuf>,
}

pub fn parse_args() -> Args {
    let mut mode = Mode::Gui;
    let mut config_path: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--daemon" | "-d" => mode = Mode::Daemon,
            "--gui" => mode = Mode::Gui,
            "--diagnose" => mode = Mode::Diagnose,
            "--version" | "-V" => mode = Mode::Version,
            "--test" => {
                let kind = it.next().unwrap_or_else(|| "constant".to_string());
                let eff = match kind.as_str() {
                    "constant" => TestEffect::Constant,
                    "spring" => TestEffect::Spring,
                    "damper" => TestEffect::Damper,
                    "rumble" => TestEffect::Rumble,
                    "sine" => TestEffect::Sine,
                    other => {
                        eprintln!("unknown test effect '{}'", other);
                        std::process::exit(2);
                    }
                };
                mode = Mode::Test(eff);
            }
            "--config" | "-c" => {
                config_path = it.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {}", other);
                print_help();
                std::process::exit(2);
            }
        }
    }

    Args { mode, config_path }
}

fn print_help() {
    println!(
        "fs25-ffb {} - Force feedback enhancer for FS25 on Linux\n\
\n\
USAGE:\n\
    fs25-ffb [OPTIONS]\n\
\n\
OPTIONS:\n\
    --gui              Launch configuration GUI (default)\n\
    --daemon, -d       Run headless (for systemd user service)\n\
    --diagnose         Print troubleshooting report to stdout and exit\n\
    --test <kind>      Play one test effect and exit\n\
                       kinds: constant, spring, damper, rumble, sine\n\
    --config, -c PATH  Use alternate config file\n\
    --version, -V      Print version\n\
    --help, -h         This message\n",
        env!("CARGO_PKG_VERSION")
    );
}
