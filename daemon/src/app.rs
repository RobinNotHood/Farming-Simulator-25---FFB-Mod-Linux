//! egui GUI. Tabs: Status, Tuning, Profiles, Troubleshooting, Logs, About.
//!
//! The GUI spawns the daemon on startup (unless it's already running as a
//! systemd service) and shares state with it via `Shared`. Every slider
//! mutates the `TuningConfig` behind a Mutex; the daemon picks up the new
//! values on the next tick without restart.

use crate::config::{preset, preset_names, Config};
use crate::daemon::{self, DaemonHandle};
use crate::shared::{new_shared, Shared};
use crate::troubleshoot::{self, Check, Severity};
use anyhow::Result;
use eframe::egui;
use egui::{Color32, RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

pub fn run(config_path: Option<PathBuf>) -> Result<()> {
    let (cfg, cfg_path) = Config::load(config_path.as_deref())?;
    let cfg = Arc::new(Mutex::new(cfg));
    let shared = new_shared();

    let handle = daemon::spawn(cfg.clone(), shared.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([720.0, 520.0])
            .with_title("FS25 FFB Enhancer"),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "fs25-ffb",
        options,
        Box::new(move |_cc| {
            Box::new(App {
                cfg,
                cfg_path,
                shared,
                handle,
                tab: Tab::Status,
                last_checks: Vec::new(),
                dirty: false,
            })
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {}", e))
}

// ---------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Status,
    Tuning,
    Profiles,
    Troubleshoot,
    Logs,
    About,
}

struct App {
    cfg: Arc<Mutex<Config>>,
    cfg_path: PathBuf,
    shared: Shared,
    #[allow(dead_code)] // kept for lifetime
    handle: DaemonHandle,
    tab: Tab,
    last_checks: Vec<Check>,
    dirty: bool,
}

impl App {
    fn save_if_dirty(&mut self) {
        if self.dirty {
            let cfg = self.cfg.lock().clone();
            if let Err(e) = cfg.save_to(&self.cfg_path) {
                tracing::error!("save config failed: {:#}", e);
            } else {
                self.dirty = false;
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("FS25 FFB Enhancer");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Status, "Status");
                ui.selectable_value(&mut self.tab, Tab::Tuning, "Tuning");
                ui.selectable_value(&mut self.tab, Tab::Profiles, "Profiles");
                ui.selectable_value(&mut self.tab, Tab::Troubleshoot, "Troubleshoot");
                ui.selectable_value(&mut self.tab, Tab::Logs, "Logs");
                ui.selectable_value(&mut self.tab, Tab::About, "About");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let s = self.shared.read();
                    let (dot, txt) = if s.daemon_running {
                        (Color32::from_rgb(0x30, 0xc0, 0x60), "daemon running")
                    } else {
                        (Color32::from_rgb(0xc0, 0x50, 0x30), "daemon down")
                    };
                    ui.colored_label(dot, "\u{25CF}");
                    ui.label(txt);
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Status => self.ui_status(ui),
            Tab::Tuning => self.ui_tuning(ui),
            Tab::Profiles => self.ui_profiles(ui),
            Tab::Troubleshoot => self.ui_troubleshoot(ui),
            Tab::Logs => self.ui_logs(ui),
            Tab::About => self.ui_about(ui),
        });

        self.save_if_dirty();
    }
}

// ---------------------------------------------------------------------------
// Status tab
// ---------------------------------------------------------------------------

impl App {
    fn ui_status(&mut self, ui: &mut Ui) {
        let s = self.shared.read();
        ui.columns(2, |cols| {
            // Left: device + telemetry text
            cols[0].group(|ui| {
                ui.heading("Device");
                match &s.device {
                    Some(d) => {
                        ui.label(format!("Name:    {}", d.name));
                        ui.label(format!("Path:    {}", d.path));
                        ui.label(format!("VID/PID: {:04x}:{:04x}", d.vendor_id, d.product_id));
                        ui.label(format!(
                            "FFB:     constant={} spring={} damper={} periodic={} rumble={}",
                            yn(d.supports_constant),
                            yn(d.supports_spring),
                            yn(d.supports_damper),
                            yn(d.supports_periodic),
                            yn(d.supports_rumble),
                        ));
                        ui.label(format!("Slots:   {}", d.ff_effects_max));
                    }
                    None => {
                        ui.colored_label(Color32::YELLOW, "no device");
                    }
                }
                if let Some(err) = &s.last_error {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(220, 80, 60), err);
                }
            });

            cols[1].group(|ui| {
                ui.heading("Telemetry");
                match &s.telemetry {
                    Some(t) => {
                        ui.label(format!("Seq:            {}", t.sequence));
                        ui.label(format!("In vehicle:     {}", yn(t.in_vehicle())));
                        ui.label(format!("Speed:          {:.1} km/h", t.speed_mps * 3.6));
                        ui.label(format!("Steering:       {:+.2}", t.steering_angle));
                        ui.label(format!(
                            "Lateral accel:  {:+.2} m/s\u{00b2}",
                            t.lateral_accel
                        ));
                        ui.label(format!(
                            "Slip F / R:     {:.2} / {:.2}",
                            t.slip_front, t.slip_rear
                        ));
                        ui.label(format!(
                            "Pitch / Roll:   {:+.2} / {:+.2} rad",
                            t.pitch, t.roll
                        ));
                        ui.label(format!("Ground hard:    {:.2}", t.ground_hardness));
                        ui.label(format!("Ground rough:   {:.2}", t.ground_roughness));
                        ui.label(format!("RPM (norm):     {:.2}", t.rpm));
                        ui.label(format!("Mass total:     {:.0} kg", t.total_mass));
                        ui.label(format!("Mass implement: {:.0} kg", t.attached_mass));
                    }
                    None => {
                        ui.colored_label(Color32::YELLOW, "no telemetry yet");
                    }
                }
                ui.separator();
                if let Some(p) = &s.telemetry_path {
                    ui.label(format!("file: {}", p.display()));
                }
            });
        });

        ui.separator();
        ui.heading("Live output");

        let hist = &s.history;
        let steering: PlotPoints = hist
            .steering
            .iter()
            .enumerate()
            .map(|(i, v)| [i as f64, *v as f64])
            .collect();
        let constant: PlotPoints = hist
            .output_constant
            .iter()
            .enumerate()
            .map(|(i, v)| [i as f64, *v as f64])
            .collect();
        let spring: PlotPoints = hist
            .output_spring
            .iter()
            .enumerate()
            .map(|(i, v)| [i as f64, *v as f64])
            .collect();

        Plot::new("ffb-plot")
            .height(220.0)
            .legend(egui_plot::Legend::default())
            .include_y(-1.1)
            .include_y(1.1)
            .show(ui, |p| {
                p.line(Line::new(steering).name("steering"));
                p.line(Line::new(constant).name("constant"));
                p.line(Line::new(spring).name("spring"));
            });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!(
                "Torque estimate: {:.1} Nm",
                s.last_output.torque_estimate_nm
            ));
            ui.separator();
            ui.label(format!(
                "Rumble: {:.0}% @ {} ms",
                s.last_output.rumble_magnitude * 100.0,
                s.last_output.rumble_period_ms
            ));
        });
    }
}

