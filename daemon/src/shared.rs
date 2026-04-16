//! Types shared between the daemon thread and the GUI.
//!
//! Live state is kept behind a parking_lot::RwLock; both sides read/write
//! with cheap short critical sections. egui polls the state each frame and
//! never blocks the FFB loop.

use crate::effects::EffectOutput;
use crate::telemetry::Telemetry;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

#[derive(Default, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub supports_constant: bool,
    pub supports_spring: bool,
    pub supports_damper: bool,
    pub supports_periodic: bool,
    pub supports_rumble: bool,
    pub ff_effects_max: usize,
}

#[derive(Clone)]
pub struct Heartbeat {
    pub sequence: u32,
    #[allow(dead_code)]
    pub updated: Instant,
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            sequence: 0,
            updated: Instant::now(),
        }
    }
}

/// Single struct the GUI inspects each frame. Bounded ring buffers live
/// here so the GUI's plot widgets can render without allocating.
#[derive(Default)]
pub struct LiveState {
    pub device: Option<DeviceInfo>,
    pub telemetry: Option<Telemetry>,
    pub last_heartbeat: Heartbeat,
    pub last_output: EffectOutput,
    pub telemetry_path: Option<std::path::PathBuf>,
    pub daemon_running: bool,
    pub last_error: Option<String>,
    pub history: History,
}

pub type Shared = Arc<RwLock<LiveState>>;

/// Fixed-size ring buffers for plot widgets.
pub struct History {
    cap: usize,
    pub steering: Vec<f32>,
    pub output_constant: Vec<f32>,
    pub output_spring: Vec<f32>,
    pub latency_ms: Vec<f32>,
}

impl Default for History {
    fn default() -> Self {
        let cap = 600; // 10 seconds at 60Hz
        Self {
            cap,
            steering: Vec::with_capacity(cap),
            output_constant: Vec::with_capacity(cap),
            output_spring: Vec::with_capacity(cap),
            latency_ms: Vec::with_capacity(cap),
        }
    }
}

impl History {
    pub fn push(&mut self, steering: f32, constant: f32, spring: f32, latency_ms: f32) {
        push_ring(&mut self.steering, steering, self.cap);
        push_ring(&mut self.output_constant, constant, self.cap);
        push_ring(&mut self.output_spring, spring, self.cap);
        push_ring(&mut self.latency_ms, latency_ms, self.cap);
    }
}

fn push_ring(v: &mut Vec<f32>, x: f32, cap: usize) {
    if v.len() >= cap {
        v.remove(0);
    }
    v.push(x);
}

pub fn new_shared() -> Shared {
    Arc::new(RwLock::new(LiveState::default()))
}
