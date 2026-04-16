//! Reads the binary telemetry frames written by the Lua mod.
//!
//! The Lua side (scripts/IPCWriter.lua) rewrites a 104-byte record each frame.
//! We read it with an explicit little-endian parser; byteorder keeps us
//! portable even though LE is the only architecture realistically running
//! FS25.

use anyhow::{anyhow, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAGIC: u32 = 0x46464245; // "FFBE" little-endian
// 12-byte header (magic u32 + version u16 + size u16 + sequence u32)
// + 1 timestamp f32 + 21 physics f32s + 2 trailing u32s (flags + hash)
// = 12 + 4 + 84 + 8 = 108 bytes.
pub const FRAME_SIZE: usize = 108;

#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    pub sequence: u32,
    pub timestamp: f32,
    pub steering_angle: f32,
    pub steering_target: f32,
    pub speed_mps: f32,
    pub lateral_accel: f32,
    pub longitudinal_accel: f32,
    pub yaw_rate: f32,
    pub pitch: f32,
    pub roll: f32,
    pub slip_front: f32,
    pub slip_rear: f32,
    pub susp_fl: f32,
    pub susp_fr: f32,
    pub susp_rl: f32,
    pub susp_rr: f32,
    pub ground_hardness: f32,
    pub ground_roughness: f32,
    pub rpm: f32,
    pub engine_load: f32,
    pub attached_mass: f32,
    pub total_mass: f32,
    pub collision: f32,
    pub flags: u32,
    pub vehicle_hash: u32,
    pub received_at: Option<Instant>,
}

impl Telemetry {
    pub fn in_vehicle(&self) -> bool {
        self.flags & 0x01 != 0
    }
    pub fn has_power_steering(&self) -> bool {
        self.flags & 0x04 != 0
    }
    pub fn reversing(&self) -> bool {
        self.flags & 0x08 != 0
    }
    #[allow(dead_code)]
    pub fn airborne(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

/// Parse a raw 104-byte frame. Returns Err on torn writes (bad magic).
#[allow(clippy::field_reassign_with_default)] // 27-field literal is worse
pub fn parse_frame(buf: &[u8]) -> Result<Telemetry> {
    if buf.len() < FRAME_SIZE {
        return Err(anyhow!("frame too short: {} bytes", buf.len()));
    }

    let mut c = Cursor::new(buf);
    let magic = c.read_u32::<LittleEndian>()?;
    if magic != MAGIC {
        return Err(anyhow!(
            "bad magic 0x{:08X} (expected 0x{:08X})",
            magic,
            MAGIC
        ));
    }
    let version = c.read_u16::<LittleEndian>()?;
    let size = c.read_u16::<LittleEndian>()?;
    if version != 1 {
        return Err(anyhow!("unsupported frame version {}", version));
    }
    if size as usize != FRAME_SIZE {
        // Don't reject hard; future versions will carry extra trailing bytes.
        tracing::debug!("frame size {} differs from expected {}", size, FRAME_SIZE);
    }

    let mut t = Telemetry::default();
    t.sequence = c.read_u32::<LittleEndian>()?;
    t.timestamp = c.read_f32::<LittleEndian>()?;
    t.steering_angle = c.read_f32::<LittleEndian>()?;
    t.steering_target = c.read_f32::<LittleEndian>()?;
    t.speed_mps = c.read_f32::<LittleEndian>()?;
    t.lateral_accel = c.read_f32::<LittleEndian>()?;
    t.longitudinal_accel = c.read_f32::<LittleEndian>()?;
    t.yaw_rate = c.read_f32::<LittleEndian>()?;
    t.pitch = c.read_f32::<LittleEndian>()?;
    t.roll = c.read_f32::<LittleEndian>()?;
    t.slip_front = c.read_f32::<LittleEndian>()?;
    t.slip_rear = c.read_f32::<LittleEndian>()?;
    t.susp_fl = c.read_f32::<LittleEndian>()?;
    t.susp_fr = c.read_f32::<LittleEndian>()?;
    t.susp_rl = c.read_f32::<LittleEndian>()?;
    t.susp_rr = c.read_f32::<LittleEndian>()?;
    t.ground_hardness = c.read_f32::<LittleEndian>()?;
    t.ground_roughness = c.read_f32::<LittleEndian>()?;
    t.rpm = c.read_f32::<LittleEndian>()?;
    t.engine_load = c.read_f32::<LittleEndian>()?;
    t.attached_mass = c.read_f32::<LittleEndian>()?;
    t.total_mass = c.read_f32::<LittleEndian>()?;
    t.collision = c.read_f32::<LittleEndian>()?;
    t.flags = c.read_u32::<LittleEndian>()?;
    t.vehicle_hash = c.read_u32::<LittleEndian>()?;
    t.received_at = Some(Instant::now());
    Ok(t)
}

/// Poll-based reader. We prefer this over inotify because the Lua side keeps
/// the file open and rewrites in-place, which inotify sometimes fails to
/// notice on certain filesystems (notably some older ext4 setups seen in
/// CachyOS's `@home` subvolume when copy-on-write quirks apply).
pub struct TelemetryReader {
    path: PathBuf,
    last_seq: u32,
    buf: [u8; FRAME_SIZE],
}

impl TelemetryReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_seq: 0,
            buf: [0u8; FRAME_SIZE],
        }
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read current frame. Returns Ok(None) if the sequence has not advanced
    /// since last call (no new data).
    pub fn poll(&mut self) -> Result<Option<Telemetry>> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("opening telemetry file"),
        };

        let n = file.read(&mut self.buf)?;
        if n < FRAME_SIZE {
            // torn write - the Lua side probably flushed mid-write; try
            // again next tick.
            return Ok(None);
        }
        match parse_frame(&self.buf) {
            Ok(t) => {
                if t.sequence == self.last_seq {
                    Ok(None)
                } else {
                    self.last_seq = t.sequence;
                    Ok(Some(t))
                }
            }
            Err(e) => {
                tracing::trace!("ignoring bad frame: {}", e);
                Ok(None)
            }
        }
    }
}

/// Stale if no new sequence for > 500ms.
#[allow(dead_code)]
pub fn is_stale(last_update: Option<Instant>) -> bool {
    match last_update {
        Some(t) => t.elapsed() > Duration::from_millis(500),
        None => true,
    }
}