// ---------------------------------------------------------------------------
// Tuning tab
// ---------------------------------------------------------------------------

impl App {
    fn ui_tuning(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let enabled = {
                let mut c = self.cfg.lock();
                let was = c.tuning.enabled;
                ui.checkbox(&mut c.tuning.enabled, "FFB enabled");
                c.tuning.enabled != was || c.tuning.enabled
            };
            if enabled {
                self.dirty = true;
            }
            ui.separator();
            if ui.button("Reset to defaults").clicked() {
                self.cfg.lock().tuning = crate::config::TuningConfig::default();
                self.dirty = true;
            }
        });

        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut c = self.cfg.lock();
            let mut any = false;

            ui.heading("Global");
            any |= slider(ui, "Master gain", &mut c.tuning.master_gain, 0.0, 2.0);
            any |= slider(
                ui,
                "Output rate (Hz)",
                &mut (c.tuning.output_hz as f32),
                60.0,
                500.0,
            );
            // Cast back - ugly but keeps slider signature uniform.

            ui.separator();
            ui.heading("Centering spring");
            any |= slider(ui, "Spring gain", &mut c.tuning.spring_gain, 0.0, 1.5);
            any |= slider(
                ui,
                "Speed ramp start (km/h)",
                &mut c.tuning.speed_curve_start_kmh,
                0.0,
                30.0,
            );
            any |= slider(
                ui,
                "Speed ramp max (km/h)",
                &mut c.tuning.speed_curve_max_kmh,
                10.0,
                80.0,
            );
            any |= slider(
                ui,
                "Mass -> spring scale",
                &mut c.tuning.mass_spring_scale,
                0.0,
                1.0,
            );
            any |= slider(
                ui,
                "Boost w/o power steering",
                &mut c.tuning.no_power_steer_boost,
                1.0,
                3.0,
            );

            ui.separator();
            ui.heading("Lateral force");
            any |= slider(ui, "Lateral gain", &mut c.tuning.lateral_gain, 0.0, 1.0);

            ui.separator();
            ui.heading("Damper");
            any |= slider(ui, "Damper gain", &mut c.tuning.damper_gain, 0.0, 1.0);
            any |= slider(
                ui,
                "Implement mass -> damper",
                &mut c.tuning.implement_damper_scale,
                0.0,
                1.0,
            );

            ui.separator();
            ui.heading("Surface rumble");
            any |= slider(ui, "Rumble gain", &mut c.tuning.rumble_gain, 0.0, 1.5);
            any |= slider(ui, "Idle vibration", &mut c.tuning.idle_rumble, 0.0, 0.4);

            ui.separator();
            ui.heading("Slope / roll");
            any |= slider(ui, "Slope gain", &mut c.tuning.slope_gain, 0.0, 1.5);
            any |= slider(ui, "Slope max offset", &mut c.tuning.slope_max, 0.0, 0.8);

            ui.separator();
            ui.heading("Collision kick");
            any |= slider(ui, "Collision gain", &mut c.tuning.collision_gain, 0.0, 1.5);

            if any {
                self.dirty = true;
            }
        });
    }
}

fn slider(ui: &mut Ui, label: &str, value: &mut f32, lo: f32, hi: f32) -> bool {
    let r = ui.add(egui::Slider::new(value, lo..=hi).text(label));
    r.changed()
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

impl App {
    fn ui_profiles(&mut self, ui: &mut Ui) {
        ui.label(
            "Presets overwrite the current tuning. Use them as a starting point and \
             fine-tune in the Tuning tab.",
        );
        ui.separator();

        for name in preset_names() {
            ui.horizontal(|ui| {
                if ui.button(format!("Apply '{}'", name)).clicked() {
                    if let Some(p) = preset(name) {
                        let mut c = self.cfg.lock();
                        c.tuning = p;
                        c.profile_name = name.to_string();
                        self.dirty = true;
                    }
                }
                ui.label(preset_description(name));
            });
        }

        ui.separator();
        ui.heading("Test effects");
        ui.horizontal(|ui| {
            if ui.button("Constant").clicked() {
                std::thread::spawn(|| {
                    let _ = crate::ffb::run_manual_test(crate::clap_lite::TestEffect::Constant);
                });
            }
            if ui.button("Spring").clicked() {
                std::thread::spawn(|| {
                    let _ = crate::ffb::run_manual_test(crate::clap_lite::TestEffect::Spring);
                });
            }
            if ui.button("Damper").clicked() {
                std::thread::spawn(|| {
                    let _ = crate::ffb::run_manual_test(crate::clap_lite::TestEffect::Damper);
                });
            }
            if ui.button("Rumble").clicked() {
                std::thread::spawn(|| {
                    let _ = crate::ffb::run_manual_test(crate::clap_lite::TestEffect::Rumble);
                });
            }
            if ui.button("Sine sweep").clicked() {
                std::thread::spawn(|| {
                    let _ = crate::ffb::run_manual_test(crate::clap_lite::TestEffect::Sine);
                });
            }
        });
        ui.label(
            "Tests briefly take exclusive control of the wheel. Stop driving in-game \
             before running them.",
        );
    }
}

fn preset_description(name: &str) -> &'static str {
    match name {
        "arcade" => "light, twitch-friendly; good for keyboard + wheel switchers",
        "realistic" => "default; tuned for Moza R5 on CachyOS",
        "heavy" => "more weight, more damping; great with big implements",
        "quiet" => "minimal rumble and low master gain",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Troubleshoot
// ---------------------------------------------------------------------------

impl App {
    fn ui_troubleshoot(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("Re-run all checks").clicked() {
                let cfg = self.cfg.lock().clone();
                self.last_checks = troubleshoot::run_all(&cfg);
            }
            if ui.button("Copy report to clipboard").clicked() {
                let cfg = self.cfg.lock().clone();
                let checks = troubleshoot::run_all(&cfg);
                let text = format_report(&checks);
                ui.output_mut(|o| o.copied_text = text);
            }
        });
        if self.last_checks.is_empty() {
            let cfg = self.cfg.lock().clone();
            self.last_checks = troubleshoot::run_all(&cfg);
        }

        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for c in &self.last_checks {
                let (colour, tag) = match c.severity {
                    Severity::Ok => (Color32::from_rgb(0x30, 0xc0, 0x60), "OK  "),
                    Severity::Warn => (Color32::from_rgb(0xe0, 0xb0, 0x30), "WARN"),
                    Severity::Fail => (Color32::from_rgb(0xd0, 0x40, 0x40), "FAIL"),
                };
                ui.horizontal(|ui| {
                    ui.colored_label(colour, RichText::new(tag).monospace().strong());
                    ui.label(RichText::new(&c.name).strong());
                    ui.label(&c.detail);
                });
                if let Some(fix) = &c.fix {
                    ui.indent(c.name.as_str(), |ui| {
                        ui.small(RichText::new(format!("fix: {}", fix)).italics());
                    });
                }
            }
            ui.separator();
            ui.heading("Guided fixes");
            ui.label("See docs/TROUBLESHOOTING.md for the full walkthrough. Highlights:");
            ui.monospace(
                "\
# Add user to input group (required unless udev rule applied)
sudo gpasswd -a $USER input

# Install the udev rule
sudo install -m0644 packaging/99-fs25-ffb.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger

# Enable the systemd user service
systemctl --user enable --now fs25-ffb.service

# Re-install the Lua mod
./packaging/install.sh --mod-only

# Steam launch options for FS25
SDL_JOYSTICK_HIDAPI=0 PROTON_LOG=1 %command%
",
            );
        });
    }
}

fn format_report(checks: &[Check]) -> String {
    let mut s = String::new();
    for c in checks {
        let tag = match c.severity {
            Severity::Ok => "OK  ",
            Severity::Warn => "WARN",
            Severity::Fail => "FAIL",
        };
        s.push_str(&format!("[{}] {}: {}\n", tag, c.name, c.detail));
        if let Some(fix) = &c.fix {
            s.push_str(&format!("     fix: {}\n", fix));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Logs / About
// ---------------------------------------------------------------------------

impl App {
    fn ui_logs(&mut self, ui: &mut Ui) {
        let lines = crate::logging::snapshot();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &lines {
                    ui.monospace(line);
                }
            });
    }

    fn ui_about(&mut self, ui: &mut Ui) {
        ui.heading("FS25 FFB Enhancer");
        ui.label(format!("version {}", env!("CARGO_PKG_VERSION")));
        ui.separator();
        ui.label(
            "Force-feedback enhancer for Farming Simulator 25 on Linux. \
             The in-game Lua mod captures vehicle physics; this daemon turns \
             them into rich FFB on your wheel via /dev/input.",
        );
        ui.label("Designed and tested on CachyOS with Moza R5.");
        ui.separator();
        ui.label("License: MIT");
        ui.label("Source: see README");
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
